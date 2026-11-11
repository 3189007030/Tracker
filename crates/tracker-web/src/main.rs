use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;

#[derive(Parser, Debug)]
#[command(version, about = "Tracker Observatory public website")]
struct Args {
    #[arg(long)]
    listen: Option<SocketAddr>,
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
}

#[derive(Default, Deserialize)]
struct FileConfig {
    data_dir: Option<PathBuf>,
    web: Option<WebConfig>,
}

#[derive(Default, Deserialize)]
struct WebConfig {
    listen: Option<SocketAddr>,
    data_dir: Option<PathBuf>,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .without_time()
        .init();
    let args = Args::parse();
    let file = match args.config {
        Some(path) => {
            let text = std::fs::read_to_string(path).context("cannot read web configuration")?;
            toml::from_str::<FileConfig>(&text).context("invalid web configuration")?
        }
        None => FileConfig::default(),
    };
    let web = file.web.unwrap_or_default();
    let listen = args
        .listen
        .or(web.listen)
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8080)));
    let data_dir = args
        .data_dir
        .or(web.data_dir)
        .or(file.data_dir)
        .unwrap_or_else(|| PathBuf::from("/var/lib/tracker-v1"));
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .context("cannot bind web listener")?;
    tracing::info!(%listen, "public web listening");
    axum::serve(listener, tracker_web::app(data_dir))
        .with_graceful_shutdown(shutdown())
        .await
        .context("web server stopped unexpectedly")?;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
