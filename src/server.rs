use crate::subjects::{Subject, SubjectError};
use crate::wire::{OutboundMessage, ServerMessage, info_message};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::{debug, error};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubscriptionKey {
    pub client_id: String,
    pub sid: String,
}

impl SubscriptionKey {
    pub fn new(client_id: impl Into<String>, sid: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            sid: sid.into(),
        }
    }
}

#[derive(Clone)]
pub struct ClientHandle {
    pub sender: mpsc::Sender<ServerMessage>,
}

#[derive(Clone)]
pub struct SubscriptionEntry {
    pub subject: Subject,
    pub queue_group: Option<String>,
    pub max_msgs: Option<u64>,
    pub delivered: u64,
    pub sender: mpsc::Sender<ServerMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct QueueKey {
    subject: String,
    queue: String,
}

impl QueueKey {
    fn new(subject: &Subject, queue: &str) -> Self {
        Self {
            subject: subject.raw().to_string(),
            queue: queue.to_string(),
        }
    }

    fn subject(&self) -> &str {
        &self.subject
    }

    fn queue(&self) -> &str {
        &self.queue
    }
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("客户端不存在: {0}")]
    ClientNotFound(String),
    #[error("订阅不存在: {0:?}")]
    SubscriptionNotFound(SubscriptionKey),
    #[error("科目解析失败: {0}")]
    InvalidSubject(#[from] SubjectError),
    #[error("消息负载超过限制: {size} > {max}")]
    PayloadTooLarge { size: usize, max: usize },
}

#[derive(Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub max_payload: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 4222,
            max_payload: 1024 * 1024,
        }
    }
}

pub struct ServerState {
    config: ServerConfig,
    server_id: String,
    clients: RwLock<HashMap<String, ClientHandle>>,
    subscriptions: RwLock<HashMap<SubscriptionKey, SubscriptionEntry>>,
    queue_counters: Mutex<HashMap<QueueKey, usize>>,
}

