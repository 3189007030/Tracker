//! Public website. All data access is through the read-only public projection.
mod flags;
mod html;
mod store;

use std::{collections::HashMap, convert::Infallible, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use bytes::Bytes;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tracker_model::{VERSION, unix_now};

use html::Lang;
use store::{Filters, PublicStore, StoreError};

#[derive(Clone)]
struct Site {
    store: PublicStore,
    feed_permits: Arc<Semaphore>,
}

/// Build a router without initializing, modifying or creating any database.
pub fn app(data_dir: PathBuf) -> Router {
    let state = Site {
        store: PublicStore::new(data_dir),
        feed_permits: Arc::new(Semaphore::new(1)),
    };
    Router::new()
        .route("/", get(home))
        .route("/trackers", get(trackers_page))
        .route("/tracker/{id}", get(tracker_detail))
        .route("/methodology", get(methodology))
        .route("/sources", get(sources))
        .route("/tools", get(tools))
        .route("/status", get(status_redirect))
        .route("/declaration", get(declaration))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/assets/style.css", get(style))
        .route("/assets/tools.js", get(tools_script))
        .route("/assets/charts.js", get(charts_script))
        .route("/assets/chart.umd.js", get(chart_library))
        .route("/assets/brand.svg", get(brand_svg))
        .route("/assets/brand.png", get(brand_png))
        .route("/favicon.ico", get(favicon))
        .route("/assets/flags/{name}", get(flag))
        .route("/api/v1/overview", get(api_overview))
        .route("/api/v1/trackers", get(api_trackers))
        .route("/api/v1/trackers/{id}", get(api_detail))
        .route("/api/v1/trackers/{id}/observations", get(api_observations))
        .route("/api/v1/stats/history", get(api_history))
        .route("/{filename}", get(feed).head(feed_head))
        .fallback(not_found)
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
        "default-src 'none'; style-src 'self'; img-src 'self' data:; script-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
    ));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

async fn read<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|_| StoreError)?
}

type Params = HashMap<String, String>;

fn language(params: &Params) -> Lang {
    Lang::from_param(params.get("lang").map(String::as_str))
}

fn filters(params: &Params) -> Result<Filters, &'static str> {
    let page = params
        .get("page")
        .map(|v| v.parse::<usize>())
        .transpose()
        .map_err(|_| "invalid_page")?
        .unwrap_or(1);
    let limit = params
        .get("limit")
        .map(|v| v.parse::<usize>())
        .transpose()
        .map_err(|_| "invalid_limit")?
        .unwrap_or(25);
    if page == 0 || page > 10_000 || limit == 0 || limit > 100 {
        return Err("invalid_pagination");
    }
    let protocol = params
        .get("protocol")
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if !protocol.is_empty() && !["udp", "http", "https"].contains(&protocol.as_str()) {
        return Err("invalid_protocol");
    }
    let query = params
        .get("q")
        .map(|s| s.chars().take(128).collect())
        .unwrap_or_default();
    Ok(Filters {
        page,
        limit,
        protocol,
        query,
    })
}

fn api_error(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"error": code})),
    )
        .into_response()
}

fn unavailable() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "public_snapshot_unavailable",
    )
}

async fn home(State(site): State<Site>, Query(params): Query<Params>) -> Response {
    let lang = language(&params);
    match read(move || {
        let overview = site.store.overview()?;
        let native = site.store.native_summary(unix_now()).unwrap_or(None);
        Ok((overview, native))
    })
    .await
    {
        Ok((overview, native)) => {
            Html(html::home(&overview, native.as_ref(), lang)).into_response()
        }
        Err(_) => unavailable_page(lang),
    }
}

async fn trackers_page(State(site): State<Site>, Query(params): Query<Params>) -> Response {
    let lang = language(&params);
    let filters = match filters(&params) {
        Ok(value) => value,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid filters").into_response(),
    };
    let query = filters.query.clone();
    let protocol = filters.protocol.clone();
    match read(move || site.store.page(filters, unix_now())).await {
        Ok(page) => Html(html::listing(&page, &query, &protocol, lang)).into_response(),
        Err(_) => unavailable_page(lang),
    }
}

