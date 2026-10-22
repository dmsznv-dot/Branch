use std::{net::SocketAddr, sync::Arc};

use network::{SignalHub, signaling_router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let address: SocketAddr = std::env::var("BRANCH_SIGNAL_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8787".to_owned())
        .parse()?;
    if !address.ip().is_loopback() {
        return Err("signaling service must bind to loopback behind a TLS proxy".into());
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, signaling_router(Arc::new(SignalHub::new()))).await?;
    Ok(())
}
