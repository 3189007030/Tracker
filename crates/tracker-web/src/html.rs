//! Server-rendered pages. All public data is untrusted and escaped here.
use chrono::{DateTime, Utc};
use tracker_model::{Endpoint, FamilyStatus, Observation, Overview, PoolStatus, VERSION};

use crate::store::Page;

#[derive(Clone, Copy)]
pub(crate) enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn from_param(value: Option<&str>) -> Self {
        if value == Some("en") {
            Self::En
        } else {
            Self::Zh
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
        }
    }
    pub fn pick<'a>(self, zh: &'a str, en: &'a str) -> &'a str {
        match self {
            Self::Zh => zh,
            Self::En => en,
        }
    }
}

pub(crate) fn esc(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

pub(crate) fn time(value: Option<i64>, lang: Lang) -> String {
    value
        .and_then(DateTime::<Utc>::from_timestamp_secs)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| lang.pick("未知", "Unknown").to_owned())
}

pub(crate) fn shell(title: &str, lang: Lang, body: &str) -> String {
    let (nav_home, nav_trackers, nav_sources, nav_declaration) = match lang {
        Lang::Zh => ("首页", "列表", "来源", "声明"),
        Lang::En => ("Home", "List", "Sources", "Statement"),
    };
    let language = match lang {
        Lang::Zh => {
            "<span aria-current=\"true\">中文</span> <a href=\"?lang=en\" lang=\"en\">English</a>"
        }
        Lang::En => {
            "<a href=\"?lang=zh\" lang=\"zh\">中文</a> <span aria-current=\"true\">English</span>"
        }
    };
    let code = lang.code();
    format!(
        r#"<!doctype html><html lang="{code}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="light dark"><title>{title} · Tracker Observatory</title><link rel="icon" type="image/svg+xml" href="/assets/brand.svg?v=1.0.2"><link rel="icon" href="/favicon.ico?v=1.0.2" sizes="any"><link rel="apple-touch-icon" href="/assets/brand.png?v=1.0.2"><link rel="stylesheet" href="/assets/style.css?v=1.0.3"></head><body><header class="masthead"><div class="wrap nav-wrap"><a class="brand" href="/?lang={code}"><img class="brand-icon" src="/assets/brand.svg?v=1.0.2" alt=""><span>Tracker <b>Observatory</b></span></a><nav aria-label="{nav_label}"><a href="/?lang={code}">{nav_home}</a><a href="/trackers?lang={code}">{nav_trackers}</a><a href="/sources?lang={code}">{nav_sources}</a><a href="/declaration?lang={code}">{nav_declaration}</a></nav><div class="nav-actions"><div class="language" aria-label="Language">{language}</div><a class="github-link" href="https://github.com/3189007030/Tracker" rel="noopener noreferrer" aria-label="{github_label}" title="{github_label}"><svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true" fill="currentColor"><path d="M12 .5a11.5 11.5 0 0 0-3.64 22.41c.58.11.79-.25.79-.56v-2.19c-3.21.7-3.89-1.36-3.89-1.36-.53-1.34-1.29-1.7-1.29-1.7-1.05-.72.08-.7.08-.7 1.16.08 1.77 1.19 1.77 1.19 1.03 1.77 2.7 1.26 3.36.97.11-.75.4-1.26.73-1.55-2.56-.29-5.26-1.28-5.26-5.71 0-1.26.45-2.29 1.18-3.1-.12-.29-.51-1.46.11-3.05 0 0 .97-.31 3.16 1.18a10.98 10.98 0 0 1 5.75 0c2.19-1.49 3.15-1.18 3.15-1.18.63 1.59.24 2.76.12 3.05.74.81 1.18 1.84 1.18 3.1 0 4.44-2.7 5.41-5.28 5.7.42.36.78 1.06.78 2.14v3.22c0 .31.21.68.79.56A11.5 11.5 0 0 0 12 .5Z"/></svg></a></div></div></header><main class="wrap">{body}</main><footer class="wrap footer"><span>Tracker Observatory · {version}</span><span>{footnote}</span></footer><script src="/assets/tools.js?v=1.0.2" defer></script></body></html>"#,
        title = esc(title),
        version = esc(VERSION),
        nav_label = lang.pick("主导航", "Main navigation"),
        github_label = lang.pick("GitHub 代码仓库", "GitHub repository"),
        footnote = lang.pick(
            "单点观测 · 基于真实证据",
            "Single vantage point · Evidence-led"
        )
    )
}

fn stat(label: &str, value: &str, note: &str) -> String {
    format!(
        r#"<div class="metric"><span>{}</span><strong>{}</strong><small>{}</small></div>"#,
        esc(label),
        esc(value),
        esc(note)
    )
}

fn copy_url(label: &str, url: &str, lang: Lang) -> String {
    let name = format!("{}: {}", lang.pick("复制", "Copy"), label);
    format!(
        r#"<div class="copy-row compact-copy" data-copy-row data-copy-ok="{}" data-copy-fail="{}"><span class="copy-label">{}</span><div class="copy-controls"><input type="text" value="{}" aria-label="{}" readonly spellcheck="false"><button class="copy-icon" type="button" data-copy-button hidden aria-label="{}" title="{}"><svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path d="M8 7V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2h-3M5 7h9a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2Z" fill="none" stroke="currentColor" stroke-width="1.8"/></svg></button></div><small class="copy-status" role="status" aria-live="polite"></small></div>"#,
        lang.pick("已复制。", "Copied."),
        lang.pick(
            "无法自动复制，请选中地址后手动复制。",
            "Could not copy automatically. Select the address and copy it manually."
        ),
        esc(label),
        esc(url),
        esc(label),
        esc(&name),
        esc(&name)
    )
}

