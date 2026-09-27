//! Config, host entries, data-file paths, and interval parsing.
//!
//! Back-compat notes:
//! - `ping-uin.json` replaced `ip-top.json`; an existing `ip-top.json` is
//!   migrated (renamed) automatically on startup.
//! - Host intervals used to be minutes-only (`interval_m`). `interval_secs`
//!   wins when > 0; otherwise `interval_m * 60` applies. Both the JSON
//!   (`interval_secs`, `interval_s`, `interval_m`, or `"30s"`-style strings)
//!   and the CSV (plain minutes or suffixed values) are accepted.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub const DEFAULT_INTERVAL_SECS: u64 = 120;
pub const MIN_INTERVAL_SECS: u64 = 5;
pub const MAX_INTERVAL_SECS: u64 = 24 * 3600;
pub const DEFAULT_TIMEOUT_MS: u64 = 1000;
pub const DEFAULT_GRAPH_WIDTH: usize = 20;
pub const MAX_HISTORY: usize = 10000;

/// Parse a human interval into seconds. Accepts `30s`, `5m`, `2h`, or a bare
/// number (legacy minutes, so `"2"` == 120s). Empty/whitespace → None.
pub fn parse_interval(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    let (num, mult) = if let Some(n) = s.strip_suffix('s') {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else {
        (s.as_str(), 60)
    };
    num.trim().parse::<u64>().ok().map(|n| n.saturating_mul(mult))
}

/// Compact display for the Int column: `30s`, `5m`, `2h`.
pub fn format_interval(secs: u64) -> String {
    if secs >= 3600 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{}s", secs)
    }
}

/// Where the config/csv/log live. Resolved once at startup.
///
/// Portable mode: if the directory containing the running executable has a
/// `ping-uin.portable` marker file, or already contains one of the data files,
/// that directory is used instead of the system config dir.
///
/// Otherwise falls back to the user config dir
/// (`~/.config/ping-uin` on Linux/macOS, `%APPDATA%\ping-uin` on Windows).
pub struct Paths {
    pub config: PathBuf,
    pub csv: PathBuf,
    pub log: PathBuf,
}

static PATHS: OnceLock<Paths> = OnceLock::new();

pub fn portable_dir() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // Don't treat Cargo build directories as portable installs.
    if exe_dir.components().any(|c| c.as_os_str() == "target") {
        return None;
    }
    let marker = exe_dir.join("ping-uin.portable");
    let has_marker = marker.exists();
    let has_data = ["ping-uin.json", "ip-top.json", "hosts.csv", "uptime-log.csv"]
        .iter()
        .any(|name| exe_dir.join(name).exists());
    if has_marker || has_data {
        Some(exe_dir)
    } else {
        None
    }
}

pub fn is_homebrew_install(exe: &std::path::Path) -> bool {
    exe.to_string_lossy().contains("/Cellar/ping-uin/")
}

pub fn homebrew_bin_path() -> Option<PathBuf> {
    for path in ["/opt/homebrew/bin/ping-uin", "/usr/local/bin/ping-uin"] {
        if std::path::Path::new(path).exists() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

fn resolve_paths() -> Paths {
    let dir = portable_dir()
        .unwrap_or_else(|| dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("ping-uin"));
    let _ = fs::create_dir_all(&dir);
    let config = dir.join("ping-uin.json");
    // One-time migration from the old config name.
    let legacy = dir.join("ip-top.json");
    if !config.exists() && legacy.exists() {
        let _ = fs::rename(&legacy, &config);
    }
    Paths {
        config,
        csv: dir.join("hosts.csv"),
        log: dir.join("uptime-log.csv"),
    }
}

pub fn paths() -> &'static Paths {
    PATHS.get_or_init(resolve_paths)
}

/// Single-instance lock for one data directory.
///
/// Long runners (the TUI, `--serve`) hold this while alive so a second copy
/// against the SAME data dir refuses to start instead of silently fighting
/// over the config (clobbered saves), the probes (duplicates), the mails
/// (duplicates), and the listener port. Different data dirs (e.g. portable
/// vs installed) have different lock files and stay independent.
///
/// The lock file holds our PID. A pre-existing lock is only honored when its
/// PID is both alive AND looks like ping-uin (PID reuse must never block a
/// legitimate start); otherwise it is stale (crash, kill -9) and taken over.
/// Released automatically on clean exit via Drop.
pub struct InstanceLock {
    path: PathBuf,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_path() -> PathBuf {
    paths().config.parent().map(|d| d.join("ping-uin.lock")).unwrap_or_else(|| PathBuf::from("ping-uin.lock"))
}

/// Process command name for `pid`, if observable. Used to tell a live
/// ping-uin apart from an unrelated PID reuse. Linux reads procfs directly
/// (no subprocess); macOS shells to `ps`; Windows to `tasklist`.
fn process_comm(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let comm = std::fs::read_to_string(format!("/proc/{}/comm", pid))
            .ok()?
            .trim()
            .to_string();
        if comm.is_empty() {
            None
        } else {
            Some(comm)
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let out = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let comm = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if comm.is_empty() {
            None
        } else {
            Some(comm)
        }
    }
    #[cfg(target_os = "windows")]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH", "/FO", "CSV"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.lines()
            .filter_map(|l| l.split(',').next())
            .map(|s| s.trim_matches('"').to_string())
            .find(|s| !s.is_empty())
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = pid;
        None
    }
}

/// Pure verdict for a pre-existing lock PID: refuse only when the process is
/// observable AND its command looks like ping-uin. Anything else (dead PID,
/// unobservable process, recycled PID now running something else) is stale
/// and safe to take over.
fn lock_verdict(pid_alive: bool, comm_matches: bool) -> bool {
    pid_alive && comm_matches
}

/// How to stop the other copy, per OS (used in the refusal message).
fn stop_hint(pid: u32) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("Task Manager, `taskkill /PID {} /F`, or `schtasks /end /tn ping-uin` for the startup task", pid)
    }
    #[cfg(not(target_os = "windows"))]
    {
        format!("`kill {}` (systemd service: `systemctl --user stop ping-uin`)", pid)
    }
}

