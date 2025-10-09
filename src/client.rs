use crate::server::{ServerError, ServerState};
use crate::wire::{self, ClientCommand, PublishCommand, ServerMessage};
use scru128::scru128_string;
use silent::{BoxedConnection, SocketAddr};
use std::sync::Arc;
use thiserror::Error;
use tokio::io::BufReader;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

const CHANNEL_SIZE: usize = 128;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Wire(#[from] wire::WireError),
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error("客户端写通道已关闭")]
    ChannelClosed,
    #[error("写循环 Join 失败: {0}")]
    Join(#[from] tokio::task::JoinError),
}

impl From<mpsc::error::SendError<ServerMessage>> for ClientError {
    fn from(_: mpsc::error::SendError<ServerMessage>) -> Self {
        ClientError::ChannelClosed
    }
}

pub async fn handle_connection(
    state: Arc<ServerState>,
    stream: BoxedConnection,
    peer: SocketAddr,
) -> Result<(), ClientError> {
    let client_id = scru128_string();
    info!(peer = ?peer, client_id = %client_id, "新客户端连接");
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let (sender, receiver) = mpsc::channel(CHANNEL_SIZE);
    state
        .register_client(client_id.clone(), sender.clone())
        .await;

    let write_task = spawn_writer(writer, receiver);

    sender.send(state.info()).await?;

    loop {
        match wire::read_command(&mut reader).await {
            Ok(Some(command)) => {
                if let Err(err) = handle_command(&state, &client_id, &sender, command).await {
                    warn!(client_id = %client_id, error = %err, "处理命令失败");
                    let _ = sender.send(ServerMessage::Err(err.to_string())).await;
                    return Err(err);
                }
            }
            Ok(None) => {
                info!(client_id = %client_id, "客户端断开连接");
                break;
            }
            Err(err) => {
                warn!(client_id = %client_id, error = %err, "读取命令出错");
                let _ = sender.send(ServerMessage::Err(err.to_string())).await;
                return Err(err.into());
            }
        }
    }

    cleanup(state, client_id, sender, write_task).await
}

async fn handle_command(
    state: &Arc<ServerState>,
    client_id: &str,
    sender: &mpsc::Sender<ServerMessage>,
    command: ClientCommand,
) -> Result<(), ClientError> {
    match command {
        ClientCommand::Connect(payload) => {
            info!(client_id = %client_id, ?payload, "客户端 CONNECT");
            let verbose = payload
                .get("verbose")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if verbose {
                sender.send(ServerMessage::Ok).await?;
            }
        }
        ClientCommand::Ping => {
            sender.send(ServerMessage::Pong).await?;
        }
        ClientCommand::Pong => {
            // no-op
        }
        ClientCommand::Sub(sub) => {
            state
                .add_subscription(client_id, &sub.sid, &sub.subject, sub.queue_group.clone())
                .await?;
        }
        ClientCommand::Unsub(unsub) => {
            if let Some(max) = unsub.max_msgs {
                state
                    .update_subscription_limit(client_id, &unsub.sid, Some(max))
                    .await?;
            } else {
                state.remove_subscription(client_id, &unsub.sid).await?;
            }
        }
        ClientCommand::Pub(publish) => {
            handle_publish(state, publish).await?;
        }
    }
    Ok(())
}

async fn handle_publish(
    state: &Arc<ServerState>,
    publish: PublishCommand,
) -> Result<(), ClientError> {
    let PublishCommand {
        subject,
        reply_to,
        payload,
    } = publish;
    state.publish(&subject, reply_to, payload).await?;
    Ok(())
}

async fn cleanup(
    state: Arc<ServerState>,
    client_id: String,
    sender: mpsc::Sender<ServerMessage>,
    write_task: JoinHandle<Result<(), wire::WireError>>,
) -> Result<(), ClientError> {
    state.unregister_client(&client_id).await;
    drop(sender);
    match write_task.await? {
        Ok(()) => Ok(()),
        Err(err) => {
            error!(client_id = %client_id, error = %err, "写入循环出错");
            Err(err.into())
        }
    }
}

fn spawn_writer(
    mut writer: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
    mut receiver: mpsc::Receiver<ServerMessage>,
) -> JoinHandle<Result<(), wire::WireError>> {
    tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            wire::write_message(&mut writer, message).await?;
        }
        Ok(())
    })
}
