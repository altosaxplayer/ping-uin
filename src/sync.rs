//! Neighbor sync: keep host lists converged across ping-uin instances.
//!
//! Pairing uses an explicit join code, not LAN discovery: device A shows a
//! code (`Y` in the TUI or `--sync-code`), device B enters it (`--sync-join`
//! or `Y` → join). The code carries A's address plus a secret token, so only
//! whoever reads the code off A's screen can pair.
//!
//! After pairing, sync is bidirectional: instances push their full host
//! state to each peer every minute. Adds and edits merge last-write-wins
//! per host via `updated_at`; removals propagate as tombstones
//! (`sync_deleted`) so a delete on one side isn't resurrected by the next
//! push from the other side. Probing, history, and uptime logs stay local —
//! only the host *config* converges. The config file has a single writer
//! (the main loop); background threads report through `SyncEvent`.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::SyncSender,
    Arc,
};
use std::thread;
use std::time::Duration;

use crate::config::{now_epoch, Config, HostConfig, SyncDeletion, SyncPeer};

/// Tombstones older than this are pruned (a month is plenty for a delete to
/// reach every peer through the minute-interval pushes).
pub const TOMBSTONE_TTL_SECS: i64 = 30 * 24 * 3600;

/// Unambiguous lowercase alphabet (no l/1/o/0).
const TOKEN_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
const TOKEN_LEN: usize = 12;

/// Pairing secret from the OS RNG, e.g. `ab3dx9kq2zv7`.
pub fn generate_token() -> String {
    let alpha = TOKEN_ALPHABET.len() as u8;
    let mut buf = [0u8; TOKEN_LEN];
    if getrandom::getrandom(&mut buf).is_err() {
        // Practically unreachable; fall back to a time/pid hash so pairing
        // still works (LAN-trusted context, code shown on screen anyway).
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        (now_epoch(), std::process::id(), fallback_counter()).hash(&mut h);
        let mut v = h.finish();
        for b in buf.iter_mut() {
            v = v.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *b = (v >> 33) as u8;
        }
    }
    buf.iter()
        .map(|b| TOKEN_ALPHABET[(b % alpha) as usize] as char)
        .collect()
}

fn fallback_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering as O};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, O::Relaxed)
}

/// `abcdefghijkl` → `abcd-efgh-ijkl` for readable codes.
pub fn group_token(token: &str) -> String {
    token
        .chars()
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

fn ungroup_token(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>()
}

fn valid_token(s: &str) -> bool {
    s.len() == TOKEN_LEN && s.bytes().all(|b| TOKEN_ALPHABET.contains(&b))
}

/// Build a join code: `PUIN-<host>-<port>-<xxxx-xxxx-xxxx>`.
pub fn make_join_code(host: &str, port: u16, token: &str) -> String {
    format!("PUIN-{}-{}-{}", host, port, group_token(token))
}

/// Parse a join code back into (`addr` = `host:port`, token).
/// Tolerant of case, spaces, and dashes inside hostnames.
///
/// If the code carries the wrong IP (VPNs and Docker interfaces often win
/// the automatic guess), override it by appending `@` plus the reachable
/// address: `PUIN-172.16.10.2-8080-abcd-efgh-jkmn @ 192.168.1.42`
/// (or `... @ 192.168.1.42:8080` to override the port too).
pub fn parse_join_code(code: &str) -> Result<(String, String), String> {
    let (code, override_addr) = match code.split_once('@') {
        Some((c, o)) => (c, Some(o.trim().to_string())),
        None => (code, None),
    };
    let c = code.trim().to_uppercase().replace(' ', "");
    let c = c.strip_prefix("PUIN-").unwrap_or(&c).to_lowercase();
    let parts: Vec<&str> = c.split('-').collect();
    if parts.len() < 5 {
        return Err("join code looks short — it should look like PUIN-192.168.1.42-8080-abcd-efgh-jklm".to_string());
    }
    // Token = last 3 groups (12 chars); port = group before those.
    let token: String = parts[parts.len() - 3..].join("");
    let token = ungroup_token(&token);
    if !valid_token(&token) {
        return Err("bad token in join code (check for typos)".to_string());
    }
    let port: u16 = parts[parts.len() - 4]
        .parse()
        .map_err(|_| "bad port in join code".to_string())?;
    if port == 0 {
        return Err("bad port in join code".to_string());
    }
    let host = parts[..parts.len() - 4].join("-");
    if host.is_empty() {
        return Err("missing address in join code".to_string());
    }
    let mut addr = format!("{}:{}", host, port);
    // Optional `@host[:port]` override for when the code's IP isn't the
    // reachable one (checked last so the error names the real target).
    if let Some(o) = override_addr {
        if o.is_empty() {
            return Err("nothing after @ — give the reachable address, e.g. code @ 192.168.1.42".to_string());
        }
        // rsplit: a trailing :port wins for plain hosts/IPv4. Any other colon
        // is rejected outright (IPv6 sync isn't targeted, and `host:abc` is
        // a typo, not a hostname) instead of misdialing later.
        let (ohost, oport) = match o.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && !h.contains(':') && p.parse::<u16>().map_or(false, |n| n > 0) => {
                (h.to_string(), p.parse::<u16>().unwrap())
            }
            _ if o.contains(':') => {
                return Err("bad address after @ — use host or host:port (1-65535)".to_string());
            }
            _ => (o, port),
        };
        if ohost.is_empty() {
            return Err("bad address after @".to_string());
        }
        addr = format!("{}:{}", ohost.to_lowercase(), oport);
    }
    Ok((addr, token))
}