/// Try to become the instance owning this data dir.
/// - `Ok(lock)`: hold it for as long as this process runs (Drop releases).
/// - `Err(msg)`: another live ping-uin owns the dir — show `msg` and exit.
pub fn acquire_instance_lock() -> Result<InstanceLock, String> {
    acquire_instance_lock_in(&lock_path())
}

/// Testable core: lock `path` itself (production passes the data-dir lock
/// file). See [`acquire_instance_lock`] for the contract.
pub fn acquire_instance_lock_in(path: &PathBuf) -> Result<InstanceLock, String> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let path = path.clone();
    let me = std::process::id();
    // Atomic: exactly one racer wins; the loser sees the file.
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut f) => {
            let _ = writeln!(f, "{}", me);
            return Ok(InstanceLock { path });
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            // Fail open with a loud warning: without a writable data dir
            // nothing else persists either, so refusing would only strand.
            eprintln!("warning: cannot create instance lock ({}); running unlocked", e);
            return Ok(InstanceLock { path });
        }
    }
    // Someone was here first: honor it only if it's a live ping-uin.
    let holder: Option<u32> = fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|pid| *pid != me);
    match holder {
        Some(pid) => {
            let comm = process_comm(pid).unwrap_or_default().to_lowercase();
            let alive = !comm.is_empty();
            if lock_verdict(alive, comm.contains("ping-uin") || comm.contains("ping_uin")) {
                return Err(format!(
                    "another ping-uin (PID {}) is already using {} — one copy per data dir.\nQuit it first ({}) or run --once for a lock-free check.",
                    pid,
                    path.parent().map(|d| d.display().to_string()).unwrap_or_else(|| ".".to_string()),
                    stop_hint(pid)
                ));
            }
            // Stale or foreign lock: take over.
            let _ = fs::remove_file(&path);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", me);
                    Ok(InstanceLock { path })
                }
                Err(e) => Err(format!("instance lock race lost or unwritable ({}); try again", e)),
            }
        }
        // Unparseable/own lock file: take over (same crash-recovery path).
        None => {
            let _ = fs::remove_file(&path);
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("cannot create instance lock ({}); is the data dir writable?", e))
                .and_then(|mut f| {
                    writeln!(f, "{}", me)
                        .map_err(|e| format!("cannot write instance lock ({})", e))?;
                    Ok(InstanceLock { path })
                })
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HostConfig {
    pub name: String,
    /// Legacy minutes. Used only when `interval_secs` is 0.
    #[serde(default)]
    pub interval_m: u64,
    /// Preferred interval in seconds. 0 = fall back to `interval_m`.
    #[serde(default)]
    pub interval_secs: u64,
    #[serde(default = "default_group")]
    pub group: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// TCP check port. None = ICMP ping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Warn threshold in ms: up-but-slow reads WARN. None = disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warn_latency_ms: Option<u32>,
    /// Custom check command run via shell; exit 0 = up, latency = wall time.
    /// Takes precedence over ping/TCP when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_cmd: Option<String>,
    /// Upstream host name: alerts suppressed while the upstream is down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<String>,
    /// Mute maintenance window as unix epoch secs. None/expired = unmuted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted_until: Option<i64>,
    /// Last local edit (epoch secs). Neighbor sync keeps the newer side per
    /// host; 0 = legacy/unknown (loses to any dated edit).
    #[serde(default)]
    pub updated_at: i64,
}

