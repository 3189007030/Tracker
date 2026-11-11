//! Bounded loopback-only native metrics. Missing data stays missing.
use std::time::Duration;
use tracker_model::StatsPoint;

fn parse(body: &[u8], now: i64) -> Option<StatsPoint> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let instance = value.get("instance_id")?.as_str()?;
    if instance.is_empty()
        || instance.len() > 128
        || !instance
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let mut point = StatsPoint {
        at: now,
        instance: instance.to_owned(),
        ..Default::default()
    };
    if let Some(engine) = value.get("engine") {
        for name in ["peers", "swarms"] {
            if let Some(v) = engine.get(name).and_then(|x| x.as_u64()) {
                point.gauges.insert(name.into(), v as f64);
            }
        }
    }
    if let Some(v) = value.get("rss_bytes").and_then(|x| x.as_u64()) {
        point.gauges.insert("rss_bytes".into(), v as f64);
    }
    if let Some(requests) = value.get("requests") {
        for name in [
            "accepted_announces",
            "business_failures",
            "http_announces",
            "udp_announces",
            "accepted_ipv4",
            "accepted_ipv6",
        ] {
            if let Some(v) = requests.get(name).and_then(|x| x.as_u64()) {
                point.counters.insert(name.into(), v);
            }
        }
    }
    Some(point)
}

pub async fn collect(store: &crate::store::Store) -> anyhow::Result<()> {
    let now = tracker_model::unix_now();
    if store
        .value("native_stats_last")?
        .is_some_and(|last| now.saturating_sub(last) < 60)
    {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(200))
        .build()?;
    let mut response = client
        .get("http://127.0.0.1:8081/stats.json")
        .send()
        .await?;
    anyhow::ensure!(response.status().is_success(), "native metrics unavailable");
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            data.len().saturating_add(chunk.len()) <= 16384,
            "native metrics too large"
        );
        data.extend_from_slice(&chunk);
    }
    let point = parse(&data, tracker_model::unix_now())
        .ok_or_else(|| anyhow::anyhow!("invalid native metrics"))?;
    store.record_stats(&point)?;
    store.set_value("native_stats_last", point.at)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allowlist_rejects_unknown_and_preserves_instance() {
        let point=parse(br#"{"instance_id":"abc123","engine":{"peers":3,"swarms":2,"secret":"x"},"requests":{"accepted_ipv4":9,"raw":"x"},"rss_bytes":100}"#,100).unwrap();
        assert_eq!(point.instance, "abc123");
        assert_eq!(point.gauges.get("peers"), Some(&3.0));
        assert_eq!(point.counters.get("accepted_ipv4"), Some(&9));
        assert!(!point.counters.contains_key("raw"));
        assert!(parse(br#"{"instance_id":"bad/key"}"#, 100).is_none());
    }
}
