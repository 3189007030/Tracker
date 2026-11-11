//! Offline migration/publication check. Use an isolated database copy only.
use anyhow::{Context, Result, ensure};
use std::path::PathBuf;

fn main() -> Result<()> {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("pass an isolated directory containing a consistent private.sqlite3 copy")?,
    );
    ensure!(
        path.join("private.sqlite3").is_file(),
        "private copy missing"
    );
    let mut store = observatory::store::Store::open(&path)?;
    store.publish(1800, &[])?;
    println!("{{\"offline_store_validation\":true,\"network_used\":false}}");
    Ok(())
}
