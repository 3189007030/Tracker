use std::path::Path;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use rusqlite::{Connection, params};
use tempfile::TempDir;
use tower::ServiceExt;
use tracker_model::{
    Endpoint, Evidence, FamilyStatus, Infrastructure, Observation, Overview, PUBLIC_SCHEMA, VERSION,
};

use crate::{app, store::public_tracker_url};

async fn get(router: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let body = to_bytes(body, 4 * 1024 * 1024).await.unwrap();
    (
        parts.status,
        parts.headers,
        String::from_utf8(body.to_vec()).unwrap(),
    )
}

fn add_endpoint(db: &Connection, endpoint: &Endpoint) {
    db.execute(
        "INSERT INTO endpoints(id,service_id,url,first_seen,baseline,updated_at,payload) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![endpoint.id, endpoint.service_id, endpoint.url, endpoint.first_seen, endpoint.baseline, endpoint.updated_at, serde_json::to_string(endpoint).unwrap()],
    ).unwrap();
}

fn add_observation(db: &Connection, observation: &Observation) {
    db.execute(
        "INSERT INTO observations(endpoint_id,observed_at,family,payload) VALUES(?1,?2,?3,?4)",
        params![
            observation.endpoint_id,
            observation.observed_at,
            observation.family,
            serde_json::to_string(observation).unwrap()
        ],
    )
    .unwrap();
}

fn fixture(root: &Path) {
    let db = Connection::open(root.join("public.sqlite3")).unwrap();
    db.execute_batch(PUBLIC_SCHEMA).unwrap();
    let now = tracker_model::unix_now();
    let overview = Overview {
        version: VERSION.to_owned(),
        generated_at: now,
        endpoints: 1,
        services: 1,
        standard_count: 1,
        best_count: 0,
        all_count: 1,
        warming: false,
    };
    db.execute(
        "INSERT INTO documents VALUES('overview',?1,?2)",
        params![serde_json::to_string(&overview).unwrap(), now],
    )
    .unwrap();
    let feed = "https://tracker.example.org/announce\nudp://tracker.example.org:6969/announce\nhttps://tracker.example.org/announce?passkey=secret\n";
    db.execute(
        "INSERT INTO documents VALUES('feed:trackers.txt',?1,?2)",
        params![serde_json::to_string(feed).unwrap(), now],
    )
    .unwrap();
    let endpoint = Endpoint {
        id: "example_id".into(),
        service_id: "tracker.example.org".into(),
        url: "https://tracker.example.org/announce".into(),
        protocol: "https".into(),
        host: "ignored-host".into(),
        first_seen: now - 60,
        baseline: false,
        updated_at: now,
        colocated: false,
        families: vec![
            FamilyStatus {
                family: "ipv4".into(),
                state: "failed".into(),
                checked_at: Some(now - 5),
                valid_until: Some(now + 60),
                http_status: Some(200),
                protocol_result: Some("failed".into()),
                error_category: Some("tracker_rejection".into()),
                latency_ms: Some(19.0),
            },
            FamilyStatus {
                family: "ipv6".into(),
                state: "not_published".into(),
                checked_at: Some(now - 5),
                valid_until: Some(now + 60),
                ..Default::default()
            },
        ],
        evidence: Evidence {
            checks_24h: 4,
            successes_24h: 3,
            replies_24h: 4,
            reply_checks_24h: 4,
            success_rate_24h: Some(0.75),
            reply_rate_24h: Some(1.0),
            ..Default::default()
        },
        infrastructure: Infrastructure {
            asn: Some("AS64500".into()),
            organization: Some("<script>provider</script>".into()),
            country: Some("US".into()),
            updated_at: Some(now - 30),
        },
    };
    add_endpoint(&db, &endpoint);
    let private = Endpoint {
        id: "private_id".into(),
        service_id: "private".into(),
        url: "https://tracker.example.org/announce?passkey=secret".into(),
        ..endpoint.clone()
    };
    add_endpoint(&db, &private);
    let first = Observation {
        endpoint_id: "example_id".into(),
        observed_at: now - 120,
        family: "ipv4".into(),
        stage: "announce".into(),
        result: "accepted".into(),
        protocol_reply: true,
        http_status: Some(200),
        latency_ms: Some(11.0),
        error_category: None,
    };
    add_observation(&db, &first);
    let timed_out = Observation {
        observed_at: now - 60,
        result: "failed".into(),
        protocol_reply: false,
        http_status: None,
        latency_ms: Some(5000.0),
        error_category: Some("timeout".into()),
        ..first.clone()
    };
    add_observation(&db, &timed_out);
    let last = Observation {
        observed_at: now - 5,
        latency_ms: Some(19.0),
        result: "failed".into(),
        error_category: Some("tracker_rejection".into()),
        ..first.clone()
    };
    add_observation(&db, &last);
    let v6 = Observation {
        family: "ipv6".into(),
        error_category: Some("203.0.113.44:9876".into()),
        ..first.clone()
    };
    add_observation(&db, &v6);
    let old = Observation {
        observed_at: now - 86_401,
        ..first.clone()
    };
    add_observation(&db, &old);
    let future = Observation {
        observed_at: now + 100,
        ..first
    };
    add_observation(&db, &future);
}