/// One-line diagnosis for a failed join, so the fix is obvious instead of
/// a bare I/O error. Used by the CLI (appended to the raw error) and the
/// TUI (shown on its own, since popups clip long lines).
pub fn join_error_hint(err: &str) -> &'static str {
    let e = err.to_lowercase();
    if e.contains("refused") {
        "connection refused — is ping-uin running there (TUI or --serve), on the same port, and allowed through its firewall?"
    } else if e.contains("timeout") || e.contains("timed out") {
        "timed out — wrong IP in the code (VPN? Docker? hotspot isolation?), firewall, or different networks"
    } else if e.contains("404") {
        "that device answered but has no sync endpoint — it needs ping-uin v0.2.0 or newer"
    } else if e.contains("lookup") || e.contains("resolve") || e.contains("dns") || e.contains("name or service") {
        "can't resolve that address — check the code for typos"
    } else if e.contains("bad token") || e.contains("rejected") {
        "rejected — generate a fresh code on the other device (codes die when regenerated)"
    } else {
        "check both devices are on the same network with ping-uin running"
    }
}
/// Outbound-interface LAN IP via a UDP `connect()` (sends no packets).
/// Returns None offline — callers fall back to the bind address.
pub fn primary_lan_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // UDP connect only picks a route; no SYN/packet leaves the host.
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr().ok().map(|a| a.ip().to_string())
}

/// This device's hostname for the sync menu (`hostname` command, then
/// `$HOSTNAME`, then "unknown").
pub fn device_hostname() -> String {
    if let Ok(out) = std::process::Command::new("hostname").output() {
        if out.status.success() {
            let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !h.is_empty() {
                return h;
            }
        }
    }
    std::env::var("HOSTNAME")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// `format_epoch(0)` → "—"; otherwise local `YYYY-MM-DD HH:MM`.
pub fn format_epoch(ep: i64) -> String {
    if ep <= 0 {
        return "\u{2014}".to_string();
    }
    chrono::Local
        .timestamp_opt(ep, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "\u{2014}".to_string())
}

/// Relative age for the sync menu: "never" / "12s" / "3m" / "2h" / "5d".
pub fn ago(ep: i64, now: i64) -> String {
    if ep <= 0 {
        return "never".to_string();
    }
    let s = (now - ep).max(0) as u64;
    if s < 60 {
        format!("{}s ago", s)
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86400)
    }
}

use chrono::TimeZone;

/// Bidirectional merge of full sync state. Rules:
/// - Tombstones union (max `at` per name), pruned past the TTL.
/// - A local host dies iff a tombstone (either side) is newer than its
///   `updated_at`; a newer incoming edit resurrects (drops the tombstone).
/// - Remaining hosts merge last-write-wins per name via `updated_at`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MergeStats {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

