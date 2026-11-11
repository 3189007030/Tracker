//! Private collection and public-only publication for the tracker observatory.
pub mod infrastructure;
pub mod network;
pub mod peer;
pub mod pool;
pub mod samples;
pub mod stats;
pub mod store;

use anyhow::Result;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub interval_seconds: u64,
    pub sources_file: PathBuf,
    pub own_hosts: Vec<String>,
    pub service_aliases: std::collections::BTreeMap<String, String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            interval_seconds: 1800,
            sources_file: PathBuf::from("/etc/tracker-v1/sources.toml"),
            own_hosts: Vec::new(),
            service_aliases: Default::default(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub settings: Settings,
}
impl Config {
    pub fn load(data_dir: PathBuf, path: &Path) -> Result<Self> {
        let root: toml::Value = toml::from_str(&std::fs::read_to_string(path)?)?;
        let settings: Settings = root
            .get("observatory")
            .cloned()
            .unwrap_or_else(|| toml::Value::Table(Default::default()))
            .try_into()?;
        anyhow::ensure!(
            settings.interval_seconds >= 1800,
            "health interval cannot be less than 1800 seconds"
        );
        Ok(Self { data_dir, settings })
    }
}

pub async fn run(config: Config, once: bool) -> Result<()> {
    std::fs::create_dir_all(&config.data_dir)?;
    let mut db = store::Store::open(&config.data_dir)?;
    db.publish(config.settings.interval_seconds, &config.settings.own_hosts)?;
    loop {
        if network::cycle(&config, &mut db).await.is_err() {
            // Request errors can embed complete tracker queries. Detailed
            // outcomes belong in the typed private/public event projections.
            tracing::warn!("observation cycle deferred");
        }
        db.publish(config.settings.interval_seconds, &config.settings.own_hosts)?;
        if once {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}