#[tokio::test]
async fn missing_public_data_stays_warming_without_creating_a_database() {
    let dir = TempDir::new().unwrap();
    let router = app(dir.path().to_path_buf());
    let (status, _, body) = get(&router, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(VERSION));
    assert_eq!(
        get(&router, "/readyz").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let (status, headers, body) = get(&router, "/trackers.txt").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(body.is_empty());
    let (_, _, body) = get(&router, "/api/v1/overview").await;
    let overview: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(overview["endpoints"].is_null());
    assert!(overview["generated_at"].is_null());
    let (_, _, body) = get(&router, "/api/v1/trackers").await;
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(page["total"].is_null());
    let (_, _, body) = get(&router, "/").await;
    assert!(body.contains("<strong>—</strong>"));
    assert!(!dir.path().join("public.sqlite3").exists());
}

#[tokio::test]
async fn initialized_empty_store_is_zero_but_a_missing_snapshot_remains_unknown() {
    let dir = TempDir::new().unwrap();
    let db = Connection::open(dir.path().join("public.sqlite3")).unwrap();
    db.execute_batch(PUBLIC_SCHEMA).unwrap();
    drop(db);
    let router = app(dir.path().to_path_buf());
    let (_, _, body) = get(&router, "/api/v1/trackers").await;
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["total"], 0);
    assert_eq!(page["has_snapshot"], false);
    let (_, _, body) = get(&router, "/trackers").await;
    assert!(body.contains("公开快照尚未生成"));
}

#[tokio::test]
async fn fresh_snapshot_is_ready_even_if_no_tracker_qualifies_yet() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let db = Connection::open(dir.path().join("public.sqlite3")).unwrap();
    let overview = Overview {
        version: VERSION.to_owned(),
        generated_at: tracker_model::unix_now(),
        warming: true,
        ..Overview::default()
    };
    db.execute(
        "UPDATE documents SET payload=?1 WHERE key='overview'",
        [serde_json::to_string(&overview).unwrap()],
    )
    .unwrap();
    drop(db);
    let router = app(dir.path().to_path_buf());
    let (status, _, body) = get(&router, "/readyz").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"evidence\":\"warming\""));
    let (_, _, filtered) = get(&router, "/trackers?q=does-not-exist").await;
    assert!(filtered.contains("没有符合条件的端点"));
    assert!(!filtered.contains("公开快照尚未生成"));
}

#[tokio::test]
async fn details_explain_protocol_rejection_and_plot_only_observed_events() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let router = app(dir.path().to_path_buf());
    assert_eq!(get(&router, "/readyz").await.0, StatusCode::OK);
    let (status, headers, body) = get(&router, "/tracker/example_id").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("HTTP 200"));
    assert!(body.contains("协议拒绝"));
    assert!(body.contains("timeline-point neutral"));
    assert!(body.contains("timeline-point failure"));
    assert!(body.contains("未发现地址"));
    assert!(body.contains("NEW"));
    assert!(body.contains("AS64500"));
    assert!(body.contains("&lt;script&gt;provider&lt;/script&gt;"));
    assert!(!body.contains("<script>provider</script>"));
    assert!(body.contains("不能精确定位机房"));
    assert!(!body.contains("203.0.113.44"));
    assert!(
        !body.contains("5000 ms"),
        "timeout duration is not shown as response latency"
    );
    assert!(
        !body.contains("class=\"chart-line ipv4\""),
        "a timeout breaks the line between successful observations"
    );
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );
    let (_, _, english) = get(&router, "/tracker/example_id?lang=en").await;
    assert!(english.contains("Protocol rejection"));
    assert!(english.contains("No address"));
}