fn copy_feed_link(lang: Lang) -> String {
    let name = lang.pick("复制标准订阅地址", "Copy standard subscription URL");
    format!(
        r#"<span class="copy-feed" data-copy-row data-copy-ok="{}" data-copy-fail="{}"><a href="/trackers.txt">trackers.txt ↗</a><input type="text" value="https://tracker.augrains.com/trackers.txt" aria-label="{}" readonly hidden><button class="copy-icon" type="button" data-copy-button hidden aria-label="{}" title="{}"><svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path d="M8 7V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2h-3M5 7h9a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2Z" fill="none" stroke="currentColor" stroke-width="1.8"/></svg></button><small class="copy-status" role="status" aria-live="polite"></small></span>"#,
        lang.pick("已复制。", "Copied."),
        lang.pick(
            "无法自动复制，请选中地址后手动复制。",
            "Could not copy automatically. Select the address and copy it manually."
        ),
        esc(name),
        esc(name),
        esc(name)
    )
}

fn charts(lang: Lang, runtime: &str) -> String {
    let title = lang.pick("统计趋势", "Trends");
    let fields: &[(&str, &str, &str)] = &[
        ("peers", "登记节点", "Registered peers"),
        ("swarms", "蜂群", "Swarms"),
        (
            "accepted_announces",
            "成功通告 / 秒",
            "Accepted announces / sec",
        ),
        (
            "protocol",
            "HTTP / UDP 通告 / 秒",
            "HTTP / UDP announces / sec",
        ),
    ];
    let mut panels = String::new();
    for (key, zh, en) in fields {
        let label = lang.pick(zh, en);
        panels.push_str(&format!(r#"<article class="panel chart-panel"><h3>{}</h3><div class="canvas-wrap"><canvas data-history-chart="{}" aria-label="{}" role="img"></canvas></div></article>"#, esc(label), esc(key), esc(label)));
    }
    format!(
        r#"<section class="trends-heading"><div class="trends-title-row"><h2>{}</h2>{runtime}</div><div class="trends-sub-row"><p>{}</p><label>{} <select data-history-window><option value="24h">24h</option><option value="7d">7d</option><option value="30d">30d</option></select></label></div></section><div class="history-grid" data-history-root data-lang="{}">{}</div><p class="footnote" data-history-rate-note></p><p class="footnote">{}</p><script src="/assets/chart.umd.js?v=4.6.1" defer></script><script src="/assets/charts.js?v=1.0.2-ui2" defer></script>"#,
        title,
        lang.pick(
            "按实际采样显示，缺测和重启后的间断不会补零。",
            "Recorded samples only; gaps and process restarts are not filled with zero."
        ),
        lang.pick("时间范围", "Window"),
        lang.code(),
        panels,
        lang.pick(
            "节点数表示本站登记，并非在线可达保证。",
            "Peer counts are local registrations, not proof of reachability."
        )
    )
}

pub(crate) fn home(_overview: &Overview, native: Option<&serde_json::Value>, lang: Lang) -> String {
    let count = |key: &str| {
        native
            .and_then(|v| v.get(key))
            .and_then(serde_json::Value::as_u64)
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".to_owned())
    };
    let mut addresses = String::new();
    for (label, url) in [
        ("HTTPS", "https://announce.augrains.com/announce"),
        ("HTTP", "http://announce.augrains.com/announce"),
        ("UDP", "udp://announce.augrains.com:6969/announce"),
    ] {
        addresses.push_str(&copy_url(label, url, lang));
    }
    let mut feeds = String::new();
    for name in [
        "trackers",
        "trackers_best",
        "trackers_all",
        "trackers_udp",
        "trackers_http",
        "trackers_https",
    ] {
        let path = format!("/{name}.txt");
        feeds.push_str(&format!(
            r#"<div class="subscription-row"><a href="{}">{}.txt ↗</a>{}</div>"#,
            esc(&path),
            esc(name),
            copy_url(
                lang.pick("订阅地址", "Subscription URL"),
                &format!("https://tracker.augrains.com{path}"),
                lang
            )
        ));
    }
    let runtime = format!(
        r#"<div class="runtime-strip" aria-label="{}"><div><span>{}</span><strong>{}</strong></div><div><span>{}</span><strong>{}</strong></div><div><span>{}</span><strong>{}</strong></div><small>{}</small></div>"#,
        lang.pick("当前运行统计", "Current runtime statistics"),
        lang.pick("登记节点", "Registered peers"),
        count("peers"),
        lang.pick("蜂群", "Swarms"),
        count("swarms"),
        lang.pick("本次启动后成功通告", "Accepted announces since start"),
        count("announces"),
        lang.pick("按分钟采样", "Sampled each minute")
    );
    let body = format!(r#"{}<div class="home-address-grid"><section id="native-trackers" class="panel prose"><h2>{}</h2><p>{}</p><div class="copy-list">{addresses}</div><p>{}</p></section><section class="panel prose"><h2>{}</h2><p>{}</p><div class="subscriptions">{feeds}</div></section></div>"#,
        charts(lang, &runtime),
        lang.pick("本站 Tracker 地址", "Native tracker addresses"), lang.pick("在 BT 客户端的种子 Tracker 列表中，每行添加一个地址并保存。", "Add each address on a separate line in your torrent's tracker list, then save."), lang.pick("这是 BT 协议入口，浏览器不提供通告参数；UDP 不是网页。", "These are BT protocol endpoints. Browsers do not supply announce parameters; UDP is not a web page."),
        lang.pick("订阅列表", "Subscriptions"), lang.pick("将订阅地址加入支持自动更新列表的客户端。证据不足时列表可以为空。", "Add a subscription URL to a compatible client. Lists may be empty while evidence accumulates."));
    shell(lang.pick("首页", "Home"), lang, &body)
}

fn pct(value: Option<f64>, lang: Lang) -> String {
    value
        .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
        .map(|v| format!("{:.1}%", v * 100.0))
        .unwrap_or_else(|| lang.pick("未知", "Unknown").to_owned())
}

fn millis(value: Option<f64>, lang: Lang) -> String {
    value
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| format!("{v:.0} ms"))
        .unwrap_or_else(|| lang.pick("未知", "Unknown").to_owned())
}

fn state_text(value: &str, lang: Lang) -> &'static str {
    match value {
        "success" => lang.pick("成功", "Success"),
        "failure" => lang.pick("失败", "Failed"),
        "partial" => lang.pick("部分异常", "Partial"),
        "unsupported" => lang.pick("未发现地址", "No address"),
        "local_unavailable" => lang.pick("本地不可用", "Local unavailable"),
        "stale" => lang.pick("已过期", "Stale"),
        _ => lang.pick("未知 / 未测", "Unknown / untested"),
    }
}

