use chrono::{Local, NaiveDateTime};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Error)]
pub enum WireError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("协议帧不是有效的 UTF-8 数据")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("JSON 解析失败: {0}")]
    Json(#[from] serde_json::Error),
    #[error("未知或不支持的命令: {0}")]
    UnsupportedCommand(String),
    #[error("命令参数缺失: {0}")]
    MissingArgument(&'static str),
    #[error("命令参数无效: {0}")]
    InvalidArgument(String),
}

#[derive(Debug, Clone)]
pub enum ClientCommand {
    Connect(Value),
    Ping,
    Pong,
    Sub(SubscribeCommand),
    Unsub(UnsubscribeCommand),
    Pub(PublishCommand),
}

#[derive(Debug, Clone)]
pub struct SubscribeCommand {
    pub subject: String,
    pub queue_group: Option<String>,
    pub sid: String,
}

#[derive(Debug, Clone)]
pub struct UnsubscribeCommand {
    pub sid: String,
    pub max_msgs: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct PublishCommand {
    pub subject: String,
    pub reply_to: Option<String>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InfoMessage {
    pub server_id: String,
    pub server_name: String,
    pub version: String,
    pub proto: u8,
    pub host: String,
    pub port: u16,
    pub max_payload: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jetstream: Option<bool>,
    pub now: NaiveDateTime,
}

#[derive(Debug, Clone)]
pub enum ServerMessage {
    Info(InfoMessage),
    Ok,
    Pong,
    Msg(OutboundMessage),
    Err(String),
}

#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub subject: String,
    pub sid: String,
    pub reply_to: Option<String>,
    pub payload: Vec<u8>,
}

pub async fn read_command<R>(reader: &mut R) -> Result<Option<ClientCommand>, WireError>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let mut buffer = Vec::new();
        let bytes = reader.read_until(b'\n', &mut buffer).await?;
        if bytes == 0 {
            return Ok(None);
        }
        if buffer.ends_with(b"\n") {
            buffer.pop();
            if buffer.ends_with(b"\r") {
                buffer.pop();
            }
        }
        let line = String::from_utf8(buffer)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (verb, rest) = split_command(trimmed);
        let result = match verb {
            "CONNECT" => parse_connect(rest),
            "PING" => Ok(Some(ClientCommand::Ping)),
            "PONG" => Ok(Some(ClientCommand::Pong)),
            "SUB" => parse_subscribe(rest),
            "UNSUB" => parse_unsubscribe(rest),
            "PUB" => parse_publish(rest, reader).await,
            other => Err(WireError::UnsupportedCommand(other.to_string())),
        };
        return result;
    }
}

pub async fn write_message<W>(writer: &mut W, message: ServerMessage) -> Result<(), WireError>
where
    W: AsyncWrite + Unpin,
{
    match message {
        ServerMessage::Info(info) => {
            let json = serde_json::to_string(&info)?;
            writer.write_all(b"INFO ").await?;
            writer.write_all(json.as_bytes()).await?;
            writer.write_all(b"\r\n").await?;
        }
        ServerMessage::Ok => {
            writer.write_all(b"+OK\r\n").await?;
        }
        ServerMessage::Pong => {
            writer.write_all(b"PONG\r\n").await?;
        }
        ServerMessage::Err(reason) => {
            writer.write_all(b"-ERR '").await?;
            writer.write_all(reason.as_bytes()).await?;
            writer.write_all(b"'\r\n").await?;
        }
        ServerMessage::Msg(message) => {
            writer.write_all(b"MSG ").await?;
            writer.write_all(message.subject.as_bytes()).await?;
            writer.write_all(b" ").await?;
            writer.write_all(message.sid.as_bytes()).await?;
            if let Some(reply) = message.reply_to {
                writer.write_all(b" ").await?;
                writer.write_all(reply.as_bytes()).await?;
            }
            writer.write_all(b" ").await?;
            writer
                .write_all(message.payload.len().to_string().as_bytes())
                .await?;
            writer.write_all(b"\r\n").await?;
            writer.write_all(&message.payload).await?;
            writer.write_all(b"\r\n").await?;
        }
    }
    writer.flush().await?;
    Ok(())
}