fn default_group() -> String {
    "default".to_string()
}

impl HostConfig {
    pub fn new(
        name: impl Into<String>,
        interval_secs: u64,
        group: impl Into<String>,
        alias: Option<String>,
        port: Option<u16>,
    ) -> Self {
        let alias = alias.filter(|a| !a.trim().is_empty());
        let group = group.into();
        let group = if group.trim().is_empty() {
            default_group()
        } else {
            group
        };
        HostConfig {
            name: name.into(),
            interval_m: 0,
            interval_secs,
            group,
            alias,
            port,
            warn_latency_ms: None,
            check_cmd: None,
            depends_on: None,
            muted_until: None,
            updated_at: now_epoch(),
        }
    }

    /// Stamp a local edit so neighbor sync prefers this side afterwards.
    pub fn touch(&mut self) {
        self.updated_at = now_epoch();
    }

    /// Seconds of mute remaining, 0 when unmuted/expired.
    pub fn mute_remaining_secs(&self, now_epoch: i64) -> i64 {
        self.muted_until
            .map(|until| (until - now_epoch).max(0))
            .unwrap_or(0)
    }
}

pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Compact `41m`-style duration for tickers.
pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        if h == 0 {
            format!("{}d", d)
        } else {
            format!("{}d{}h", d, h)
        }
    }
}

impl HostConfig {
    /// Effective check interval, clamped to sane bounds.
    pub fn effective_interval_secs(&self) -> u64 {
        let raw = if self.interval_secs > 0 {
            self.interval_secs
        } else if self.interval_m > 0 {
            self.interval_m.saturating_mul(60)
        } else {
            DEFAULT_INTERVAL_SECS
        };
        raw.clamp(MIN_INTERVAL_SECS, MAX_INTERVAL_SECS)
    }

    /// Alias if set, else the raw name.
    pub fn display_name(&self) -> String {
        self.alias.clone().unwrap_or_else(|| self.name.clone())
    }

    /// `db.internal:5432`-style display target.
    pub fn target(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{}", self.name, p),
            None => self.name.clone(),
        }
    }
}

fn default_theme_name() -> String {
    "btop".to_string()
}

/// Optional SMTP settings for down/recovery email alerts.
///
/// `to` accepts a single address or a comma-separated list. `use_tls`
/// selects encrypted transport (SMTPS on 465, STARTTLS otherwise);
/// `false` sends plaintext (local relays only).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SmtpConfig {
    /// Master switch. `false` (or missing) disables all email alerts.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Envelope sender, e.g. `ping-uin@example.com`.
    #[serde(default)]
    pub from: String,
    /// Recipient(s), comma-separated, e.g. `ops@example.com, noc@example.com`.
    #[serde(default)]
    pub to: String,
    #[serde(default = "default_true")]
    pub use_tls: bool,
    /// Consecutive failures before a DOWN email fires. Default 3.
    #[serde(default = "default_smtp_threshold")]
    pub down_threshold: u32,
    /// Also email the `still_down_5m` / `still_down_30m` escalations.
    /// Default false (DOWN + recovery only).
    #[serde(default)]
    pub escalations: bool,
}

fn default_smtp_port() -> u16 {
    587
}

fn default_smtp_threshold() -> u32 {
    SMTP_DOWN_THRESHOLD
}

fn default_true() -> bool {
    true
}

impl SmtpConfig {
    /// True when the config is complete enough to attempt delivery.
    pub fn is_configured(&self) -> bool {
        self.enabled && !self.host.trim().is_empty() && !self.from.trim().is_empty() && !self.to.trim().is_empty()
    }

    /// Effective DOWN threshold, clamped to 1..=100 so a stray 0 can't
    /// either spam on first failure or never fire. Missing/legacy (0)
    /// falls back to the default of 3.
    pub fn effective_threshold(&self) -> u32 {
        if self.down_threshold == 0 {
            SMTP_DOWN_THRESHOLD
        } else {
            self.down_threshold.clamp(1, 100)
        }
    }