fn result_text(value: &str, lang: Lang) -> &'static str {
    match value {
        "announce_ok" | "accepted" | "success" => lang.pick("通告成功", "Announce accepted"),
        "zero_peers" => lang.pick("通告成功，无节点", "Announce accepted, no peers"),
        "rejected" | "tracker_rejection" | "resource_unregistered" => {
            lang.pick("协议拒绝", "Protocol rejection")
        }
        "timeout" | "connect_timeout" | "response_timeout" => lang.pick("超时", "Timed out"),
        "http_error" => lang.pick("HTTP 错误", "HTTP error"),
        "invalid_protocol" | "invalid_response" | "protocol_error" => {
            lang.pick("协议响应无效", "Invalid protocol response")
        }
        "transport" => lang.pick("传输失败", "Transport failure"),
        "dns" | "dns_error" => lang.pick("DNS 失败", "DNS failure"),
        "rate_limited" => lang.pick("频率限制", "Rate limited"),
        "local_unavailable" | "network_unavailable" => {
            lang.pick("本地网络不可用", "Local network unavailable")
        }
        "local_restriction" | "skipped" => lang.pick("本地跳过", "Locally deferred"),
        "unsupported" => lang.pick("未发现地址", "No address"),
        "handshake_success" => lang.pick("握手成功", "Handshake succeeded"),
        "handshake_failed" => lang.pick("握手失败", "Handshake failed"),
        "bitmap_available" => lang.pick("收到位图声明", "Bitmap claim received"),
        "bitmap_missing" => lang.pick("无位图", "Bitmap unavailable"),
        "failure" => lang.pick("失败", "Failed"),
        _ => lang.pick("未知", "Unknown"),
    }
}

fn observed_result(stage: &str, value: &str, error: Option<&str>, lang: Lang) -> &'static str {
    if error == Some("incomplete_protocol") {
        return lang.pick(
            "协议回复不完整，未验证",
            "Incomplete protocol reply; unverified",
        );
    }
    if stage == "handshake" {
        return match (value, error) {
            ("accepted", _) => lang.pick("节点握手成功", "Peer handshake succeeded"),
            ("failed", Some("timeout")) => lang.pick("节点握手超时", "Peer handshake timed out"),
            ("failed", _) => lang.pick("节点握手失败", "Peer handshake failed"),
            (_, Some("budget_exhausted")) => lang.pick("本地预算不足", "Local budget exhausted"),
            (_, Some("local_unavailable")) => {
                lang.pick("本地网络不可用", "Local network unavailable")
            }
            _ => lang.pick("握手结果未知", "Handshake result unknown"),
        };
    }
    if stage == "sample_announce" {
        match (value, error) {
            ("accepted", Some("peers_unusable")) => {
                return lang.pick(
                    "有效回复，但无可用于验证的节点",
                    "Valid reply, but no usable peers for verification",
                );
            }
            ("accepted", _) => return lang.pick("已获得候选节点", "Candidate peers returned"),
            ("empty", _) => return lang.pick("有效回复，节点为空", "Valid reply, no peers"),
            ("failed", Some("timeout")) => {
                return lang.pick("样本通告超时", "Sample announce timed out");
            }
            ("failed", None) => return lang.pick("样本通告失败", "Sample announce failed"),
            ("unknown", None) => {
                return lang.pick("样本通告结果未知", "Sample announce result unknown");
            }
            _ => {}
        }
    }
    match error {
        Some("tracker_rejection" | "resource_unregistered") => {
            lang.pick("协议拒绝", "Protocol rejection")
        }
        Some("rate_limited") => lang.pick("频率限制", "Rate limited"),
        Some("invalid_protocol" | "invalid_response" | "protocol_error") => {
            lang.pick("协议响应无效", "Invalid protocol response")
        }
        Some("http_error" | "http_status") => lang.pick("HTTP 错误", "HTTP error"),
        Some("family_unavailable") => lang.pick("未发现地址", "No address"),
        Some("local_or_network_unknown" | "local_unavailable" | "network_unavailable") => {
            lang.pick("本地网络不可用", "Local network unavailable")
        }
        Some("operator_denied" | "local_restriction" | "budget_exhausted") => {
            lang.pick("本地跳过", "Locally deferred")
        }
        Some("timeout" | "connect_timeout" | "response_timeout" | "dns_timeout") => {
            lang.pick("超时", "Timed out")
        }
        _ => result_text(value, lang),
    }
}

fn stage_text(stage: &str, lang: Lang) -> &'static str {
    match stage {
        "announce" => lang.pick("健康通告", "Health announce"),
        "sample_announce" => lang.pick("样本通告", "Sample announce"),
        "handshake" => lang.pick("节点握手", "Peer handshake"),
        "bitmap" => lang.pick("位图观测", "Bitmap observation"),
        _ => lang.pick("未知", "Unknown"),
    }
}

fn observation_state(item: &Observation) -> &'static str {
    let reason = item.error_category.as_deref().unwrap_or("");
    if matches!(
        reason,
        "rate_limited" | "tracker_rejection" | "resource_unregistered" | "peers_unusable"
    ) || matches!(
        item.result.as_str(),
        "rate_limited" | "rejected" | "tracker_rejection" | "resource_unregistered"
    ) {
        "neutral"
    } else if matches!(
        reason,
        "operator_denied"
            | "incomplete_protocol"
            | "family_unavailable"
            | "local_or_network_unknown"
            | "local_unavailable"
            | "local_restriction"
            | "network_unavailable"
            | "budget_exhausted"
            | "dns_timeout"
    ) || matches!(
        item.result.as_str(),
        "unknown"
            | "local_unavailable"
            | "local_restriction"
            | "network_unavailable"
            | "skipped"
            | "unsupported"
    ) {
        "unknown"
    } else if matches!(
        item.result.as_str(),
        "announce_ok" | "accepted" | "success" | "zero_peers"
    ) {
        "success"
    } else {
        "failure"
    }
}