impl ServerState {
    pub fn new(server_id: String, config: ServerConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            server_id,
            clients: RwLock::new(HashMap::new()),
            subscriptions: RwLock::new(HashMap::new()),
            queue_counters: Mutex::new(HashMap::new()),
        })
    }

    pub fn info(&self) -> ServerMessage {
        ServerMessage::Info(info_message(
            self.server_id.clone(),
            self.config.port,
            self.config.max_payload,
        ))
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub async fn register_client(&self, client_id: String, sender: mpsc::Sender<ServerMessage>) {
        let mut clients = self.clients.write().await;
        clients.insert(client_id, ClientHandle { sender });
    }

    pub async fn unregister_client(&self, client_id: &str) {
        {
            let mut clients = self.clients.write().await;
            clients.remove(client_id);
        }
        let mut removed_queues = Vec::new();
        {
            let mut subscriptions = self.subscriptions.write().await;
            subscriptions.retain(|key, entry| {
                if key.client_id == client_id {
                    if let Some(queue) = entry.queue_group.as_ref() {
                        removed_queues.push(QueueKey::new(&entry.subject, queue));
                    }
                    false
                } else {
                    true
                }
            });
        }
        for queue_key in removed_queues {
            self.prune_queue_key(&queue_key).await;
        }
    }

    pub async fn add_subscription(
        &self,
        client_id: &str,
        sid: &str,
        subject: &str,
        queue_group: Option<String>,
    ) -> Result<(), ServerError> {
        let subject = Subject::parse(subject)?;
        let sender = {
            let clients = self.clients.read().await;
            clients
                .get(client_id)
                .cloned()
                .ok_or_else(|| ServerError::ClientNotFound(client_id.to_string()))?
                .sender
        };
        let key = SubscriptionKey::new(client_id, sid);
        let mut subscriptions = self.subscriptions.write().await;
        subscriptions.insert(
            key,
            SubscriptionEntry {
                subject,
                queue_group,
                max_msgs: None,
                delivered: 0,
                sender,
            },
        );
        Ok(())
    }

    pub async fn update_subscription_limit(
        &self,
        client_id: &str,
        sid: &str,
        max_msgs: Option<u64>,
    ) -> Result<(), ServerError> {
        let key = SubscriptionKey::new(client_id.to_string(), sid.to_string());
        let mut subscriptions = self.subscriptions.write().await;
        if let Some(sub) = subscriptions.get_mut(&key) {
            sub.max_msgs = max_msgs;
            Ok(())
        } else {
            Err(ServerError::SubscriptionNotFound(key))
        }
    }

    pub async fn remove_subscription(&self, client_id: &str, sid: &str) -> Result<(), ServerError> {
        let key = SubscriptionKey::new(client_id.to_string(), sid.to_string());
        let removed = {
            let mut subscriptions = self.subscriptions.write().await;
            subscriptions.remove(&key)
        };
        if let Some(entry) = removed {
            let queue_key = entry
                .queue_group
                .as_ref()
                .map(|queue| QueueKey::new(&entry.subject, queue));
            if let Some(queue_key) = queue_key {
                self.prune_queue_key(&queue_key).await;
            }
            Ok(())
        } else {
            Err(ServerError::SubscriptionNotFound(key))
        }
    }

    pub async fn publish(
        &self,
        subject: &str,
        reply_to: Option<String>,
        payload: Vec<u8>,
    ) -> Result<(), ServerError> {
        if payload.len() > self.config.max_payload {
            return Err(ServerError::PayloadTooLarge {
                size: payload.len(),
                max: self.config.max_payload,
            });
        }
        let mut matches = Vec::new();
        {
            let subscriptions = self.subscriptions.read().await;
            for (key, entry) in subscriptions.iter() {
                if entry.subject.matches(subject) {
                    matches.push((key.clone(), entry.clone()));
                }
            }
        }
        if matches.is_empty() {
            return Ok(());
        }
        let reply_ref = reply_to.as_ref();
        let mut direct = Vec::new();
        let mut grouped: HashMap<QueueKey, Vec<(SubscriptionKey, SubscriptionEntry)>> =
            HashMap::new();
        for (key, entry) in matches {
            if let Some(queue) = entry.queue_group.clone() {
                let queue_key = QueueKey::new(&entry.subject, &queue);
                grouped.entry(queue_key).or_default().push((key, entry));
            } else {
                direct.push((key, entry));
            }
        }
        for (key, entry) in direct {
            self.dispatch_subscription(key, entry, subject, reply_ref, payload.as_slice())
                .await;
        }
        for (queue_key, entries) in grouped {
            self.dispatch_queue_group(queue_key, entries, subject, reply_ref, payload.as_slice())
                .await;
        }
        Ok(())
    }

    async fn dispatch_subscription(
        &self,
        key: SubscriptionKey,
        entry: SubscriptionEntry,
        subject: &str,
        reply_to: Option<&String>,
        payload: &[u8],
    ) -> bool {
        let message = OutboundMessage {
            subject: subject.to_string(),
            sid: key.sid.clone(),
            reply_to: reply_to.cloned(),
            payload: payload.to_vec(),
        };
        match entry.sender.send(ServerMessage::Msg(message)).await {
            Ok(()) => {
                let mut queue_key = None;
                {
                    let mut subscriptions = self.subscriptions.write().await;
                    if let Some(target) = subscriptions.get_mut(&key) {
                        target.delivered = target.delivered.saturating_add(1);
                        if let Some(max) = target.max_msgs
                            && target.delivered >= max
                        {
                            debug!(
                                client_id = %key.client_id,
                                sid = %key.sid,
                                "达到最大消息数，自动取消订阅"
                            );
                            queue_key = target
                                .queue_group
                                .as_ref()
                                .map(|queue| QueueKey::new(&target.subject, queue));
                            subscriptions.remove(&key);
                        }
                    }
                }
                if let Some(queue_key) = queue_key {
                    self.prune_queue_key(&queue_key).await;
                }
                true
            }
            Err(_) => {
                error!(client_id = %key.client_id, sid = %key.sid, "发送消息失败，移除客户端");
                self.unregister_client(&key.client_id).await;
                false
            }
        }
    }

    async fn dispatch_queue_group(
        &self,
        queue_key: QueueKey,
        mut entries: Vec<(SubscriptionKey, SubscriptionEntry)>,
        subject: &str,
        reply_to: Option<&String>,
        payload: &[u8],
    ) {
        if entries.is_empty() {
            return;
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let len = entries.len();
        let start_index = {
            let mut counters = self.queue_counters.lock().await;
            let cursor = counters.entry(queue_key.clone()).or_insert(0);
            *cursor % len
        };
        let mut selected_index = None;
        for attempt in 0..len {
            let idx = (start_index + attempt) % len;
            let (key, entry) = entries[idx].clone();
            if self
                .dispatch_subscription(key, entry, subject, reply_to, payload)
                .await
            {
                selected_index = Some(idx);
                break;
            }
        }
        {
            let mut counters = self.queue_counters.lock().await;
            if let Some(success_idx) = selected_index {
                counters.insert(queue_key.clone(), (success_idx + 1) % len);
            } else {
                counters.remove(&queue_key);
            }
        }
        self.prune_queue_key(&queue_key).await;
    }

    async fn prune_queue_key(&self, queue_key: &QueueKey) {
        let subscriptions = self.subscriptions.read().await;
        let exists = subscriptions.values().any(|entry| {
            entry.queue_group.as_deref() == Some(queue_key.queue())
                && entry.subject.raw() == queue_key.subject()
        });
        drop(subscriptions);
        if !exists {
            let mut counters = self.queue_counters.lock().await;
            counters.remove(queue_key);
        }
    }
}
