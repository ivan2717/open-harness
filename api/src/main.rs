use std::{net::SocketAddr, path::PathBuf};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let address: SocketAddr = std::env::var("OPENHARNESS_BIND")
        .unwrap_or_else(|_| "127.0.0.1:7788".into())
        .parse()?;
    if !address.ip().is_loopback() {
        return Err("OPENHARNESS_BIND must be a loopback address".into());
    }
    let directory = std::env::var_os("OPENHARNESS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("open-harness-desktop"));
    let state = api::AppState::open(directory, std::env::var("OPENHARNESS_API_TOKEN").ok()).await?;
    let listener = TcpListener::bind(address).await?;
    let address = listener.local_addr()?;

    println!(
        "{}",
        serde_json::json!({"event":"ready", "address":address.to_string()})
    );

    axum::serve(listener, api::app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("api=debug"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

async fn shutdown_signal() {
    let interrupt = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to listen for interrupt");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to listen for termination");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = interrupt => {}, _ = terminate => {} }
    tracing::info!("shutdown signal received");
}