#[tokio::test]
async fn stale_snapshot_is_marked_stale_and_not_ready() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let db = Connection::open(dir.path().join("public.sqlite3")).unwrap();
    let overview = Overview {
        version: VERSION.to_owned(),
        generated_at: tracker_model::unix_now() - 601,
        warming: false,
        ..Overview::default()
    };
    db.execute(
        "UPDATE documents SET payload=?1 WHERE key='overview'",
        [serde_json::to_string(&overview).unwrap()],
    )
    .unwrap();
    drop(db);
    let router = app(dir.path().to_path_buf());
    assert_eq!(
        get(&router, "/readyz").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let (_, _, body) = get(&router, "/").await;
    assert!(body.contains("runtime-strip"));
    assert!(!body.contains("class=\"hero\""));
}

#[tokio::test]
async fn api_filters_future_old_wrong_family_and_untrusted_details() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let router = app(dir.path().to_path_buf());
    let (status, _, body) = get(
        &router,
        "/api/v1/trackers/example_id/observations?family=ipv4&window=24h",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["items"].as_array().unwrap().len(), 3);
    assert!(
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["error_category"] == "timeout" && row["latency_ms"].is_null())
    );
    assert!(!body.contains("203.0.113.44"));
    assert_eq!(
        get(
            &router,
            "/api/v1/trackers/example_id/observations?family=other"
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(
            &router,
            "/api/v1/trackers/example_id/observations?limit=501"
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&router, "/api/v1/trackers?limit=101").await.0,
        StatusCode::BAD_REQUEST
    );
    let (_, _, endpoints) = get(&router, "/api/v1/trackers").await;
    assert!(!endpoints.contains("passkey"));
    assert!(!endpoints.contains("secret"));
    assert!(!endpoints.contains("score"));
}

#[tokio::test]
async fn incomplete_reply_and_unusable_peers_remain_distinct() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let db = Connection::open(dir.path().join("public.sqlite3")).unwrap();
    let now = tracker_model::unix_now();
    let incomplete = Observation {
        observed_at: now - 1,
        endpoint_id: "example_id".into(),
        family: "ipv4".into(),
        stage: "announce".into(),
        result: "unknown".into(),
        protocol_reply: false,
        http_status: Some(200),
        latency_ms: Some(1234.0),
        error_category: Some("incomplete_protocol".into()),
    };
    add_observation(&db, &incomplete);
    add_observation(
        &db,
        &Observation {
            stage: "sample_announce".into(),
            result: "accepted".into(),
            protocol_reply: true,
            latency_ms: Some(17.0),
            error_category: Some("peers_unusable".into()),
            ..incomplete.clone()
        },
    );
    add_observation(
        &db,
        &Observation {
            stage: "sample_announce".into(),
            result: "empty".into(),
            protocol_reply: true,
            latency_ms: Some(18.0),
            error_category: None,
            ..incomplete.clone()
        },
    );
    add_observation(
        &db,
        &Observation {
            error_category: Some("secret-peer-address".into()),
            latency_ms: None,
            ..incomplete
        },
    );
    drop(db);
    let router = app(dir.path().to_path_buf());
    let (_, _, body) = get(&router, "/api/v1/trackers/example_id/observations").await;
    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    let items = payload["items"].as_array().unwrap();
    let incomplete = items
        .iter()
        .find(|event| event["error_category"] == "incomplete_protocol")
        .unwrap();
    assert_eq!(incomplete["result"], "unknown");
    assert_eq!(incomplete["http_status"], 200);
    assert_eq!(incomplete["protocol_reply"], false);
    assert!(incomplete["latency_ms"].is_null());
    assert!(
        items
            .iter()
            .any(|event| event["error_category"] == "peers_unusable"
                && event["result"] == "accepted"
                && event["protocol_reply"] == true)
    );
    assert!(
        items
            .iter()
            .any(|event| event["stage"] == "sample_announce" && event["result"] == "empty")
    );
    assert!(!body.contains("secret-peer-address"));
    let (_, _, body) = get(
        &router,
        "/api/v1/trackers/example_id/observations?stage=handshake",
    )
    .await;
    let handshakes: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(handshakes["items"].as_array().unwrap().is_empty());

    let (_, _, chinese) = get(&router, "/tracker/example_id").await;
    assert!(chinese.contains("协议回复不完整，未验证"));
    assert!(!chinese.contains("有效回复，但无可用于验证的节点"));
    assert!(!chinese.contains("有效回复，节点为空"));
    assert!(!chinese.contains("已获得候选节点"));
    assert!(!chinese.contains("节点握手成功"));
    assert!(!chinese.contains("1234 ms"));
    assert!(chinese.contains("timeline-point unknown"));
    assert_eq!(chinese.matches("class=\"chart-point ").count(), 3);
    let (_, _, english) = get(&router, "/tracker/example_id?lang=en").await;
    assert!(english.contains("Incomplete protocol reply; unverified"));
    assert!(!english.contains("Valid reply, but no usable peers for verification"));
    assert!(!english.contains("Valid reply, no peers"));
}

