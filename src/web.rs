//! Read-only LAN status page + neighbor-sync listener (std-only HTTP).
//!
//! - `GET /`       live HTML status page (auto-refreshes, no JS required;
//!                 `?sort=name|status|group|latency|uptime|sla&order=asc|desc`,
//!                 `?group=<label>` filters to one group)
//! - `GET /health` `ok` for load-balancers / startup probes
//! - `POST /sync/join|push` token-guarded neighbor sync (pairing protocol,
//!                 not a public API)
//!
//! Only GET serves the page and only POST mutates sync state; anything else
//! gets 405. The page itself is read-only (no ping-now, no mute, no config
//! writes). Bind `0.0.0.0` to be visible anywhere on the local network,
//! `127.0.0.1` for local-only.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::SyncSender,
    Arc, RwLock,
};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{Config, HostConfig, SyncDeletion};
use crate::sync::{device_hostname, primary_lan_ip, SyncEvent};

/// Point-in-time copy of one host for the web thread. Built by the probing
/// loop (TUI or headless) and served via `SharedPage`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostSnapshot {
    pub name: String,
    pub display_name: String,
    pub target: String,
    pub group: String,
    /// UP / DOWN / WARN / FLAP / MUTED / DEP — same priority as the TUI.
    pub status: String,
    pub up: bool,
    pub latency_ms: f64,
    /// Session uptime from the in-memory counters.
    pub uptime_pct: f64,
    /// 24h SLA from uptime-log.csv, None when no data.
    pub sla_24h: Option<f64>,
    /// 1 = up sample, 0 = down sample (newest last here; page reverses).
    pub history: Vec<u64>,
    pub muted: bool,
    pub suppressed: bool,
    pub flapping: bool,
    pub warn: bool,
    pub down_for_secs: Option<u64>,
}

/// Theme colors mirrored from the serving instance (TUI theme or headless
/// config theme) so the page visibly matches the desktop.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedTheme {
    pub name: String,
    pub bg: String,
    pub fg: String,
    pub title: String,
    pub accent: String,
    pub muted: String,
    pub good: String,
    pub danger: String,
    pub graph: String,
    pub divider: String,
    pub card: String,
}

impl Default for SharedTheme {
    fn default() -> Self {
        // btop-ish dark fallback (matches the historical static page).
        SharedTheme {
            name: "btop".to_string(),
            bg: "#161a22".to_string(),
            fg: "#c8ccd4".to_string(),
            title: "#eef0f6".to_string(),
            accent: "#8fb573".to_string(),
            muted: "#5a6375".to_string(),
            good: "#a3be8c".to_string(),
            danger: "#dc6d6d".to_string(),
            graph: "#8fb573".to_string(),
            divider: "#2c313c".to_string(),
            card: "#1c1f28".to_string(),
        }
    }
}

/// Everything the page needs in one lock: hosts + serving theme.
#[derive(Clone, Debug, Default)]
pub struct PageState {
    pub hosts: Vec<HostSnapshot>,
    pub theme: SharedTheme,
}

pub type SharedPage = Arc<RwLock<PageState>>;

pub fn new_shared_page() -> SharedPage {
    Arc::new(RwLock::new(PageState::default()))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn status_class(status: &str) -> &'static str {
    match status {
        "UP" => "up",
        "DOWN" => "down",
        "WARN" | "FLAP" => "warn",
        _ => "muted",
    }
}

/// Sortable web column. `None` = grouped view (matches the TUI).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortKey {
    #[default]
    None,
    Name,
    Status,
    Group,
    Latency,
    Uptime,
    Sla,
}

impl SortKey {
    fn param(&self) -> &'static str {
        match self {
            SortKey::None => "off",
            SortKey::Name => "name",
            SortKey::Status => "status",
            SortKey::Group => "group",
            SortKey::Latency => "latency",
            SortKey::Uptime => "uptime",
            SortKey::Sla => "sla",
        }
    }

    fn from_param(s: &str) -> SortKey {
        match s.trim().to_lowercase().as_str() {
            "name" | "host" => SortKey::Name,
            "status" | "state" => SortKey::Status,
            "group" => SortKey::Group,
            "latency" | "latency_ms" | "ms" => SortKey::Latency,
            "uptime" => SortKey::Uptime,
            "sla" | "sla_24h" => SortKey::Sla,
            _ => SortKey::None,
        }
    }
}

/// Severity rank for status sorting: problems first ascending.
fn status_rank(status: &str) -> u8 {
    match status {
        "DOWN" => 0,
        "FLAP" => 1,
        "WARN" => 2,
        "DEP" => 3,
        "MUTED" => 4,
        _ => 5,
    }
}