fn split_command(line: &str) -> (&str, &str) {
    for (idx, ch) in line.char_indices() {
        if ch.is_whitespace() {
            let verb = &line[..idx];
            let rest = line[idx..].trim_start();
            return (verb, rest);
        }
    }
    (line, "")
}

fn parse_connect(rest: &str) -> Result<Option<ClientCommand>, WireError> {
    if rest.is_empty() {
        return Err(WireError::MissingArgument("CONNECT"));
    }
    let json = rest.trim_start();
    let value: Value = serde_json::from_str(json)?;
    Ok(Some(ClientCommand::Connect(value)))
}

fn parse_subscribe(rest: &str) -> Result<Option<ClientCommand>, WireError> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(WireError::MissingArgument("SUB"));
    }
    let (subject, queue_group, sid) = match parts.len() {
        2 => (parts[0], None, parts[1]),
        3 => (parts[0], Some(parts[1]), parts[2]),
        _ => return Err(WireError::InvalidArgument(rest.to_string())),
    };
    Ok(Some(ClientCommand::Sub(SubscribeCommand {
        subject: subject.to_string(),
        queue_group: queue_group.map(|v| v.to_string()),
        sid: sid.to_string(),
    })))
}

fn parse_unsubscribe(rest: &str) -> Result<Option<ClientCommand>, WireError> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.is_empty() {
        return Err(WireError::MissingArgument("UNSUB"));
    }
    if parts.len() > 2 {
        return Err(WireError::InvalidArgument(rest.to_string()));
    }
    let max_msgs = if parts.len() == 2 {
        Some(parts[1].parse::<u64>().map_err(|_| {
            WireError::InvalidArgument(format!("UNSUB 无法解析最大消息数: {}", parts[1]))
        })?)
    } else {
        None
    };
    Ok(Some(ClientCommand::Unsub(UnsubscribeCommand {
        sid: parts[0].to_string(),
        max_msgs,
    })))
}

async fn parse_publish<R>(rest: &str, reader: &mut R) -> Result<Option<ClientCommand>, WireError>
where
    R: AsyncBufRead + Unpin,
{
    let mut parts = rest.split_whitespace();
    let subject = parts
        .next()
        .ok_or(WireError::MissingArgument("PUB subject"))?;
    let first = parts
        .next()
        .ok_or(WireError::MissingArgument("PUB length"))?;
    let (reply_to, len_value) = if let Some(next) = parts.next() {
        (Some(first), next)
    } else {
        (None, first)
    };
    let length: usize = len_value
        .parse()
        .map_err(|_| WireError::InvalidArgument(format!("PUB 无法解析消息长度: {}", len_value)))?;
    if parts.next().is_some() {
        return Err(WireError::InvalidArgument(rest.to_string()));
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload).await?;
    let mut trailer = [0u8; 2];
    reader.read_exact(&mut trailer).await?;
    if trailer != *b"\r\n" {
        return Err(WireError::InvalidArgument(
            "PUB 负载未以 CRLF 结尾".to_string(),
        ));
    }
    Ok(Some(ClientCommand::Pub(PublishCommand {
        subject: subject.to_string(),
        reply_to: reply_to.map(|v| v.to_string()),
        payload,
    })))
}

pub fn info_message(server_id: String, port: u16, max_payload: usize) -> InfoMessage {
    InfoMessage {
        server_id,
        server_name: "silent-nats".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        proto: 1,
        host: "0.0.0.0".to_string(),
        port,
        max_payload,
        headers: Some(false),
        auth_required: Some(false),
        tls_required: Some(false),
        jetstream: Some(false),
        now: Local::now().naive_local(),
    }
}