#[tokio::test]
async fn diagnostic_stages_keep_empty_timeout_and_handshake_distinct() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let db = Connection::open(dir.path().join("public.sqlite3")).unwrap();
    let now = tracker_model::unix_now();
    let base = Observation {
        observed_at: now - 2,
        endpoint_id: "example_id".into(),
        family: "ipv4".into(),
        stage: "sample_announce".into(),
        result: "accepted".into(),
        protocol_reply: true,
        http_status: Some(200),
        latency_ms: Some(23.0),
        error_category: None,
    };
    add_observation(&db, &base);
    add_observation(
        &db,
        &Observation {
            result: "empty".into(),
            ..base.clone()
        },
    );
    add_observation(
        &db,
        &Observation {
            result: "failed".into(),
            protocol_reply: false,
            http_status: None,
            latency_ms: Some(9000.0),
            error_category: Some("timeout".into()),
            ..base.clone()
        },
    );
    add_observation(
        &db,
        &Observation {
            stage: "handshake".into(),
            result: "accepted".into(),
            // Invalid Tracker-only fields are discarded for peer handshakes.
            protocol_reply: true,
            http_status: Some(200),
            latency_ms: Some(13.0),
            ..base.clone()
        },
    );
    add_observation(
        &db,
        &Observation {
            stage: "handshake".into(),
            result: "failed".into(),
            protocol_reply: false,
            http_status: None,
            error_category: Some("transport".into()),
            ..base.clone()
        },
    );
    add_observation(
        &db,
        &Observation {
            stage: "handshake".into(),
            result: "unknown".into(),
            protocol_reply: false,
            http_status: None,
            error_category: Some("budget_exhausted".into()),
            ..base
        },
    );
    drop(db);
    let router = app(dir.path().to_path_buf());
    let (_, _, body) = get(
        &router,
        "/api/v1/trackers/example_id/observations?stage=sample_announce",
    )
    .await;
    let samples: serde_json::Value = serde_json::from_str(&body).unwrap();
    let samples = samples["items"].as_array().unwrap();
    assert_eq!(samples.len(), 3);
    assert!(
        samples
            .iter()
            .all(|event| event["stage"] == "sample_announce")
    );
    assert!(samples.iter().any(|event| event["result"] == "empty"));
    assert!(samples.iter().any(|event| event["result"] == "failed"
        && event["error_category"] == "timeout"
        && event["latency_ms"].is_null()));
    let (_, _, body) = get(
        &router,
        "/api/v1/trackers/example_id/observations?stage=handshake",
    )
    .await;
    let handshakes: serde_json::Value = serde_json::from_str(&body).unwrap();
    let handshakes = handshakes["items"].as_array().unwrap();
    assert_eq!(handshakes.len(), 3);
    assert!(handshakes.iter().all(|event| event["stage"] == "handshake"
        && event["http_status"].is_null()
        && event["protocol_reply"] == false));
    assert!(
        handshakes
            .iter()
            .any(|event| event["result"] == "accepted" && event["latency_ms"] == 13.0)
    );
    assert_eq!(
        get(
            &router,
            "/api/v1/trackers/example_id/observations?stage=unexpected"
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    let (_, _, chinese) = get(&router, "/tracker/example_id").await;
    for text in [
        "样本通告",
        "已获得候选节点",
        "有效回复，节点为空",
        "样本通告超时",
        "节点握手成功",
        "节点握手失败",
        "本地预算不足",
    ] {
        assert!(!chinese.contains(text), "removed diagnostic leaked: {text}");
    }
    assert_eq!(chinese.matches("class=\"timeline-point ").count(), 4);
    assert_eq!(chinese.matches("class=\"chart-point ").count(), 3);
    assert!(!chinese.contains("9000 ms"));
    let (_, _, english) = get(&router, "/tracker/example_id?lang=en").await;
    for text in [
        "Sample announce",
        "Candidate peers returned",
        "Valid reply, no peers",
        "Sample announce timed out",
        "Peer handshake succeeded",
        "Peer handshake failed",
        "Local budget exhausted",
    ] {
        assert!(!english.contains(text), "removed diagnostic leaked: {text}");
    }
}

#[tokio::test]
async fn many_diagnostic_events_do_not_hide_the_health_chart() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let mut db = Connection::open(dir.path().join("public.sqlite3")).unwrap();
    let tx = db.transaction().unwrap();
    let event = Observation {
        observed_at: tracker_model::unix_now(),
        endpoint_id: "example_id".into(),
        family: "ipv4".into(),
        stage: "sample_announce".into(),
        result: "empty".into(),
        protocol_reply: true,
        http_status: Some(200),
        latency_ms: Some(10.0),
        error_category: None,
    };
    for _ in 0..501 {
        add_observation(&tx, &event);
    }
    tx.commit().unwrap();
    drop(db);
    let router = app(dir.path().to_path_buf());
    let (_, _, health) = get(
        &router,
        "/api/v1/trackers/example_id/observations?stage=announce",
    )
    .await;
    let health: serde_json::Value = serde_json::from_str(&health).unwrap();
    assert_eq!(health["items"].as_array().unwrap().len(), 4);
    let (_, _, page) = get(&router, "/tracker/example_id").await;
    assert_eq!(page.matches("class=\"timeline-point ").count(), 4);
    assert_eq!(page.matches("class=\"chart-point ").count(), 3);
}