/// Parsed page query: sort + direction + optional group-label filter.
#[derive(Clone, Debug, Default)]
pub struct PageQuery {
    pub sort: SortKey,
    pub desc: bool,
    pub group: Option<String>,
}

/// Parse `sort`/`order`/`group` from a raw query string.
/// Unknown values fall back to grouped view, ascending.
pub fn parse_query(query: &str) -> PageQuery {
    let mut out = PageQuery::default();
    for pair in query.split('&') {
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        match k.to_lowercase().as_str() {
            "sort" | "sort_by" | "order_by" => out.sort = SortKey::from_param(v),
            "order" | "dir" | "direction" => {
                out.desc = matches!(v.to_lowercase().as_str(), "desc" | "descending" | "down" | "1");
            }
            "group" | "label" => {
                let v = v.trim();
                if !v.is_empty() {
                    out.group = Some(v.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

/// Sort a snapshot in place. Stable, so ties keep probe order.
pub fn apply_sort(hosts: &mut [HostSnapshot], sort: SortKey, desc: bool) {
    match sort {
        SortKey::None => {}
        SortKey::Name => hosts.sort_by(|a, b| {
            a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase())
        }),
        SortKey::Status => hosts.sort_by(|a, b| {
            status_rank(&a.status).cmp(&status_rank(&b.status)).then_with(|| {
                a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase())
            })
        }),
        SortKey::Group => hosts.sort_by(|a, b| {
            a.group.to_lowercase().cmp(&b.group.to_lowercase()).then_with(|| {
                a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase())
            })
        }),
        SortKey::Latency => hosts.sort_by(|a, b| {
            a.latency_ms.partial_cmp(&b.latency_ms).unwrap_or(std::cmp::Ordering::Equal)
        }),
        SortKey::Uptime => hosts.sort_by(|a, b| {
            a.uptime_pct.partial_cmp(&b.uptime_pct).unwrap_or(std::cmp::Ordering::Equal)
        }),
        SortKey::Sla => hosts.sort_by(|a, b| {
            a.sla_24h.unwrap_or(-1.0).partial_cmp(&b.sla_24h.unwrap_or(-1.0)).unwrap_or(std::cmp::Ordering::Equal)
        }),
    }
    if desc {
        hosts.reverse();
    }
}

fn wildcard_bind(bind: &str) -> bool {
    bind == "0.0.0.0" || bind == "::" || bind == "[::]" || bind == "*"
}

/// Viewable URL(s) for humans: wildcard binds resolve to the real LAN IP
/// plus a localhost URL; concrete binds return the single direct URL.
pub fn lan_urls(bind: &str, port: u16) -> Vec<String> {
    if wildcard_bind(bind) {
        let mut urls = Vec::new();
        match primary_lan_ip() {
            Some(ip) => urls.push(format!("http://{}:{}/", ip, port)),
            None => urls.push(format!("http://{}:{}/", bind, port)),
        }
        let local = format!("http://127.0.0.1:{}/", port);
        if !urls.contains(&local) {
            urls.push(local);
        }
        urls
    } else {
        vec![format!("http://{}:{}/", bind, port)]
    }
}

fn sparkstrip(history: &[u64]) -> String {
    history
        .iter()
        .rev()
        .map(|lat| {
            if *lat > 0 {
                "<span class=\"u\">\u{25a0}</span>"
            } else {
                "<span class=\"d\">_</span>"
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn host_row(h: &HostSnapshot) -> String {
    let sla = h.sla_24h.map(|v| format!("{:.1}%", v)).unwrap_or_else(|| "\u{2014}".to_string());
    let lat = if h.up {
        format!("{:.0} ms", h.latency_ms)
    } else if h.muted {
        "muted".to_string()
    } else if let Some(s) = h.down_for_secs {
        format!("down {}s", s)
    } else {
        "\u{2014}".to_string()
    };
    format!(
        "<tr><td>{}</td><td class=\"dim mono\">{}</td><td><span class=\"status {}\">\u{25cf} {}</span></td><td class=\"mono\">{}</td><td class=\"dim\">{}</td><td class=\"mono\">{:.1}%</td><td class=\"mono\">{}</td><td class=\"spark mono\">{}</td></tr>\n",
        html_escape(&h.display_name),
        html_escape(&h.target),
        status_class(&h.status),
        html_escape(&h.status),
        html_escape(&lat),
        html_escape(&h.group),
        h.uptime_pct,
        sla,
        sparkstrip(&h.history),
    )
}

fn group_label(h: &HostSnapshot) -> String {
    if h.group.trim().is_empty() {
        "default".to_string()
    } else {
        h.group.clone()
    }
}

/// Header link for a sortable column: clicking the active column toggles
/// asc/desc, clicking another column sorts ascending by it. Preserves the
/// group filter.
fn sort_link(label: &str, key: SortKey, q: &PageQuery) -> String {
    let arrow = if q.sort == key {
        if q.desc { " \u{25bc}" } else { " \u{25b2}" }
    } else {
        ""
    };
    let order = if q.sort == key && !q.desc { "desc" } else { "asc" };
    let group = q.group.as_deref().map(|g| format!("&amp;group={}", html_escape(g))).unwrap_or_default();
    format!(
        "<a href=\"/?sort={}&amp;order={}{}\">{}{}</a>",
        key.param(),
        order,
        group,
        html_escape(label),
        arrow
    )
}

/// Full HTML status page in the serving instance's theme. Static CSS + meta
/// refresh so it works in any browser with no JS. Default view groups hosts
/// by label (TUI-style, groups down-first, collapsible via `<details>`);
/// any `?sort=` switches to a flat sorted table.
pub fn render_status_page(
    hosts: &[HostSnapshot],
    theme: &SharedTheme,
    version: &str,
    generated: &str,
    q: &PageQuery,
) -> String {
    let visible: Vec<&HostSnapshot> = hosts
        .iter()
        .filter(|h| q.group.as_deref().map_or(true, |g| group_label(h) == g))
        .collect();
    let up = visible.iter().filter(|h| h.up).count();
    let down = visible.len().saturating_sub(up);

    let mut state_note = String::new();
    if q.sort != SortKey::None {
        state_note.push_str(&format!(
            " \u{00b7} sorted by <code>{} {}</code>",
            q.sort.param(),
            if q.desc { "desc" } else { "asc" }
        ));
    }
    if let Some(g) = q.group.as_deref() {
        state_note.push_str(&format!(" \u{00b7} group <code>{}</code>", html_escape(g)));
    }
    if q.sort != SortKey::None || q.group.is_some() {
        state_note.push_str(" \u{00b7} <a href=\"/\" style=\"color:accent\">reset</a>");
    }
    let state_note = state_note.replace("color:accent", &format!("color:{}", html_escape(&theme.accent)));

    let head = |label: &str, key: SortKey, q: &PageQuery| {
        format!(
            "<th{}>{}</th>",
            if q.sort == key { " class=\"active\"" } else { "" },
            sort_link(label, key, q)
        )
    };
    let header = format!(
        "<thead><tr>{}<th>Target</th>{}{}{}<th>Uptime</th>{}<th>History</th></tr></thead>",
        head("Host", SortKey::Name, q),
        head("Status", SortKey::Status, q),
        head("Latency", SortKey::Latency, q),
        head("Group", SortKey::Group, q),
        head("SLA 24h", SortKey::Sla, q),
    );
    // Fixed column order (Host/Target/Status/Latency/Group/Uptime/SLA/
    // History, like the TUI); sort links ride on the sortable columns.

    // All groups, down-first like the TUI: drives both the filter chips and
    // the grouped sections so the two always agree on ordering.
    let mut all_groups: BTreeMap<String, Vec<&HostSnapshot>> = BTreeMap::new();
    for h in hosts.iter() {
        all_groups.entry(group_label(h)).or_default().push(h);
    }
    let mut group_order: Vec<&String> = all_groups.keys().collect();
    group_order.sort_by(|a, b| {
        let a_down = all_groups[*a].iter().any(|h| !h.up);
        let b_down = all_groups[*b].iter().any(|h| !h.up);
        b_down.cmp(&a_down).then_with(|| a.cmp(b))
    });

    // Group filter chips (pure links, no JS), preserving the current sort.
    let base_qs = if q.sort == SortKey::None {
        String::new()
    } else {
        format!("sort={}&order={}", q.sort.param(), if q.desc { "desc" } else { "asc" })
    };
    let href_for = |group: Option<&str>| {
        let escaped = |s: &str| html_escape(s).replace('&', "&amp;");
        match (base_qs.is_empty(), group) {
            (true, None) => "/".to_string(),
            (false, None) => format!("/?{}", escaped(&base_qs)),
            (true, Some(g)) => format!("/?group={}", html_escape(g)),
            (false, Some(g)) => format!("/?{}&amp;group={}", escaped(&base_qs), html_escape(g)),
        }
    };
    let mut chips = String::from("<nav class=\"chips\">");
    chips.push_str(&format!(
        "<a class=\"chip{}\" href=\"{}\">All</a>",
        if q.group.is_none() { " active" } else { "" },
        href_for(None),
    ));
    for g in &group_order {
        chips.push_str(&format!(
            "<a class=\"chip{}\" href=\"{}\">{}</a>",
            if q.group.as_deref() == Some(g.as_str()) { " active" } else { "" },
            href_for(Some(g)),
            html_escape(g),
        ));
    }
    chips.push_str("</nav>");

    let body = if q.sort != SortKey::None {
        let mut owned: Vec<HostSnapshot> = visible.into_iter().cloned().collect();
        apply_sort(&mut owned, q.sort, q.desc);
        format!(
            "<div class=\"table-wrap\"><table>{}<tbody>{}</tbody></table></div>",
            header,
            owned.iter().map(host_row).collect::<String>()
        )
    } else {
        let mut out = String::new();
        for name in group_order {
            if q.group.as_deref().map_or(false, |g| g != name.as_str()) {
                continue;
            }
            let members = &all_groups[name];
            // Down-first within the group, like the TUI.
            let mut members = members.clone();
            members.sort_by(|a, b| {
                b.up.cmp(&a.up).then_with(|| {
                    a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase())
                })
            });
            let g_up = members.iter().filter(|h| h.up).count();
            let g_down = members.len().saturating_sub(g_up);
            // Native collapsible: no JS needed.
            out.push_str(&format!(
                "<details open><summary><span class=\"gname\">{}</span><span class=\"tally\"> \u{00b7} <span class=\"up\">{} up</span> \u{00b7} <span class=\"down\">{} down</span></span></summary><div class=\"table-wrap\"><table>{}<tbody>{}</tbody></table></div></details>\n",
                html_escape(name),
                g_up,
                g_down,
                header,
                members.iter().map(|h| host_row(h)).collect::<String>(),
            ));
        }
        if out.is_empty() {
            out.push_str("<p class=\"sub\">No hosts in this view.</p>");
        }
        out
    };

    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
        <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
        <meta http-equiv=\"refresh\" content=\"15\">\
        <title>ping-uin status</title><style>\
        :root{{--bg:{bg};--fg:{fg};--title:{title};--accent:{accent};--muted:{muted};--good:{good};--danger:{danger};--graph:{graph};--divider:{divider};--card:{card}}}\
        *{{box-sizing:border-box}}\
        body{{background:var(--bg);color:var(--fg);font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",Roboto,Helvetica,Arial,sans-serif;margin:0;padding:32px 24px 48px}}\
        .wrap{{max-width:1100px;margin:0 auto}}\
        .hero{{display:flex;justify-content:space-between;align-items:flex-end;gap:16px;flex-wrap:wrap}}\
        h1{{color:var(--title);font-size:24px;margin:0;font-weight:700}}\
        .sub{{color:var(--muted);font-size:12.5px;margin:8px 0 0}}\
        .pills{{display:flex;gap:8px}}\
        .pill{{display:inline-flex;align-items:center;gap:6px;padding:7px 16px;border-radius:999px;font-weight:700;font-size:14px;border:1px solid;background:var(--card)}}\
        .pill.up{{color:var(--good);border-color:var(--good)}}\
        .pill.down{{color:var(--danger);border-color:var(--danger)}}\
        .chips{{display:flex;gap:8px;flex-wrap:wrap;margin:20px 0 4px}}\
        .chip{{padding:5px 14px;border-radius:999px;border:1px solid var(--divider);color:var(--muted);text-decoration:none;font-size:12.5px;background:var(--card)}}\
        .chip:hover{{color:var(--title);border-color:var(--accent)}}\
        .chip.active{{color:var(--title);border-color:var(--accent);font-weight:700}}\
        table{{width:100%;border-collapse:collapse;font-size:13px}}\
        thead th{{position:sticky;top:0;background:var(--card);text-align:left;padding:9px 12px;color:var(--muted);font-size:11px;text-transform:uppercase;letter-spacing:.05em;border-bottom:1px solid var(--divider);white-space:nowrap;z-index:1}}\
        tbody td{{padding:9px 12px;border-bottom:1px solid var(--divider);vertical-align:middle}}\
        tbody tr:last-child td{{border-bottom:none}}\
        tbody tr:hover{{background:rgba(128,128,128,.07)}}\
        th a{{color:inherit;text-decoration:none}}th a:hover{{color:var(--accent)}}th.active{{color:var(--title)}}th.active a{{color:var(--title)}}\
        .up{{color:var(--good)}}.down{{color:var(--danger)}}.warn{{color:var(--accent)}}.muted{{color:var(--muted)}}.dim{{color:var(--muted)}}\
        .status{{display:inline-flex;align-items:center;gap:6px;font-weight:700;font-size:12px;padding:3px 11px;border-radius:999px;border:1px solid;white-space:nowrap}}\
        .status.up{{color:var(--good);border-color:var(--good)}}\
        .status.down{{color:var(--danger);border-color:var(--danger)}}\
        .status.warn{{color:var(--accent);border-color:var(--accent)}}\
        .status.muted{{color:var(--muted);border-color:var(--muted)}}\
        .mono{{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12.5px}}\
        details{{background:var(--card);border:1px solid var(--divider);border-radius:12px;margin:12px 0}}\
        summary{{list-style:none;cursor:pointer;display:flex;align-items:center;gap:8px;padding:13px 16px;font-weight:700;color:var(--title);font-size:14px}}\
        summary::-webkit-details-marker{{display:none}}\
        summary::before{{content:\"\u{25b8}\";color:var(--accent);font-size:12px}}\
        details[open] > summary::before{{content:\"\u{25be}\"}}\
        summary .gname{{color:var(--title)}}summary .tally{{font-weight:400;font-size:12.5px;color:var(--muted)}}\
        details .table-wrap{{border-top:1px solid var(--divider)}}\
        .table-wrap{{overflow-x:auto}}\
        .spark{{letter-spacing:2px;white-space:nowrap}}.u{{color:var(--graph)}}.d{{color:var(--danger)}}\
        code{{background:var(--card);border:1px solid var(--divider);padding:2px 6px;border-radius:4px;font-size:12px}}\
        @media (max-width:720px){{body{{padding:16px 12px 32px}}h1{{font-size:19px}}thead th,tbody td{{padding:7px 8px}}.pill{{font-size:12.5px;padding:5px 12px}}}}\
        </style></head><body><div class=\"wrap\">\
        <header class=\"hero\"><div><h1>((\u{2022}O\u{2022})) ping-uin status</h1>\
        <p class=\"sub\">generated {generated} \u{00b7} v{version} \u{00b7} theme {themename} \u{00b7} auto-refreshes every 15s{note}</p></div>\
        <div class=\"pills\"><span class=\"pill up\">\u{25cf} {up} up</span><span class=\"pill down\">\u{25cf} {down} down</span></div></header>\
        {chips}\
        {body}\
        </div></body></html>",
        body = body,
        up = up,
        down = down,
        generated = html_escape(generated),
        version = html_escape(version),
        themename = html_escape(&theme.name),
        note = state_note,
        bg = html_escape(&theme.bg),
        fg = html_escape(&theme.fg),
        title = html_escape(&theme.title),
        muted = html_escape(&theme.muted),
        divider = html_escape(&theme.divider),
        good = html_escape(&theme.good),
        danger = html_escape(&theme.danger),
        accent = html_escape(&theme.accent),
        graph = html_escape(&theme.graph),
        card = html_escape(&theme.card),
    )
}

fn http_response(status: &str, content_type: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
        status,
        content_type,
        body.len(),
        body
    )
    .into_bytes()
}

/// Read one HTTP request: headers + `Content-Length` body (sync pushes can
/// exceed a single read with large host lists). Caps at 2 MiB.
fn read_request(stream: &mut impl Read) -> Option<(String, String, String)> {
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 4096];
    // Headers first.
    loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 2 * 1024 * 1024 {
            return None;
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if n < tmp.len() {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, mut body) = match text.split_once("\r\n\r\n") {
        Some((h, b)) => (h.to_string(), b.as_bytes().to_vec()),
        None => (text, Vec::new()),
    };
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let content_len: usize = head
        .lines()
        .skip(1)
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse().ok()
            } else {
                None
            }
        })
        .next()
        .unwrap_or(0);
    // The first read may already hold the whole body; top up if short.
    while body.len() < content_len && body.len() < 2 * 1024 * 1024 {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_len.min(body.len()));
    let body = String::from_utf8_lossy(&body).into_owned();
    Some((method, target, body))
}

fn sync_error(msg: &str) -> Vec<u8> {
    http_response(
        "200 OK",
        "application/json",
        &serde_json::json!({ "ok": false, "error": msg }).to_string(),
    )
}

/// Load the on-disk config and check the presented sync token. The token
/// lives on disk (written at pairing/rotation), so this is always fresh.
fn check_sync_token(presented: &str) -> Option<Config> {
    let cfg = Config::load();
    match cfg.sync_token.as_deref() {
        Some(t) if !t.is_empty() && t == presented => Some(cfg),
        _ => None,
    }
}

fn handle_sync_join(body: &str) -> Vec<u8> {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return sync_error("bad JSON"),
    };
    let token = v.get("token").and_then(|t| t.as_str()).unwrap_or("");
    let cfg = match check_sync_token(token) {
        Some(c) => c,
        None => return sync_error("bad token (generate a fresh join code on the other device?)"),
    };
    let from_addr = v.get("from_addr").and_then(|a| a.as_str()).unwrap_or("").to_string();
    let from_token = v.get("from_token").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let from_hostname = v.get("from_hostname").and_then(|h| h.as_str()).unwrap_or("").to_string();
    if from_addr.is_empty() || from_token.is_empty() {
        return sync_error("missing from_addr/from_token");
    }
    // Record the new peer for the main loop (single config writer).
    let _ = SYNC_TX.with(|tx| {
        if let Some(tx) = tx.borrow().as_ref() {
            let _ = tx.try_send(SyncEvent::peer(
                from_addr,
                from_token,
                from_hostname,
                cfg.hosts.clone(),
                cfg.sync_deleted.clone(),
            ));
        }
    });
    http_response(
        "200 OK",
        "application/json",
        &serde_json::json!({
            "ok": true,
            "hosts": cfg.hosts,
            "deleted": cfg.sync_deleted,
            "hostname": device_hostname(),
        })
        .to_string(),
    )
}