async fn tracker_detail(
    State(site): State<Site>,
    Path(id): Path<String>,
    Query(params): Query<Params>,
) -> Response {
    let lang = language(&params);
    if !store::valid_id(&id) {
        return not_found_page(lang);
    }
    match read(move || {
        let now = unix_now();
        let endpoint = site.store.endpoint(&id, now)?;
        let health = if endpoint.is_some() {
            site.store
                .observations(&id, None, Some("announce"), now, 500)?
        } else {
            Vec::new()
        };
        Ok((endpoint, health, now))
    })
    .await
    {
        Ok((Some(endpoint), health, now)) => {
            Html(html::detail(&endpoint, &health, &health, now, lang)).into_response()
        }
        Ok((None, _, _)) => not_found_page(lang),
        Err(_) => unavailable_page(lang),
    }
}

async fn methodology(State(site): State<Site>, Query(params): Query<Params>) -> Response {
    let _ = site;
    (
        StatusCode::MOVED_PERMANENTLY,
        [(
            header::LOCATION,
            format!("/sources?lang={}", language(&params).code()),
        )],
    )
        .into_response()
}
async fn sources(State(site): State<Site>, Query(params): Query<Params>) -> Response {
    let lang = language(&params);
    match read(move || {
        Ok((
            site.store.overview()?,
            site.store.pools().unwrap_or_default(),
        ))
    })
    .await
    {
        Ok((overview, pools)) => {
            Html(html::simple("sources", &overview, None, &pools, lang)).into_response()
        }
        Err(_) => unavailable_page(lang),
    }
}
async fn tools(State(site): State<Site>, Query(params): Query<Params>) -> Response {
    let _ = site;
    (
        StatusCode::MOVED_PERMANENTLY,
        [(
            header::LOCATION,
            format!("/?lang={}#native-trackers", language(&params).code()),
        )],
    )
        .into_response()
}
async fn status_redirect(Query(params): Query<Params>) -> Response {
    (
        StatusCode::MOVED_PERMANENTLY,
        [(
            header::LOCATION,
            format!("/?lang={}", language(&params).code()),
        )],
    )
        .into_response()
}

async fn declaration(Query(params): Query<Params>) -> Response {
    Html(html::declaration(language(&params))).into_response()
}

async fn health() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"status":"ok", "version": VERSION})),
    )
}

async fn ready(State(site): State<Site>) -> Response {
    match read(move || site.store.overview()).await {
        Ok(overview) => {
            let age = unix_now().saturating_sub(overview.generated_at);
            // A fresh, valid snapshot can have no eligible trackers for days.
            // Evidence warming must not be confused with process readiness.
            let ready = overview.generated_at > 0 && (0..=600).contains(&age);
            let status = if ready {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            (status, [(header::CACHE_CONTROL, "no-store")], Json(json!({"status":if ready {"ready"} else {"warming"},"evidence":if overview.warming {"warming"} else {"available"},"version":VERSION,"generated_at":overview.generated_at}))).into_response()
        }
        Err(_) => unavailable(),
    }
}

async fn api_overview(State(site): State<Site>) -> Response {
    match read(move || site.store.overview()).await {
        Ok(overview) => {
            let cache = if overview.warming {
                "no-store"
            } else {
                "public, max-age=15"
            };
            let mut body = match serde_json::to_value(&overview) {
                Ok(value) => value,
                Err(_) => return unavailable(),
            };
            if overview.generated_at <= 0 {
                for field in [
                    "generated_at",
                    "endpoints",
                    "services",
                    "standard_count",
                    "best_count",
                    "all_count",
                ] {
                    body[field] = Value::Null;
                }
            }
            ([(header::CACHE_CONTROL, cache)], Json(body)).into_response()
        }
        Err(_) => unavailable(),
    }
}

async fn api_trackers(State(site): State<Site>, Query(params): Query<Params>) -> Response {
    let filters = match filters(&params) {
        Ok(value) => value,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    match read(move || site.store.page(filters, unix_now())).await {
        Ok(page) => ([(header::CACHE_CONTROL, "public, max-age=15")], Json(page)).into_response(),
        Err(_) => unavailable(),
    }
}

async fn api_detail(State(site): State<Site>, Path(id): Path<String>) -> Response {
    if !store::valid_id(&id) {
        return api_error(StatusCode::NOT_FOUND, "not_found");
    }
    match read(move || site.store.endpoint(&id, unix_now())).await {
        Ok(Some(endpoint)) => (
            [(header::CACHE_CONTROL, "public, max-age=15")],
            Json(endpoint),
        )
            .into_response(),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "not_found"),
        Err(_) => unavailable(),
    }
}

async fn api_history(State(site): State<Site>, Query(params): Query<Params>) -> Response {
    let window = params.get("window").map(String::as_str).unwrap_or("24h");
    if !matches!(window, "24h" | "7d" | "30d") {
        return api_error(StatusCode::BAD_REQUEST, "invalid_window");
    }
    let window = window.to_owned();
    match read(move || site.store.history(&window, unix_now())).await {
        Ok(history) => (
            [(header::CACHE_CONTROL, "public, max-age=30, s-maxage=60")],
            Json(history),
        )
            .into_response(),
        Err(_) => unavailable(),
    }
}

async fn api_observations(
    State(site): State<Site>,
    Path(id): Path<String>,
    Query(params): Query<Params>,
) -> Response {
    if !store::valid_id(&id) {
        return api_error(StatusCode::NOT_FOUND, "not_found");
    }
    if params.get("window").is_some_and(|v| v != "24h") {
        return api_error(StatusCode::BAD_REQUEST, "invalid_window");
    }
    let family = params.get("family").cloned();
    if family
        .as_deref()
        .is_some_and(|f| !matches!(f, "ipv4" | "ipv6"))
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid_family");
    }
    let stage = params.get("stage").cloned();
    if stage
        .as_deref()
        .is_some_and(|s| !matches!(s, "announce" | "sample_announce" | "handshake" | "bitmap"))
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid_stage");
    }
    let limit = match params.get("limit").map(|s| s.parse::<usize>()).transpose() {
        Ok(Some(n)) if (1..=500).contains(&n) => n,
        Ok(None) => 500,
        _ => return api_error(StatusCode::BAD_REQUEST, "invalid_limit"),
    };
    match read(move || {
        let now = unix_now();
        if site.store.endpoint(&id, now)?.is_none() {
            return Ok(None);
        }
        site.store
            .observations(&id, family.as_deref(), stage.as_deref(), now, limit)
            .map(Some)
    })
    .await
    {
        Ok(Some(items)) => (
            [(header::CACHE_CONTROL, "public, max-age=15")],
            Json(json!({"window":"24h", "items":items})),
        )
            .into_response(),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "not_found"),
        Err(_) => unavailable(),
    }
}