    /// Parsed recipient list (trimmed, non-empty).
    pub fn recipients(&self) -> Vec<String> {
        self.to
            .split([',', ';'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Default consecutive failed checks before a down-email fires.
pub const SMTP_DOWN_THRESHOLD: u32 = 3;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub hosts: Vec<HostConfig>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_graph_width")]
    pub graph_width: usize,
    #[serde(default = "default_theme_name")]
    pub theme: String,
    #[serde(default)]
    pub group_by: bool,
    #[serde(default)]
    pub sort_mode: SortMode,
    /// Generic webhook POSTed on up/down transitions. None = disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
    /// Terminal bell (`\x07`) on down-transitions.
    #[serde(default)]
    pub notify_bell: bool,
    /// Optional SMTP settings for down/recovery emails. None = disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smtp: Option<SmtpConfig>,
    /// Collapsed group names in grouped view.
    #[serde(default)]
    pub collapsed_groups: Vec<String>,
    /// Compact table density (hides IP + Group columns).
    #[serde(default)]
    pub compact: bool,
    /// Selected host name restored on startup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    /// Neighbor-sync pairing secret. None = sync never enabled (nothing
    /// listens for sync). Set when this device generates a join code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_token: Option<String>,
    /// Paired neighbors: pushes go out every minute, pushes come in anytime.
    #[serde(default)]
    pub sync_peers: Vec<SyncPeer>,
    /// Propagated deletions (tombstones). See `SyncDeletion`.
    #[serde(default)]
    pub sync_deleted: Vec<SyncDeletion>,
    /// Persisted outage-mail state per host. See `EmailOutageState`.
    #[serde(default)]
    pub email_state: std::collections::HashMap<String, EmailOutageState>,
    /// Whether the read-only LAN page was showing when the TUI last ran.
    /// `W` toggles it; a set value auto-serves on the next startup so the
    /// page survives reboots without any flags or clicks.
    #[serde(default)]
    pub serve_page: bool,
}

/// One paired neighbor instance: where to push + which token it expects.
/// Shown in the sync menu with hostname, join date, and last sync time.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncPeer {
    /// `ip:port` of the neighbor's sync listener (shares its web port).
    #[serde(default)]
    pub addr: String,
    /// Token the neighbor issued (presented on every push to it).
    #[serde(default)]
    pub token: String,
    /// Neighbor's hostname at pairing time (`hostname` command).
    #[serde(default)]
    pub hostname: String,
    /// When pairing happened (unix epoch secs).
    #[serde(default)]
    pub joined_at: i64,
    /// Last successful sync either way (unix epoch secs, 0 = never).
    #[serde(default)]
    pub last_sync: i64,
}

/// A propagated host removal: name + when it was deleted (unix epoch).
/// Tombstones stop a deleted host from being resurrected by the next push
/// from a device that hasn't seen the delete yet. Pruned after 30 days.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncDeletion {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub at: i64,
}

/// Persisted per-host outage-mail state so a restart doesn't resend DOWN
/// mail for an outage that was already mailed (or skip a recovery that is
/// still owed). Keyed by host name in `Config::email_state`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct EmailOutageState {
    /// A DOWN mail went out for the current outage; cleared on recovery.
    #[serde(default)]
    pub down_sent: bool,
    /// Escalation ladder level reached (0 none, 1 = 5m, 2 = 30m).
    #[serde(default)]
    pub escalation: u8,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_graph_width() -> usize {
    DEFAULT_GRAPH_WIDTH
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hosts: vec![
                HostConfig::new("8.8.8.8", 60, "external", None, None),
                HostConfig::new("1.1.1.1", 120, "external", Some("Cloudflare".to_string()), None),
                HostConfig::new("192.168.1.1", 120, "router", None, None),
                HostConfig::new("google.com", 120, "external", None, None),
            ],
            timeout_ms: DEFAULT_TIMEOUT_MS,
            graph_width: DEFAULT_GRAPH_WIDTH,
            theme: default_theme_name(),
            group_by: false,
            sort_mode: SortMode::None,
            webhook_url: None,
            notify_bell: false,
            smtp: None,
            collapsed_groups: Vec::new(),
            compact: false,
            selected: None,
            sync_token: None,
            sync_peers: Vec::new(),
            sync_deleted: Vec::new(),
            email_state: std::collections::HashMap::new(),
            serve_page: false,
        }
    }
}

/// View applied to the host list. Combines ordering (flat view and inside
/// each group) with the down-only filter that replaced the old down box.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum SortMode {
    #[default]
    None,
    DownFirst,
    UpFirst,
    Name,
    Group,
    DownOnly,
}

impl SortMode {
    pub const ALL: [SortMode; 6] = [
        SortMode::None,
        SortMode::DownFirst,
        SortMode::UpFirst,
        SortMode::Name,
        SortMode::Group,
        SortMode::DownOnly,
    ];

