mod client;
mod server;
mod subjects;
mod wire;

use crate::client::handle_connection;
use crate::server::{ServerConfig, ServerState};
use scru128::scru128_string;
use silent::{BoxError, Server as SilentServer};
use std::net::SocketAddr;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() {
    init_tracing();

    let host = std::env::var("NATS_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port: u16 = std::env::var("NATS_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4222);
    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .expect("监听地址格式错误");

    let server_id = scru128_string();
    let config = ServerConfig {
        port,
        ..ServerConfig::default()
    };
    let state = ServerState::new(server_id, config);
    let server = SilentServer::new().bind(addr);

    info!(address = %addr, server_id = %state.server_id(), "Silent NATS 服务器启动");

    server
        .serve(move |stream, peer| {
            let state = state.clone();
            async move {
                match handle_connection(state, stream, peer).await {
                    Ok(()) => Ok(()),
                    Err(err) => {
                        tracing::warn!(error = %err, "客户端连接异常结束");
                        Err::<(), BoxError>(Box::new(err))
                    }
                }
            }
        })
        .await;
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
