"use strict";

(() => {
    const zh = document.documentElement.lang !== "en";
    const text = (cn, en) => zh ? cn : en;
    const formatTime = (seconds, precision) => {
        if (!Number.isFinite(seconds)) return "";
        try {
            const local = window.trackerTime?.format(seconds, precision);
            if (local) return local;
            // Preserve an explicitly labelled UTC fallback if the shared
            // formatter is unavailable; never imply an unconverted local time.
            const date = new Date(seconds * 1000);
            if (!Number.isFinite(date.getTime())) return "";
            const iso = date.toISOString();
            const label = precision === "tick" ? iso.slice(5, 16) : iso.slice(0, precision === "second" ? 19 : 16);
            return `${label.replace("T", " ")} UTC`;
        } catch (_) {
            return "";
        }
    };
    const request = async (url) => {
        const response = await fetch(url, {headers: {Accept: "application/json"}});
        if (!response.ok) throw new Error("unavailable");
        return response.json();
    };
    // Chart ink comes from CSS custom properties so both themes stay readable;
    // the literals below are only a fallback for a document without the sheet.
    const fallbackColors = {
        text: "#cbd5e1",
        muted: "#94a3b8",
        grid: "rgba(255, 255, 255, 0.05)",
        line1: "#10b981",
        line2: "#38bdf8",
        tooltipBg: "rgba(14, 20, 26, 0.92)",
        tooltipText: "#f8fafc",
        tooltipBorder: "rgba(255, 255, 255, 0.12)"
    };
    const themeColors = () => {
        const style = window.getComputedStyle(document.documentElement);
        const read = (name, fallback) => style.getPropertyValue(name).trim() || fallback;
        return {
            text: read("--chart-text", fallbackColors.text),
            muted: read("--chart-muted", fallbackColors.muted),
            grid: read("--chart-grid", fallbackColors.grid),
            line1: read("--chart-line-1", fallbackColors.line1),
            line2: read("--chart-line-2", fallbackColors.line2),
            tooltipBg: read("--chart-tooltip-bg", fallbackColors.tooltipBg),
            tooltipText: read("--chart-tooltip-text", fallbackColors.tooltipText),
            tooltipBorder: read("--chart-tooltip-border", fallbackColors.tooltipBorder)
        };
    };
    const options = (colors) => ({
        responsive: true, maintainAspectRatio: false, animation: false,
        parsing: false, spanGaps: false,
        plugins: {
            legend: {
                display: true,
                labels: { color: colors.text, font: { size: 12, family: "Inter, system-ui, sans-serif" }, boxWidth: 14, padding: 12 }
            },
            tooltip: {
                backgroundColor: colors.tooltipBg,
                titleColor: colors.tooltipText,
                bodyColor: colors.tooltipText,
                borderColor: colors.tooltipBorder,
                borderWidth: 1,
                padding: 10,
                cornerRadius: 8,
                callbacks: {
                    title(items) {
                        return formatTime(items[0]?.parsed?.x, "minute");
                    }
                }
            }
        },
        scales: {
            x: {
                type: "linear",
                grid: { color: colors.grid },
                ticks: {
                    color: colors.muted,
                    maxTicksLimit: 6,
                    callback(value) {
                        return formatTime(value, "tick");
                    }
                }
            },
            y: {
                beginAtZero: true,
                grid: { color: colors.grid },
                ticks: { color: colors.muted }
            }
        }
    });
    const optionsForWindow = (colors, seconds) => {
        const now = Date.now() / 1000;
        const value = options(colors);
        value.scales.x.min = now - seconds;
        value.scales.x.max = now;
        return value;
    };
    // Every live chart plus the option builder that created it, so a theme
    // change can rebuild that chart's options and repaint it.
    const charts = [];
    const registerChart = (chart, builder) => {
        charts.push({chart, builder});
        return chart;
    };
    const forgetChart = (chart) => {
        const index = charts.findIndex((entry) => entry.chart === chart);
        if (index >= 0) charts.splice(index, 1);
    };
    const paintChart = ({chart, builder}) => {
        const colors = themeColors();
        // Chart.js re-resolves its options from `config.options` on every
        // update, so replacing that object is what makes the new colors stick
        // (the read-only `chart.options` resolver would be rebuilt).
        chart.config.options = builder(colors);
        chart.data.datasets.forEach((dataset) => {
            // `lineKey` is ours, not a Chart.js option: it keeps a series on
            // the same palette slot (an IPv6-only chart stays on line 2).
            dataset.borderColor = colors[dataset.lineKey] || colors.line1;
        });
        chart.update("none");
    };
    if (typeof MutationObserver === "function") {
        new MutationObserver(() => {
            charts.slice().forEach((entry) => {
                try {
                    paintChart(entry);
                } catch (_) {
                    // A chart that can no longer update is dropped, so one
                    // failure cannot stop the remaining charts from repainting.
                    forgetChart(entry.chart);
                }
            });
        }).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    }
    const historyRoot = document.querySelector("[data-history-root]");
    if (historyRoot && !window.Chart) {
        historyRoot.dataset.state = "unavailable";
        historyRoot.setAttribute("aria-label", text("历史数据暂不可用", "History data unavailable"));
    }
    if (historyRoot && window.Chart) {
        let rendered = [], requestId = 0;
        const selector = document.querySelector("[data-history-window]");
        const note = document.querySelector("[data-history-rate-note]");
        const draw = async () => {
            const current = ++requestId;
            try {
                const windowName = selector.value;
                const history = await request(`/api/v1/stats/history?window=${encodeURIComponent(windowName)}`);
                if (current !== requestId) return;
                const step = history.step_seconds;
                if (!Number.isFinite(step) || step <= 0 || !Array.isArray(history.items)) throw new Error("invalid history");
                // The API supplies time order. Preserve malformed positions as breaks.
                const rows = history.items.slice();
                const duration = ({"24h":300, "7d":1800, "30d":10800})[windowName];
                const bucketLabel = windowName === "24h" ? text("5分钟", "5-minute") : windowName === "7d" ? text("30分钟", "30-minute") : text("3小时", "3-hour");
                const averageLabel = text(`${bucketLabel}已发布采样均值`, `${bucketLabel} published-sample average`);
                if (note) note.textContent = text(`速率为${averageLabel}，按已发布的相邻采样间隔加权；缺测不补零，较长窗口未覆盖所有原始分钟采样。`, `Rates are ${averageLabel}s weighted by published intervals; gaps remain gaps, and longer windows do not contain every original minute sample.`);
                const gaugeSeries = (key) => {
                    let previous = null, maxSeen = -Infinity;
                    const points = [];
                    for (const row of rows) {
                        if (!Number.isFinite(row?.at) || row.at <= maxSeen) {
                            if (points.length && points[points.length-1].y !== null) points.push({x:points[points.length-1].x + 0.001, y:null});
                            previous = null;
                            continue;
                        }
                        maxSeen = row.at;
                        if (previous && (row.at - previous.at > step * 1.5 || !previous.instance || previous.instance !== row.instance)) points.push({x:(previous.at + row.at) / 2, y:null});
                        const raw = row.gauges?.[key];
                        points.push({x:row.at, y:Number.isFinite(raw) ? raw : null});
                        previous = row;
                    }
                    return points;
                };
                const rateSeries = (key) => {
                    const points = [];
                    let previous = null, bucket = null, maxSeen = -Infinity;
                    const flush = () => {
                        if (bucket) points.push({x:bucket.end, y:bucket.delta / bucket.elapsed, start:bucket.start, end:bucket.end, elapsed:bucket.elapsed});
                        bucket = null;
                    };
                    for (const row of rows) {
                        if (!Number.isFinite(row?.at) || row.at <= maxSeen) {
                            flush();
                            if (points.length && points[points.length-1].y !== null) points.push({x:points[points.length-1].x + 0.001, y:null});
                            previous = null;
                            continue;
                        }
                        maxSeen = row.at;
                        const value = row.counters?.[key];
                        if (previous) {
                            const elapsed = row.at - previous.row.at;
                            const valid = typeof row.instance === "string" && row.instance.length > 0 && previous.row.instance === row.instance && Number.isFinite(value) && Number.isFinite(previous.value) && elapsed > 0 && elapsed <= step * 1.5 && value >= previous.value;
                            if (!valid) {
                                flush();
                                if (points.length && points[points.length-1].y !== null) points.push({x:(previous.row.at + row.at) / 2, y:null});
                            } else {
                                const slot = Math.floor(row.at / duration);
                                if (bucket && bucket.slot !== slot) flush();
                                if (!bucket) bucket = {slot, start:previous.row.at, end:row.at, delta:0, elapsed:0};
                                bucket.end = row.at;
                                bucket.delta += value - previous.value;
                                bucket.elapsed += elapsed;
                            }
                        }
                        previous = {row, value};
                    }
                    flush();
                    return points;
                };
                rendered.forEach((chart) => {
                    forgetChart(chart);
                    chart.destroy();
                });
                rendered = [];
                const palette = themeColors();
                const windowSeconds = ({"24h":86400, "7d":604800, "30d":2592000})[windowName];
                historyRoot.querySelectorAll("[data-history-chart]").forEach((canvas) => {
                    const key = canvas.dataset.historyChart;
                    const rate = !["peers", "swarms", "rss_bytes"].includes(key);
                    const keys = key === "protocol" ? ["http_announces", "udp_announces"] : [key];
                    const title = canvas.closest("article")?.querySelector("h3");
                    if (title && rate) title.textContent = `${canvas.getAttribute("aria-label")} · ${averageLabel}`;
                    const sets = keys.map((entry, i) => {
                        const data = rate ? rateSeries(entry) : gaugeSeries(entry);
                        const pointRadius = data.map((point, index) => {
                            if (!Number.isFinite(point.y)) return 0;
                            const left = data[index-1], right = data[index+1];
                            return Number.isFinite(left?.y) || Number.isFinite(right?.y) ? 0 : 2.5;
                        });
                        return {label:entry === "http_announces" ? "HTTP(S)" : entry === "udp_announces" ? "UDP" : canvas.getAttribute("aria-label"), data, lineKey:i === 0 ? "line1" : "line2", borderColor:i === 0 ? palette.line1 : palette.line2, pointRadius, pointHoverRadius:4, borderWidth:2, tension:0};
                    });
                    const callbacks = rate ? {label(context) {
                        const point = context.raw;
                        return `${context.dataset.label}: ${context.parsed.y.toFixed(2)} /s · ${averageLabel} · ${text("实际观测", "observed")} ${point.elapsed}s`;
                    }} : undefined;
                    const makeOptions = (colors) => {
                        const value = optionsForWindow(colors, windowSeconds);
                        if (callbacks) value.plugins.tooltip.callbacks = {...value.plugins.tooltip.callbacks, ...callbacks};
                        return value;
                    };
                    rendered.push(registerChart(new Chart(canvas, {type:"line", data:{datasets:sets}, options:makeOptions(palette)}), makeOptions));
                });
                historyRoot.dataset.state = rows.length ? "ready" : "empty";
            } catch (_) { if (current === requestId) historyRoot.dataset.state = "unavailable"; }
        };
        selector.addEventListener("change", draw);
        draw();
    }
    const detail = document.querySelector("[data-detail-root]");
    if (!detail) return;
    const id = detail.dataset.endpointId;
    const observations = detail.querySelector("[data-observation-chart]");
    const timeline = detail.querySelector("[data-observation-timeline]");
    const dataNode = detail.querySelector("[data-detail-observations]");
    let entries = [];
    try {
        entries = JSON.parse(dataNode?.textContent || "[]");
        if (!Array.isArray(entries)) entries = [];
    } catch (_) { entries = []; }
    const svgNS = "http://www.w3.org/2000/svg";
    const svgNode = (name, attrs, parent) => {
        const node = document.createElementNS(svgNS, name);
        for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, String(value));
        parent.appendChild(node);
        return node;
    };
    const state = (o) => ["success", "failure", "neutral", "unknown"].includes(o.display_state) ? o.display_state : "failure";
    const stateLabel = (o) => {
        const label = ({success:text("成功", "Success"), failure:text("失败", "Failed"), neutral:text("限流 / 拒绝", "Rate limit / rejection"), unknown:text("未知", "Unknown")})[state(o)];
        const http = Number.isInteger(o.http_status) ? ` · HTTP ${o.http_status}` : "";
        const reason = typeof o.display_reason === "string" && o.display_reason !== "-" ? ` · ${o.display_reason}` : "";
        return `${label}${http}${reason}`;
    };
    const displayFamily = (family) => family === "ipv4" ? "IPv4" : "IPv6";
    const drawTimeline = (items, families, start, end, width) => {
        timeline.textContent = "";
        if (!families.length) { timeline.textContent = text("暂无可用地址族的观测。", "No observations for a supported address family."); return; }
        const axisY = 20 + families.length * 30;
        const axisRight = width - 33;
        const svg = svgNode("svg", {viewBox:`0 0 ${width} ${axisY + 20}`, class:"timeline", "aria-hidden":"true"}, timeline);
        const x = (at) => 52 + Math.max(0, Math.min(1, (at - start) / (end - start))) * (axisRight - 52);
        families.forEach((family, index) => {
            const y = 20 + index * 30;
            svgNode("text", {x:0,y:y+4}, svg).textContent = displayFamily(family);
            let previous = null;
            const familyItems = items.filter((entry) => entry.observation.family === family);
            if (!familyItems.length) svgNode("text", {x:60,y:y+4,class:"timeline-untested"}, svg).textContent = text("未测", "Not yet checked");
            for (const entry of familyItems) {
                const item = entry.observation;
                const px = x(item.observed_at), currentState = state(item);
                if (previous && item.observed_at > previous.at && item.observed_at - previous.at <= 7200 && previous.state !== "unknown" && currentState !== "unknown") svgNode("line", {class:"timeline-segment",x1:previous.x,y1:y,x2:px,y2:y}, svg);
                const params = new URLSearchParams({lang:detail.dataset.lang || document.documentElement.lang,window:detail.dataset.window || "24h",focus:String(entry.id)});
                if (detail.dataset.includeNoAddress === "1") params.set("include_no_address", "1");
                const href = `/tracker/${encodeURIComponent(id)}?${params.toString()}#observation-${encodeURIComponent(entry.id)}`;
                const link = svgNode("a", {class:"timeline-link",href}, svg);
                const dot = svgNode("circle", {class:`timeline-point ${currentState}`,cx:px,cy:y,r:4}, link);
                const when = formatTime(item.observed_at, "second");
                const title = [when, displayFamily(family), stateLabel(item)].filter(Boolean).join(" · ");
                link.setAttribute("aria-label", `${title}. ${text("定位到日志", "Open log entry")}`);
                svgNode("title", {}, dot).textContent = title;
                previous = {at:item.observed_at, x:px, state:currentState};
            }
        });
        svgNode("line", {class:"timeline-axis",x1:52,y1:axisY,x2:axisRight,y2:axisY}, svg);
        svgNode("text", {x:52,y:axisY+16}, svg).textContent = "−24 h";
        svgNode("text", {x:width-60,y:axisY+16}, svg).textContent = text("现在", "Now");
    };
    if (timeline && observations) {
        const items = entries
            .filter((entry) => entry && Number.isInteger(entry.id) && entry.observation)
            .slice()
            .sort((a, b) => a.observation.observed_at - b.observation.observed_at);
        const current = (detail.dataset.currentFamilies || "").split(",");
        const families = ["ipv4", "ipv6"].filter((family) => current.includes(family) || items.some(({observation:o}) => o.family === family && o.family_supported));
        const end = Date.now() / 1000, start = end - 86400;
        let lastTimelineWidth = null;
        const redraw = () => {
            const measured = timeline.clientWidth;
            const width = Number.isFinite(measured) && measured > 0 ? Math.max(280, measured) : 760;
            if (width === lastTimelineWidth) return;
            lastTimelineWidth = width;
            drawTimeline(items, families, start, end, width);
        };
        redraw();
        if (typeof ResizeObserver === "function") {
            const observer = new ResizeObserver(redraw);
            observer.observe(timeline);
        }
        if (!families.length || !window.Chart) { observations.parentElement.hidden = true; return; }
        const palette = themeColors();
        const datasets = families.map((family) => {
            const points = [];
            let previous = null;
            for (const {observation} of items.filter((entry) => entry.observation.family === family)) {
                if (previous !== null && observation.observed_at - previous > 7200) points.push({x:(previous + observation.observed_at) / 2, y:null});
                points.push({x:observation.observed_at, y:Number.isFinite(observation.latency_ms) ? observation.latency_ms : null});
                previous = observation.observed_at;
            }
            return {label:displayFamily(family), lineKey:family === "ipv4" ? "line1" : "line2", borderColor:family === "ipv4" ? palette.line1 : palette.line2, pointRadius:2, pointHoverRadius:4, borderWidth:2, spanGaps:false, tension:0, data:points};
        });
        const makeOptions = (colors) => optionsForWindow(colors, 86400);
        registerChart(new Chart(observations, {type:"line", data:{datasets}, options:makeOptions(palette)}), makeOptions);
    }
})();