    pub fn index(&self) -> usize {
        match self {
            SortMode::None => 0,
            SortMode::DownFirst => 1,
            SortMode::UpFirst => 2,
            SortMode::Name => 3,
            SortMode::Group => 4,
            SortMode::DownOnly => 5,
        }
    }

    pub fn from_index(i: usize) -> Self {
        match i {
            1 => SortMode::DownFirst,
            2 => SortMode::UpFirst,
            3 => SortMode::Name,
            4 => SortMode::Group,
            5 => SortMode::DownOnly,
            _ => SortMode::None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            SortMode::None => "off",
            SortMode::DownFirst => "down first",
            SortMode::UpFirst => "up first",
            SortMode::Name => "name",
            SortMode::Group => "group",
            SortMode::DownOnly => "down only",
        }
    }
}

fn parse_sort_mode(s: &str) -> SortMode {
    match s {
        "DownFirst" | "down_first" | "down first" | "down-first" => SortMode::DownFirst,
        "UpFirst" | "up_first" | "up first" | "up-first" => SortMode::UpFirst,
        "Name" | "name" => SortMode::Name,
        "Group" | "group" => SortMode::Group,
        "DownOnly" | "down_only" | "down only" | "down-only" => SortMode::DownOnly,
        _ => SortMode::None,
    }
}

fn parse_host_interval(obj: &serde_json::Map<String, serde_json::Value>) -> u64 {
    // Newest first: explicit seconds, "30s"-style strings, then legacy minutes.
    if let Some(s) = obj.get("interval_secs").and_then(|v| v.as_u64()) {
        if s > 0 {
            return s;
        }
    }
    if let Some(s) = obj.get("interval").and_then(|v| v.as_str()) {
        if let Some(parsed) = parse_interval(s) {
            return parsed;
        }
    }
    if let Some(s) = obj.get("interval_s").and_then(|v| v.as_u64()) {
        if s > 0 {
            return s;
        }
    }
    let mins = obj
        .get("interval_m")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if mins > 0 {
        return mins.saturating_mul(60);
    }
    DEFAULT_INTERVAL_SECS
}

fn parse_port(obj: &serde_json::Map<String, serde_json::Value>) -> Option<u16> {
    obj.get("port")
        .and_then(|v| v.as_u64())
        .filter(|p| *p > 0 && *p <= 65535)
        .map(|p| p as u16)
}

impl Config {
    pub fn load() -> Self {
        let text = match fs::read_to_string(&paths().config) {
            Ok(t) => t,
            Err(_) => return Self::default(),
        };
        if let Ok(cfg) = serde_json::from_str::<Config>(&text) {
            return cfg;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            let mut hosts = Vec::new();
            if let Some(arr) = value.get("hosts").and_then(|v| v.as_array()) {
                for v in arr {
                    if let Some(name) = v.as_str() {
                        hosts.push(HostConfig::new(name, DEFAULT_INTERVAL_SECS, "default", None, None));
                    } else if let Some(obj) = v.as_object() {
                        let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let interval_secs = parse_host_interval(obj);
                        let group = obj
                            .get("group")
                            .and_then(|v| v.as_str())
                            .unwrap_or("default")
                            .to_string();
                        let alias = obj.get("alias").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let mut h = HostConfig::new(name, interval_secs, group, alias, parse_port(obj));
                        h.warn_latency_ms = obj
                            .get("warn_latency_ms")
                            .and_then(|v| v.as_u64())
                            .filter(|w| *w > 0)
                            .map(|w| w as u32);
                        h.check_cmd = obj
                            .get("check_cmd")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.trim().is_empty())
                            .map(|s| s.to_string());
                        h.depends_on = obj
                            .get("depends_on")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.trim().is_empty())
                            .map(|s| s.to_string());
                        h.muted_until = obj.get("muted_until").and_then(|v| v.as_i64());
                        h.updated_at = obj.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);
                        hosts.push(h);
                    }
                }
            }
            return Config {
                hosts,
                timeout_ms: value.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_TIMEOUT_MS),
                graph_width: value
                    .get("graph_width")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(DEFAULT_GRAPH_WIDTH as u64) as usize,
                theme: value.get("theme").and_then(|v| v.as_str()).unwrap_or("btop").to_string(),
                group_by: value.get("group_by").and_then(|v| v.as_bool()).unwrap_or(false),
                sort_mode: value
                    .get("sort_mode")
                    .and_then(|v| v.as_str())
                    .map(parse_sort_mode)
                    .unwrap_or(SortMode::None),
                webhook_url: value
                    .get("webhook_url")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.to_string()),
                notify_bell: value.get("notify_bell").and_then(|v| v.as_bool()).unwrap_or(false),
                smtp: value.get("smtp").and_then(|v| serde_json::from_value::<SmtpConfig>(v.clone()).ok()),
                collapsed_groups: value
                    .get("collapsed_groups")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
                compact: value.get("compact").and_then(|v| v.as_bool()).unwrap_or(false),
                selected: value
                    .get("selected")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                sync_token: value
                    .get("sync_token")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.to_string()),
                sync_peers: value
                    .get("sync_peers")
                    .and_then(|v| serde_json::from_value::<Vec<SyncPeer>>(v.clone()).ok())
                    .unwrap_or_default(),
                sync_deleted: value
                    .get("sync_deleted")
                    .and_then(|v| serde_json::from_value::<Vec<SyncDeletion>>(v.clone()).ok())
                    .unwrap_or_default(),
                email_state: value
                    .get("email_state")
                    .and_then(|v| serde_json::from_value::<std::collections::HashMap<String, EmailOutageState>>(v.clone()).ok())
                    .unwrap_or_default(),
                serve_page: value.get("serve_page").and_then(|v| v.as_bool()).unwrap_or(false),
            };
        }
        // Corrupt config: back it up instead of silently discarding user data.
        let backup = paths().config.with_extension("json.corrupt");
        let _ = fs::write(&backup, &text);
        Self::default()
    }

    pub fn save(&self) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::other(e.to_string()))?;
        // Atomic-ish write: temp file + rename so a crash can't truncate config.
        let tmp = paths().config.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &paths().config)?;
        Ok(())
    }
}

