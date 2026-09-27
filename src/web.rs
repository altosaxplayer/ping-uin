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

use crate::config::{now_epoch, Config, HostConfig, SyncDeletion};
use crate::config::format_duration;
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

/// Everything the page needs in one lock: hosts + serving theme. The
/// process start epoch is written once at startup and never changes.
#[derive(Clone, Debug, Default)]
pub struct PageState {
    pub hosts: Vec<HostSnapshot>,
    pub theme: SharedTheme,
    pub started_unix: i64,
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

/// Parsed page query: sort + direction + optional group-label filter +
/// text search + grouped-cards view. Default is grouped cards (flat table
/// via `?view=flat`); any `?sort=` switches to a flat sorted table.
#[derive(Clone, Debug)]
pub struct PageQuery {
    pub sort: SortKey,
    pub desc: bool,
    pub group: Option<String>,
    pub grouped: bool,
    pub q: Option<String>,
}

impl Default for PageQuery {
    fn default() -> Self {
        PageQuery {
            sort: SortKey::None,
            desc: false,
            group: None,
            grouped: true,
            q: None,
        }
    }
}

/// Decode `application/x-www-form-urlencoded` (`+` → space, `%XX` bytes).
/// Query values arrive encoded from links and the search form.
pub fn decode_query_value(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Encode for query strings (unreserved chars raw, space → `+`, rest `%XX`).
pub fn encode_query_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Parse `sort`/`order`/`group`/`view`/`q` from a raw query string.
/// Unknown values fall back to grouped view, ascending, no filter.
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
                let v = decode_query_value(v);
                let v = v.trim();
                if !v.is_empty() {
                    out.group = Some(v.to_string());
                }
            }
            "view" | "layout" => {
                // Explicit flat only; anything else (incl. missing) is grouped.
                out.grouped = !matches!(v.to_lowercase().as_str(), "flat" | "off" | "0" | "false" | "list");
            }
            "q" | "query" | "search" => {
                let v = decode_query_value(v);
                let v = v.trim();
                if !v.is_empty() {
                    out.q = Some(v.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

/// True when a host passes the group filter AND the text search (name,
/// target/IP, or group — same coverage as the TUI's `/` search).
pub fn matches_query(h: &HostSnapshot, q: &PageQuery) -> bool {
    if q.group.as_deref().map_or(false, |g| group_label(h) != g) {
        return false;
    }
    match q.q.as_deref() {
        None => true,
        Some(needle) => {
            let needle = needle.to_lowercase();
            h.display_name.to_lowercase().contains(&needle)
                || h.target.to_lowercase().contains(&needle)
                || h.group.to_lowercase().contains(&needle)
        }
    }
}

fn visible_hosts<'a>(hosts: &'a [HostSnapshot], q: &PageQuery) -> Vec<&'a HostSnapshot> {
    hosts.iter().filter(|h| matches_query(h, q)).collect()
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

fn sparkstrip(history: &[u64]) -> String {    history
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

fn latency_text(h: &HostSnapshot) -> String {
    if h.up {
        format!("{:.0} ms", h.latency_ms)
    } else if h.muted {
        "muted".to_string()
    } else if let Some(s) = h.down_for_secs {
        format!("down {}s", s)
    } else {
        "\u{2014}".to_string()
    }
}

fn sla_text(h: &HostSnapshot) -> String {
    h.sla_24h
        .map(|v| format!("{:.1}%", v))
        .unwrap_or_else(|| "\u{2014}".to_string())
}

fn host_row(h: &HostSnapshot) -> String {
    let sla = sla_text(h);
    let lat = latency_text(h);
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

/// Canonical navigation URL preserving view state. Only non-defaults are
/// emitted (no `view` param means grouped): sort/order when sorting, the
/// group filter, and the search text — all encoded.
fn nav_href(sort: SortKey, desc: bool, grouped: bool, group: Option<&str>, q: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if sort != SortKey::None {
        parts.push(format!("sort={}", sort.param()));
        parts.push(format!("order={}", if desc { "desc" } else { "asc" }));
    }
    if !grouped {
        parts.push("view=flat".to_string());
    }
    if let Some(g) = group {
        parts.push(format!("group={}", encode_query_value(g)));
    }
    if let Some(query) = q {
        parts.push(format!("q={}", encode_query_value(query)));
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/?{}", parts.join("&amp;"))
    }
}

/// Header link for a sortable column: clicking the active column toggles
/// asc/desc, clicking another column sorts ascending by it. View state
/// (group, search, grouped/flat) rides along.
fn sort_link(label: &str, key: SortKey, q: &PageQuery) -> String {
    let arrow = if q.sort == key {
        if q.desc { " \u{25bc}" } else { " \u{25b2}" }
    } else {
        ""
    };
    let desc = q.sort == key && !q.desc;
    format!(
        "<a href=\"{}\">{}{}</a>",
        nav_href(key, desc, q.grouped, q.group.as_deref(), q.q.as_deref()),
        html_escape(label),
        arrow
    )
}

/// App uptime display ("3d4h") from the process start epoch. Shared by the
/// initial HTML and the live feed so both agree.
pub fn app_uptime_text(started_unix: i64) -> String {
    if started_unix <= 0 {
        return "\u{2014}".to_string();
    }
    let secs = (now_epoch() - started_unix).max(0) as u64;
    format_duration(secs)
}

/// Live data feed for the page's own poller (`GET /api/state`, same query
/// params as `/`). NOT a public API: no stability promise, shaped exactly
/// for the inline script (preformatted display strings so formatting logic
/// lives here once). Read-only and page-gated like `/` itself.
pub fn state_json(hosts: &[HostSnapshot], version: &str, generated: &str, q: &PageQuery, started_unix: i64) -> String {
    let mut owned: Vec<HostSnapshot> = hosts
        .iter()
        .filter(|h| q.group.as_deref().map_or(true, |g| group_label(h) == g))
        .cloned()
        .collect();
    if q.sort != SortKey::None {
        apply_sort(&mut owned, q.sort, q.desc);
    }
    let up = owned.iter().filter(|h| h.up).count();
    let items: Vec<serde_json::Value> = owned
        .iter()
        .map(|h| {
            serde_json::json!({
                    "display": h.display_name,
                    "target": h.target,
                "group": group_label(h),
                "status": h.status,
                "up": h.up,
                "latency": latency_text(h),
                "uptime": format!("{:.1}%", h.uptime_pct),
                "sla": sla_text(h),
                "history": h.history,
            })
        })
        .collect();
    serde_json::json!({
        "generated": generated,
        "version": version,
        "up": up,
        "down": items.len().saturating_sub(up),
        "app_uptime": app_uptime_text(started_unix),
        "hosts": items,
    })
    .to_string()
}

/// Inline live-update script (vanilla JS, no dependencies). Raw string so
/// quoting stays natural; must never contain the sequence `"##`.
/// Inline live-update script (vanilla JS, no dependencies). Raw string so
/// quoting stays natural: single-quoted JS strings; HTML attributes use
/// double quotes inside them. Must never contain the sequence `"##`.
const POLLER_SCRIPT: &str = r##"
</div><script>
(function(){
var content=document.getElementById('content');
if(!content){return;}
var params=new URLSearchParams(window.location.search);
var grouped=!/^(flat|off|0|false|list)$/i.test(params.get('view')||'');
var api='/api/state'+window.location.search;
var prev={};
var fails=0,timer=null;
function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}
function cls(s){return s==='UP'?'up':(s==='DOWN'?'down':((s==='WARN'||s==='FLAP')?'warn':'muted'));}
function spark(h){var o='',i;for(i=h.length-1;i>=0;i--){if(i<h.length-1){o+=' ';}o+=h[i]>0?'<span class="u">■</span>':'<span class="d">_</span>';}return o;}
function headHtml(){
var g=params.get('group'),v=grouped?'&amp;view=grouped':'';
var gg=g?('&amp;group='+encodeURIComponent(g)):'';
function th(label,key){
var active=params.get('sort')===key;
var order=(active&&params.get('order')!=='desc')?'desc':'asc';
var arrow=active?(params.get('order')==='desc'?' ▼':' ▲'):'';
return '<th'+(active?' class="active"' :'')+'><a href="/?sort='+key+'&amp;order='+order+gg+v+'">'+label+arrow+'</a></th>';
}
return '<thead><tr>'+th('Host','name')+'<th>Target</th>'+th('Status','status')+th('Latency','latency')+th('Group','group')+'<th>Uptime</th>'+th('SLA 24h','sla')+'<th>History</th></tr></thead>';
}
function row(h){
var changed=prev[h.display]!==undefined&&prev[h.display]!==h.status;
return '<tr'+(changed?' class="flash"' :'')+'><td>'+esc(h.display)+'</td><td class="dim mono">'+esc(h.target)+'</td><td><span class="status '+cls(h.status)+'">● '+esc(h.status)+'</span></td><td class="mono">'+esc(h.latency)+'</td><td class="dim">'+esc(h.group)+'</td><td class="mono">'+esc(h.uptime)+'</td><td class="mono">'+esc(h.sla)+'</td><td class="spark mono">'+spark(h.history)+'</td></tr>';
}
function renderFlat(hosts){return '<div class="table-wrap"><table>'+headHtml()+'<tbody>'+hosts.map(row).join('')+'</tbody></table></div>';}
function renderGrouped(hosts){
var map={},names=[],i,h;
for(i=0;i<hosts.length;i++){h=hosts[i];if(!map[h.group]){map[h.group]=[];names.push(h.group);}map[h.group].push(h);}
names.sort(function(a,b){
var ad=map[a].some(function(x){return !x.up;}),bd=map[b].some(function(x){return !x.up;});
if(ad!==bd){return ad?-1:1;}
return a<b?-1:(a>b?1:0);
});
var open={};
content.querySelectorAll('details[data-group]').forEach(function(d){open[d.getAttribute('data-group')]=d.open;});
var out=names.map(function(n){
var ms=map[n].slice().sort(function(a,b){
if(a.up!==b.up){return a.up?1:-1;}
var x=a.display.toLowerCase(),y=b.display.toLowerCase();
return x<y?-1:(x>y?1:0);
});
var u=ms.filter(function(x){return x.up;}).length;
var isOpen=open[n]!==false;
return '<details'+(isOpen?' open':'')+' data-group="'+esc(n)+'"><summary><span class="gname">'+esc(n)+'</span><span class="tally"> · <span class="up">'+u+' up</span> · <span class="down">'+(ms.length-u)+' down</span></span></summary><div class="table-wrap"><table>'+headHtml()+'<tbody>'+ms.map(row).join('')+'</tbody></table></div></details>';
}).join('');
return out||'<p class="sub">No hosts in this view.</p>';
}
function render(d){
var i,h,next={};
for(i=0;i<d.hosts.length;i++){h=d.hosts[i];next[h.display]=h.status;}
document.getElementById('pillUp').textContent='● '+d.up+' up';
document.getElementById('pillDown').textContent='● '+d.down+' down';
document.getElementById('metaGen').textContent='generated '+d.generated;
document.getElementById('appUp').textContent='up '+d.app_uptime;
document.getElementById('liveNote').textContent='live';
content.innerHTML=grouped?renderGrouped(d.hosts):renderFlat(d.hosts);
prev=next;
}
function poll(){
if(document.hidden){timer=setTimeout(poll,5000);return;}
fetch(api,{cache:'no-store'}).then(function(r){if(!r.ok){throw new Error('http '+r.status);}return r.json();}).then(function(d){
fails=0;render(d);timer=setTimeout(poll,5000);
}).catch(function(){
fails++;var note=document.getElementById('liveNote');if(note){note.textContent='reconnecting…';}
timer=setTimeout(poll,Math.min(30000,5000*fails));
});
}
timer=setTimeout(poll,5000);
})();
</script>
"##;


/// Full HTML status page in the serving instance's theme. Static CSS + a
/// live-polling script (meta refresh only under <noscript>). The default
/// view is collapsible per-label group cards, like the TUI; `?view=flat`
/// switches to a flat table — every host its own row — and any `?sort=`
/// switches to a flat sorted table.
pub fn render_status_page(
    hosts: &[HostSnapshot],
    theme: &SharedTheme,
    version: &str,
    generated: &str,
    q: &PageQuery,
    started_unix: i64,
) -> String {
    let visible: Vec<&HostSnapshot> = visible_hosts(hosts, q);
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
    if let Some(s) = q.q.as_deref() {
        state_note.push_str(&format!(" \u{00b7} search <code>{}</code>", html_escape(s)));
    }
    // Reset only when the view differs from the default (grouped, unfiltered).
    if q.sort != SortKey::None || q.group.is_some() || q.q.is_some() || !q.grouped {
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

    // Group filter chips (pure links, no JS) plus a text search form and a
    // flat/grouped toggle. Everything preserves the rest of the view state.
    let mut chips = String::from("<nav class=\"chips\">");
    chips.push_str(&format!(
        "<a class=\"chip{}\" href=\"{}\">All</a>",
        if q.group.is_none() { " active" } else { "" },
        nav_href(q.sort, q.desc, q.grouped, None, q.q.as_deref()),
    ));
    for g in &group_order {
        chips.push_str(&format!(
            "<a class=\"chip{}\" href=\"{}\">{}</a>",
            if q.group.as_deref() == Some(g.as_str()) { " active" } else { "" },
            nav_href(q.sort, q.desc, q.grouped, Some(g), q.q.as_deref()),
            html_escape(g),
        ));
    }
    // View toggle: the alternative layout, keeping sort/filter/search.
    if q.grouped {
        chips.push_str(&format!(
            "<a class=\"chip\" href=\"{}\">Flat</a>",
            nav_href(q.sort, q.desc, false, q.group.as_deref(), q.q.as_deref()),
        ));
    } else {
        chips.push_str(&format!(
            "<a class=\"chip\" href=\"{}\">Grouped</a>",
            nav_href(q.sort, q.desc, true, q.group.as_deref(), q.q.as_deref()),
        ));
    }
    // Text search across name, IP, and group. Plain GET form: no JS, and
    // the live poller never touches it (it lives outside #content), so
    // typing is never interrupted by refreshes.
    chips.push_str("<form class=\"searchform\" method=\"get\" action=\"/\">");
    if q.sort != SortKey::None {
        chips.push_str(&format!(
            "<input type=\"hidden\" name=\"sort\" value=\"{}\">",
            q.sort.param()
        ));
        chips.push_str(&format!(
            "<input type=\"hidden\" name=\"order\" value=\"{}\">",
            if q.desc { "desc" } else { "asc" }
        ));
    }
    if !q.grouped {
        chips.push_str("<input type=\"hidden\" name=\"view\" value=\"flat\">");
    }
    if let Some(g) = q.group.as_deref() {
        chips.push_str(&format!(
            "<input type=\"hidden\" name=\"group\" value=\"{}\">",
            html_escape(g)
        ));
    }
    chips.push_str(&format!(
        "<input type=\"search\" name=\"q\" value=\"{}\" placeholder=\"Search name or IP…\" aria-label=\"Search hosts\">",
        q.q.as_deref().map(html_escape).unwrap_or_default()
    ));
    chips.push_str("<input type=\"submit\" value=\"Search\">");
    chips.push_str("</form>");
    chips.push_str("</nav>");

    let body = if q.sort != SortKey::None {
        let mut owned: Vec<HostSnapshot> = visible.into_iter().cloned().collect();
        apply_sort(&mut owned, q.sort, q.desc);
        format!(
            "<div class=\"table-wrap\"><table>{}<tbody>{}</tbody></table></div>",
            header,
            owned.iter().map(host_row).collect::<String>()
        )
    } else if q.grouped {
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
            // Native collapsible: no JS needed. data-group lets the live
            // poller preserve open/closed state across re-renders.
            out.push_str(&format!(
                "<details open data-group=\"{}\"><summary><span class=\"gname\">{}</span><span class=\"tally\"> \u{00b7} <span class=\"up\">{} up</span> \u{00b7} <span class=\"down\">{} down</span></span></summary><div class=\"table-wrap\"><table>{}<tbody>{}</tbody></table></div></details>\n",
                html_escape(name),
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
    } else {
        // Default: flat table, one row per host in probe order — like the TUI.
        let rows: String = visible.iter().map(|h| host_row(h)).collect();
        if rows.is_empty() {
            "<p class=\"sub\">No hosts in this view.</p>".to_string()
        } else {
            format!(
                "<div class=\"table-wrap\"><table>{}<tbody>{}</tbody></table></div>",
                header, rows
            )
        }
    };

    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
        <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
        <noscript><meta http-equiv=\"refresh\" content=\"15\"></noscript>\
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
        tbody tr.flash{{animation:flashbg 1.6s ease-out}}\
        @keyframes flashbg{{0%{{background:transparent}}25%{{background:rgba(128,128,128,.22)}}100%{{background:transparent}}}}\
        code{{background:var(--card);border:1px solid var(--divider);padding:2px 6px;border-radius:4px;font-size:12px}}\
        @media (max-width:720px){{body{{padding:16px 12px 32px}}h1{{font-size:19px}}thead th,tbody td{{padding:7px 8px}}.pill{{font-size:12.5px;padding:5px 12px}}}}\
        </style></head><body><div class=\"wrap\">\
        <header class=\"hero\"><div><h1>((\u{2022}O\u{2022})) ping-uin status</h1>\
        <p class=\"sub\"><span id=\"metaGen\">generated {generated}</span> \u{00b7} v{version} \u{00b7} theme {themename} \u{00b7} <span id=\"appUp\">up {appup}</span> \u{00b7} <span id=\"liveNote\">auto-refresh</span>{note}</p></div>\
        <div class=\"pills\"><span class=\"pill up\" id=\"pillUp\">\u{25cf} {up} up</span><span class=\"pill down\" id=\"pillDown\">\u{25cf} {down} down</span></div></header>\
        {chips}\
        <main id=\"content\">{body}</main>\
        </div>{script}</body></html>",
        body = body,
        up = up,
        down = down,
        generated = html_escape(generated),
        version = html_escape(version),
        themename = html_escape(&theme.name),
        note = state_note,
        appup = html_escape(&app_uptime_text(started_unix)),
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
        script = POLLER_SCRIPT,
    )
}

fn http_response(status: &str, content_type: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {}\r\nServer: ping-uin\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
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
    peer: std::net::SocketAddr,
) {
    let Some((method, target, body)) = read_request(&mut stream) else {
        // Accepted but unreadable (RST, timeout, garbage): say so instead of
        // closing silently, or every such case looks like "empty reply".
        eprintln!("web: dropping {}: unreadable request", peer);
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
            let (hosts, theme, started_unix) = page
                .read()
                .map(|p| (p.hosts.clone(), p.theme.clone(), p.started_unix))
                .unwrap_or_default();
            let generated = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let body = render_status_page(&hosts, &theme, version, &generated, &q, started_unix);
            let _ = stream.write_all(&http_response("200 OK", "text/html; charset=utf-8", &body));
        }
        "/health" | "/healthz" => {
            // Versioned body: `curl` tells you WHO answers (us vs a port
            // squatter), which decides the whole "empty reply" class of bugs.
            let body = format!("ok ping-uin {}\n", version);
            let _ = stream.write_all(&http_response("200 OK", "text/plain; charset=utf-8", &body));
        }
        "/api/state" | "/api/state/" => {
            // Live feed for the page's own poller (same query params as /).
            // Page-gated: no page, no feed.
            if !web_enabled.load(Ordering::Relaxed) {
                let _ = stream.write_all(&http_response(
                    "404 Not Found",
                    "text/plain; charset=utf-8",
                    "web page not enabled on this device (press W in its TUI)\n",
                ));
                return;
            }
            let q = parse_query(&query);
            let (hosts, started_unix) = page
                .read()
                .map(|p| (p.hosts.clone(), p.started_unix))
                .unwrap_or_default();
            let generated = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let body = state_json(&hosts, version, &generated, &q, started_unix);
            let _ = stream.write_all(&http_response("200 OK", "application/json", &body));
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

/// How many ports past the requested one to try before giving up. Nobody
/// should have to pass `--port` just because the default is busy: the app
/// takes the first free port and reports the actual address everywhere
/// (page URLs, join codes, push callbacks). An explicit `--port` is still
/// honored as the first choice (matters for services with stable peers).
pub const FALLBACK_TRIES: u16 = 32;

/// Bind `port`, falling back upward to the first free one. Returns the
/// listener plus the port actually won. Only address-in-use falls back:
/// permission errors (e.g. ports <1024 without privilege) fail fast with
/// the original error — silently serving elsewhere would strand everyone
/// holding the requested address.
pub fn bind_first_free(bind: &str, port: u16) -> std::io::Result<(TcpListener, u16)> {
    let mut last_err = std::io::Error::new(std::io::ErrorKind::AddrInUse, "no ports tried");
    for p in port..=port.saturating_add(FALLBACK_TRIES) {
        match bind_listener(bind, p) {
            Ok(l) => return Ok((l, p)),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => last_err = e,
            Err(e) => return Err(e),
        }
    }
    Err(last_err)
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
            Ok((mut stream, peer)) => {
                // Accepted sockets inherit the listener's non-blocking mode
                // on Windows, which makes the handler's first read fail
                // instantly and drops every connection. Force blocking:
                // handlers do one request per thread and want plain reads.
                // Harmless no-op where streams are already blocking.
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                // Stalled connections must not hold a thread forever (slow
                // scanners, dead peers, half-open health checks). If the
                // timeout itself can't be set, serve anyway: dropping the
                // connection here would look like "empty reply from server".
                if stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .is_err()
                {
                    eprintln!("web: {}: cannot set read timeout, serving without", peer);
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
                    handle_connection(stream, &page, &version, &web_enabled, peer);
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
        render_status_page(hosts, &SharedTheme::default(), "0.1.0", "2026-01-01 00:00:00", q, 1790460000)
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
    fn status_page_default_is_grouped_like_the_tui() {
        let html = render_default(&sample(), &PageQuery::default());
        // Group cards by default (NB: the inline script mentions
        // "<details"+suffix without a space, so "<details " only matches
        // real elements).
        assert!(html.contains("<details open data-group"));
        assert!(html.contains("Google DNS"));
        assert!(html.contains("db:5432"));
        let goog = html.find("Google DNS").unwrap();
        let db = html.find("db:5432").unwrap();
        assert!(db < goog, "grouped view puts the DOWN host's group first");
    }

    #[test]
    fn status_page_groups_by_label() {
        let q = PageQuery { sort: SortKey::None, desc: false, group: None, grouped: true, q: None };
        let html = render_default(&sample(), &q);
        // Group blocks with tallies, down-group first.
        assert!(html.contains("<details open data-group"));
        assert!(html.contains(">g<"));
        assert!(html.contains(">external<"));
        let g_pos = html.find(">g<").unwrap();
        let e_pos = html.find(">external<").unwrap();
        assert!(g_pos < e_pos, "group with the DOWN host sorts first");
    }

    #[test]
    fn status_page_group_filter() {
        let q = PageQuery { sort: SortKey::None, desc: false, group: Some("g".to_string()), grouped: false, q: None };
        let html = render_default(&sample(), &q);
        assert!(html.contains("db:5432"));
        assert!(!html.contains("Google DNS"));
    }

    #[test]
    fn status_page_uses_serving_theme() {
        let mut theme = SharedTheme::default();
        theme.name = "dracula".to_string();
        theme.bg = "#282a36".to_string();
        let html = render_status_page(&sample(), &theme, "0.1.0", "t", &PageQuery::default(), 1790460000);
        assert!(html.contains("#282a36"));
        assert!(html.contains("theme dracula"));
    }

    #[test]
    fn status_page_headers_are_sort_links() {
        let q = PageQuery { sort: SortKey::Name, desc: false, group: None, grouped: false, q: None };
        let html = render_default(&sample(), &q);
        assert!(html.contains("?sort=name"));
        assert!(html.contains("?sort=status"));
        assert!(html.contains("sorted by <code>name asc</code>"));
    }

    #[test]
    fn view_param_toggles_grouped_cards() {
        let q = parse_query("view=grouped");
        assert!(q.grouped);
        assert!(parse_query("").grouped);
        assert!(parse_query("view=flat").grouped == false);
        assert!(parse_query("view=list").grouped == false);
        assert!(parse_query("sort=name&view=grouped").sort == SortKey::Name);
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
        // listening" case when the port is already taken. No rebind
        // assertion: under parallel tests another test may grab a freed
        // ephemeral port first (that race is what bind_first_free is for).
        let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = holder.local_addr().unwrap().port();
        assert!(bind_listener("127.0.0.1", port).is_err());
        drop(holder);
    }

    #[test]
    fn state_feed_shape_sort_and_filter() {
        // Flat default: probe order, counts, app uptime, preformatted cells.
        let body = state_json(&sample(), "0.1.0", "2026-01-01 00:00:00", &PageQuery::default(), 1790460000);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["down"], 1);
        assert_eq!(v["hosts"].as_array().unwrap().len(), 2);
        assert_eq!(v["hosts"][0]["display"], "Google DNS");
        assert_eq!(v["hosts"][1]["status"], "DOWN");
        assert_eq!(v["hosts"][0]["latency"], "12 ms");
        assert_eq!(v["hosts"][1]["latency"], "down 90s");
        assert_eq!(v["hosts"][0]["history"], serde_json::json!([12, 11, 0]));
        assert!(v["app_uptime"].as_str().unwrap().len() > 0);
        // Sort + group params apply to the feed exactly like the page.
        let q = parse_query("sort=status&order=desc");
        let v: serde_json::Value =
            serde_json::from_str(&state_json(&sample(), "0.1.0", "t", &q, 0)).unwrap();
        assert_eq!(v["hosts"][0]["status"], "UP");
        let q = parse_query("group=g");
        let v: serde_json::Value =
            serde_json::from_str(&state_json(&sample(), "0.1.0", "t", &q, 0)).unwrap();
        assert_eq!(v["hosts"].as_array().unwrap().len(), 1);
        assert_eq!(v["hosts"][0]["target"], "db:5432");
    }

    #[test]
    fn page_updates_without_reload() {
        // No full-page meta refresh anymore; the poller + pill/meta IDs the
        // script needs are present; noscript keeps a refresh fallback.
        let html = render_default(&sample(), &PageQuery::default());
        // No bare full-page refresh: exactly one refresh tag exists, and it
        // lives inside <noscript> as the JS-less fallback.
        assert_eq!(html.matches("http-equiv=\"refresh\"").count(), 1);
        assert!(html.contains("<noscript><meta http-equiv=\"refresh\" content=\"15\"></noscript>"));
        for id in ["content", "pillUp", "pillDown", "metaGen", "appUp", "liveNote"] {
            assert!(html.contains(&format!("id=\"{}\"", id)), "missing #{}", id);
        }
        assert!(html.contains("/api/state"));
        assert!(html.contains("setTimeout(poll,5000)"));
    }

    #[test]
    fn bind_first_free_moves_past_busy_ports() {
        // Occupy a port, request exactly it, and prove the fallback wins a
        // nearby free one and reports it (callers display/join-code the
        // actual port). The probe holds `busy` for the duration.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = probe.local_addr().unwrap().port();
        let (listener, actual) = bind_first_free("127.0.0.1", busy).unwrap();
        let won = listener.local_addr().unwrap().port();
        assert_eq!(won, actual);
        assert_ne!(won, busy, "must not take the held port");
        drop(probe);
        drop(listener);
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