#[tokio::test]
async fn udp_never_inherits_http_status_and_expired_family_is_stale() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let db = Connection::open(dir.path().join("public.sqlite3")).unwrap();
    let payload: String = db
        .query_row(
            "SELECT payload FROM endpoints WHERE id='example_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut endpoint: Endpoint = serde_json::from_str(&payload).unwrap();
    endpoint.url = "udp://tracker.example.org:6969/announce".into();
    endpoint.protocol = "udp".into();
    endpoint.families[0].valid_until = Some(tracker_model::unix_now() - 1);
    db.execute(
        "UPDATE endpoints SET url=?1,payload=?2 WHERE id='example_id'",
        params![endpoint.url, serde_json::to_string(&endpoint).unwrap()],
    )
    .unwrap();
    let (row_id, payload): (i64, String) = db.query_row("SELECT id,payload FROM observations WHERE endpoint_id='example_id' AND family='ipv4' ORDER BY observed_at DESC LIMIT 1", [], |r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let mut observation: Observation = serde_json::from_str(&payload).unwrap();
    observation.result = "failed".into();
    observation.protocol_reply = true; // UDP connect succeeded; announce then timed out.
    observation.error_category = Some("timeout".into());
    db.execute(
        "UPDATE observations SET payload=?1 WHERE id=?2",
        params![serde_json::to_string(&observation).unwrap(), row_id],
    )
    .unwrap();
    drop(db);
    let router = app(dir.path().to_path_buf());
    let (_, _, detail) = get(&router, "/api/v1/trackers/example_id").await;
    let value: serde_json::Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(value["families"][0]["state"], "stale");
    assert!(value["families"][0]["http_status"].is_null());
    let (_, _, observations) = get(
        &router,
        "/api/v1/trackers/example_id/observations?family=ipv4",
    )
    .await;
    let value: serde_json::Value = serde_json::from_str(&observations).unwrap();
    assert!(
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["http_status"].is_null())
    );
    assert!(
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["protocol_reply"] == true && row["result"] == "failure")
    );
    let (_, _, html) = get(&router, "/tracker/example_id").await;
    assert!(!html.contains("HTTP 200"));
    assert!(html.contains("已过期"));
}