pub fn merge_state(
    local_hosts: &mut Vec<HostConfig>,
    local_deleted: &mut Vec<SyncDeletion>,
    incoming_hosts: &[HostConfig],
    incoming_deleted: &[SyncDeletion],
    now: i64,
) -> MergeStats {
    let mut stats = MergeStats::default();
    // 1. Union tombstones (max at), prune stale ones.
    for inc in incoming_deleted {
        match local_deleted.iter_mut().find(|d| d.name == inc.name) {
            Some(cur) => cur.at = cur.at.max(inc.at),
            None => local_deleted.push(inc.clone()),
        }
    }
    local_deleted.retain(|d| now - d.at < TOMBSTONE_TTL_SECS);
    // 2. Incoming tombstones kill local hosts edited before the delete.
    let before = local_hosts.len();
    local_hosts.retain(|h| {
        !local_deleted.iter().any(|d| d.name == h.name && d.at > h.updated_at)
    });
    stats.removed = before.saturating_sub(local_hosts.len());
    // 3. Incoming hosts: skip ones our newer delete already covers, drop
    //    tombstones superseded by their newer edit, then upsert by date.
    for inc in incoming_hosts {
        if local_deleted.iter().any(|d| d.name == inc.name && d.at > inc.updated_at) {
            continue;
        }
        local_deleted.retain(|d| !(d.name == inc.name && d.at <= inc.updated_at));
        match local_hosts.iter_mut().find(|h| h.name == inc.name) {
            Some(cur) => {
                if inc.updated_at > cur.updated_at {
                    *cur = inc.clone();
                    stats.updated += 1;
                }
            }
            None => {
                local_hosts.push(inc.clone());
                stats.added += 1;
            }
        }
    }
    stats
}

/// Inbound sync activity for the main loop (the single config writer).
pub struct SyncEvent {
    pub hosts: Vec<HostConfig>,
    pub deleted: Vec<SyncDeletion>,
    /// Sender's listen addr (`ip:port`), for stamping peer `last_sync`.
    pub from_addr: String,
    pub from_hostname: String,
    /// Set for `/sync/join`: store this peer after merging.
    pub new_peer: Option<SyncPeer>,
    /// Set for push-loop results: `ok` tells whether to stamp `last_sync`.
    pub push_result: Option<(String, bool)>,
}

impl SyncEvent {
    pub fn push(hosts: Vec<HostConfig>, deleted: Vec<SyncDeletion>, from_addr: String, from_hostname: String) -> Self {
        SyncEvent { hosts, deleted, from_addr, from_hostname, new_peer: None, push_result: None }
    }

    pub fn peer(addr: String, token: String, hostname: String, hosts: Vec<HostConfig>, deleted: Vec<SyncDeletion>) -> Self {
        let peer = SyncPeer {
            addr: addr.clone(),
            token,
            hostname: hostname.clone(),
            joined_at: now_epoch(),
            last_sync: now_epoch(),
        };
        SyncEvent { hosts, deleted, from_addr: addr, from_hostname: hostname, new_peer: Some(peer), push_result: None }
    }
}

