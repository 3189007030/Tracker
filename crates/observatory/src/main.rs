use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/var/lib/tracker-v1")]
    data_dir: PathBuf,
    #[arg(long, default_value = "/etc/tracker-v1/config.toml")]
    config: PathBuf,
    #[arg(long)]
    once: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let config = observatory::Config::load(args.data_dir, &args.config)?;
    observatory::run(config, args.once).await
}