fn handle_sync_push(body: &str) -> Vec<u8> {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return sync_error("bad JSON"),
    };
    let token = v.get("token").and_then(|t| t.as_str()).unwrap_or("");
    if check_sync_token(token).is_none() {
        return sync_error("bad token");
    }
    let hosts: Vec<HostConfig> = v
        .get("hosts")
        .and_then(|h| serde_json::from_value(h.clone()).ok())
        .unwrap_or_default();
    let deleted: Vec<SyncDeletion> = v
        .get("deleted")
        .and_then(|d| serde_json::from_value(d.clone()).ok())
        .unwrap_or_default();
    let from_addr = v.get("from_addr").and_then(|a| a.as_str()).unwrap_or("").to_string();
    let from_hostname = v.get("from_hostname").and_then(|h| h.as_str()).unwrap_or("").to_string();
    let _ = SYNC_TX.with(|tx| {
        if let Some(tx) = tx.borrow().as_ref() {
            let _ = tx.try_send(SyncEvent::push(hosts, deleted, from_addr, from_hostname));
        }
    });
    http_response("200 OK", "application/json", r#"{"ok":true}"#)
}

thread_local! {
    /// Set per server thread so sync handlers can reach the main loop without
    /// threading a channel through every connection spawn.
    static SYNC_TX: std::cell::RefCell<Option<SyncSender<SyncEvent>>> = const { std::cell::RefCell::new(None) };
}

