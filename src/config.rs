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
        }
    }

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
    /// Collapsed group names in grouped view.
    #[serde(default)]
    pub collapsed_groups: Vec<String>,
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
            collapsed_groups: Vec::new(),
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
                        hosts.push(HostConfig::new(name, interval_secs, group, alias, parse_port(obj)));
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
                collapsed_groups: value
                    .get("collapsed_groups")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
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

/// Read hosts.csv rows into HostConfig entries. Accepts both the new
/// `name,interval,group,alias,port` layout and legacy 4-column files.
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
        out.push(HostConfig::new(name, interval, group, alias, port));
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
    fn tcp_target_display() {
        let h = HostConfig::new("db", 60, "g", None, Some(5432));
        assert_eq!(h.target(), "db:5432");
        let p = HostConfig::new("db", 60, "g", None, None);
        assert_eq!(p.target(), "db");
    }
}