#[tokio::test]
async fn feed_formats_share_entries_and_support_conditional_responses() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let router = app(dir.path().to_path_buf());
    let (status, headers, text) = get(&router, "/trackers.txt").await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.ends_with('\n'));
    assert_eq!(text.lines().count(), 2);
    assert!(!text.contains("passkey"));
    let etag = headers[header::ETAG].clone();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/trackers.txt")
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    let (_, _, json) = get(&router, "/trackers.json").await;
    let values: Vec<String> = serde_json::from_str(&json).unwrap();
    assert_eq!(values, text.lines().map(str::to_owned).collect::<Vec<_>>());
    let (_, _, rss) = get(&router, "/trackers.rss").await;
    assert_eq!(rss.matches("<item>").count(), 2);
    assert!(!rss.contains("secret"));
    assert_eq!(
        get(&router, "/unsupported.txt").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn large_feed_memory_is_bounded_until_the_response_body_is_released() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let router = app(dir.path().to_path_buf());
    let first = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/trackers.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let head = router
        .clone()
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/trackers.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert!(to_bytes(head.into_body(), 1024).await.unwrap().is_empty());
    let second = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/trackers_all.rss")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(first);
    assert_eq!(get(&router, "/trackers.txt").await.0, StatusCode::OK);
}

#[tokio::test]
async fn search_input_is_escaped_and_corrupt_public_store_is_unavailable() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let router = app(dir.path().to_path_buf());
    let (_, _, body) = get(
        &router,
        "/trackers?q=%3Cscript%3Ealert%281%29%3C%2Fscript%3E",
    )
    .await;
    assert!(!body.contains("<script>"));
    assert!(body.contains("&lt;script&gt;"));
    let other = TempDir::new().unwrap();
    std::fs::write(other.path().join("public.sqlite3"), b"not a database").unwrap();
    let corrupt = app(other.path().to_path_buf());
    assert_eq!(
        get(&corrupt, "/api/v1/overview").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(get(&corrupt, "/healthz").await.0, StatusCode::OK);
}

#[test]
fn tracker_url_filter_rejects_credentials_and_untrusted_schemes() {
    assert!(public_tracker_url("udp://example.org:6969/announce").is_some());
    assert!(public_tracker_url("https://example.org/announce").is_some());
    assert!(public_tracker_url("http://8.8.8.8/announce").is_some());
    for bad in [
        "file:///etc/passwd",
        "https://u:p@example.org/announce",
        "https://example.org/announce?passkey=secret",
        "https://example.org/%70asskey/announce",
        "https://example.org/0123456789abcdef0123456789abcdef/announce",
        "http://127.0.0.1/announce",
        "http://192.168.0.1/announce",
        "http://100.64.0.1/announce",
        "http://[::1]/announce",
        "http://localhost/announce",
        "http://tracker.local/announce",
    ] {
        assert!(public_tracker_url(bad).is_none(), "accepted {bad}");
    }
}

#[test]
fn protocol_reply_uses_its_own_denominator() {
    let endpoint = Endpoint {
        id: "sample".into(),
        host: "tracker.example.org".into(),
        evidence: Evidence {
            checks_24h: 4,
            successes_24h: 3,
            replies_24h: 5,
            reply_checks_24h: 5,
            reply_rate_24h: Some(1.0),
            ..Default::default()
        },
        ..Default::default()
    };
    let page = crate::html::detail(
        &endpoint,
        &[],
        &[],
        tracker_model::unix_now(),
        crate::html::Lang::Zh,
    );
    assert!(page.contains("5 / 5"));
    assert!(!page.contains("5 / 4"));
}