fn handle_connection(
    mut stream: impl Read + Write,
    page: &SharedPage,
    version: &str,
    web_enabled: &Arc<AtomicBool>,
) {
    let Some((method, target, body)) = read_request(&mut stream) else {
        return;
    };
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };

    if method == "POST" {
        let resp = match path.as_str() {
            "/sync/join" => handle_sync_join(&body),
            "/sync/push" => handle_sync_push(&body),
            _ => http_response("404 Not Found", "text/plain; charset=utf-8", "not found\n"),
        };
        let _ = stream.write_all(&resp);
        return;
    }
    if method != "GET" {
        let _ = stream.write_all(&http_response(
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            "read-only server: GET the page, POST sync\n",
        ));
        return;
    }

    match path.as_str() {
        "/" | "/index.html" => {
            if !web_enabled.load(Ordering::Relaxed) {
                let _ = stream.write_all(&http_response(
                    "404 Not Found",
                    "text/plain; charset=utf-8",
                    "web page not enabled on this device (press W in its TUI)\n",
                ));
                return;
            }
            let q = parse_query(&query);
            let (hosts, theme) = page
                .read()
                .map(|p| (p.hosts.clone(), p.theme.clone()))
                .unwrap_or_default();
            let generated = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let body = render_status_page(&hosts, &theme, version, &generated, &q);
            let _ = stream.write_all(&http_response("200 OK", "text/html; charset=utf-8", &body));
        }
        "/health" | "/healthz" => {
            let _ = stream.write_all(&http_response("200 OK", "text/plain; charset=utf-8", "ok\n"));
        }
        _ => {
            let _ = stream.write_all(&http_response(
                "404 Not Found",
                "text/plain; charset=utf-8",
                "not found: try /\n",
            ));
        }
    }
}