/// Host identity for matching: trimmed + case-insensitive. DNS names are
/// case-insensitive and CSV imports accumulate whitespace/case variants, so
/// every add/edit/import comparison goes through here — exact-match checks
/// false-block edits whenever a near-duplicate exists.
pub fn names_equal(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Merge one CSV import row keyed by immutable IP.
///
/// Matching is trimmed + case-insensitive so `DB.internal` updates `db.internal`
/// instead of duplicating it. On a match the stored IP spelling is kept and
/// every other field comes from the row (the CSV is authoritative, including
/// clearing an alias by leaving its cell empty). One piece of local-only
/// state survives: the maintenance mute window (the CSV has no mute column,
/// so an import must never silently unmute) — everything else is stamped
/// fresh via `touch()` so neighbor sync prefers this side afterwards.
/// Returns `(index, is_new)`.
pub fn upsert_imported_host(hosts: &mut Vec<HostConfig>, mut entry: HostConfig) -> (usize, bool) {
    entry.touch();
    entry.name = entry.name.trim().to_string();
    match hosts.iter().position(|h| names_equal(&h.name, &entry.name)) {
        Some(i) => {
            let muted_until = hosts[i].muted_until;
            entry.name = hosts[i].name.clone();
            entry.muted_until = muted_until;
            hosts[i] = entry;
            (i, false)
        }
        None => {
            hosts.push(entry);
            (hosts.len() - 1, true)
        }
    }
}

/// Read hosts.csv rows into HostConfig entries. Accepts the full
/// `name,interval,group,alias,port,warn_ms,check_cmd,depends_on` layout as
/// well as shorter legacy files (missing trailing columns stay unset).
pub fn read_entries_csv(path: &std::path::Path) -> io::Result<Vec<HostConfig>> {
    let mut rdr = csv::Reader::from_path(path)?;
    let mut out = Vec::new();
    for record in rdr.records() {
        let r = record?;
        let name = r.get(0).unwrap_or("").trim().to_string();
        if name.is_empty() {
            continue;
        }
        let interval = r
            .get(1)
            .and_then(parse_interval)
            .unwrap_or(DEFAULT_INTERVAL_SECS);
        let group = r.get(2).unwrap_or("").trim();
        let group = if group.is_empty() {
            "default".to_string()
        } else {
            group.to_string()
        };
        let alias = r.get(3).map(|s| s.trim().to_string());
        let port = r
            .get(4)
            .and_then(|s| s.trim().parse::<u16>().ok())
            .filter(|p| *p > 0);
        let mut h = HostConfig::new(name, interval, group, alias, port);
        h.warn_latency_ms = r
            .get(5)
            .and_then(|s| s.trim().parse::<u32>().ok())
            .filter(|w| *w > 0);
        h.check_cmd = r
            .get(6)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        h.depends_on = r
            .get(7)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        out.push(h);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_interval_units() {
        assert_eq!(parse_interval("30s"), Some(30));
        assert_eq!(parse_interval("5m"), Some(300));
        assert_eq!(parse_interval("2h"), Some(7200));
        assert_eq!(parse_interval("2"), Some(120)); // legacy bare minutes
        assert_eq!(parse_interval(" 10S "), Some(10));
        assert_eq!(parse_interval(""), None);
        assert_eq!(parse_interval("abc"), None);
    }

    #[test]
    fn effective_interval_prefers_secs_and_clamps() {
        let mut h = HostConfig::new("x", 30, "g", None, None);
        assert_eq!(h.effective_interval_secs(), 30);
        h.interval_secs = 0;
        h.interval_m = 2;
        assert_eq!(h.effective_interval_secs(), 120);
        h.interval_secs = 1; // below minimum
        assert_eq!(h.effective_interval_secs(), MIN_INTERVAL_SECS);
        h.interval_secs = 0;
        h.interval_m = 0;
        assert_eq!(h.effective_interval_secs(), DEFAULT_INTERVAL_SECS);
    }

    #[test]
    fn format_interval_roundtrip() {
        assert_eq!(format_interval(30), "30s");
        assert_eq!(format_interval(300), "5m");
        assert_eq!(format_interval(7200), "2h");
        assert_eq!(parse_interval(&format_interval(90)), Some(90));
    }

    #[test]
    fn legacy_json_minutes_migrate_to_secs() {
        let mut obj = serde_json::Map::new();
        obj.insert("interval_m".to_string(), serde_json::Value::from(3u64));
        assert_eq!(parse_host_interval(&obj), 180);
        obj.insert("interval_s".to_string(), serde_json::Value::from(45u64));
        assert_eq!(parse_host_interval(&obj), 45);
        obj.insert(
            "interval".to_string(),
            serde_json::Value::from("10m".to_string()),
        );
        // explicit string wins over nothing... interval_secs absent, string present
        let mut obj2 = serde_json::Map::new();
        obj2.insert(
            "interval".to_string(),
            serde_json::Value::from("10m".to_string()),
        );
        assert_eq!(parse_host_interval(&obj2), 600);
    }

    #[test]
    fn sort_mode_labels_roundtrip() {
        for m in SortMode::ALL {
            assert_eq!(SortMode::from_index(m.index()), m);
        }
        assert_eq!(parse_sort_mode("DownOnly"), SortMode::DownOnly);
        assert_eq!(parse_sort_mode("down only"), SortMode::DownOnly);
        assert_eq!(parse_sort_mode("bogus"), SortMode::None);
    }

    #[test]
    fn format_duration_units() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(600), "10m");
        assert_eq!(format_duration(5400), "1h30m");
        assert_eq!(format_duration(86400), "1d");
        assert_eq!(format_duration(90000), "1d1h");
    }

    #[test]
    fn mute_remaining_counts_down() {
        let mut h = HostConfig::new("x", 60, "g", None, None);
        assert_eq!(h.mute_remaining_secs(1000), 0);
        h.muted_until = Some(1060);
        assert_eq!(h.mute_remaining_secs(1000), 60);
        assert_eq!(h.mute_remaining_secs(2000), 0); // expired clamps to 0
    }

    #[test]
    fn tcp_target_display() {
        let h = HostConfig::new("db", 60, "g", None, Some(5432));
        assert_eq!(h.target(), "db:5432");
        let p = HostConfig::new("db", 60, "g", None, None);
        assert_eq!(p.target(), "db");
    }

    #[test]
    fn lock_verdict_only_refuses_live_ping_uin() {
        // Live ping-uin holder: refuse.
        assert!(lock_verdict(true, true));
        // Dead PID, unobservable process, or recycled PID running something
        // else: all stale, all take over.
        assert!(!lock_verdict(false, false));
        assert!(!lock_verdict(false, true));
        assert!(!lock_verdict(true, false));
    }

    #[test]
    fn instance_lock_roundtrip_and_takeover() {
        let dir = std::env::temp_dir().join(format!("puin-lock-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ping-uin.lock");
        // Fresh acquire works and holds.
        let lock = acquire_instance_lock_in(&path).expect("fresh acquire");
        assert!(path.exists());
        // Our own test binary is NOT named ping-uin, so a second acquire
        // correctly treats us as foreign (PID-reuse safety) and takes over.
        // To test the live-holder path deterministically, simulate it via
        // lock_verdict above; here assert takeover never errors.
        drop(lock);
        assert!(!path.exists(), "Drop releases the lock file");
        // Stale lock (dead PID) is taken over.
        std::fs::write(&path, "4294967295").unwrap();
        let lock2 = acquire_instance_lock_in(&path).expect("stale takeover");
        assert!(path.exists());
        drop(lock2);
        // Garbage lock file is taken over, not fatal.
        std::fs::write(&path, "not-a-pid").unwrap();
        assert!(acquire_instance_lock_in(&path).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_matching_ip_overwrites_fields_keeps_ip_and_mute() {
        let mut hosts = vec![{
            let mut h = HostConfig::new("db.internal", 60, "old-group", Some("Old Alias".to_string()), None);
            h.warn_latency_ms = Some(100);
            h.muted_until = Some(9_999_999_999);
            h.updated_at = 100;
            h
        }];
        // Same IP, different case: updates in place, no duplicate.
        let mut row = HostConfig::new("DB.INTERNAL", 120, "new-group", None, Some(5432));
        row.updated_at = 50; // stale stamp is refreshed by the import itself
        let (i, is_new) = upsert_imported_host(&mut hosts, row);
        assert_eq!((i, is_new), (0, false));
        assert_eq!(hosts.len(), 1);
        let h = &hosts[0];
        // IP spelling is immutable: the stored form wins.
        assert_eq!(h.name, "db.internal");
        // Every other CSV-managed field comes from the row (alias cleared).
        assert_eq!(h.group, "new-group");
        assert_eq!(h.alias, None);
        assert_eq!(h.port, Some(5432));
        assert_eq!(h.warn_latency_ms, None);
        // Local-only mute window survives; edit is stamped fresh for sync.
        assert_eq!(h.muted_until, Some(9_999_999_999));
        assert!(h.updated_at > 100);
    }

    #[test]
    fn names_match_trimmed_case_insensitive() {
        assert!(names_equal("db.internal", "db.internal"));
        assert!(names_equal("DB.internal", "db.internal"));
        assert!(names_equal("  db.internal  ", "db.internal"));
        assert!(!names_equal("db.internal", "db2.internal"));
        assert!(!names_equal("", "db.internal"));
    }

    #[test]
    fn import_unknown_ip_appends() {
        let mut hosts = vec![HostConfig::new("a", 60, "g", None, None)];
        let (i, is_new) = upsert_imported_host(&mut hosts, HostConfig::new("b", 60, "g", None, None));
        assert_eq!((i, is_new), (1, true));
        assert_eq!(hosts.len(), 2);
    }

    #[test]
    fn smtp_config_gating_and_recipients() {
        let mut s = SmtpConfig {
            enabled: true,
            host: "smtp.example.com".to_string(),
            port: 587,
            username: None,
            password: None,
            from: "ping-uin@example.com".to_string(),
            to: "ops@example.com, noc@example.com ; ".to_string(),
            use_tls: true,
            down_threshold: 3,
            escalations: false,
        };
        assert!(s.is_configured());
        assert_eq!(s.recipients(), vec!["ops@example.com", "noc@example.com"]);
        s.enabled = false;
        assert!(!s.is_configured());
        s.enabled = true;
        s.host.clear();
        assert!(!s.is_configured());
    }

    #[test]
    fn smtp_threshold_clamps_and_defaults() {
        let mut s = SmtpConfig::default();
        assert_eq!(s.effective_threshold(), SMTP_DOWN_THRESHOLD); // 0 -> default
        s.down_threshold = 1;
        assert_eq!(s.effective_threshold(), 1);
        s.down_threshold = 10;
        assert_eq!(s.effective_threshold(), 10);
        s.down_threshold = 500;
        assert_eq!(s.effective_threshold(), 100);
        // Legacy JSON without the new keys falls back to defaults.
        let legacy: SmtpConfig = serde_json::from_str(
            r#"{"enabled":true,"host":"m","from":"f@x","to":"t@x"}"#,
        )
        .unwrap();
        assert_eq!(legacy.effective_threshold(), SMTP_DOWN_THRESHOLD);
        assert!(!legacy.escalations);
    }

    #[test]
    fn smtp_roundtrips_through_config_json() {
        let mut cfg = Config::default();
        cfg.smtp = Some(SmtpConfig {
            enabled: true,
            host: "mail.example.com".to_string(),
            port: 465,
            username: Some("user".to_string()),
            password: Some("secret".to_string()),
            from: "from@example.com".to_string(),
            to: "to@example.com".to_string(),
            use_tls: true,
            down_threshold: 5,
            escalations: true,
        });
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        let smtp = back.smtp.expect("smtp survives round-trip");
        assert_eq!(smtp.host, "mail.example.com");
        assert_eq!(smtp.port, 465);
        assert_eq!(smtp.effective_threshold(), 5);
        assert!(smtp.escalations);
        // Old configs without smtp still load with None.
        let legacy: Config =
            serde_json::from_str(r#"{"hosts":[],"timeout_ms":1000,"graph_width":20}"#).unwrap();
        assert!(legacy.smtp.is_none());
    }
}