#[tokio::test]
async fn legacy_pages_redirect_and_home_has_copyable_endpoints() {
    let dir = TempDir::new().unwrap();
    let router = app(dir.path().to_path_buf());
    let (status, headers, _) = get(&router, "/tools?lang=en").await;
    assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(headers[header::LOCATION], "/?lang=en#native-trackers");
    let (status, headers, _) = get(&router, "/methodology?lang=zh").await;
    assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(headers[header::LOCATION], "/sources?lang=zh");
    let (_, _, home) = get(&router, "/?lang=en").await;
    for url in [
        "https://announce.augrains.com/announce",
        "http://announce.augrains.com/announce",
        "udp://announce.augrains.com:6969/announce",
        "trackers_best.txt",
        "trackers_https.txt",
    ] {
        assert!(home.contains(url));
    }
    assert!(home.contains("id=\"native-trackers\""));
    assert!(home.contains("/assets/style.css?v=1.0.3"));
    assert!(home.contains("href=\"https://github.com/3189007030/Tracker\""));
    assert!(home.contains("/assets/tools.js?v=1.0.2"));
    assert!(home.contains("/assets/charts.js?v=1.0.2"));
    assert!(home.contains("/assets/chart.umd.js?v=4.6.1"));
    assert!(!home.contains("/api/v1/experiments"));
    assert!(!home.contains("href=\"/tools"));
    let (status, headers, js) = get(&router, "/assets/charts.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("javascript")
    );
    assert!(js.contains("/api/v1/stats/history"));
}

#[tokio::test]
async fn legacy_sample_records_are_unreachable_and_history_stays_bounded() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let db = Connection::open(dir.path().join("public.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE public_samples(id TEXT PRIMARY KEY,payload TEXT,updated_at INTEGER); CREATE TABLE public_runs(id TEXT PRIMARY KEY,endpoint_id TEXT,sample_id TEXT,observed_at INTEGER,family TEXT,payload TEXT);").unwrap();
    let now = tracker_model::unix_now();
    let sample = serde_json::json!({"id":"sample_1","pool":"software","source":"Debian","name":"Public sample","magnet":"magnet:?xt=urn:btih:0123456789012345678901234567890123456789","metadata_available":true,"discovered_at":now,"private_address":"203.0.113.77"});
    db.execute(
        "INSERT INTO public_samples VALUES(?1,?2,?3)",
        params!["sample_1", sample.to_string(), now],
    )
    .unwrap();
    let bad = serde_json::json!({"id":"sample_bad","pool":"forum","source":"论坛","name":"论坛","magnet":"magnet:?xt=urn:btih:0123456789012345678901234567890123456789&dn=secret","metadata_available":false,"discovered_at":now});
    db.execute(
        "INSERT INTO public_samples VALUES(?1,?2,?3)",
        params!["sample_bad", bad.to_string(), now],
    )
    .unwrap();
    let run = serde_json::json!({"id":"run_1","endpoint_id":"example_id","sample_id":"sample_1","observed_at":now,"family":"ipv4","announce_result":"accepted","error_category":null,"nodes":[{"ordinal":1,"handshake":true,"bitmap_available":true,"error_category":null,"peer_address":"203.0.113.77"}],"bitmap_observations":1,"coverage":0.5,"info_hash":"do-not-publish"});
    db.execute(
        "INSERT INTO public_runs VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            "run_1",
            "example_id",
            "sample_1",
            now,
            "ipv4",
            run.to_string()
        ],
    )
    .unwrap();
    let point = serde_json::json!({"at":now-60,"instance":"boot_1","counters":{"accepted_announces":20,"untrusted_address":13},"gauges":{"peers":3.0,"secret":4.0}});
    db.execute(
        "INSERT INTO stats_history VALUES(?1,?2,?3)",
        params![now - 60, "boot_1", point.to_string()],
    )
    .unwrap();
    drop(db);
    let router = app(dir.path().to_path_buf());
    for path in [
        "/api/v1/samples/sample_1",
        "/api/v1/samples/sample_bad",
        "/api/v1/trackers/example_id/runs?page=1&limit=25",
    ] {
        let (status, _, body) = get(&router, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!body.contains("magnet:?xt=urn:btih:"));
        assert!(!body.contains("do-not-publish"));
    }
    let (status, _, body) = get(&router, "/api/v1/stats/history?window=24h").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("accepted_announces"));
    assert!(!body.contains("untrusted_address"));
    assert!(!body.contains("secret"));
    assert_eq!(
        get(&router, "/api/v1/stats/history?window=2y").await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn new_public_tables_warm_without_creating_or_writing_a_database() {
    let dir = TempDir::new().unwrap();
    let router = app(dir.path().to_path_buf());
    let (status, _, body) = get(&router, "/api/v1/stats/history?window=7d").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"items\":[]"));
    assert_eq!(
        get(&router, "/api/v1/samples/missing").await.0,
        StatusCode::NOT_FOUND
    );
    assert!(!dir.path().join("public.sqlite3").exists());
}