/// Bind the shared listener. Done by the caller — never inside the server
/// thread — so bind failures (port busy, no permission) surface in the UI
/// or CLI instead of dying silently in a background thread's stderr.
pub fn bind_listener(bind: &str, port: u16) -> std::io::Result<TcpListener> {
    let listener = TcpListener::bind(format!("{}:{}", bind, port))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Cap on concurrent HTTP connections (page views + sync pushes). Past it,
/// newcomers get an instant 503 instead of queueing another thread.
pub const MAX_CONNECTIONS: usize = 64;

/// Blocking serve loop over an already-bound listener. `web_enabled` gates
/// the HTML page (sync routes are always live once a token exists);
/// `sync_tx` carries inbound sync events to the main loop. Returns when
/// `shutdown` is set.
pub fn run_server(
    page: SharedPage,
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    web_enabled: Arc<AtomicBool>,
    sync_tx: SyncSender<SyncEvent>,
) {
    let version = env!("CARGO_PKG_VERSION").to_string();
    // Cap concurrent connections: without a cap, stalled scanners pile up
    // one thread each. Over the cap we answer 503 and close immediately.
    let in_flight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // Handlers run on spawned threads; publish the channel thread-locally.
    SYNC_TX.with(|tx| *tx.borrow_mut() = Some(sync_tx.clone()));
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                // Accepted sockets inherit the listener's non-blocking mode
                // on Windows, which makes the handler's first read fail
                // instantly and drops every connection. Force blocking:
                // handlers do one request per thread and want plain reads.
                // Harmless no-op where streams are already blocking.
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                // Stalled connections must not hold a thread forever (slow
                // scanners, dead peers, half-open health checks).
                if stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .is_err()
                {
                    continue;
                }
                let n = in_flight.fetch_add(1, Ordering::Relaxed);
                if n >= MAX_CONNECTIONS {
                    in_flight.fetch_sub(1, Ordering::Relaxed);
                    let _ = stream.write_all(&http_response(
                        "503 Service Unavailable",
                        "text/plain; charset=utf-8",
                        "busy\n",
                    ));
                    continue;
                }
                let page = page.clone();
                let version = version.clone();
                let web_enabled = web_enabled.clone();
                let sync_tx = sync_tx.clone();
                let in_flight = in_flight.clone();
                thread::spawn(move || {
                    SYNC_TX.with(|tx| *tx.borrow_mut() = Some(sync_tx));
                    handle_connection(stream, &page, &version, &web_enabled);
                    in_flight.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                eprintln!("web server accept error: {}", e);
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Spawn the server in the background; the thread exits when `shutdown` flips.
/// Takes an already-bound listener from [`bind_listener`].
pub fn start_in_background(
    page: SharedPage,
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    web_enabled: Arc<AtomicBool>,
    sync_tx: SyncSender<SyncEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || run_server(page, listener, shutdown, web_enabled, sync_tx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    fn get(port: u16, target: &str) -> String {
        let mut s = std::net::TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(format!("GET {} HTTP/1.0\r\n\r\n", target).as_bytes()).unwrap();
        let mut body = String::new();
        s.read_to_string(&mut body).unwrap();
        body
    }

    /// Full stack over real TCP: bind → accept → read → route → respond.
    /// Guards the Windows failure where accepted sockets inherited the
    /// listener's non-blocking mode and every connection was dropped.
    #[test]
    fn live_server_serves_page_and_health() {
        let page = new_shared_page();
        {
            let mut p = page.write().unwrap();
            p.hosts = sample();
        }
        let shutdown = Arc::new(AtomicBool::new(false));
        let enabled = Arc::new(AtomicBool::new(true));
        let (tx, _rx) = sync_channel::<SyncEvent>(8);
        // Bind :0 directly (no probe-then-rebind race with parallel tests);
        // read the assigned port back off the bound socket.
        let listener = bind_listener("127.0.0.1", 0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = start_in_background(
            page,
            listener,
            shutdown.clone(),
            enabled,
            tx,
        );
        let mut page_body = String::new();
        let mut health_body = String::new();
        for _ in 0..100 {
            if std::net::TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
                page_body = get(port, "/");
                health_body = get(port, "/health");
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();
        assert!(page_body.contains("200 OK"), "page head: {}", &page_body[..page_body.len().min(200)]);
        assert!(page_body.contains("ping-uin status"));
        assert!(page_body.contains("Google DNS"));
        assert!(health_body.contains("200 OK") && health_body.contains("ok"));
    }

    fn sample() -> Vec<HostSnapshot> {
        vec![
            HostSnapshot {
                name: "8.8.8.8".to_string(),
                display_name: "Google DNS".to_string(),
                target: "8.8.8.8".to_string(),
                group: "external".to_string(),
                status: "UP".to_string(),
                up: true,
                latency_ms: 12.0,
                uptime_pct: 100.0,
                sla_24h: Some(99.9),
                history: vec![12, 11, 0],
                muted: false,
                suppressed: false,
                flapping: false,
                warn: false,
                down_for_secs: None,
            },
            HostSnapshot {
                name: "db".to_string(),
                display_name: "<b>evil</b>".to_string(),
                target: "db:5432".to_string(),
                group: "g".to_string(),
                status: "DOWN".to_string(),
                up: false,
                latency_ms: 0.0,
                uptime_pct: 50.0,
                sla_24h: None,
                history: vec![0, 0],
                muted: false,
                suppressed: false,
                flapping: false,
                warn: false,
                down_for_secs: Some(90),
            },
        ]
    }

    fn render_default(hosts: &[HostSnapshot], q: &PageQuery) -> String {
        render_status_page(hosts, &SharedTheme::default(), "0.1.0", "2026-01-01 00:00:00", q)
    }

    #[test]
    fn status_page_escapes_and_counts() {
        let html = render_default(&sample(), &PageQuery::default());
        assert!(html.contains("1 up"));
        assert!(html.contains("1 down"));
        assert!(html.contains("&lt;b&gt;evil&lt;/b&gt;"));
        assert!(!html.contains("<b>evil</b>"));
        assert!(html.contains("http-equiv=\"refresh\""));
    }

    #[test]
    fn status_page_groups_by_label() {
        let html = render_default(&sample(), &PageQuery::default());
        // Group blocks with tallies, down-group first.
        assert!(html.contains("<details open>"));
        assert!(html.contains(">g<"));
        assert!(html.contains(">external<"));
        let g_pos = html.find(">g<").unwrap();
        let e_pos = html.find(">external<").unwrap();
        assert!(g_pos < e_pos, "group with the DOWN host sorts first");
    }

    #[test]
    fn status_page_group_filter() {
        let q = PageQuery { sort: SortKey::None, desc: false, group: Some("g".to_string()) };
        let html = render_default(&sample(), &q);
        assert!(html.contains("db:5432"));
        assert!(!html.contains("Google DNS"));
    }

    #[test]
    fn status_page_uses_serving_theme() {
        let mut theme = SharedTheme::default();
        theme.name = "dracula".to_string();
        theme.bg = "#282a36".to_string();
        let html = render_status_page(&sample(), &theme, "0.1.0", "t", &PageQuery::default());
        assert!(html.contains("#282a36"));
        assert!(html.contains("theme dracula"));
    }

    #[test]
    fn status_page_headers_are_sort_links() {
        let q = PageQuery { sort: SortKey::Name, desc: false, group: None };
        let html = render_default(&sample(), &q);
        assert!(html.contains("?sort=name"));
        assert!(html.contains("?sort=status"));
        assert!(html.contains("sorted by <code>name asc</code>"));
    }

    #[test]
    fn sort_parsing_and_application() {
        let q = parse_query("sort=name");
        assert_eq!((q.sort, q.desc), (SortKey::Name, false));
        let q = parse_query("sort=status&order=desc&group=g");
        assert_eq!((q.sort, q.desc), (SortKey::Status, true));
        assert_eq!(q.group.as_deref(), Some("g"));
        assert_eq!(parse_query("bogus=1").sort, SortKey::None);
        let mut hosts = sample();
        apply_sort(&mut hosts, SortKey::Status, false);
        assert_eq!(hosts[0].status, "DOWN"); // problems first
        apply_sort(&mut hosts, SortKey::Name, false);
        assert!(hosts[0].display_name.contains("evil")); // <b>evil</b> < Google DNS
        let mut hosts = sample();
        apply_sort(&mut hosts, SortKey::Name, true);
        assert_eq!(hosts[0].display_name, "Google DNS");
    }

    #[test]
    fn bind_conflict_fails_loudly() {
        // Hold a port, then prove a second bind fails (surfaced to the
        // caller, never swallowed): this is the "joins fail, nothing
        // listening" case when the port is already taken.
        let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = holder.local_addr().unwrap().port();
        assert!(bind_listener("127.0.0.1", port).is_err());
        drop(holder);
        assert!(bind_listener("127.0.0.1", port).is_ok());
    }

    #[test]
    fn lan_urls_resolve_wildcard() {
        let urls = lan_urls("0.0.0.0", 8080);
        // Always at least the LAN guess + localhost; concrete binds stay single.
        assert!(urls.len() >= 2);
        assert!(urls.iter().any(|u| u.contains(":8080/")));
        assert_eq!(lan_urls("127.0.0.1", 8080), vec!["http://127.0.0.1:8080/".to_string()]);
    }

    #[test]
    fn sync_join_rejects_bad_token() {
        // No server needed: handler reads the real on-disk config. With no
        // token configured (or a wrong one presented) it must refuse.
        let (tx, _rx) = sync_channel::<SyncEvent>(4);
        SYNC_TX.with(|t| *t.borrow_mut() = Some(tx));
        let resp = String::from_utf8_lossy(&handle_sync_join(
            r#"{"token":"wrong","from_addr":"1.2.3.4:8080","from_token":"abcdefghjklm"}"#,
        ))
        .into_owned();
        assert!(resp.contains("\"ok\":false"));
    }
}