fn feed_name(filename: &str) -> Option<(&'static str, &'static str)> {
    let (name, format) = filename.rsplit_once('.')?;
    let name = match name {
        "trackers" => "trackers.txt",
        "trackers_best" => "trackers_best.txt",
        "trackers_all" => "trackers_all.txt",
        "trackers_udp" => "trackers_udp.txt",
        "trackers_http" => "trackers_http.txt",
        "trackers_https" => "trackers_https.txt",
        _ => return None,
    };
    match format {
        "txt" => Some((name, "txt")),
        "json" => Some((name, "json")),
        "rss" | "xml" => Some((name, "rss")),
        _ => None,
    }
}

fn rss(entries: &[String]) -> String {
    let escaped_len = |text: &str| {
        text.chars()
            .map(|c| match c {
                '&' => 5,
                '<' | '>' => 4,
                '"' => 6,
                '\'' => 5,
                _ => c.len_utf8(),
            })
            .sum::<usize>()
    };
    let header = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><rss version=\"2.0\"><channel><title>Tracker Observatory</title><link>https://tracker.augrains.com/</link><description>Observed public tracker list</description>";
    let footer = "</channel></rss>";
    let markup = "<item><title>".len() + "</title><link>".len() + "</link></item>".len();
    let estimated = entries
        .iter()
        .map(|item| {
            let host = url::Url::parse(item)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default();
            markup
                .saturating_add(escaped_len(&host))
                .saturating_add(escaped_len(item))
        })
        .sum::<usize>()
        .saturating_add(header.len())
        .saturating_add(footer.len());
    let mut result = String::with_capacity(estimated);
    result.push_str(header);
    for item in entries {
        let host = url::Url::parse(item)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        let title = html::esc(&host);
        let safe = html::esc(item);
        result.push_str(&format!(
            "<item><title>{title}</title><link>{safe}</link></item>"
        ));
    }
    result.push_str(footer);
    result
}