#[tokio::test]
async fn home_summary_uses_fresh_published_counts_and_keeps_missing_unknown() {
    let dir = TempDir::new().unwrap();
    let store = crate::store::PublicStore::new(dir.path());
    let now = tracker_model::unix_now();
    assert!(store.native_summary(now).unwrap().is_none());
    assert!(!dir.path().join("public.sqlite3").exists());
    fixture(dir.path());
    let db = Connection::open(dir.path().join("public.sqlite3")).unwrap();
    let point = serde_json::json!({"at":now-60,"instance":"boot_1","counters":{"accepted_announces":0,"private_counter":7},"gauges":{"peers":0.0,"swarms":0.0,"secret":9.0}});
    db.execute(
        "INSERT INTO stats_history VALUES(?1,?2,?3)",
        params![now - 60, "boot_1", point.to_string()],
    )
    .unwrap();
    let summary = store.native_summary(now).unwrap().unwrap();
    assert_eq!(summary["peers"], 0);
    assert_eq!(summary["swarms"], 0);
    assert_eq!(summary["announces"], 0);
    assert!(summary.get("secret").is_none());
    assert!(summary.get("private_counter").is_none());
    assert!(store.native_summary(now + 181).unwrap().is_none());
    let router = app(dir.path().to_path_buf());
    let (status, headers, _) = get(&router, "/status?lang=en").await;
    assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(headers[header::LOCATION], "/?lang=en");
    let (status, _, body) = get(&router, "/declaration?lang=en").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("magnet:?xt=urn:btih:"));
}

#[tokio::test]
async fn pool_reasons_and_staleness_are_shown_independently() {
    let dir = TempDir::new().unwrap();
    fixture(dir.path());
    let db = Connection::open(dir.path().join("public.sqlite3")).unwrap();
    let now = tracker_model::unix_now();
    let pools = serde_json::json!([
        {"pool":"software","count":12,"target":100,"updated_at":now,"last_attempt_at":now,"stale":false,"status":"partial","error_category":"http_502"},
        {"pool":"forum","count":0,"target":100,"updated_at":0,"last_attempt_at":now,"stale":true,"status":"deferred","error_category":"robots_denied"}
    ]);
    db.execute(
        "INSERT INTO documents VALUES ('sample_pools',?1,?2)",
        params![pools.to_string(), now],
    )
    .unwrap();
    drop(db);
    let router = app(dir.path().to_path_buf());
    let (_, _, body) = get(&router, "/sources").await;
    assert!(body.contains("公共资源样本池"));
    assert!(!body.contains("公开软件样本池"));
    let software = body
        .split("<article class=\"metric\">")
        .nth(1)
        .unwrap()
        .split("</article>")
        .next()
        .unwrap();
    assert!(software.contains("部分来源延后"));
    assert!(software.contains("来源返回 HTTP 502"));
    assert!(!software.contains("数据已过期"));
    assert!(body.contains("等待来源"));
    assert!(body.contains("来源 robots 规则不允许"));
    assert!(body.contains("更新时间: 未知"));
    let (_, _, english) = get(&router, "/sources?lang=en").await;
    assert!(english.contains("Public resource pool"));
    let (_, _, listing) = get(&router, "/trackers").await;
    assert!(listing.contains("href=\"/trackers.txt\""));
    assert!(listing.contains("复制标准订阅地址"));
}