/// Fast reachability gate: ureq's timeout does not bound the TCP connect
/// phase against blackholes (30s stall observed), so probe with our own
/// short-timeout connect first. Error strings stay in the vocabulary that
/// [`is_unreachable`] classifies (refused / timed out / resolve).
fn check_reachable(addr: &str) -> Result<(), String> {
    use std::net::{TcpStream, ToSocketAddrs};
    let sock = addr
        .to_socket_addrs()
        .map_err(|_| format!("can't resolve {}", addr))?
        .next()
        .ok_or_else(|| format!("can't resolve {}", addr))?;
    match TcpStream::connect_timeout(&sock, Duration::from_secs(2)) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            Err("connection refused".to_string())
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            Err("connection timed out".to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn post_json(url: &str, body: &str) -> Result<String, String> {
    post_json_timeout(url, body, Duration::from_secs(10))
}

fn post_json_timeout(url: &str, body: &str, timeout: Duration) -> Result<String, String> {
    ureq::post(url)
        .set("Content-Type", "application/json")
        .set("User-Agent", "ping-uin-sync")
        .timeout(timeout)
        .send_string(body)
        .map_err(|e| match &e {
            // Keep the HTTP status in the message: a 404 from /sync/* means
            // the other side predates sync support (pre-v0.2.0), and the
            // hint mapper + humans both need to see it.
            ureq::Error::Status(code, _) => format!("request failed: HTTP {}: {}", code, e),
            _ => format!("request failed: {}", e),
        })?
        .into_string()
        .map_err(|e| format!("bad response: {}", e))
}

/// Push our full sync state to one peer. Ok(()) on `{"ok":true}`.
pub fn push_to_peer(
    peer: &SyncPeer,
    hosts: &[HostConfig],
    deleted: &[SyncDeletion],
    from_addr: &str,
    from_hostname: &str,
) -> Result<(), String> {
    // Fail fast: a blackholed peer must not stall a background push thread.
    check_reachable(&peer.addr).map_err(|e| format!("{}: {}", peer.addr, e))?;
    let body = serde_json::json!({
        "token": peer.token,
        "from_addr": from_addr,
        "from_hostname": from_hostname,
        "hosts": hosts,
        "deleted": deleted,
    })
    .to_string();
    let resp = post_json(&format!("http://{}/sync/push", peer.addr), &body)?;
    let v: serde_json::Value =
        serde_json::from_str(&resp).map_err(|e| format!("bad response: {}", e))?;
    if v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
        Ok(())
    } else {
        Err(v.get("error").and_then(|e| e.as_str()).unwrap_or("rejected").to_string())
    }
}

/// Join via a code with a caller-chosen timeout: presents our callback so
/// pairing is two-way. Returns the other side's (hosts, tombstones, hostname)
/// to merge locally.
fn join_with_code_timeout(
    addr: &str,
    token: &str,
    from_addr: &str,
    from_token: &str,
    from_hostname: &str,
    timeout: Duration,
) -> Result<(Vec<HostConfig>, Vec<SyncDeletion>, String), String> {
    let body = serde_json::json!({
        "token": token,
        "from_addr": from_addr,
        "from_token": from_token,
        "from_hostname": from_hostname,
    })
    .to_string();
    // Fail fast before ureq (see check_reachable): its timeout doesn't bound
    // blackholed connects, which would stall the join for ~30s.
    check_reachable(addr).map_err(|e| format!("request failed: http://{}/sync/join: {}", addr, e))?;
    let resp = post_json_timeout(&format!("http://{}/sync/join", addr), &body, timeout)?;
    let v: serde_json::Value =
        serde_json::from_str(&resp).map_err(|e| format!("bad response: {}", e))?;
    if !v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
        return Err(v.get("error").and_then(|e| e.as_str()).unwrap_or("join rejected").to_string());
    }
    let hosts: Vec<HostConfig> = v
        .get("hosts")
        .and_then(|h| serde_json::from_value(h.clone()).ok())
        .ok_or_else(|| "bad host list in response".to_string())?;
    let deleted: Vec<SyncDeletion> = v
        .get("deleted")
        .and_then(|d| serde_json::from_value(d.clone()).ok())
        .unwrap_or_default();
    let hostname: String = v
        .get("hostname")
        .and_then(|h| h.as_str())
        .unwrap_or("")
        .to_string();
    Ok((hosts, deleted, hostname))
}

/// True when the join failed before any application answer came back
/// (unreachable host) as opposed to being answered and rejected (wrong
/// token, outdated peer). Only the former is worth a LAN scan — a rejection
/// is definitive and must not spray the neighborhood.
pub fn is_unreachable(err: &str) -> bool {
    let e = err.to_lowercase();
    [
        "refus", "timed out", "timeout", "timedout", "network error", "eof",
        "reset", "broken pipe", "unreachable", "no route", "couldn't connect",
        "connect error", "connection failed", "connection closed",
        "resolv", "lookup", "dns", "nodename", "name or service",
    ]
    .iter()
    .any(|k| e.contains(k))
}

/// First three octets of an IPv4 address → the /24 to scan. Pure helper so
/// the derivation itself is unit-testable (the live address comes from
/// [`primary_lan_ip`]).
pub fn subnet24_of(ip: &str) -> Option<String> {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok()) {
        Some(format!("{}.{}.{}", parts[0], parts[1], parts[2]))
    } else {
        None
    }
}

/// Our own last octet, so the scan skips self (our listener would just
/// reject our own token anyway, but no point knocking).
fn own_last_octet() -> Option<u8> {
    primary_lan_ip()?.rsplit('.').next()?.parse().ok()
}

