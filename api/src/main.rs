use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let listener = TcpListener::bind("0.0.0.0:7788").await?;
    let address = listener.local_addr()?;

    tracing::info!("open-harness api listening on {}", address);

    axum::serve(listener, api::app())
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("api=debug,tower_http=debug"));

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    match tokio::signal::ctrl_c().await {
        Ok(()) => {
            tracing::info!("shutdown signal received");
        }
        Err(error) => {
            tracing::error!(%error, "failed to listen for shutdown signal");
        }
    }
}