fn measured_reply(item: &Observation) -> bool {
    if matches!(
        item.error_category.as_deref(),
        Some(
            "timeout"
                | "connect_timeout"
                | "response_timeout"
                | "dns_timeout"
                | "dns_error"
                | "local_unavailable"
                | "local_or_network_unknown"
                | "network_unavailable"
                | "family_unavailable"
                | "operator_denied"
                | "budget_exhausted"
        )
    ) {
        return false;
    }
    item.latency_ms.is_some()
        && (item.http_status.is_some()
            || item.protocol_reply
            || matches!(
                item.result.as_str(),
                "announce_ok"
                    | "accepted"
                    | "success"
                    | "zero_peers"
                    | "rejected"
                    | "tracker_rejection"
                    | "resource_unregistered"
            ))
}

fn pill(family: &str, status: Option<&FamilyStatus>, lang: Lang) -> String {
    let state = status.map(|s| s.state.as_str()).unwrap_or("unknown");
    let label = format!("{} · {}", family.to_uppercase(), state_text(state, lang));
    format!(
        r#"<span class="pill pill--{}">{}</span>"#,
        esc(state),
        esc(&label)
    )
}

fn country_flag(country: Option<&str>, lang: Lang) -> String {
    let Some(code) = country.filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()))
    else {
        return format!(
            r#"<span class="country-unknown" title="{}">—</span>"#,
            lang.pick("注册国家未知", "Registration country unknown")
        );
    };
    let lower = code.to_ascii_lowercase();
    if crate::flags::flag(&lower).is_none() {
        return format!(
            r#"<span class="country-unknown" title="{}">—</span>"#,
            lang.pick("注册国家未知", "Registration country unknown")
        );
    }
    let title = format!(
        "{}: {}",
        lang.pick(
            "网络前缀注册国家，非机房定位",
            "Network-prefix registration country, not server location"
        ),
        code
    );
    format!(
        r#"<img class="country-flag" src="/assets/flags/{lower}.svg" alt="{}" title="{}">"#,
        esc(&title),
        esc(&title)
    )
}