/// Minimal `GET /health` probe over a short-timeout raw connection: true
/// only for a 200 whose body is exactly `ok` (i.e. a ping-uin listener).
fn health_ok(addr: &str) -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let sock: std::net::SocketAddr = match addr.parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let mut stream = match TcpStream::connect_timeout(&sock, Duration::from_millis(250)) {
        Ok(s) => s,
        Err(_) => return false,
    };
    if stream.set_read_timeout(Some(Duration::from_millis(800))).is_err() {
        return false;
    }
    if stream.write_all(b"GET /health HTTP/1.0\r\n\r\n").is_err() {
        return false;
    }
    let mut buf = [0u8; 512];
    let mut raw = Vec::new();
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > 2048 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&raw);
    match text.split_once("\r\n\r\n") {
        Some((head, body)) => head.contains("200") && body.trim() == "ok",
        None => false,
    }
}

/// Try one candidate address: health-gate first (cheap), full join second.
/// Returns the join result plus the address that answered.
fn try_candidate(
    addr: &str,
    token: &str,
    from_addr: &str,
    from_token: &str,
    from_hostname: &str,
) -> Option<(Vec<HostConfig>, Vec<SyncDeletion>, String, String)> {
    if !health_ok(addr) {
        return None;
    }
    match join_with_code_timeout(addr, token, from_addr, from_token, from_hostname, Duration::from_secs(4)) {
        Ok((hosts, deleted, hostname)) => Some((hosts, deleted, hostname, addr.to_string())),
        Err(_) => None,
    }
}

/// Join with automatic LAN discovery: try the code's address first (fast,
/// 3s budget), and only when it is unreachable — never on rejection — scan
/// our own /24 for a ping-uin listener holding this token. `notify` reports
/// progress (e.g. "scanning 192.168.1.0/24 …") to the UI/CLI.
/// Returns (hosts, tombstones, peer hostname, address that answered).
pub fn join_device(
    peer_addr: &str,
    token: &str,
    from_addr: &str,
    from_token: &str,
    from_hostname: &str,
    notify: &dyn Fn(String),
) -> Result<(Vec<HostConfig>, Vec<SyncDeletion>, String, String), String> {
    match join_with_code_timeout(peer_addr, token, from_addr, from_token, from_hostname, Duration::from_secs(3)) {
        Ok((hosts, deleted, hostname)) => return Ok((hosts, deleted, hostname, peer_addr.to_string())),
        Err(e) if !is_unreachable(&e) => return Err(e),
        Err(first_err) => {
            let subnet = subnet24_of(&primary_lan_ip().unwrap_or_default()).ok_or_else(|| {
                format!("{} (and no local subnet to scan)", first_err)
            })?;
            let port = peer_addr.rsplit(':').next().unwrap_or("8080").to_string();
            notify(format!("code address unreachable, scanning {}.0/24 …", subnet));
            match scan_subnet(&subnet, &port, own_last_octet(), token, from_addr, from_token, from_hostname) {
                Some((hosts, deleted, hostname, via)) => Ok((hosts, deleted, hostname, via)),
                None => Err(format!("{} (also scanned {}.0/24: no ping-uin answering)", first_err, subnet)),
            }
        }
    }
}

/// Probe every host in `subnet` (a "192.168.1" prefix) on `port` in parallel
/// and join the first ping-uin listener that accepts `token`. Skips our own
/// last octet. Bounded by the per-connection timeouts above.
fn scan_subnet(
    subnet: &str,
    port: &str,
    skip_octet: Option<u8>,
    token: &str,
    from_addr: &str,
    from_token: &str,
    from_hostname: &str,
) -> Option<(Vec<HostConfig>, Vec<SyncDeletion>, String, String)> {
    use std::sync::atomic::{AtomicBool, Ordering as O};
    let found = AtomicBool::new(false);
    let result = std::sync::Mutex::new(None);
    // Batched scopes (32 at a time) instead of one 253-thread scope:
    // bounds concurrent threads *and* sequential spawn cost, and lets an
    // early hit skip the remaining batches.
    let mut addrs: Vec<String> = (1u8..=254u8)
        .filter(|last| Some(*last) != skip_octet)
        .map(|last| format!("{}.{}:{}", subnet, last, port))
        .collect();
    // Probe our closest neighbors first — DHCP hands out nearby addresses,
    // so the peer is usually within a few doors either way.
    if let Some(own) = skip_octet {
        addrs.sort_by_key(|a| {
            let ip = a.rsplit_once(':').map(|(h, _)| h).unwrap_or(a.as_str());
            let last: u8 = ip.rsplit('.').next().and_then(|o| o.parse().ok()).unwrap_or(0);
            last.abs_diff(own)
        });
    }
    for chunk in addrs.chunks(32) {
        if found.load(O::Relaxed) {
            break;
        }
        std::thread::scope(|s| {
            for addr in chunk {
                if found.load(O::Relaxed) {
                    break;
                }
                let addr = addr.clone();
                let token = token.to_string();
                let from_addr = from_addr.to_string();
                let from_token = from_token.to_string();
                let from_hostname = from_hostname.to_string();
                let found_ref = &found;
                let result_ref = &result;
                s.spawn(move || {
                    if found_ref.load(O::Relaxed) {
                        return;
                    }
                    if let Some(out) = try_candidate(&addr, &token, &from_addr, &from_token, &from_hostname) {
                        if let Ok(mut slot) = result_ref.lock() {
                            if slot.is_none() {
                                *slot = Some(out);
                                found_ref.store(true, O::Relaxed);
                            }
                        }
                    }
                });
            }
        });
    }
    result.into_inner().ok().flatten()
}