async fn feed_head(State(site): State<Site>, Path(filename): Path<String>) -> Response {
    let Some((name, format)) = feed_name(&filename) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let published = match read(move || site.store.feed_published(name)).await {
        Ok(value) => value,
        Err(_) => return unavailable(),
    };
    let content_type = match format {
        "json" => "application/json; charset=utf-8",
        "rss" => "application/rss+xml; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    };
    let cache = if published {
        "public, max-age=30, s-maxage=60"
    } else {
        "no-store"
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache),
        ],
    )
        .into_response()
}

async fn feed(
    State(site): State<Site>,
    Path(filename): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some((name, format)) = feed_name(&filename) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    // Keep the permit until the complete body has left the response stream.
    // Otherwise many simultaneous large RSS requests can exceed the web memory budget.
    let permit = match site.feed_permits.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [
                    (header::RETRY_AFTER, "2"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                "feed busy",
            )
                .into_response();
        }
    };
    let entries = match read(move || site.store.feed(name)).await {
        Ok(value) => value,
        Err(_) => return unavailable(),
    };
    let published = entries.is_some();
    let entries = entries.unwrap_or_default();
    let (body, content_type) = match format {
        "json" => (
            serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_owned()),
            "application/json; charset=utf-8",
        ),
        "rss" => (rss(&entries), "application/rss+xml; charset=utf-8"),
        _ => (
            if entries.is_empty() {
                String::new()
            } else {
                format!("{}\n", entries.join("\n"))
            },
            "text/plain; charset=utf-8",
        ),
    };
    if !published {
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response();
    }
    let etag = format!("\"{}\"", hex::encode(Sha256::digest(body.as_bytes())));
    let matches = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|token| token.trim() == "*" || token.trim().trim_start_matches("W/") == etag)
        });
    let mut response = if matches {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let bytes = Bytes::from(body);
        let stream = futures_util::stream::unfold(
            (bytes, 0usize, permit),
            |(bytes, offset, permit)| async move {
                if offset >= bytes.len() {
                    return None;
                }
                let end = (offset + 65_536).min(bytes.len());
                let chunk = bytes.slice(offset..end);
                Some((Ok::<Bytes, Infallible>(chunk), (bytes, end, permit)))
            },
        );
        Response::new(Body::from_stream(stream))
    };
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=30, s-maxage=60"),
    );
    if let Ok(value) = HeaderValue::from_str(&etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

async fn style() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        include_str!("../assets/style.css"),
    )
        .into_response()
}

async fn tools_script() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        include_str!("../assets/tools.js"),
    )
        .into_response()
}

async fn charts_script() -> Response {
    asset(
        "text/javascript; charset=utf-8",
        include_bytes!("../assets/charts.js"),
    )
}
async fn chart_library() -> Response {
    asset(
        "text/javascript; charset=utf-8",
        include_bytes!("../assets/chart.umd.js"),
    )
}
async fn brand_svg() -> Response {
    asset("image/svg+xml", include_bytes!("../assets/brand.svg"))
}
async fn brand_png() -> Response {
    asset("image/png", include_bytes!("../assets/brand.png"))
}
async fn favicon() -> Response {
    asset("image/x-icon", include_bytes!("../assets/favicon.ico"))
}
async fn flag(Path(name): Path<String>) -> Response {
    let code = name.strip_suffix(".svg").unwrap_or("");
    match flags::flag(code) {
        Some(body) => asset("image/svg+xml", body.as_bytes()),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
fn asset(content_type: &'static str, bytes: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        bytes,
    )
        .into_response()
}

fn not_found_page(lang: Lang) -> Response {
    let title = lang.pick("未找到", "Not found");
    (
        StatusCode::NOT_FOUND,
        Html(html::shell(
            title,
            lang,
            &format!(
                "<section class=\"page-head\"><h1>{}</h1><p>{}</p></section>",
                title,
                lang.pick(
                    "该页面或端点不存在。",
                    "This page or endpoint does not exist."
                )
            ),
        )),
    )
        .into_response()
}

fn unavailable_page(lang: Lang) -> Response {
    let title = lang.pick("数据暂不可用", "Data temporarily unavailable");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CACHE_CONTROL, "no-store")],
        Html(html::shell(
            title,
            lang,
            &format!(
                "<section class=\"page-head\"><h1>{title}</h1><p>{}</p></section>",
                lang.pick("请稍后重试。", "Please try again shortly.")
            ),
        )),
    )
        .into_response()
}

async fn not_found() -> Response {
    not_found_page(Lang::Zh)
}

#[cfg(test)]
mod tests;