pub(crate) fn listing(page: &Page, query: &str, protocol: &str, lang: Lang) -> String {
    let code = lang.code();
    let mut rows = String::new();
    for (offset, endpoint) in page.items.iter().enumerate() {
        let newest = !endpoint.baseline
            && endpoint.first_seen > 0
            && (0..86_400).contains(&tracker_model::unix_now().saturating_sub(endpoint.first_seen));
        let new = if newest {
            format!(
                r#" <span class="new-label" title="{}">NEW</span>"#,
                lang.pick(
                    "本站近24小时新发现",
                    "First discovered here within 24 hours"
                )
            )
        } else {
            String::new()
        };
        let v4 = pill(
            "ipv4",
            endpoint.families.iter().find(|f| f.family == "ipv4"),
            lang,
        );
        let v6 = pill(
            "ipv6",
            endpoint.families.iter().find(|f| f.family == "ipv6"),
            lang,
        );
        let flag = country_flag(endpoint.infrastructure.country.as_deref(), lang);
        let rank = page
            .page
            .saturating_sub(1)
            .saturating_mul(page.limit)
            .saturating_add(offset + 1);
        rows.push_str(&format!(r#"<article class="tracker-row"><span class="tracker-rank">{rank}</span><div class="tracker-name">{flag} <a href="/tracker/{}?lang={code}">{}</a>{new}<div class="url">{}</div></div><div class="stack-pills">{v4}{v6}</div><div class="row-stat"><small>{}</small><strong>{}</strong></div><div class="row-stat"><small>{}</small><strong>{}</strong></div><a class="row-link" href="/tracker/{}?lang={code}" aria-label="{}">↗</a></article>"#,
            esc(&endpoint.id), esc(&endpoint.host), copy_url(lang.pick("Tracker 地址", "Tracker URL"), &endpoint.url, lang), lang.pick("24h 成功率", "24h success"),
            esc(&pct(endpoint.evidence.success_rate_24h, lang)), lang.pick("响应延迟中位数", "Median latency"),
            esc(&millis(endpoint.evidence.median_latency_ms, lang)), esc(&endpoint.id), lang.pick("查看详情", "Details")));
    }
    if rows.is_empty() {
        let message = if !page.has_snapshot {
            lang.pick(
                "公开快照尚未生成。",
                "The public snapshot has not been generated yet.",
            )
        } else if page.warming && query.is_empty() && protocol.is_empty() {
            lang.pick(
                "观测证据仍在积累，暂无端点。",
                "Evidence is still accumulating; no endpoints are available yet.",
            )
        } else {
            lang.pick("没有符合条件的端点。", "No endpoints match these filters.")
        };
        rows = format!(r#"<div class="empty">{}</div>"#, message);
    }
    let selected = |name: &str| if protocol == name { " selected" } else { "" };
    let query_url: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("lang", code)
        .append_pair("q", query)
        .append_pair("protocol", protocol)
        .finish();
    let previous = if page.page > 1 {
        format!(
            r#"<a href="/trackers?{}&amp;page={}">← {}</a>"#,
            esc(&query_url),
            page.page - 1,
            lang.pick("上一页", "Previous")
        )
    } else {
        String::new()
    };
    let next = if page
        .total
        .is_some_and(|total| (page.page as u64).saturating_mul(page.limit as u64) < total)
    {
        format!(
            r#"<a href="/trackers?{}&amp;page={}">{} →</a>"#,
            esc(&query_url),
            page.page + 1,
            lang.pick("下一页", "Next")
        )
    } else {
        String::new()
    };
    let body = format!(
        r#"<form class="filters card" method="get" action="/trackers"><input type="hidden" name="lang" value="{code}"><label>{search}<input name="q" value="{query}" maxlength="128" placeholder="{placeholder}"></label><label>{proto}<select name="protocol"><option value="">{all}</option><option value="udp"{udp}>UDP</option><option value="http"{http}>HTTP</option><option value="https"{https}>HTTPS</option></select></label><button class="button" type="submit">{filter}</button></form><div class="list-meta"><span>{count}: {total}</span>{feed_link}</div><div class="tracker-list">{rows}</div><div class="pagination">{previous}<span>{page_label} {current}</span>{next}</div>"#,
        feed_link = copy_feed_link(lang),
        search = lang.pick("搜索", "Search"),
        query = esc(query),
        placeholder = lang.pick("域名或地址", "Host or address"),
        proto = lang.pick("协议", "Protocol"),
        all = lang.pick("全部协议", "All protocols"),
        udp = selected("udp"),
        http = selected("http"),
        https = selected("https"),
        filter = lang.pick("筛选", "Filter"),
        count = lang.pick("端点", "Endpoints"),
        total = page
            .total
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".to_owned()),
        page_label = lang.pick("第", "Page"),
        current = page.page
    );
    shell(lang.pick("Tracker 列表", "Trackers"), lang, &body)
}

fn latest(f: Option<&FamilyStatus>, protocol: &str, lang: Lang) -> String {
    let Some(f) = f else {
        return esc(lang.pick("未知 / 未测", "Unknown / untested"));
    };
    let mut parts = Vec::new();
    if let Some(code) = f.http_status.filter(|_| protocol != "udp") {
        parts.push(format!("HTTP {code}"));
    }
    if let Some(result) = f.protocol_result.as_deref() {
        parts.push(
            observed_result("announce", result, f.error_category.as_deref(), lang).to_owned(),
        );
    } else if f.error_category.is_some() {
        parts.push(
            observed_result("announce", "unknown", f.error_category.as_deref(), lang).to_owned(),
        );
    }
    if parts.is_empty() {
        parts.push(state_text(&f.state, lang).to_owned());
    }
    esc(&parts.join(" · "))
}

fn chart(observations: &[Observation], now: i64, lang: Lang) -> String {
    let mut points: Vec<_> = observations
        .iter()
        .filter(|o| o.stage == "announce")
        .collect();
    points.sort_by_key(|o| o.observed_at);
    if !points.iter().any(|o| measured_reply(o)) {
        return format!(
            r#"<div class="empty chart-empty">{}</div>"#,
            lang.pick("暂无带延迟的观测点。", "No latency observations yet.")
        );
    }
    let max = points
        .iter()
        .filter(|o| measured_reply(o))
        .filter_map(|o| o.latency_ms)
        .fold(50.0_f64, f64::max)
        .min(600_000.0);
    let start = now.saturating_sub(86_400);
    let x = |t: i64| 38.0 + ((t - start).clamp(0, 86_400) as f64 / 86_400.0) * 700.0;
    let y = |latency: f64| 180.0 - (latency / max) * 150.0;
    let mut svg = String::new();
    for family in ["ipv4", "ipv6"] {
        let mut previous: Option<&Observation> = None;
        for item in points.iter().filter(|p| p.family == family) {
            if !measured_reply(item) {
                previous = None;
                continue;
            }
            let Some(latency) = item.latency_ms else {
                previous = None;
                continue;
            };
            let state = observation_state(item);
            let ok = state == "success";
            if ok {
                if let Some(old) = previous.filter(|old| item.observed_at - old.observed_at <= 7200)
                {
                    svg.push_str(&format!(r#"<line class="chart-line {family}" x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}"/>"#, x(old.observed_at), y(old.latency_ms.unwrap_or(0.0)), x(item.observed_at), y(latency)));
                }
                previous = Some(item);
            } else {
                previous = None;
            }
            let title = format!(
                "{} · {} · {} · {:.0} ms",
                time(Some(item.observed_at), lang),
                family.to_uppercase(),
                observed_result(
                    &item.stage,
                    &item.result,
                    item.error_category.as_deref(),
                    lang
                ),
                latency
            );
            let class = match state {
                "neutral" => " is-neutral",
                "failure" => " is-failure",
                "unknown" => " is-unknown",
                _ => "",
            };
            svg.push_str(&format!(r#"<circle class="chart-point {family}{class}" cx="{:.2}" cy="{:.2}" r="4"><title>{}</title></circle>"#, x(item.observed_at), y(latency), esc(&title)));
        }
    }
    format!(
        r#"<svg class="chart" viewBox="0 0 760 210" role="img" aria-label="{}"><line class="chart-axis" x1="38" y1="180" x2="738" y2="180"/><line class="chart-grid" x1="38" y1="30" x2="738" y2="30"/><text x="2" y="34">{max:.0}</text><text x="16" y="184">0</text><text x="38" y="204">−24 h</text><text x="710" y="204">{}</text>{svg}</svg>"#,
        lang.pick(
            "24小时已观测响应延迟，单位毫秒",
            "Observed response latency over 24 hours, in milliseconds"
        ),
        lang.pick("现在", "Now")
    )
}

fn supported_families(endpoint: &Endpoint, observations: &[Observation]) -> Vec<&'static str> {
    ["ipv4", "ipv6"]
        .into_iter()
        .filter(|family| {
            endpoint.families.iter().any(|f| {
                f.family == *family
                    && f.state != "unsupported"
                    && f.protocol_result.as_deref() != Some("unsupported")
                    && !matches!(
                        f.error_category.as_deref(),
                        Some("family_unavailable" | "unsupported")
                    )
            }) || observations.iter().any(|o| {
                o.stage == "announce"
                    && o.family == *family
                    && o.result != "unsupported"
                    && !matches!(
                        o.error_category.as_deref(),
                        Some("family_unavailable" | "unsupported")
                    )
            })
        })
        .collect()
}

fn timeline(observations: &[Observation], families: &[&str], now: i64, lang: Lang) -> String {
    if families.is_empty() {
        return format!(
            r#"<div class="empty chart-empty">{}</div>"#,
            lang.pick(
                "暂无可用地址族的观测。",
                "No observations for a supported address family."
            )
        );
    }
    let start = now.saturating_sub(86_400);
    let axis_y = 20 + families.len() * 30;
    let mut content = String::new();
    for (index, family) in families.iter().enumerate() {
        let y = 20 + index * 30;
        let display = if *family == "ipv4" { "IPv4" } else { "IPv6" };
        content.push_str(&format!(r#"<text x="0" y="{}">{display}</text>"#, y + 4));
        let mut items: Vec<_> = observations
            .iter()
            .filter(|o| o.stage == "announce" && o.family == *family)
            .collect();
        items.sort_by_key(|o| o.observed_at);
        if items.is_empty() {
            content.push_str(&format!(
                r#"<text class="timeline-untested" x="60" y="{}">{}</text>"#,
                y + 4,
                lang.pick("未测", "Not yet checked")
            ));
        }
        let mut previous: Option<&Observation> = None;
        for item in items {
            let x = 52.0 + ((item.observed_at - start).clamp(0, 86_400) as f64 / 86_400.0) * 675.0;
            let state = observation_state(item);
            if let Some(old) = previous.filter(|old| {
                item.observed_at > old.observed_at
                    && item.observed_at - old.observed_at <= 7200
                    && observation_state(old) != "unknown"
                    && state != "unknown"
            }) {
                let old_x =
                    52.0 + ((old.observed_at - start).clamp(0, 86_400) as f64 / 86_400.0) * 675.0;
                content.push_str(&format!(r#"<line class="timeline-segment" x1="{old_x:.2}" y1="{y}" x2="{x:.2}" y2="{y}"/>"#));
            }
            let when = DateTime::<Utc>::from_timestamp_secs(item.observed_at)
                .map(|v| v.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                .unwrap_or_else(|| lang.pick("未知", "Unknown").to_owned());
            let title = format!(
                "{} · {} · {}",
                when,
                display,
                observed_result(
                    &item.stage,
                    &item.result,
                    item.error_category.as_deref(),
                    lang
                )
            );
            content.push_str(&format!(r#"<circle class="timeline-point {state}" cx="{x:.2}" cy="{y}" r="4"><title>{}</title></circle>"#, esc(&title)));
            previous = Some(item);
        }
    }
    format!(
        r#"<svg class="timeline" viewBox="0 0 760 {}" role="img" aria-label="{}"><line class="timeline-axis" x1="52" y1="{axis_y}" x2="727" y2="{axis_y}"/><text x="52" y="{}">−24 h</text><text x="700" y="{}">{}</text>{content}</svg>"#,
        axis_y + 20,
        lang.pick(
            "过去24小时逐次通告观测",
            "Individual announce observations over 24 hours"
        ),
        axis_y + 16,
        axis_y + 16,
        lang.pick("现在", "Now")
    )
}

pub(crate) fn detail(
    endpoint: &Endpoint,
    _observations: &[Observation],
    health_observations: &[Observation],
    now: i64,
    lang: Lang,
) -> String {
    let v4 = endpoint.families.iter().find(|f| f.family == "ipv4");
    let v6 = endpoint.families.iter().find(|f| f.family == "ipv6");
    let mut rows = String::new();
    for o in health_observations.iter().take(100) {
        let transport = if o.stage == "handshake" {
            "TCP".to_owned()
        } else if endpoint.protocol != "udp" {
            o.http_status
                .map(|x| format!("HTTP {x}"))
                .unwrap_or_else(|| "—".to_owned())
        } else {
            "UDP".to_owned()
        };
        let reply = if matches!(o.stage.as_str(), "handshake" | "bitmap" | "unknown") {
            "—"
        } else if o.protocol_reply {
            lang.pick("是", "Yes")
        } else if (o.stage == "sample_announce"
            && matches!(o.result.as_str(), "accepted" | "empty"))
            || observation_state(o) == "unknown"
        {
            lang.pick("未知", "Unknown")
        } else {
            lang.pick("否", "No")
        };
        rows.push_str(&format!(r#"<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>"#,
            esc(&time(Some(o.observed_at), lang)), esc(&o.family.to_uppercase()), esc(stage_text(&o.stage, lang)), esc(&transport), esc(observed_result(&o.stage, &o.result, o.error_category.as_deref(), lang)), reply, esc(&millis(if measured_reply(o) { o.latency_ms } else { None }, lang)), esc(o.error_category.as_deref().unwrap_or("—"))));
    }
    if rows.is_empty() {
        rows = format!(
            r#"<tr><td colspan="8">{}</td></tr>"#,
            lang.pick("暂无记录。", "No observations yet.")
        );
    }
    let evidence = &endpoint.evidence;
    let metrics = [
        stat(
            lang.pick("24h 通告成功率", "24h announce success"),
            &pct(evidence.success_rate_24h, lang),
            &format!("{} / {}", evidence.successes_24h, evidence.checks_24h),
        ),
        stat(
            lang.pick("24h 有效回复率", "24h protocol reply"),
            &pct(evidence.reply_rate_24h, lang),
            &if evidence.reply_checks_24h > 0 {
                format!("{} / {}", evidence.replies_24h, evidence.reply_checks_24h)
            } else {
                format!(
                    "{} {}",
                    evidence.replies_24h,
                    lang.pick("次有效回复", "valid replies")
                )
            },
        ),
        stat(
            lang.pick("响应延迟中位数", "Median latency"),
            &millis(evidence.median_latency_ms, lang),
            lang.pick("实际响应样本", "Observed responses"),
        ),
        stat(
            lang.pick("验证节点", "Verified peers"),
            &evidence.handshake_nodes_7d.to_string(),
            lang.pick("近 7 天握手", "Handshakes in 7 days"),
        ),
        stat(
            lang.pick("样本资源", "Sample resources"),
            &evidence.sample_count_7d.to_string(),
            lang.pick("公共样本", "Public samples"),
        ),
        stat(
            lang.pick("位图声明覆盖", "Claimed bitmap coverage"),
            &pct(evidence.coverage_mean, lang),
            &format!(
                "{} {}",
                evidence.bitmap_observations_7d,
                lang.pick("份观测", "observations")
            ),
        ),
    ]
    .concat();
    let newest = !endpoint.baseline
        && endpoint.first_seen > 0
        && (0..86_400).contains(&now.saturating_sub(endpoint.first_seen));
    let new = if newest {
        " <span class=\"new-label\">NEW</span>"
    } else {
        ""
    };
    let colocated = if endpoint.colocated {
        format!(r#"<p class="notice">{}</p>"#, lang.pick("该服务与观测点同址，路径不代表其他地区。", "This tracker shares the monitor's host; its path is not representative of other regions."))
    } else {
        String::new()
    };
    let families = supported_families(endpoint, health_observations);
    let current_families = supported_families(endpoint, &[]).join(",");
    let graph = if families.is_empty() {
        format!(
            r#"<div class="empty chart-empty">{}</div>"#,
            lang.pick(
                "暂无可用地址族的观测。",
                "No observations for a supported address family."
            )
        )
    } else {
        chart(health_observations, now, lang)
    };
    let timeline = timeline(health_observations, &families, now, lang);
    let unknown = lang.pick("未知", "Unknown");
    let infra = &endpoint.infrastructure;
    let infrastructure = [
        (
            "ASN",
            infra
                .asn
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(unknown)
                .to_owned(),
        ),
        (
            lang.pick("组织", "Organization"),
            infra
                .organization
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(unknown)
                .to_owned(),
        ),
        (
            lang.pick("国家 / 地区", "Country / region"),
            infra
                .country
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(unknown)
                .to_owned(),
        ),
        (
            lang.pick("缓存更新时间", "Cache updated"),
            time(infra.updated_at.filter(|v| *v > 0), lang),
        ),
    ]
    .into_iter()
    .map(|(label, value)| {
        format!(
            r#"<div><dt>{}</dt><dd>{}</dd></div>"#,
            esc(label),
            esc(&value)
        )
    })
    .collect::<String>();
    let family_card = |name: &str, f: Option<&FamilyStatus>| {
        format!(
            r#"<div class="family-card">{}<strong>{}</strong><small>{}: {}</small></div>"#,
            pill(name, f, lang),
            latest(f, &endpoint.protocol, lang),
            lang.pick("检测时间", "Checked"),
            esc(&time(f.and_then(|v| v.checked_at), lang))
        )
    };
    let body = format!(r#"<section class="page-head"><a class="back" href="/trackers?lang={code}">← {back}</a><p class="eyebrow">TRACKER DETAIL</p><h1>{host}{new}</h1>{url}{colocated}</section><section class="family-grid">{family4}{family6}</section><section class="section-heading"><div><p class="eyebrow">EVIDENCE</p><h2>{evidence_title}</h2><p>{evidence_note}</p></div></section><section class="metric-grid detail-metrics">{metrics}</section><section class="panel"><div class="section-heading compact"><div><p class="eyebrow">NETWORK</p><h2>{network_title}</h2><p>{network_note}</p></div></div><dl class="infra-compact">{infrastructure}</dl></section><section class="panel"><div class="section-heading compact"><div><p class="eyebrow">LAST 24 HOURS</p><h2>{history}</h2><p>{history_note}</p></div></div><p class="timeline-legend">{timeline_legend}</p><div data-observation-timeline class="observation-timeline" role="img" aria-label="24-hour timeline"></div><noscript>{timeline}</noscript><div class="chart-header"><strong>{latency_chart}</strong></div><div class="canvas-wrap"><canvas data-observation-chart role="img" aria-label="24-hour response latency"></canvas></div><noscript>{graph}</noscript><div class="table-wrap"><table><thead><tr><th>{time_label}</th><th>{family_label}</th><th>{stage}</th><th>{transport}</th><th>{result}</th><th>{reply}</th><th>{latency}</th><th>{reason}</th></tr></thead><tbody>{rows}</tbody></table></div></section><p class="footnote">{single}</p>"#,
        code=lang.code(), back=lang.pick("列表", "List"), host=esc(&endpoint.host), url=copy_url(lang.pick("Tracker 地址", "Tracker URL"), &endpoint.url, lang),
        family4=family_card("ipv4",v4), family6=family_card("ipv6",v6),
        evidence_title=lang.pick("观测证据", "Observed evidence"), evidence_note=lang.pick("握手仅验证采样节点；位图是节点声明，不证明文件可下载。", "Handshakes verify sampled peers; bitmaps are claims, not proof of downloadable content."),
        network_title=lang.pick("网络归属", "Network information"), network_note=lang.pick("归属信息来自公开网络数据，不能精确定位机房。", "Network ownership comes from public data and does not pinpoint a datacenter."),
        history=lang.pick("24 小时探测记录", "24-hour observations"), history_note=lang.pick("图和事件表仅显示健康通告；连线不代表持续可用。", "The charts and event table show health announces only. Lines do not prove continuous availability."),
        timeline_legend=lang.pick("绿：成功　红：失败　黄：限流/拒绝　灰：未知或本地不可用。IPv4 与 IPv6 两行各自代表独立探测，时间可能相近。", "Green: success · Red: failure · Amber: rate limit/rejection · Grey: unknown/local unavailable. IPv4 and IPv6 are separate checks and can occur close together."),
        latency_chart=lang.pick("实测响应延迟 (ms)", "Observed response latency (ms)"),
        time_label=lang.pick("时间 (UTC)", "Time (UTC)"), family_label=lang.pick("地址族", "Family"), stage=lang.pick("阶段", "Stage"), transport=lang.pick("传输", "Transport"), result=lang.pick("观测结果", "Observation result"), reply=lang.pick("有效 Tracker 回复", "Valid tracker reply"), latency=lang.pick("阶段延迟", "Stage latency"), reason=lang.pick("原因", "Reason"),
        single=lang.pick("观测来自单一网络位置；缺测和探针本地故障不等于 Tracker 离线。", "Measurements come from one network; missing data and local probe failures are not tracker downtime."));
    let scripts = r#"<script src="/assets/chart.umd.js?v=4.6.1" defer></script><script src="/assets/charts.js?v=1.0.2-ui2" defer></script>"#;
    shell(
        &endpoint.host,
        lang,
        &format!(
            r#"<div data-detail-root data-endpoint-id="{}" data-lang="{}" data-current-families="{}">{}{}</div>"#,
            esc(&endpoint.id),
            lang.code(),
            esc(&current_families),
            body,
            scripts
        ),
    )
}

pub(crate) fn simple(
    kind: &str,
    _overview: &Overview,
    _native: Option<&serde_json::Value>,
    pools: &[PoolStatus],
    lang: Lang,
) -> String {
    if kind == "sources" {
        let mut pool_rows = String::new();
        for pool in pools {
            let label = match pool.pool.as_str() {
                "software" => lang.pick("公共资源样本池", "Public resource pool"),
                "forum" => lang.pick("论坛样本池", "Forum samples"),
                _ => continue,
            };
            let status = match pool.status.as_str() {
                "ready" => lang.pick("正常", "Ready"),
                "partial" => lang.pick("部分来源延后", "Some sources deferred"),
                "deferred" => lang.pick("等待来源", "Waiting for sources"),
                _ => lang.pick("未知", "Unknown"),
            };
            let reason = match pool.error_category.as_deref() {
                Some("http_502") => {
                    Some(lang.pick("来源返回 HTTP 502", "Source returned HTTP 502"))
                }
                Some("robots_denied") => Some(lang.pick(
                    "来源 robots 规则不允许",
                    "Source robots policy disallows access",
                )),
                Some("source_not_configured") => {
                    Some(lang.pick("来源尚未配置", "Source is not configured"))
                }
                Some("source_deferred") => Some(lang.pick("来源暂缓", "Source deferred")),
                Some(_) => Some(lang.pick("来源暂不可用", "Source temporarily unavailable")),
                None => None,
            };
            let stale = if pool.stale {
                lang.pick(" · 数据已过期", " · Data stale")
            } else {
                ""
            };
            let reason = reason
                .map(|value| format!(" · {}", esc(value)))
                .unwrap_or_default();
            pool_rows.push_str(&format!(r#"<article class="metric"><span>{}</span><strong>{} / {}</strong><small>{}{}{} · {}: {} · {}: {}</small></article>"#, esc(label), pool.count, pool.target, esc(status), stale, reason, lang.pick("更新时间", "Updated"), esc(&time((pool.updated_at>0).then_some(pool.updated_at),lang)), lang.pick("最近尝试", "Last attempt"), esc(&time((pool.last_attempt_at>0).then_some(pool.last_attempt_at),lang))));
        }
        if pool_rows.is_empty() {
            pool_rows = format!(
                r#"<p class="empty">{}</p>"#,
                lang.pick(
                    "样本池数据尚不可用。",
                    "Sample pool data is not yet available."
                )
            );
        }
        let mut sources = String::new();
        for (name, url) in [
            ("ngosang", "https://github.com/ngosang/trackerslist"),
            ("XIU2", "https://github.com/XIU2/TrackersListCollection"),
            ("newTrackon", "https://github.com/CorralPeltzer/newTrackon"),
        ] {
            sources.push_str(&format!(r#"<div class="source-link"><a href="{}" rel="noopener noreferrer">{} ↗</a>{}</div>"#, esc(url), esc(name), copy_url(lang.pick("来源地址", "Source URL"), url, lang)));
        }
        let body = format!(r#"<section class="metric-grid">{pool_rows}</section><section class="panel prose"><h2>{}</h2>{sources}<p>{}</p><p>{}</p></section>"#,
            lang.pick("Tracker 候选来源", "Tracker candidate sources"),
            lang.pick("公共资源样本优先来自官方发行渠道；论坛来源只用于公开资源的有限测量。", "Public resource samples prioritize official distribution sources; forum sources support bounded measurements of public resources."),
            lang.pick("每个地址族独立实测；限流、本地故障和缺测保持区分。握手与位图抽样不请求文件分块。", "Address families are checked independently; rate limits, local failures and missing data remain distinct. Handshakes and bitmap sampling do not request file pieces."));
        return shell(lang.pick("来源", "Sources"), lang, &body);
    }
    shell(lang.pick("来源", "Sources"), lang, "")
}

pub(crate) fn declaration(lang: Lang) -> String {
    let body = lang.pick(
        "<section class=\"panel prose\"><p>本站提供 BitTorrent Tracker 节点发现和公开 Tracker 观测服务。原生 Tracker 不托管或传输文件正文；客户端之间的数据由各自直接传输。</p><p>客户端通告会发送资源标识、节点地址和端口等必要信息；Tracker 会向同一资源的客户端返回已登记的节点地址和端口。本站不承诺节点匿名。原生节点登记保存在内存中，重启后重新积累。</p><p>观测站保存公开来源的资源元数据及握手、位图汇总，用于质量观测；后台不请求文件分块。本版页面不公开具体样本或磁力地址。</p><p>应用日志不记录原始资源标识、节点 IP 或完整查询串；CDN 和其他基础设施可能按其自身策略处理数据。</p><p>请仅用于合法资源。本站不保证资源的合法性、可下载性或节点可达性。观测仅代表特定时间和观测位置，网络、限流或缺测都可能影响结果。</p></section>",
        "<section class=\"panel prose\"><p>This site provides BitTorrent tracker peer discovery and public tracker observations. The native tracker does not host or transfer file payloads; clients transfer data directly.</p><p>Announces include resource identifiers, peer addresses and ports needed for discovery. The tracker returns registered peer addresses and ports to clients for the same resource; peer anonymity is not promised. Native peer registrations reside in memory and rebuild after restart.</p><p>The observatory stores public-source resource metadata and handshake and bitmap summaries for measurement; it does not request file pieces. This version does not display individual samples or magnet links.</p><p>Application logs do not record raw resource identifiers, peer IP addresses or complete query strings. CDN and other infrastructure may process data under their own policies.</p><p>Use the service only for lawful resources. Legality, downloadability and peer reachability are not guaranteed. Observations reflect a particular time and vantage point and can be affected by networks, rate limits or missing checks.</p></section>"
    );
    shell(lang.pick("声明", "Statement"), lang, body)
}