/// Background push loop: every 60s, load the on-disk config and push to all
/// peers (each in its own thread). Results come back as `SyncEvent`s so the
/// main loop stays the single config writer. `port`/`hostname` identify us.
pub fn spawn_push_loop(
    shutdown: Arc<AtomicBool>,
    port: u16,
    hostname: String,
    tx: SyncSender<SyncEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Small delay so startup isn't competing with the first probes.
        for _ in 0..6 {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(Duration::from_secs(10));
        }
        while !shutdown.load(Ordering::Relaxed) {
            let from_addr = format!(
                "{}:{}",
                primary_lan_ip().unwrap_or_else(|| "127.0.0.1".to_string()),
                port
            );
            let cfg = Config::load();
            for peer in cfg.sync_peers.clone() {
                let tx = tx.clone();
                let hosts = cfg.hosts.clone();
                let deleted = cfg.sync_deleted.clone();
                let hostname = hostname.clone();
                let from_addr = from_addr.clone();
                thread::spawn(move || {
                    let ok = push_to_peer(&peer, &hosts, &deleted, &from_addr, &hostname).is_ok();
                    #[cfg(debug_assertions)]
                    if !ok {
                        eprintln!("sync push to {} failed", peer.addr);
                    }
                    let _ = tx.try_send(SyncEvent {
                        hosts: Vec::new(),
                        deleted: Vec::new(),
                        from_addr: peer.addr.clone(),
                        from_hostname: String::new(),
                        new_peer: None,
                        push_result: Some((peer.addr, ok)),
                    });
                });
            }
            for _ in 0..60 {
                if shutdown.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_errors_map_to_actionable_hints() {
        assert!(join_error_hint("request failed: Connection refused (os error 111)").contains("firewall"));
        assert!(join_error_hint("request failed: timed out").contains("wrong IP"));
        assert!(join_error_hint("request failed: HTTP 404").contains("v0.2.0"));
        assert!(join_error_hint("bad token (generate a fresh join code?)").contains("fresh code"));
        assert!(!join_error_hint("something weird").is_empty());
    }

    #[test]
    fn unreachable_means_no_answer_not_rejection() {
        // The exact failure from the field: connected-ish, then EOF.
        assert!(is_unreachable("request failed: http://172.16.10.2:8080/sync/join: Network Error: Unexpected EOF"));
        assert!(is_unreachable("request failed: Connection refused (os error 61)"));
        assert!(is_unreachable("request failed: timed out"));
        assert!(!is_unreachable("bad token in join code"));
        assert!(!is_unreachable("request failed: HTTP 404: foo"));
        assert!(!is_unreachable("join rejected"));
    }

    #[test]
    fn subnet_derivation() {
        assert_eq!(subnet24_of("192.168.1.25"), Some("192.168.1".to_string()));
        assert_eq!(subnet24_of("10.0.0.1"), Some("10.0.0".to_string()));
        assert_eq!(subnet24_of("::1"), None);
        assert_eq!(subnet24_of("fe80::1"), None);
        assert_eq!(subnet24_of("garbage"), None);
        assert_eq!(subnet24_of("1.2.3.256"), None);
    }

    /// Minimal stub peer: answers /health and /sync/join like the real server.
    /// Takes an already-bound listener so no other test can steal the port.
    fn stub_peer(listener: std::net::TcpListener) {
        use std::io::{Read, Write};
        for mut stream in listener.incoming().take(64) {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
            let mut buf = vec![0u8; 0];
            let mut tmp = [0u8; 1024];
            loop {
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let req = String::from_utf8_lossy(&buf);
            let first = req.lines().next().unwrap_or("");
            let resp = if first.starts_with("GET /health") {
                "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nok\n".to_string()
            } else if first.starts_with("POST /sync/join") {
                let body = r#"{"ok":true,"hosts":[],"deleted":[],"hostname":"stub"}"#;
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            } else {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
            };
            let _ = stream.write_all(resp.as_bytes());
        }
    }

    #[test]
    fn join_falls_back_from_dead_code_ip_to_scanned_peer() {
        // A dead address first (nothing on :1 → refused), then a live stub.
        // Proves the fallback path reaches a peer the code didn't name.
        // The stub listener is bound before spawning, so no parallel test
        // can steal the port; readiness is retried, not slept on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stub = std::thread::spawn(move || stub_peer(listener));
        let stub_addr = format!("127.0.0.1:{}", port);
        let mut ready = false;
        for _ in 0..40 {
            if health_ok(&stub_addr) {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(ready, "stub peer never answered /health");
        assert!(!health_ok("127.0.0.1:1"));
        // Simulate the fallback's inner loop directly: dead then live.
        let token = generate_token();
        let r1 = try_candidate("127.0.0.1:1", &token, "127.0.0.1:9999", &token, "tester");
        assert!(r1.is_none());
        let r2 = try_candidate(&format!("127.0.0.1:{}", port), &token, "127.0.0.1:9999", &token, "tester");
        let (hosts, _, hostname, via) = r2.expect("stub peer should accept the join");
        assert!(hosts.is_empty());
        assert_eq!(hostname, "stub");
        assert!(via.ends_with(&port.to_string()));
        drop(stub);
    }

    #[test]
    fn scan_subnet_finds_a_live_peer() {
        // End-to-end scan over loopback: 253 instant-refused probes plus one
        // live stub must resolve to the stub. Should take ~2s, not tens.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stub = std::thread::spawn(move || stub_peer(listener));
        let token = generate_token();
        let start = std::time::Instant::now();
        let found = scan_subnet(
            "127.0.0",
            &port.to_string(),
            None,
            &token,
            "127.0.0.1:9999",
            &token,
            "tester",
        );
        let elapsed = start.elapsed();
        let (_, _, hostname, via) = found.expect("scan should find the stub peer");
        assert_eq!(hostname, "stub");
        assert!(via.ends_with(&port.to_string()));
        assert!(
            elapsed < Duration::from_secs(20),
            "scan took too long: {:?}",
            elapsed
        );
        drop(stub);
    }

    #[test]
    fn reachable_gate_fails_fast_with_classifiable_errors() {
        // Closed localhost port: refused, immediately (no 30s blackhole stall).
        let t = std::time::Instant::now();
        let err = check_reachable("127.0.0.1:1").unwrap_err();
        assert!(t.elapsed() < Duration::from_secs(5), "took {:?}", t.elapsed());
        assert!(is_unreachable(&err), "classifiable: {}", err);
        // Unresolvable name: resolve error, also classifiable.
        let err = check_reachable("no-such-host.invalid:8080").unwrap_err();
        assert!(is_unreachable(&err), "classifiable: {}", err);
    }

    #[test]
    fn token_shape_and_grouping() {
        // 1000 draws: every byte value must map inside the alphabet.
        for _ in 0..1000 {
            let t = generate_token();
            assert_eq!(t.len(), TOKEN_LEN);
            assert!(valid_token(&t));
        }
        let t = generate_token();
        let g = group_token(&t);
        assert_eq!(g.len(), TOKEN_LEN + 2);
        assert_eq!(ungroup_token(&g), t);
    }

    #[test]
    fn join_code_roundtrip() {
        let code = make_join_code("192.168.1.42", 8080, "abcdefghjkmn");
        assert!(code.starts_with("PUIN-192.168.1.42-8080-"));
        let (addr, token) = parse_join_code(&code).unwrap();
        assert_eq!(addr, "192.168.1.42:8080");
        assert_eq!(token, "abcdefghjkmn");
        // Tolerant: lowercase, spaces.
        let (addr2, _) = parse_join_code(&format!("  {}  ", code.to_lowercase())).unwrap();
        assert_eq!(addr2, addr);
    }

    #[test]
    fn join_code_rejects_garbage() {
        assert!(parse_join_code("hello").is_err());
        assert!(parse_join_code("PUIN-1.2.3.4-8080-abc").is_err());
        assert!(parse_join_code("PUIN-1.2.3.4-0-abcdefgh-jklm-nopq").is_err());
    }

    #[test]
    fn join_code_at_override_replaces_unreachable_ip() {
        let base = "PUIN-172.16.10.2-8080-abcd-efgh-jkmn";
        // Bare code keeps its own address.
        assert_eq!(
            parse_join_code(base).unwrap(),
            ("172.16.10.2:8080".to_string(), "abcdefghjkmn".to_string())
        );
        // @host overrides the IP, keeps port + token.
        let (addr, token) = parse_join_code(&format!("{} @ 192.168.1.42", base)).unwrap();
        assert_eq!(addr, "192.168.1.42:8080");
        assert_eq!(token.len(), 12);
        // @host:port overrides both.
        let (addr, _) = parse_join_code(&format!("{}@mylan:9999", base)).unwrap();
        assert_eq!(addr, "mylan:9999");
        // Dangling @ and bad ports are errors, not silent misdials.
        assert!(parse_join_code(&format!("{} @", base)).is_err());
        assert!(parse_join_code(&format!("{} @ 192.168.1.42:0", base)).is_err());
        assert!(parse_join_code(&format!("{} @ :8080", base)).is_err());
    }

    fn host(name: &str, at: i64, group: &str) -> HostConfig {
        let mut h = HostConfig::new(name, 60, "g", None, None);
        h.updated_at = at;
        h.group = group.to_string();
        h
    }

    #[test]
    fn merge_adds_and_prefers_newer() {
        let mut local = vec![host("a", 100, "g"), host("b", 100, "old")];
        let mut deleted = Vec::new();
        let incoming = vec![host("a", 50, "g"), host("b", 200, "new"), host("c", 10, "g")];
        let stats = merge_state(&mut local, &mut deleted, &incoming, &[], 1000);
        assert_eq!((stats.added, stats.updated, stats.removed), (1, 1, 0));
        assert_eq!(local.iter().find(|h| h.name == "a").unwrap().updated_at, 100);
        assert_eq!(local.iter().find(|h| h.name == "b").unwrap().group, "new");
        assert!(local.iter().any(|h| h.name == "c"));
    }

    #[test]
    fn merge_propagates_deletes_both_ways() {
        let now = 1000;
        // B deleted "a" at 900; A still has it (edited at 100).
        let mut local = vec![host("a", 100, "g"), host("b", 100, "g")];
        let mut deleted = Vec::new();
        let incoming_del = vec![SyncDeletion { name: "a".to_string(), at: 900 }];
        let stats = merge_state(&mut local, &mut deleted, &[], &incoming_del, now);
        assert_eq!(stats.removed, 1);
        assert!(!local.iter().any(|h| h.name == "a"));
        // Stale copy of "a" (edited at 50, before the delete) must not resurrect.
        let stats = merge_state(&mut local, &mut deleted, &[host("a", 50, "g")], &[], now);
        assert_eq!((stats.added, stats.updated), (0, 0));
        assert!(!local.iter().any(|h| h.name == "a"));
        // But an edit newer than the delete resurrects and clears the tombstone.
        let stats = merge_state(&mut local, &mut deleted, &[host("a", 950, "g")], &[], now);
        assert_eq!(stats.added, 1);
        assert!(!deleted.iter().any(|d| d.name == "a"));
    }

    #[test]
    fn tombstones_prune_after_ttl() {
        let now = TOMBSTONE_TTL_SECS + 1000;
        let mut local = vec![host("a", 10, "g")];
        let mut deleted = vec![SyncDeletion { name: "old".to_string(), at: 10 }];
        merge_state(&mut local, &mut deleted, &[], &[], now);
        assert!(!deleted.iter().any(|d| d.name == "old"));
    }
}
