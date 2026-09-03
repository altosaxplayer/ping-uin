use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use chrono::{Local, TimeZone};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use ratatui::{Frame, Terminal};
use regex::Regex;

mod config;
mod net;

use config::{
    format_interval, homebrew_bin_path, is_homebrew_install, parse_interval, paths,
    portable_dir, read_entries_csv, Config, HostConfig, SortMode, DEFAULT_INTERVAL_SECS,
    MAX_HISTORY,
};
use net::{check_host, host_jitter, post_webhook, schedules_from_config, HostSchedule};

#[derive(Clone)]
struct Theme {
    name: &'static str,
    main_bg: Color,
    main_fg: Color,
    title: Color,
    hi_fg: Color,
    selected_bg: Color,
    selected_fg: Color,
    inactive_fg: Color,
    graph_text: Color,
    box_color: Color,
    status_good: Color,
    status_danger: Color,
    graph_start: Color,
    divider: Color,
    popup_bg: Color,
}

fn rgb(hex: &str) -> Color {
    let h = hex.trim_start_matches('#');
    let r = u8::from_str_radix(&h[0..2], 16).unwrap();
    let g = u8::from_str_radix(&h[2..4], 16).unwrap();
    let b = u8::from_str_radix(&h[4..6], 16).unwrap();
    Color::Rgb(r, g, b)
}

fn build_themes() -> Vec<Theme> {
    vec![
        // btop-inspired: muted gray box borders, soft green accent, soft status colors
        Theme {
            name: "btop",
            main_bg: rgb("#161a22"),
            main_fg: rgb("#c8ccd4"),
            title: rgb("#eef0f6"),
            hi_fg: rgb("#8fb573"),
            selected_bg: rgb("#3f4a3e"),
            selected_fg: rgb("#e8f0e4"),
            inactive_fg: rgb("#5a6375"),
            graph_text: rgb("#a8adb9"),
            box_color: rgb("#5a6375"),
            status_good: rgb("#a3be8c"),
            status_danger: rgb("#dc6d6d"),
            graph_start: rgb("#8fb573"),
            divider: rgb("#2c313c"),
            popup_bg: rgb("#1c1f28"),
        },
        Theme {
            name: "dracula",
            main_bg: rgb("#282a36"),
            main_fg: rgb("#f8f8f2"),
            title: rgb("#f8f8f2"),
            hi_fg: rgb("#bd93f9"),
            selected_bg: rgb("#44475a"),
            selected_fg: rgb("#f8f8f2"),
            inactive_fg: rgb("#6272a4"),
            graph_text: rgb("#c0c2d0"),
            box_color: rgb("#44475a"),
            status_good: rgb("#50fa7b"),
            status_danger: rgb("#ff5555"),
            graph_start: rgb("#50fa7b"),
            divider: rgb("#21222c"),
            popup_bg: rgb("#21222c"),
        },
        Theme {
            name: "nord",
            main_bg: rgb("#2e3440"),
            main_fg: rgb("#d8dee9"),
            title: rgb("#eceff4"),
            hi_fg: rgb("#88c0d0"),
            selected_bg: rgb("#4c566a"),
            selected_fg: rgb("#eceff4"),
            inactive_fg: rgb("#4c566a"),
            graph_text: rgb("#b5bcc9"),
            box_color: rgb("#4c566a"),
            status_good: rgb("#a3be8c"),
            status_danger: rgb("#bf616a"),
            graph_start: rgb("#a3be8c"),
            divider: rgb("#3b4252"),
            popup_bg: rgb("#242933"),
        },
        Theme {
            name: "gruvbox-dark",
            main_bg: rgb("#282828"),
            main_fg: rgb("#ebdbb2"),
            title: rgb("#ebdbb2"),
            hi_fg: rgb("#b8bb26"),
            selected_bg: rgb("#504945"),
            selected_fg: rgb("#ebdbb2"),
            inactive_fg: rgb("#665c54"),
            graph_text: rgb("#bdae93"),
            box_color: rgb("#504945"),
            status_good: rgb("#b8bb26"),
            status_danger: rgb("#fb4934"),
            graph_start: rgb("#98971a"),
            divider: rgb("#3c3836"),
            popup_bg: rgb("#1d2021"),
        },
        Theme {
            name: "ayu-light",
            main_bg: rgb("#f8f9fa"),
            main_fg: rgb("#5c6166"),
            title: rgb("#3199e1"),
            hi_fg: rgb("#ea6c6d"),
            selected_bg: rgb("#f7f7f7"),
            selected_fg: rgb("#5c6166"),
            inactive_fg: rgb("#c7c7c7"),
            graph_text: rgb("#5c6166"),
            box_color: rgb("#9e75c7"),
            status_good: rgb("#6cbf43"),
            status_danger: rgb("#ea6c6d"),
            graph_start: rgb("#6cbf43"),
            divider: rgb("#c7c7c7"),
            popup_bg: rgb("#f8f9fa"),
        },
        Theme {
            name: "archwave",
            main_bg: rgb("#1a0d2e"),
            main_fg: rgb("#d4a5ff"),
            title: rgb("#5ffbf1"),
            hi_fg: rgb("#f9f871"),
            selected_bg: rgb("#2d1b4e"),
            selected_fg: rgb("#5ffbf1"),
            inactive_fg: rgb("#543a6e"),
            graph_text: rgb("#fef6ff"),
            box_color: rgb("#ff6ec7"),
            status_good: rgb("#5ffbf1"),
            status_danger: rgb("#ff6ec7"),
            graph_start: rgb("#8b9aff"),
            divider: rgb("#8b9aff"),
            popup_bg: rgb("#1a0d2e"),
        },
    ]
}

#[derive(Clone)]
struct HostState {
    name: String,
    alias: Option<String>,
    group: String,
    interval_secs: u64,
    port: Option<u16>,
    warn_latency_ms: Option<u32>,
    check_cmd: Option<String>,
    depends_on: Option<String>,
    muted_until: Option<i64>,
    next_ping: Instant,
    history: VecDeque<u64>,
    up: bool,
    latency_ms: f64,
    total_checks: u64,
    up_checks: u64,
    /// When the up/down state last changed. Drives the transition flash.
    last_change: Option<Instant>,
    /// Recent state-change timestamps (pruned to 10 min). 3+ = flapping.
    flaps: VecDeque<Instant>,
    /// When the current outage started (session-local). Drives the ticker.
    down_since: Option<Instant>,
    /// Escalation level reached: 0 none, 1 = 5min, 2 = 30min.
    escalation: u8,
}

impl HostState {
    fn new(entry: &HostConfig) -> Self {
        HostState {
            name: entry.name.clone(),
            alias: entry.alias.clone().filter(|a| !a.trim().is_empty()),
            group: entry.group.clone(),
            interval_secs: entry.effective_interval_secs(),
            port: entry.port,
            warn_latency_ms: entry.warn_latency_ms,
            check_cmd: entry.check_cmd.clone(),
            depends_on: entry.depends_on.clone(),
            muted_until: entry.muted_until,
            next_ping: Instant::now(),
            history: VecDeque::with_capacity(config::DEFAULT_GRAPH_WIDTH),
            up: false,
            latency_ms: 0.0,
            total_checks: 0,
            up_checks: 0,
            last_change: None,
            flaps: VecDeque::new(),
            down_since: None,
            escalation: 0,
        }
    }

    /// Refresh check parameters from the config entry (add/edit/import).
    fn sync_config(&mut self, entry: &HostConfig) {
        self.interval_secs = entry.effective_interval_secs();
        self.group = entry.group.clone();
        self.alias = entry.alias.clone();
        self.port = entry.port;
        self.warn_latency_ms = entry.warn_latency_ms;
        self.check_cmd = entry.check_cmd.clone();
        self.depends_on = entry.depends_on.clone();
        self.muted_until = entry.muted_until;
    }

    fn muted(&self) -> bool {
        self.muted_until.map_or(false, |until| until > config::now_epoch())
    }

    /// Name shown in the UI: alias if set, else the target IP/hostname.
    fn display_name(&self) -> String {
        self.alias.clone().unwrap_or_else(|| self.target())
    }

    /// `db.internal:5432`-style target for display and TCP checks.
    fn target(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{}", self.name, p),
            None => self.name.clone(),
        }
    }

    /// Record a state change; returns true when the host is flapping
    /// (3+ transitions in the last 10 minutes).
    fn note_result(&mut self, up: bool, now: Instant) -> bool {
        if up != self.up {
            self.last_change = Some(now);
            self.flaps.push_back(now);
        }
        while self.flaps.front().map_or(false, |t| now.duration_since(*t) > Duration::from_secs(600)) {
            self.flaps.pop_front();
        }
        self.flaps.len() >= 3
    }

    fn flapping(&self) -> bool {
        self.flaps.len() >= 3
    }

    /// True when the state changed within the flash window.
    fn just_changed(&self) -> bool {
        self.last_change.map_or(false, |t| t.elapsed() < Duration::from_secs(15))
    }

    /// Up but slower than the warn threshold.
    fn warn_active(&self) -> bool {
        self.up
            && self
                .warn_latency_ms
                .map_or(false, |w| self.latency_ms > w as f64)
    }

    /// How long the current outage has lasted, if down.
    fn down_for(&self) -> Option<Duration> {
        if self.up {
            return None;
        }
        self.down_since.map(|t| t.elapsed())
    }
}

/// Down because an upstream dependency is down (single-level resolution,
/// matched by name or alias). Suppressed hosts keep their raw status but
/// skip notifications and escalations.
fn is_suppressed(hosts: &[HostState], name: &str) -> bool {
    let h = match hosts.iter().find(|h| h.name == name) {
        Some(h) => h,
        None => return false,
    };
    let upstream = match &h.depends_on {
        Some(u) => u,
        None => return false,
    };
    hosts
        .iter()
        .find(|x| x.name == *upstream || x.alias.as_deref() == Some(upstream.as_str()))
        .map_or(false, |u| !u.up)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HostConfig;

    fn state(name: &str, up: bool, depends_on: Option<&str>) -> HostState {
        let mut cfg = HostConfig::new(name, 60, "g", None, None);
        cfg.depends_on = depends_on.map(|s| s.to_string());
        let mut h = HostState::new(&cfg);
        h.up = up;
        h
    }

    #[test]
    fn suppression_follows_upstream() {
        let hosts = vec![
            state("gw", false, None),
            state("web", false, Some("gw")),
            state("db", false, Some("missing")),
            state("cache", true, Some("gw")),
            state("solo", false, None),
        ];
        assert!(is_suppressed(&hosts, "web")); // upstream down
        assert!(is_suppressed(&hosts, "cache")); // up but upstream down
        assert!(!is_suppressed(&hosts, "db")); // unknown upstream
        assert!(!is_suppressed(&hosts, "solo")); // no dependency
        assert!(!is_suppressed(&hosts, "nope")); // unknown host
    }

    #[test]
    fn warn_state_needs_up_and_slow() {
        let mut cfg = HostConfig::new("x", 60, "g", None, None);
        cfg.warn_latency_ms = Some(100);
        let mut h = HostState::new(&cfg);
        h.up = true;
        h.latency_ms = 250.0;
        assert!(h.warn_active());
        h.latency_ms = 50.0;
        assert!(!h.warn_active());
        h.up = false;
        h.latency_ms = 250.0;
        assert!(!h.warn_active()); // down, not warn
    }
}

/// All-in-one add-host form state (focused field editable).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct AddHostForm {
    host: String,
    interval: String,
    group: String,
    alias: String,
    port: String,
    focus: usize,
}

impl AddHostForm {
    const FIELDS: usize = 5;

    fn for_host(h: &HostState) -> Self {
        AddHostForm {
            host: h.name.clone(),
            interval: format_interval(h.interval_secs),
            group: h.group.clone(),
            alias: h.alias.clone().unwrap_or_default(),
            port: h.port.map(|p| p.to_string()).unwrap_or_default(),
            focus: 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum HistoryRange {
    Hours8,
    Hours24,
    Days7,
}

impl HistoryRange {
    const ALL: [HistoryRange; 3] = [HistoryRange::Hours8, HistoryRange::Hours24, HistoryRange::Days7];

    fn label(&self) -> &'static str {
        match self {
            HistoryRange::Hours8 => "8h",
            HistoryRange::Hours24 => "24h",
            HistoryRange::Days7 => "7d",
        }
    }

    fn duration(&self) -> chrono::Duration {
        match self {
            HistoryRange::Hours8 => chrono::Duration::hours(8),
            HistoryRange::Hours24 => chrono::Duration::hours(24),
            HistoryRange::Days7 => chrono::Duration::days(7),
        }
    }

    fn bucket_count(&self) -> usize {
        match self {
            HistoryRange::Hours8 => 32,
            HistoryRange::Hours24 => 48,
            HistoryRange::Days7 => 84,
        }
    }
}

enum InputMode {
    Normal,
    AddHost(AddHostForm),
    EditEntry { original: String, form: AddHostForm },
    SortPicker { selected: usize },
    GroupFilterPicker { groups: Vec<String>, selected: usize },
    ImportPath { path: String },
    ExportPath { path: String },
    HistoryView { host_idx: usize, range: HistoryRange, compare_idx: Option<usize> },
    ThemePicker { original: usize, selected: usize },
    MenuModal,
    KeysHelp,
    Search { query: String },
    ConfirmDelete,
}

#[derive(Clone, Debug)]
enum UpdateState {
    Idle,
    Checking,
    Downloading { version: String },
    Replacing { version: String },
    Error(String),
    Info(String),
    Done { version: String, restart_required: bool },
}

struct App {
    themes: Vec<Theme>,
    theme_idx: usize,
    config: Config,
    hosts: Vec<HostState>,
    selected_idx: usize,
    table_state: TableState,
    group_by: bool,
    group_filter: Option<String>,
    sort_mode: SortMode,
    collapsed: HashSet<String>,
    collapsed_rev: u64,
    search: Option<String>,
    compact: bool,
    wizard_dismissed: bool,
    last_esc_check: Instant,
    /// Cached visible rows + the fingerprint they were built from. Rebuilt
    /// only when results or view settings change — not every frame.
    row_cache: Vec<VisibleRow>,
    row_key: RowKey,
    /// Last rendered table geometry + visible slice, for mouse clicks.
    table_rect: Rect,
    table_slice: (usize, usize),
    input_mode: InputMode,
    update_available: Option<String>,
    update_state: UpdateState,
    last_check: String,
    last_result_time: Option<Instant>,
    restart_after_exit: bool,
    history_cache: HashMap<(String, HistoryRange), (Option<SystemTime>, HistorySummary)>,
    last_trim: Instant,
}

impl App {
    fn theme(&self) -> &Theme { &self.themes[self.theme_idx] }

    fn add_host(&mut self, name: String, interval_secs: u64, group: String, alias: String, port: Option<u16>, shared_hosts: &Arc<RwLock<Vec<HostSchedule>>>) {
        let name = name.trim().to_string();
        if name.is_empty() || self.config.hosts.iter().any(|h| h.name == name) { return; }
        let interval_secs = interval_secs.clamp(config::MIN_INTERVAL_SECS, config::MAX_INTERVAL_SECS);
        let group = if group.trim().is_empty() { "default".to_string() } else { group.trim().to_string() };
        let alias = if alias.trim().is_empty() { None } else { Some(alias.trim().to_string()) };
        let entry = HostConfig::new(&name, interval_secs, &group, alias, port);
        self.hosts.push(HostState::new(&entry));
        self.config.hosts.push(entry);
        self.persist();
        if let Ok(mut h) = shared_hosts.write() {
            *h = schedules_from_config(&self.config.hosts);
        }
    }

    fn remove_selected(&mut self, shared_hosts: &Arc<RwLock<Vec<HostSchedule>>>) {
        if self.hosts.len() <= 1 { return; }
        if self.selected_idx < self.hosts.len() {
            self.hosts.remove(self.selected_idx);
            self.config.hosts.remove(self.selected_idx);
            // Drop cached history so removed hosts free their summaries.
            self.history_cache.clear();
            self.persist();
            if self.selected_idx >= self.hosts.len() {
                self.selected_idx = self.hosts.len().saturating_sub(1);
            }
            if let Ok(mut h) = shared_hosts.write() {
                *h = schedules_from_config(&self.config.hosts);
            }
        }
    }

    /// Persist config JSON + shadow CSV export.
    fn persist(&mut self) {
        self.config.save().ok();
        self.write_entries_csv().ok();
    }

    fn write_host_records(wtr: &mut csv::Writer<std::fs::File>, hosts: &[HostConfig]) -> io::Result<()> {
        wtr.write_record(["name", "interval", "group", "alias", "port", "warn_ms", "check_cmd", "depends_on"])?;
        for h in hosts {
            wtr.write_record([
                h.name.clone(),
                format_interval(h.effective_interval_secs()),
                h.group.clone(),
                h.alias.clone().unwrap_or_default(),
                h.port.map(|p| p.to_string()).unwrap_or_default(),
                h.warn_latency_ms.map(|w| w.to_string()).unwrap_or_default(),
                h.check_cmd.clone().unwrap_or_default(),
                h.depends_on.clone().unwrap_or_default(),
            ])?;
        }
        wtr.flush()?;
        Ok(())
    }

    /// Write all entries to hosts.csv for bulk editing/import.
    fn write_entries_csv(&self) -> io::Result<()> {
        let mut wtr = csv::Writer::from_path(&paths().csv)?;
        Self::write_host_records(&mut wtr, &self.config.hosts)
    }

    /// Export current host list to a timestamped CSV in the chosen directory.
    fn export_entries(&self, dir: &std::path::Path) -> io::Result<PathBuf> {
        fs::create_dir_all(dir)?;
        let timestamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
        let dest = dir.join(format!("ping-uin-hosts-{}.csv", timestamp));
        let mut wtr = csv::Writer::from_path(&dest)?;
        Self::write_host_records(&mut wtr, &self.config.hosts)?;
        Ok(dest)
    }

    /// Static HTML status page: host table with status/latency/uptime/SLA
    /// plus text sparklines. `scp` it anywhere — no server needed.
    fn html_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    fn export_status_page(&mut self, dir: &std::path::Path) -> io::Result<PathBuf> {
        fs::create_dir_all(dir)?;
        let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
        let human = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let dest = dir.join(format!("ping-uin-status-{}.html", stamp));
        let mut rows = String::new();
        for h in &self.hosts {
            let summary = cached_history_summary(&mut self.history_cache, &h.name, HistoryRange::Hours24);
            let sla = if summary.total == 0 {
                "—".to_string()
            } else {
                format!("{:.1}%", summary.uptime_pct)
            };
            let (cls, label) = if h.muted() {
                ("muted", "MUTED")
            } else if h.flapping() {
                ("flap", "FLAP")
            } else if h.up && h.warn_active() {
                ("warn", "WARN")
            } else if h.up {
                ("up", "UP")
            } else if is_suppressed(&self.hosts, &h.name) {
                ("dep", "DEP")
            } else {
                ("down", "DOWN")
            };
            let spark: String = h
                .history
                .iter()
                .map(|lat| {
                    if *lat > 0 {
                        "<span class=\"u\">■</span>"
                    } else {
                        "<span class=\"d\">_</span>"
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            let uptime = if h.total_checks > 0 {
                format!("{:.1}%", h.up_checks as f64 / h.total_checks as f64 * 100.0)
            } else {
                "—".to_string()
            };
            rows.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td class=\"{}\">● {}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"spark\">{}</td></tr>\n",
                Self::html_escape(&h.display_name()),
                Self::html_escape(&h.target()),
                cls,
                label,
                if h.up { format!("{:.0} ms", h.latency_ms) } else { "—".to_string() },
                h.group,
                uptime,
                sla,
                spark,
            ));
        }
        let up = self.hosts.iter().filter(|h| h.up).count();
        let down = self.hosts.len().saturating_sub(up);
        let html = format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>ping-uin status {human}</title><style>\
            body{{background:#161a22;color:#c8ccd4;font-family:monospace;padding:24px}}\
            h1{{color:#eef0f6}}table{{border-collapse:collapse;width:100%}}\
            th,td{{text-align:left;padding:6px 10px;border-bottom:1px solid #2c313c}}\
            th{{color:#5a6375}}.up{{color:#a3be8c}}.down{{color:#dc6d6d}}\
            .warn,.flap{{color:#e5c07b}}.muted,.dep{{color:#5a6375}}\
            .spark{{letter-spacing:2px}}.u{{color:#8fb573}}.d{{color:#dc6d6d}}\
            </style></head><body><h1>((•O•)) ping-uin status</h1>\
            <p>{up} up · {down} down · generated {human}</p>\
            <table><tr><th>Host</th><th>Target</th><th>Status</th><th>Latency</th>\
            <th>Group</th><th>Uptime</th><th>SLA 24h</th><th>History</th></tr>{rows}\
            </table></body></html>"
        );
        fs::write(&dest, html)?;
        Ok(dest)
    }

    /// Apply one all-fields edit (from the EditEntry form) by original name.
    fn edit_entry(&mut self, original: String, form: AddHostForm, shared_hosts: &Arc<RwLock<Vec<HostSchedule>>>) {
        let new_name = form.host.trim().to_string();
        let interval_secs = parse_interval(&form.interval).unwrap_or(DEFAULT_INTERVAL_SECS)
            .clamp(config::MIN_INTERVAL_SECS, config::MAX_INTERVAL_SECS);
        let group = if form.group.trim().is_empty() { "default".to_string() } else { form.group.trim().to_string() };
        let alias = if form.alias.trim().is_empty() { None } else { Some(form.alias.trim().to_string()) };
        let port = form.port.trim().parse::<u16>().ok().filter(|p| *p > 0);
        if let Some(idx) = self.config.hosts.iter().position(|h| h.name == original) {
            let renamed = !new_name.is_empty() && new_name != self.config.hosts[idx].name;
            if renamed && self.config.hosts.iter().any(|h| h.name == new_name) { return; }
            if !new_name.is_empty() {
                self.config.hosts[idx].name = new_name.clone();
                if let Some(h) = self.hosts.get_mut(idx) { h.name = new_name.clone(); }
            }
            self.config.hosts[idx].interval_secs = interval_secs;
            self.config.hosts[idx].interval_m = 0;
            self.config.hosts[idx].group  = group.clone();
            self.config.hosts[idx].alias  = alias.clone();
            self.config.hosts[idx].port   = port;
            if let Some(h) = self.hosts.get_mut(idx) {
                h.sync_config(&self.config.hosts[idx]);
            }
            self.history_cache.clear();
            self.persist();
            if let Ok(mut h) = shared_hosts.write() {
                *h = schedules_from_config(&self.config.hosts);
            }
        }
    }

    /// Read and merge hosts.csv: new rows get added; existing rows get updated.
    fn import_entries(&mut self, path: &std::path::Path, shared_hosts: &Arc<RwLock<Vec<HostSchedule>>>) {
        if let Ok(entries) = read_entries_csv(path) {
            for entry in entries {
                match self.config.hosts.iter().position(|h| h.name == entry.name) {
                    Some(i) => {
                        self.config.hosts[i] = entry.clone();
                        if let Some(h) = self.hosts.get_mut(i) {
                            h.sync_config(&entry);
                        }
                    }
                    None => {
                        self.hosts.push(HostState::new(&entry));
                        self.config.hosts.push(entry.clone());
                    }
                }
            }
            self.history_cache.clear();
            self.persist();
            if let Ok(mut h) = shared_hosts.write() {
                *h = schedules_from_config(&self.config.hosts);
            }
        }
    }

    /// Copy runtime UI prefs into config and save (theme/group/sort/collapsed).
    /// Reload-safe: theme + view prefs survive restarts.
    fn save_prefs(&mut self) {
        self.config.theme = self.themes.get(self.theme_idx).map(|t| t.name.to_string()).unwrap_or_else(|| "btop".to_string());
        self.config.group_by = self.group_by;
        self.config.sort_mode = self.sort_mode;
        self.config.collapsed_groups = self.collapsed.iter().cloned().collect();
        self.config.compact = self.compact;
        let _ = self.config.save();
    }

    /// Mute/unmute the selected host for an hour (maintenance windows).
    fn toggle_mute_selected(&mut self, shared_hosts: &Arc<RwLock<Vec<HostSchedule>>>) {
        let idx = self.selected_idx;
        let entry = match self.config.hosts.get_mut(idx) {
            Some(e) => e,
            None => return,
        };
        let now = config::now_epoch();
        if entry.mute_remaining_secs(now) > 0 {
            entry.muted_until = None;
            self.update_state = UpdateState::Info("host unmuted".to_string());
        } else {
            entry.muted_until = Some(now + 3600);
            self.update_state = UpdateState::Info("host muted for 1h".to_string());
        }
        self.persist();
        if let Ok(mut h) = shared_hosts.write() {
            *h = schedules_from_config(&self.config.hosts);
        }
    }

    /// Collapse/expand the selected host's group (grouped view).
    fn toggle_collapse_selected(&mut self) {
        let group = match self.hosts.get(self.selected_idx) {
            Some(h) if h.group.is_empty() => "default".to_string(),
            Some(h) => h.group.clone(),
            None => return,
        };
        if !self.collapsed.remove(&group) {
            self.collapsed.insert(group);
        }
        self.collapsed_rev = self.collapsed_rev.wrapping_add(1);
        self.save_prefs();
    }

    fn clear_selected_stats(&mut self) {
        if let Some(h) = self.hosts.get_mut(self.selected_idx) {
            h.total_checks = 0;
            h.up_checks = 0;
            h.history.clear();
            h.up = false;
            h.latency_ms = 0.0;
        }
        self.history_cache.clear();
    }

    /// Force the selected host to ping ASAP by resetting its schedule.
    fn ping_selected_now(&self, shared_hosts: &Arc<RwLock<Vec<HostSchedule>>>) {
        let name = match self.hosts.get(self.selected_idx) {
            Some(h) => h.name.clone(),
            None => return,
        };
        if let Ok(mut list) = shared_hosts.write() {
            if let Some(s) = list.iter_mut().find(|s| s.name == name) {
                s.next_ping = Instant::now();
            }
        }
    }
}

enum Message {
    Result { host: String, up: bool, latency_ms: f64, timestamp: String, next_ping: Instant },
    UpdateAvailable { version: String },
    UpdateState(UpdateState),
}

fn ensure_log() -> io::Result<()> {
    let log = &paths().log;
    if !log.exists() {
        let mut file = OpenOptions::new().create(true).write(true).open(log)?;
        writeln!(file, "Timestamp,Host,Status,LatencyMs")?;
    }
    Ok(())
}

fn log_result(timestamp: &str, host: &str, status: &str, latency_ms: f64) -> io::Result<()> {
    let file = OpenOptions::new().create(true).append(true).open(&paths().log)?;
    let mut wtr = csv::Writer::from_writer(file);
    let latency = if status == "UP" { format!("{:.0}", latency_ms) } else { String::new() };
    wtr.write_record([timestamp, host, status, &latency])?;
    wtr.flush()?;
    Ok(())
}

fn seed_from_log(hosts: &mut [HostState], graph_width: usize) -> io::Result<()> {
    ensure_log()?;
    let mut rdr = csv::Reader::from_path(&paths().log)?;
    for result in rdr.records() {
        let rec = result?;
        let host = rec.get(1).unwrap_or("");
        if let Some(idx) = hosts.iter().position(|h| h.name == host) {
            hosts[idx].total_checks += 1;
            if rec.get(2) == Some("UP") { hosts[idx].up_checks += 1; }
            let lat = rec.get(3).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            hosts[idx].history.push_back(lat);
        }
    }
    for h in hosts.iter_mut() {
        while h.history.len() > graph_width {
            h.history.pop_front();
        }
    }
    Ok(())
}

fn trim_log(hosts: &mut [HostState], graph_width: usize) -> io::Result<()> {
    let contents = fs::read_to_string(&paths().log)?;
    let mut lines: Vec<&str> = contents.lines().collect();
    if lines.len() > MAX_HISTORY + 1 {
        let kept: Vec<String> = std::iter::once(lines[0].to_string())
            .chain(lines.drain(lines.len() - MAX_HISTORY..).map(|s| s.to_string()))
            .collect();
        fs::write(&paths().log, kept.join("\n") + "\n")?;
        for h in hosts.iter_mut() { h.total_checks = 0; h.up_checks = 0; h.history.clear(); }
        seed_from_log(hosts, graph_width)?;
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
struct LogEntry {
    timestamp: chrono::DateTime<chrono::Local>,
    up: bool,
    latency_ms: f64,
}

#[derive(Clone, Debug, Default)]
struct HistorySummary {
    total: usize,
    up: usize,
    down: usize,
    uptime_pct: f64,
    avg_latency_ms: f64,
    last_down: Option<String>,
    buckets: Vec<bool>, // true = mostly up in bucket
}

fn parse_log_entries(host: &str) -> Vec<LogEntry> {
    let mut entries = Vec::new();
    if let Ok(mut rdr) = csv::Reader::from_path(&paths().log) {
        for rec in rdr.records().flatten() {
            let entry_host = rec.get(1).unwrap_or("");
            if entry_host != host { continue; }
            let ts_str = rec.get(0).unwrap_or("");
            if let Ok(ts) = chrono::NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M:%S") {
                let timestamp = chrono::Local.from_local_datetime(&ts).single().unwrap_or_else(chrono::Local::now);
                let up = rec.get(2) == Some("UP");
                let latency_ms = rec.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);
                entries.push(LogEntry { timestamp, up, latency_ms });
            }
        }
    }
    entries.sort_by_key(|a| a.timestamp);
    entries
}

fn history_summary(host: &str, range: HistoryRange) -> HistorySummary {
    let entries = parse_log_entries(host);
    let now = chrono::Local::now();
    let cutoff = now - range.duration();
    let window: Vec<&LogEntry> = entries.iter().filter(|e| e.timestamp >= cutoff).collect();

    let total = window.len();
    let up = window.iter().filter(|e| e.up).count();
    let down = total.saturating_sub(up);
    let uptime_pct = if total > 0 { up as f64 / total as f64 * 100.0 } else { 0.0 };

    let up_latencies: Vec<f64> = window.iter().filter(|e| e.up).map(|e| e.latency_ms).collect();
    let avg_latency_ms = if !up_latencies.is_empty() {
        up_latencies.iter().sum::<f64>() / up_latencies.len() as f64
    } else {
        0.0
    };

    let last_down = window.iter().rev().find(|e| !e.up).map(|e| e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string());

    let bucket_count = range.bucket_count();
    let bucket_duration = range.duration() / bucket_count as i32;
    let mut buckets = vec![false; bucket_count];
    for (i, bucket) in buckets.iter_mut().enumerate().take(bucket_count) {
        let bucket_start = cutoff + bucket_duration * i as i32;
        let bucket_end = bucket_start + bucket_duration;
        let bucket_entries: Vec<&LogEntry> = window.iter().filter(|e| e.timestamp >= bucket_start && e.timestamp < bucket_end).copied().collect();
        if !bucket_entries.is_empty() {
            let up_in_bucket = bucket_entries.iter().filter(|e| e.up).count();
            *bucket = up_in_bucket * 2 >= bucket_entries.len();
        } else {
            // No data in bucket: mark as up if overall window is mostly up, else down.
            *bucket = uptime_pct >= 50.0;
        }
    }

    HistorySummary { total, up, down, uptime_pct, avg_latency_ms, last_down, buckets }
}

fn log_mtime() -> Option<SystemTime> {
    fs::metadata(&paths().log).and_then(|m| m.modified()).ok()
}

/// Cached wrapper: only re-parses uptime-log.csv when the file changed.
/// Called every frame while HistoryView is open, so caching avoids a full
/// CSV scan + sort at 20fps.
fn cached_history_summary(
    cache: &mut HashMap<(String, HistoryRange), (Option<SystemTime>, HistorySummary)>,
    host: &str,
    range: HistoryRange,
) -> HistorySummary {
    let mtime = log_mtime();
    let key = (host.to_string(), range);
    if let Some((cached_mtime, summary)) = cache.get(&key) {
        if *cached_mtime == mtime {
            return summary.clone();
        }
    }
    let summary = history_summary(host, range);
    cache.insert(key, (mtime, summary.clone()));
    summary
}

fn render_graph(history: &VecDeque<u64>, theme: &Theme, width: usize) -> Text<'static> {
    // btop-disks style: each ping is a block; green ■ if up, red bottom line _ if down.
    // One space between blocks. Newest sample on the LEFT; the strip fills
    // left-to-right as history accumulates.
    let max_show = width / 2;
    let start = history.len().saturating_sub(max_show);
    let shown = history.len() - start;
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, &lat) in history.iter().skip(start).rev().enumerate() {
        if i > 0 { spans.push(Span::raw(" ")); }
        if lat > 0 {
            spans.push(Span::styled("■", Style::default().fg(theme.graph_start)));
        } else {
            // Down ping: a thin red bottom line underscores where the gap is.
            spans.push(Span::styled("_", Style::default().fg(theme.status_danger)));
        }
    }
    // Trailing pad so young histories hug the left edge.
    let used = if shown > 0 { shown * 2 - 1 } else { 0 };
    let pad = width.saturating_sub(used);
    if pad > 0 { spans.push(Span::raw(" ".repeat(pad))); }
    Text::from(Line::from(spans))
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// btop-style hotkey hint: [ key ]  with divider brackets + hi_fg key
fn key_hint(key: &str, label: &str, theme: &Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled("[", Style::default().fg(theme.divider)),
        Span::styled(key.to_string(), Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD)),
        Span::styled("]", Style::default().fg(theme.divider)),
        Span::styled(format!(" {} ", label), Style::default().fg(theme.inactive_fg)),
    ]
}

/// btop-style box title: ▐ Title ▌ with hi_fg markers
fn accent_title(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(" ▐ ", Style::default().fg(theme.hi_fg)),
        Span::styled(text.to_string(), Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
        Span::styled(" ▌ ", Style::default().fg(theme.hi_fg)),
    ])
}

/// Newest-left bucket timeline + now/ago axis, shared by history + compare.
fn timeline_lines(theme: &Theme, summary: &HistorySummary, range: HistoryRange, popup_width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        "timeline (green = up, red = down)",
        Style::default().fg(theme.inactive_fg),
    ))];
    let timeline_width = popup_width.saturating_sub(6).min(summary.buckets.len());
    let start = summary.buckets.len().saturating_sub(timeline_width);
    let mut timeline_spans: Vec<Span> = Vec::new();
    for (i, &up) in summary.buckets.iter().skip(start).rev().enumerate() {
        if i > 0 {
            timeline_spans.push(Span::raw(" "));
        }
        if up {
            timeline_spans.push(Span::styled("▓", Style::default().fg(theme.status_good)));
        } else {
            timeline_spans.push(Span::styled("▓", Style::default().fg(theme.status_danger)));
        }
    }
    lines.push(Line::from(timeline_spans));
    let row_w = if timeline_width > 0 { timeline_width * 2 - 1 } else { 0 };
    let ago_label = format!("{} ago", range.label());
    let gap = row_w.saturating_sub(3 + ago_label.chars().count()).max(1);
    lines.push(Line::from(vec![
        Span::styled("now", Style::default().fg(theme.inactive_fg)),
        Span::raw(" ".repeat(gap)),
        Span::styled(ago_label, Style::default().fg(theme.inactive_fg)),
    ]));
    lines
}

#[derive(Clone)]
enum RowKind {
    GroupHeader { name: String, collapsed: bool, hidden: usize },
    Host,
}

#[derive(Clone)]
struct VisibleRow {
    kind: RowKind,
    host_idx: Option<usize>,
}

/// Fingerprint for the visible-row cache. `checks` is the sum of per-host
/// check counters, so any probe result invalidates the cache.
#[derive(Clone, PartialEq, Eq, Default)]
struct RowKey {
    checks: u64,
    host_count: usize,
    group_by: bool,
    group_filter: Option<String>,
    sort_mode: SortMode,
    search: Option<String>,
    collapsed_rev: u64,
}

/// Borrow the cached visible rows, rebuilding only on change. Cuts per-frame
/// allocation churn (rows + all their strings) to ~zero at steady state.
fn cached_visible_rows(app: &mut App) -> &Vec<VisibleRow> {
    let key = RowKey {
        checks: app.hosts.iter().map(|h| h.total_checks).sum(),
        host_count: app.hosts.len(),
        group_by: app.group_by,
        group_filter: app.group_filter.clone(),
        sort_mode: app.sort_mode,
        search: app.search.clone(),
        collapsed_rev: app.collapsed_rev,
    };
    if key != app.row_key {
        app.row_cache = build_visible_rows(
            &app.hosts,
            app.group_by,
            app.group_filter.as_deref(),
            app.sort_mode,
            app.search.as_deref(),
            &app.collapsed,
        );
        app.row_key = key;
    }
    &app.row_cache
}

fn sort_host_indices(indices: &mut Vec<usize>, hosts: &[HostState], sort_mode: SortMode) {
    match sort_mode {
        SortMode::None => {}
        SortMode::DownFirst => indices.sort_by(|&a, &b| {
            hosts[a].up.cmp(&hosts[b].up)
                .then_with(|| hosts[a].display_name().cmp(&hosts[b].display_name()))
        }),
        SortMode::UpFirst => indices.sort_by(|&a, &b| {
            hosts[b].up.cmp(&hosts[a].up)
                .then_with(|| hosts[a].display_name().cmp(&hosts[b].display_name()))
        }),
        SortMode::Name => indices.sort_by(|&a, &b| {
            hosts[a].display_name().cmp(&hosts[b].display_name())
        }),
        SortMode::Group => indices.sort_by(|&a, &b| {
            hosts[a].group.cmp(&hosts[b].group)
                .then_with(|| hosts[a].display_name().cmp(&hosts[b].display_name()))
        }),
        // DownOnly is a filter, not an ordering: keep config order here.
        SortMode::DownOnly => {}
    }
}

fn host_matches_search(h: &HostState, search: Option<&str>) -> bool {
    match search {
        None => true,
        Some(q) if q.trim().is_empty() => true,
        Some(q) => {
            let q = q.to_lowercase();
            h.name.to_lowercase().contains(&q)
                || h.alias.as_ref().map_or(false, |a| a.to_lowercase().contains(&q))
                || h.group.to_lowercase().contains(&q)
        }
    }
}

fn build_visible_rows(
    hosts: &[HostState],
    group_by: bool,
    group_filter: Option<&str>,
    sort_mode: SortMode,
    search: Option<&str>,
    collapsed: &HashSet<String>,
) -> Vec<VisibleRow> {
    let in_group = |h: &HostState| {
        let g = if h.group.is_empty() { "default" } else { &h.group };
        group_filter.map_or(true, |f| g == f)
    };
    // Down-only (ex down box) hides up hosts everywhere, grouped or flat.
    let visible = |h: &HostState| {
        in_group(h)
            && (sort_mode != SortMode::DownOnly || !h.up)
            && host_matches_search(h, search)
    };
    if !group_by {
        let mut indices: Vec<usize> = hosts.iter().enumerate()
            .filter(|(_, h)| visible(h))
            .map(|(i, _)| i)
            .collect();
        sort_host_indices(&mut indices, hosts, sort_mode);
        // Status sorts get Down/Up section headers in flat view.
        if sort_mode == SortMode::DownFirst || sort_mode == SortMode::UpFirst {
            let mut rows = Vec::new();
            let mut last_up: Option<bool> = None;
            for idx in indices {
                let up = hosts[idx].up;
                if last_up != Some(up) {
                    let label = if up { "Up".to_string() } else { "Down".to_string() };
                    rows.push(VisibleRow { kind: RowKind::GroupHeader { name: label, collapsed: false, hidden: 0 }, host_idx: None });
                    last_up = Some(up);
                }
                rows.push(VisibleRow { kind: RowKind::Host, host_idx: Some(idx) });
            }
            return rows;
        }
        // Group sort and down-only get group/status section headers in flat view.
        if sort_mode == SortMode::Group || sort_mode == SortMode::DownOnly {
            let mut rows = Vec::new();
            let mut last_header: Option<String> = None;
            for idx in indices {
                let header = if sort_mode == SortMode::DownOnly {
                    "Down".to_string()
                } else if hosts[idx].group.is_empty() {
                    "default".to_string()
                } else {
                    hosts[idx].group.clone()
                };
                if last_header.as_deref() != Some(&header) {
                    rows.push(VisibleRow { kind: RowKind::GroupHeader { name: header.clone(), collapsed: false, hidden: 0 }, host_idx: None });
                    last_header = Some(header);
                }
                rows.push(VisibleRow { kind: RowKind::Host, host_idx: Some(idx) });
            }
            return rows;
        }
        return indices.into_iter()
            .map(|idx| VisibleRow { kind: RowKind::Host, host_idx: Some(idx) })
            .collect();
    }
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, h) in hosts.iter().enumerate() {
        if !visible(h) { continue; }
        let group = if h.group.is_empty() { "default".to_string() } else { h.group.clone() };
        groups.entry(group).or_default().push(idx);
    }
    let mut group_names: Vec<_> = groups.keys().cloned().collect();
    group_names.sort_by(|a, b| {
        let a_down = groups[a].iter().any(|&i| !hosts[i].up);
        let b_down = groups[b].iter().any(|&i| !hosts[i].up);
        b_down.cmp(&a_down).then_with(|| a.cmp(b))
    });
    let mut rows = Vec::new();
    for group in group_names {
        let is_collapsed = collapsed.contains(&group);
        let mut indices = groups[&group].clone();
        // Default grouped behavior is down-first per group; explicit sort overrides.
        let mode = if sort_mode == SortMode::None { SortMode::DownFirst } else { sort_mode };
        sort_host_indices(&mut indices, hosts, mode);
        if is_collapsed {
            rows.push(VisibleRow {
                kind: RowKind::GroupHeader { name: group.clone(), collapsed: true, hidden: indices.len() },
                host_idx: None,
            });
            continue;
        }
        rows.push(VisibleRow {
            kind: RowKind::GroupHeader { name: group.clone(), collapsed: false, hidden: 0 },
            host_idx: None,
        });
        for idx in indices {
            rows.push(VisibleRow { kind: RowKind::Host, host_idx: Some(idx) });
        }
    }
    rows
}

fn selected_visible_position(rows: &[VisibleRow], selected_idx: usize) -> Option<usize> {
    rows.iter().position(|r| r.host_idx == Some(selected_idx))
}

fn move_selection_up(app: &mut App) {
    let sel = app.selected_idx;
    let next = {
        let rows = cached_visible_rows(app);
        let pos = match selected_visible_position(rows, sel) {
            Some(p) => p,
            None => return,
        };
        (0..pos).rev().find_map(|i| rows[i].host_idx)
    };
    if let Some(idx) = next {
        app.selected_idx = idx;
    }
}

fn move_selection_down(app: &mut App) {
    let sel = app.selected_idx;
    let next = {
        let rows = cached_visible_rows(app);
        let pos = match selected_visible_position(rows, sel) {
            Some(p) => p,
            None => return,
        };
        (pos + 1..rows.len()).find_map(|i| rows[i].host_idx)
    };
    if let Some(idx) = next {
        app.selected_idx = idx;
    }
}

fn render_host_row(
    h: &HostState,
    is_selected: bool,
    theme: &Theme,
    graph_width: usize,
    muted: bool,
    suppressed: bool,
    sla_24h: Option<f64>,
) -> Row<'static> {
    let flapping = h.flapping();
    // Priority: MUTED > FLAP > DOWN > WARN > UP; DEP dims a down host whose
    // upstream is down.
    let (status_color, status_label) = if muted {
        (theme.inactive_fg, "MUTED")
    } else if flapping {
        (theme.hi_fg, "FLAP")
    } else if h.up && h.warn_active() {
        (theme.hi_fg, "WARN")
    } else if h.up {
        (theme.status_good, "UP")
    } else if suppressed {
        (theme.inactive_fg, "DEP")
    } else {
        (theme.status_danger, "DOWN")
    };
    // Fresh transitions flash underlined for ~15s.
    let status_style = if h.just_changed() && !muted {
        Style::default().fg(status_color).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(status_color).add_modifier(Modifier::BOLD)
    };
    // Latency cell doubles as ticker: mute remaining, outage duration, ms.
    let latency_str = if muted {
        let rem = h.muted_until.map_or(String::new(), |until| {
            let s = (until - config::now_epoch()).max(0) as u64;
            format!("mut {}", config::format_duration(s))
        });
        rem
    } else if !h.up {
        h.down_for()
            .map(|d| format!("↓ {}", config::format_duration(d.as_secs())))
            .unwrap_or_else(|| "—".to_string())
    } else if h.up {
        format!("{:.0} ms", h.latency_ms)
    } else {
        "—".to_string()
    };
    let uptime = if h.total_checks > 0 { h.up_checks as f64 / h.total_checks as f64 * 100.0 } else { 0.0 };
    let row_style = if is_selected {
        Style::default().bg(theme.selected_bg).fg(theme.selected_fg)
    } else {
        Style::default().bg(theme.main_bg).fg(theme.main_fg)
    };
    // Name cell: alias, or the raw target (`host:port` for TCP checks).
    let name_line = match &h.alias {
        Some(alias) => Span::styled(alias.clone(), Style::default().fg(theme.title)),
        None => Span::styled(h.target(), Style::default().fg(theme.title)),
    };
    // IP column shows the full target when an alias is set, otherwise empty.
    let ip_line = if h.alias.is_some() {
        Span::styled(h.target(), Style::default().fg(theme.inactive_fg))
    } else {
        Span::styled("", Style::default())
    };

    Row::new(vec![
        Cell::from(name_line),
        Cell::from(ip_line),
        Cell::from(Line::from(vec![
            Span::styled("● ", Style::default().fg(status_color)),
            Span::styled(status_label, status_style),
        ])),
        Cell::from(Span::styled(latency_str, Style::default().fg(theme.main_fg))),
        Cell::from(Span::styled(format_interval(h.interval_secs), Style::default().fg(theme.inactive_fg))),
        Cell::from(Span::styled(format!("{:.1}%", uptime), Style::default().fg(theme.graph_text))),
        Cell::from(sla_cell(sla_24h, theme)),
        Cell::from(Span::styled(h.group.clone(), Style::default().fg(theme.inactive_fg))),
        Cell::from(render_graph(&h.history, theme, graph_width)),
    ]).style(row_style)
}

/// 24h SLA cell: green ≥99%, accent ≥95%, red below, dim dash when no data.
fn sla_cell(sla_24h: Option<f64>, theme: &Theme) -> Span<'static> {
    match sla_24h {
        None => Span::styled("—", Style::default().fg(theme.inactive_fg)),
        Some(pct) => {
            let color = if pct >= 99.0 {
                theme.status_good
            } else if pct >= 95.0 {
                theme.hi_fg
            } else {
                theme.status_danger
            };
            Span::styled(format!("{:.1}%", pct), Style::default().fg(color))
        }
    }
}

fn render_group_header(group: &str, hosts: &[HostState], theme: &Theme, collapsed: bool, hidden: usize, show_counts: bool) -> Row<'static> {
    // In flat-sort-by-status mode the pseudo-group is "Down" / "Up".
    let is_status_label = group == "Down" || group == "Up";
    let indices: Vec<usize> = hosts.iter().enumerate()
        .filter(|(_, h)| {
            if is_status_label { h.up == (group == "Up") } else { h.group == group }
        })
        .map(|(i, _)| i)
        .collect();
    let up = if is_status_label { indices.len() } else { indices.iter().filter(|&&i| hosts[i].up).count() };
    let down = indices.len() - up;
    // Subtle divider-style header: "── name ── X up · Y down"
    let label_fg = if is_status_label {
        if group == "Up" { theme.status_good } else { theme.status_danger }
    } else {
        theme.title
    };
    let mut spans = vec![
        Span::styled("── ", Style::default().fg(theme.divider)),
        Span::styled(group.to_string(), Style::default().fg(label_fg).add_modifier(Modifier::BOLD)),
        Span::styled(" ── ", Style::default().fg(theme.divider)),
    ];
    // Group sort hides the up/down tallies — the ordering already groups
    // them, so the counts are noise.
    if !is_status_label && show_counts {
        if up > 0 {
            spans.push(Span::styled(format!("{} up", up), Style::default().fg(theme.status_good)));
            spans.push(Span::styled(" · ", Style::default().fg(theme.divider)));
        }
        if down > 0 {
            spans.push(Span::styled(format!("{} down", down), Style::default().fg(theme.status_danger)));
            spans.push(Span::styled(" · ", Style::default().fg(theme.divider)));
        }
    } else {
        spans.push(Span::styled(format!("{} host(s)", indices.len()), Style::default().fg(theme.inactive_fg)));
        spans.push(Span::styled(" · ", Style::default().fg(theme.divider)));
    }
    if collapsed {
        spans.push(Span::styled(
            format!("{} hidden — Enter to expand", hidden),
            Style::default().fg(theme.hi_fg),
        ));
        spans.push(Span::styled(" · ", Style::default().fg(theme.divider)));
    }
    spans.push(Span::styled("──────────", Style::default().fg(theme.divider)));
    Row::new(vec![
        Cell::from(Line::from(spans)),
        Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""),
    ]).style(Style::default().bg(theme.main_bg))
}

/// Fixed-height menu box: exactly MENU_ROWS content rows + top/bottom
/// borders. Height never changes, so the table above never jumps and the
/// menu reads as one distinct bar pinned to the bottom.
const MENU_BOX_H: u16 = 4;
const MENU_ROWS: usize = 2;

fn footer_hints() -> Vec<(&'static str, &'static str)> {
    vec![
        ("↑/↓", "select"),
        ("Space", "ping now"),
        ("a", "add"),
        ("d", "delete"),
        ("e", "edit"),
        ("h", "history"),
        ("c", "clear stats"),
        ("i", "import"),
        ("E", "export"),
        ("g", "group"),
        ("f", "filter"),
        ("s", "sort"),
        ("/", "search"),
        ("?", "keys"),
        ("t", "theme"),
        ("u", "update"),
        ("q", "quit"),
    ]
}

/// Abbreviated labels used when the full menu doesn't fit in MENU_ROWS.
fn short_footer_hints() -> Vec<(&'static str, &'static str)> {
    vec![
        ("↑↓", "sel"),
        ("Spc", "ping"),
        ("a", "add"),
        ("d", "del"),
        ("e", "edit"),
        ("h", "hist"),
        ("c", "clear"),
        ("i", "imp"),
        ("E", "exp"),
        ("g", "grp"),
        ("f", "flt"),
        ("s", "sort"),
        ("/", "find"),
        ("?", "keys"),
        ("t", "thm"),
        ("u", "upd"),
        ("q", "quit"),
    ]
}

/// Keys-only last resort: every binding as a bare key, packed into MENU_ROWS.
fn keys_only_lines(theme: &Theme, max_width: usize) -> Vec<Line<'static>> {
    let keys = ["↑↓", "Spc", "a", "d", "e", "h", "c", "i", "E", "g", "f", "s", "/", "?", "t", "u", "q", "Esc"];
    let mut rows: Vec<Vec<Span<'static>>> = vec![vec![Span::raw("  ")]];
    let mut used = 2usize;
    for k in keys {
        let w = k.chars().count() + 3; // " [k]"
        if used + w > max_width {
            if rows.len() >= MENU_ROWS {
                break;
            }
            rows.push(vec![Span::raw("  ")]);
            used = 2;
        }
        rows.last_mut().unwrap().push(Span::styled(
            format!("[{}]", k),
            Style::default().fg(theme.hi_fg),
        ));
        rows.last_mut().unwrap().push(Span::raw(" "));
        used += w;
    }
    while rows.len() < MENU_ROWS {
        rows.push(vec![Span::raw("")]);
    }
    rows.into_iter().map(Line::from).collect()
}

fn hint_cell_width(key: &str, label: &str) -> usize {
    // Rendered as ` [key] label`.
    format!("[{}] {}", key, label).chars().count() + 1
}

/// Try to pack all hints (+ badge) into MENU_ROWS rows. Returns None when
/// they don't fit, so the caller can fall back to shorter labels.
fn pack_menu_rows(
    theme: &Theme,
    hints: &[(&'static str, &'static str)],
    badge: Option<&str>,
    max_width: usize,
) -> Option<Vec<Line<'static>>> {
    let mut rows: Vec<Vec<Span<'static>>> = vec![vec![Span::raw("  ")]];
    let mut used = 2usize;
    for (k, l) in hints {
        let w = hint_cell_width(k, l);
        if used + w > max_width {
            if rows.len() >= MENU_ROWS {
                return None;
            }
            rows.push(vec![Span::raw("  ")]);
            used = 2;
        }
        rows.last_mut().unwrap().push(Span::raw(" "));
        rows.last_mut().unwrap().extend(key_hint(k, l, theme));
        used += w;
    }
    if let Some(b) = badge {
        let bw = b.chars().count() + 1;
        if used + bw > max_width {
            if rows.len() >= MENU_ROWS {
                return None;
            }
            rows.push(vec![Span::raw("  ")]);
        }
        rows.last_mut().unwrap().push(Span::raw(" "));
        rows.last_mut().unwrap().push(
            Span::styled(b.to_string(), Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD)),
        );
    }
    while rows.len() < MENU_ROWS {
        rows.push(vec![Span::raw("")]);
    }
    Some(rows.into_iter().map(Line::from).collect())
}

/// Build the fixed MENU_ROWS content lines for the menu box: full labels,
/// then abbreviated labels, then fill-what-fits plus a "+N more [M]"
/// overflow marker so nothing silently vanishes on narrow windows.
fn build_footer_lines(theme: &Theme, update_available: Option<&str>, max_width: usize) -> Vec<Line<'static>> {
    let max_width = max_width.max(20);
    let badge = update_available.map(|v| format!("↑v{}", v));
    let full = footer_hints();
    if let Some(lines) = pack_menu_rows(theme, &full, badge.as_deref(), max_width) {
        return lines;
    }
    let short = short_footer_hints();
    if let Some(lines) = pack_menu_rows(theme, &short, badge.as_deref(), max_width) {
        return lines;
    }
    if badge.is_none() {
        return keys_only_lines(theme, max_width);
    }
    // Very narrow: fill rows with short hints, put the rest behind [M].
    let mut rows: Vec<Vec<Span<'static>>> = vec![vec![Span::raw("  ")]];
    let mut used = 2usize;
    let mut placed = 0usize;
    for (k, l) in &short {
        let w = hint_cell_width(k, l);
        if used + w > max_width {
            if rows.len() >= MENU_ROWS {
                break;
            }
            rows.push(vec![Span::raw("  ")]);
            used = 2;
            if used + w > max_width {
                break;
            }
        }
        rows.last_mut().unwrap().push(Span::raw(" "));
        rows.last_mut().unwrap().extend(key_hint(k, l, theme));
        used += w;
        placed += 1;
    }
    let mut tail = String::new();
    if let Some(v) = update_available {
        tail.push_str(&format!("↑v{} · ", v));
    }
    tail.push_str(&format!("+{} more [M]", short.len() - placed));
    let tail_style = Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD);
    if used + tail.chars().count() + 1 > max_width {
        // No room on the last row: overflow marker replaces it so the
        // update badge / more-count always stays visible.
        *rows.last_mut().unwrap() = vec![Span::raw("  "), Span::styled(tail, tail_style)];
    } else {
        rows.last_mut().unwrap().push(Span::raw(" "));
        rows.last_mut().unwrap().push(Span::styled(tail, tail_style));
    }
    while rows.len() < MENU_ROWS {
        rows.push(vec![Span::raw("")]);
    }
    rows.into_iter().map(Line::from).collect()
}

fn ui(frame: &mut Frame, app: &mut App) {
    let theme = app.theme().clone();
    let area = frame.area();

    frame.render_widget(Block::default().style(Style::default().bg(theme.main_bg)), area);

    // Layout: title / stats / table / fixed-height menu box.
    // The menu box never changes height, so the table never jumps.
    let up_count = app.hosts.iter().filter(|h| h.up).count();
    let total = app.hosts.len();
    let muted_total = app.hosts.iter().filter(|h| h.muted()).count();
    let down_count = app.hosts.iter().filter(|h| !h.up && !h.muted()).count();
    let pct_up = if total > 0 { (up_count as f64 / total as f64 * 100.0).round() as u64 } else { 0 };
    let now = Local::now().format("%H:%M:%S").to_string();

    // Inner width of the menu box: margin + box borders.
    let footer_width = (area.width as usize).saturating_sub(2 + 2).saturating_sub(2);
    let footer_lines = build_footer_lines(&theme, app.update_available.as_deref(), footer_width);

    let constraints = vec![
        Constraint::Length(5),
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(MENU_BOX_H),
    ];

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(constraints)
        .split(area);

    let (stats_area, table_area, footer_area) = (main_layout[1], main_layout[2], main_layout[3]);
    let title_area = main_layout[0];

    // ── Title box: centered ping-uin with penguin face in its own border ──
    let title_box = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.box_color))
        .style(Style::default().bg(theme.main_bg));
    let title_inner = title_box.inner(title_area);
    frame.render_widget(title_box, title_area);
    // Two-line centered logo: penguin above, name below.
    let logo_penguin = Line::from(vec![
        Span::styled("((•O•))", Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD)),
    ]);
    let logo_name = Line::from(vec![
        Span::styled("▐ ", Style::default().fg(theme.hi_fg)),
        Span::styled("ping-uin", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
        Span::styled(" ▌", Style::default().fg(theme.hi_fg)),
    ]);
    let logo_text = Text::from(vec![
        logo_penguin,
        logo_name,
    ]);
    frame.render_widget(
        Paragraph::new(logo_text).alignment(Alignment::Center),
        title_inner,
    );

    // Update pill in the top-right corner: ambient "update ready" notice
    // tied to the `u` key. Skipped on narrow windows where it would collide
    // with the centered logo (the footer badge still shows there).
    if let Some(ref version) = app.update_available {
        let pill = format!(" ↑ v{} ready — u to update ", version);
        let pill_w = pill.chars().count() as u16;
        if title_inner.width >= 72 && pill_w + 2 < title_inner.width {
            let pill_area = Rect {
                x: title_inner.x + title_inner.width - pill_w - 1,
                y: title_inner.y,
                width: pill_w,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    pill,
                    Style::default()
                        .fg(theme.hi_fg)
                        .add_modifier(Modifier::BOLD)
                        .bg(theme.popup_bg),
                )))
                .alignment(Alignment::Right),
                pill_area,
            );
        }
    }

    // ── Stats box: dedicated box under header with up / down / % up ──
    let stats_title = Line::from(vec![
        Span::styled(" ▐ ", Style::default().fg(theme.hi_fg)),
        Span::styled("stats", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
        Span::styled(" ▌ ", Style::default().fg(theme.hi_fg)),
    ]);
    let stats_block = Block::default()
        .title(stats_title)
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.box_color))
        .style(Style::default().bg(theme.main_bg));
    let stats_inner = stats_block.inner(stats_area);
    frame.render_widget(stats_block, stats_area);
    let flap_count = app.hosts.iter().filter(|h| h.flapping()).count();
    let mut stats_spans = vec![
        Span::styled("● ", Style::default().fg(theme.status_good)),
        Span::styled(format!("{} up", up_count), Style::default().fg(theme.status_good).add_modifier(Modifier::BOLD)),
        Span::styled("   ", Style::default()),
        Span::styled("● ", Style::default().fg(theme.status_danger)),
        Span::styled(format!("{} down", down_count), Style::default().fg(theme.status_danger).add_modifier(Modifier::BOLD)),
        Span::styled("   ", Style::default()),
        Span::styled("◐ ", Style::default().fg(theme.hi_fg)),
        Span::styled(format!("{}% up", pct_up), Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  ·  {} hosts", total), Style::default().fg(theme.inactive_fg)),
    ];
    if flap_count > 0 {
        stats_spans.push(Span::styled(
            format!("  ·  ~{} flapping", flap_count),
            Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD),
        ));
    }
    if muted_total > 0 {
        stats_spans.push(Span::styled(
            format!("  ·  {} muted", muted_total),
            Style::default().fg(theme.inactive_fg),
        ));
    }
    let suppressed_count = app
        .hosts
        .iter()
        .filter(|h| !h.up && !h.muted() && is_suppressed(&app.hosts, &h.name))
        .count();
    if suppressed_count > 0 {
        stats_spans.push(Span::styled(
            format!("  ·  {} via upstream", suppressed_count),
            Style::default().fg(theme.inactive_fg),
        ));
    }
    if let Some(q) = app.search.as_deref().filter(|q| !q.trim().is_empty()) {
        stats_spans.push(Span::styled(
            format!("  ·  /{}", q),
            Style::default().fg(theme.hi_fg),
        ));
    }
    let stats_line = Line::from(stats_spans);
    let stats_right = Line::from(vec![
        Span::styled(format!("v{} ", env!("CARGO_PKG_VERSION")), Style::default().fg(theme.inactive_fg)),
        Span::styled(now, Style::default().fg(theme.inactive_fg)),
        Span::styled(" · ", Style::default().fg(theme.divider)),
        Span::styled(theme.name, Style::default().fg(theme.hi_fg)),
    ]);
    let stats_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(30), Constraint::Length(30)])
        .split(stats_inner);
    frame.render_widget(Paragraph::new(Text::from(stats_line)), stats_layout[0]);
    frame.render_widget(Paragraph::new(Text::from(stats_right)).alignment(Alignment::Right), stats_layout[1]);

    let mut title_spans = accent_title("last check", &theme).spans;
    title_spans.push(Span::styled(if app.last_check.is_empty() { "—".to_string() } else { app.last_check.clone() }, Style::default().fg(theme.inactive_fg)));
    let table_block = Block::default()
        .title(Line::from(title_spans))
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.box_color))
        .style(Style::default().bg(theme.main_bg));

    // Compact density hides the IP + Group columns (zero-width constraints).
    let (ip_w, group_w): (u16, u16) = if app.compact { (0, 0) } else { (18, 10) };
    let header = Row::new(vec!["Host", "IP", "Status", "Latency", "Int", "Uptime", "SLA", "Group", "History"])
        .style(Style::default().fg(theme.inactive_fg).add_modifier(Modifier::BOLD))
        .height(1);

    // Viewport culling: only build widget rows for what's on screen.
    // Keeps per-frame allocation flat no matter how many hosts exist.
    let viewport = table_area.height.saturating_sub(3).max(1) as usize;
    let sel = app.selected_idx;
    // Clone only the on-screen slice: O(viewport), not O(hosts).
    let (slice, selected_pos, slice_start) = {
        let visible = cached_visible_rows(app);
        let pos = selected_visible_position(visible, sel).unwrap_or(0);
        let start = if visible.len() <= viewport {
            0
        } else {
            pos.saturating_sub(viewport / 2)
                .min(visible.len().saturating_sub(viewport))
        };
        let end = (start + viewport).min(visible.len());
        (visible[start..end].to_vec(), pos, start)
    };
    app.table_state.select(Some(selected_pos.saturating_sub(slice_start)));
    app.table_rect = table_area;
    app.table_slice = (slice_start, slice_start + slice.len());

    let mut rows = Vec::new();
    if app.hosts.is_empty() {
        rows.push(Row::new(vec![
            Cell::from(Span::styled("No hosts — press 'a' to add one", Style::default().fg(theme.inactive_fg))),
            Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""),
        ]));
    } else {
        let show_counts = app.sort_mode != SortMode::Group;
        let graph_width = app.config.graph_width;
        for row in &slice {
            match &row.kind {
                RowKind::GroupHeader { name, collapsed, hidden } => rows.push(render_group_header(name, &app.hosts, &theme, *collapsed, *hidden, show_counts)),
                RowKind::Host => {
                    let idx = row.host_idx.unwrap();
                    let is_sel = idx == app.selected_idx;
                    let (muted, suppressed) = {
                        let h = &app.hosts[idx];
                        (h.muted(), !h.up && is_suppressed(&app.hosts, &h.name))
                    };
                    let host_name = app.hosts[idx].name.clone();
                    let sla = {
                        let summary = cached_history_summary(&mut app.history_cache, &host_name, HistoryRange::Hours24);
                        if summary.total == 0 { None } else { Some(summary.uptime_pct) }
                    };
                    rows.push(render_host_row(&app.hosts[idx], is_sel, &theme, graph_width, muted, suppressed, sla));
                }
            }
        }
    }

    let table = Table::new(rows, [
        Constraint::Length(18),
        Constraint::Length(ip_w),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Length(group_w),
        Constraint::Length(app.config.graph_width as u16),
    ])
    .header(header)
    .block(table_block);
    frame.render_stateful_widget(table, table_area, &mut app.table_state);

    // Render footer: fixed-height bordered menu box — distinct bar that
    // never changes height. Modal modes reuse the same box + height so the
    // table above doesn't jump when popups open/close.
    let menu_box = |title: Line<'static>| {
        Block::default()
            .title(title)
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.box_color))
            .style(Style::default().bg(theme.popup_bg))
    };
    match app.input_mode {
        InputMode::Normal => {
            let block = menu_box(accent_title("menu", &theme));
            let inner = block.inner(footer_area);
            frame.render_widget(block, footer_area);
            let footer = Paragraph::new(Text::from(footer_lines))
                .style(Style::default().bg(theme.popup_bg).fg(theme.main_fg));
            frame.render_widget(footer, inner);
        }
        _ => {
            let footer_text = match app.input_mode {
                InputMode::AddHost(_) => Text::from(Line::from(vec![
                    Span::styled("Add host", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                    Span::raw("   "),
                    Span::raw("[Tab]/[↑↓] move field   [Enter] add   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)),
                ])),
                InputMode::SortPicker { .. } => Text::from(Line::from(vec![
                    Span::styled("View", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                    Span::raw("   "),
                    Span::raw("[↑↓] pick   [1-6] quick   [Space] show all   [Enter] apply   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)),
                ])),
                InputMode::GroupFilterPicker { .. } => Text::from(Line::from(vec![
                    Span::styled("Filter group", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                    Span::raw("   "),
                    Span::raw("[↑↓] pick   [Enter] apply   [Space] show all   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)),
                ])),
                InputMode::ImportPath { .. } => Text::from(Line::from(vec![
                    Span::styled("Import CSV", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                    Span::raw("   "),
                    Span::raw("[Enter] import   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)),
                ])),
                InputMode::EditEntry { ref original, .. } => Text::from(Line::from(vec![
                    Span::styled(format!("Edit {}", original), Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                    Span::raw("   "),
                    Span::raw("[Tab]/[↑↓] move field   [Enter] save   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)),
                ])),
                InputMode::ConfirmDelete => {
                    let name = app.hosts.get(app.selected_idx).map(|h| h.name.clone()).unwrap_or_default();
                    Text::from(Line::from(vec![
                        Span::styled("Delete ", Style::default().fg(theme.status_danger)),
                        Span::styled(name, Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                        Span::styled("? [y/n]", Style::default().fg(theme.status_danger)),
                    ]))
                }
                InputMode::HistoryView { .. } => Text::from(Line::from(vec![
                    Span::styled("History", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                    Span::raw("   "),
                    Span::raw("[←/→] range   [Esc/h] close").style(Style::default().fg(theme.inactive_fg)),
                ])),
                InputMode::ExportPath { .. } => Text::from(Line::from(vec![
                    Span::styled("Export", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                    Span::raw("   "),
                    Span::raw("[Enter] export   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)),
                ])),
                InputMode::ThemePicker { .. } => Text::from(Line::from(vec![
                    Span::styled("Theme", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                    Span::raw("   "),
                    Span::raw("[↑/↓] preview   [Enter] apply   [Esc/t] cancel").style(Style::default().fg(theme.inactive_fg)),
                ])),
                InputMode::MenuModal => Text::from(Line::from(vec![
                    Span::styled("Menu", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                    Span::raw("   "),
                    Span::raw("[Esc/M] close").style(Style::default().fg(theme.inactive_fg)),
                ])),
                InputMode::KeysHelp => Text::from(Line::from(vec![
                    Span::styled("Keys", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                    Span::raw("   "),
                    Span::raw("[Esc/?] close").style(Style::default().fg(theme.inactive_fg)),
                ])),
                InputMode::Search { ref query } => {
                    let q = if query.is_empty() { " ".to_string() } else { format!("{}▌", query) };
                    Text::from(Line::from(vec![
                        Span::styled("Search ", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                        Span::styled(q, Style::default().fg(theme.hi_fg)),
                        Span::styled("   [Enter] keep   [Esc] clear", Style::default().fg(theme.inactive_fg)),
                    ]))
                }
                InputMode::Normal => unreachable!(),
            };
            // Same fixed-height box as the menu so the layout never shifts.
            let mode_title = match app.input_mode {
                InputMode::AddHost(_) => "add host",
                InputMode::SortPicker { .. } => "view",
                InputMode::GroupFilterPicker { .. } => "filter",
                InputMode::ImportPath { .. } => "import",
                InputMode::EditEntry { .. } => "edit",
                InputMode::ConfirmDelete => "delete",
                InputMode::HistoryView { .. } => "history",
                InputMode::ExportPath { .. } => "export",
                InputMode::ThemePicker { .. } => "theme",
                InputMode::MenuModal => "menu",
                InputMode::KeysHelp => "keys",
                InputMode::Search { .. } => "search",
                InputMode::Normal => unreachable!(),
            };
            let block = menu_box(accent_title(mode_title, &theme));
            let inner = block.inner(footer_area);
            frame.render_widget(block, footer_area);
            let footer = Paragraph::new(footer_text).wrap(Wrap { trim: true });
            frame.render_widget(footer, inner);
        }
    }

    match app.input_mode {
        InputMode::AddHost(ref form) | InputMode::EditEntry { ref form, .. } => {
            let title_text = if matches!(app.input_mode, InputMode::AddHost(_)) { "Add host" } else { "Edit host" };
            let popup_area = centered_rect(56, 48, area);
            let labels = ["host (IP/name)", "interval (30s/5m)", "group", "display name", "TCP port"];
            let values = [&form.host, &form.interval, &form.group, &form.alias, &form.port];
            let placeholder = ["e.g. 8.8.8.8", "2m", "default", "optional", "blank = ping"];
            let mut lines: Vec<Line> = Vec::new();
            for i in 0..AddHostForm::FIELDS {
                let focused = form.focus == i;
                let marker = if focused { "▶ " } else { "  " };
                // Visible text cursor on the focused field so keyboard-first
                // use is obvious even though editing is append-only.
                let mut value = if values[i].is_empty() { placeholder[i].to_string() } else { values[i].clone() };
                if focused {
                    value.push('▌');
                }
                let style = if values[i].is_empty() {
                    Style::default().fg(theme.inactive_fg)
                } else if focused {
                    Style::default().fg(theme.title).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.main_fg)
                };
                let label_style = if focused { Style::default().fg(theme.hi_fg) } else { Style::default().fg(theme.inactive_fg) };
                lines.push(Line::from(vec![
                    Span::styled(marker, Style::default().fg(theme.hi_fg)),
                    Span::styled(format!("{:<16}", labels[i]), label_style),
                    Span::styled(value, style),
                ]));
                lines.push(Line::from(""));
            }
            lines.push(Line::from("[Enter] save   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)));
            let popup = Paragraph::new(Text::from(lines))
                .block(Block::default()
                    .title(accent_title(title_text, &theme))
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.box_color))
                    .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        InputMode::SortPicker { selected } => {
            let popup_area = centered_rect(34, 46, area);
            let mut lines: Vec<Line> = vec![Line::from("")];
            for (i, mode) in SortMode::ALL.iter().enumerate() {
                let selected_here = i == selected;
                let active_here = *mode == app.sort_mode;
                let marker = if selected_here { "▶ " } else { "  " };
                let check = if active_here { " ✓" } else { "" };
                let style = if selected_here {
                    Style::default().fg(theme.title).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.main_fg)
                };
                lines.push(Line::from(vec![
                    Span::styled(marker, Style::default().fg(theme.hi_fg)),
                    Span::styled(format!("{} {}", i + 1, mode.label()), style),
                    Span::styled(check, Style::default().fg(theme.status_good)),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("[↑↓/1-6] pick   [Space] show all   [Enter] apply   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)));
            let popup = Paragraph::new(Text::from(lines))
                .block(Block::default()
                    .title(accent_title("View: sort & filter", &theme))
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.box_color))
                    .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        InputMode::GroupFilterPicker { ref groups, selected } => {
            let popup_area = centered_rect(36, 42, area);
            let mut lines: Vec<Line> = vec![Line::from("")];
            if groups.is_empty() {
                lines.push(Line::from("No groups defined").style(Style::default().fg(theme.inactive_fg)));
            } else {
                for (i, group) in groups.iter().enumerate() {
                    let selected_here = i == selected;
                    let active_here = app.group_filter.as_ref() == Some(group);
                    let marker = if selected_here { "▶ " } else { "  " };
                    let check = if active_here { " ✓" } else { "" };
                    let style = if selected_here {
                        Style::default().fg(theme.title).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.main_fg)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(marker, Style::default().fg(theme.hi_fg)),
                        Span::styled(group.clone(), style),
                        Span::styled(check, Style::default().fg(theme.status_good)),
                    ]));
                }
            }
            lines.push(Line::from(""));
            lines.push(Line::from("[Enter] filter group   [Space] show all   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)));
            let popup = Paragraph::new(Text::from(lines))
                .block(Block::default()
                    .title(accent_title("Filter by group", &theme))
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.box_color))
                    .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        InputMode::ImportPath { ref path } => {
            let popup_area = centered_rect(60, 30, area);
            let display_path = if path.is_empty() { " ".to_string() } else { path.clone() };
            let popup = Paragraph::new(Text::from(vec![
                Line::from(""),
                Line::from("Path to hosts.csv:").style(Style::default().fg(theme.inactive_fg)),
                Line::from(""),
                Line::from(Span::styled(display_path, Style::default().fg(theme.title))),
                Line::from(""),
                Line::from("[Enter] import   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)),
            ]))
            .block(Block::default()
                .title(accent_title("Import CSV", &theme))
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.box_color))
                .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        InputMode::ExportPath { ref path } => {
            let popup_area = centered_rect(60, 30, area);
            let display_path = if path.is_empty() { " ".to_string() } else { path.clone() };
            let popup = Paragraph::new(Text::from(vec![
                Line::from(""),
                Line::from("Export host list to directory:").style(Style::default().fg(theme.inactive_fg)),
                Line::from(""),
                Line::from(Span::styled(display_path, Style::default().fg(theme.title))),
                Line::from(""),
                Line::from("A timestamped CSV will be created here.").style(Style::default().fg(theme.inactive_fg)),
                Line::from(""),
                Line::from("[Enter] export   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)),
            ]))
            .block(Block::default()
                .title(accent_title("Export hosts", &theme))
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.box_color))
                .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        InputMode::HistoryView { host_idx, range, compare_idx } => {
            let tall = compare_idx.is_some();
            let popup_area = centered_rect(74, if tall { 66 } else { 48 }, area);
            let host_name = app.hosts.get(host_idx).map(|h| h.name.clone()).unwrap_or_default();
            let name = app.hosts.get(host_idx).map(|h| h.display_name()).unwrap_or_default();
            let summary = cached_history_summary(&mut app.history_cache, &host_name, range);

            let mut lines = vec![Line::from("")];

            // Range selector
            let mut range_spans = vec![Span::styled("range: ", Style::default().fg(theme.inactive_fg))];
            for (i, r) in HistoryRange::ALL.iter().enumerate() {
                if i > 0 { range_spans.push(Span::styled("  ", Style::default())); }
                let style = if *r == range {
                    Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.inactive_fg)
                };
                range_spans.push(Span::styled(format!("[{}]", r.label()), style));
            }
            lines.push(Line::from(range_spans));
            lines.push(Line::from(""));

            // Stats
            lines.push(Line::from(vec![
                Span::styled("checks: ", Style::default().fg(theme.inactive_fg)),
                Span::styled(format!("{}", summary.total), Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                Span::styled("   up: ", Style::default().fg(theme.inactive_fg)),
                Span::styled(format!("{}", summary.up), Style::default().fg(theme.status_good).add_modifier(Modifier::BOLD)),
                Span::styled("   down: ", Style::default().fg(theme.inactive_fg)),
                Span::styled(format!("{}", summary.down), Style::default().fg(theme.status_danger).add_modifier(Modifier::BOLD)),
            ]));
            let uptime_color = if summary.uptime_pct >= 99.0 { theme.status_good } else if summary.uptime_pct >= 95.0 { theme.hi_fg } else { theme.status_danger };
            lines.push(Line::from(vec![
                Span::styled("uptime: ", Style::default().fg(theme.inactive_fg)),
                Span::styled(format!("{:.2}%", summary.uptime_pct), Style::default().fg(uptime_color).add_modifier(Modifier::BOLD)),
                Span::styled("   avg latency: ", Style::default().fg(theme.inactive_fg)),
                Span::styled(format!("{:.1} ms", summary.avg_latency_ms), Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
            ]));
            if let Some(ref last_down) = summary.last_down {
                lines.push(Line::from(vec![
                    Span::styled("last down: ", Style::default().fg(theme.inactive_fg)),
                    Span::styled(last_down.clone(), Style::default().fg(theme.status_danger)),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled("last down: ", Style::default().fg(theme.inactive_fg)),
                    Span::styled("none", Style::default().fg(theme.status_good)),
                ]));
            }
            lines.push(Line::from(""));

            // Timeline, newest bucket on the left to match the main strip.
            lines.extend(timeline_lines(&theme, &summary, range, popup_area.width as usize));
            // Side-by-side compare against a second host (Tab cycles target).
            if let Some(c_idx) = compare_idx.and_then(|c| app.hosts.get(c).map(|h| (c, h.name.clone(), h.display_name()))) {
                let (c_idx, c_name, c_display) = c_idx;
                let _ = c_idx;
                let c_summary = cached_history_summary(&mut app.history_cache, &c_name, range);
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("vs ", Style::default().fg(theme.inactive_fg)),
                    Span::styled(c_display, Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  {:.2}% up", c_summary.uptime_pct), Style::default().fg(theme.graph_text)),
                ]));
                lines.extend(timeline_lines(&theme, &c_summary, range, popup_area.width as usize));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("[←/→] range   [Tab] compare   [Esc/h] close").style(Style::default().fg(theme.inactive_fg)));

            let popup = Paragraph::new(Text::from(lines))
                .block(Block::default()
                    .title(accent_title(&format!("history: {}", name), &theme))
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.box_color))
                    .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        InputMode::ThemePicker { original, selected } => {
            let popup_area = centered_rect(45, 46, area);
            let mut lines = vec![Line::from("")];
            for (i, t) in app.themes.iter().enumerate() {
                let marker = if i == selected { "▶ " } else { "  " };
                let is_current = i == original;
                let mut spans = vec![
                    Span::styled(marker, Style::default().fg(theme.hi_fg)),
                ];
                if is_current {
                    spans.push(Span::styled("* ", Style::default().fg(theme.status_good)));
                } else {
                    spans.push(Span::raw("  "));
                }
                let name_style = if i == selected {
                    Style::default().fg(theme.title).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.main_fg)
                };
                spans.push(Span::styled(t.name.to_string(), name_style));
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("[↑/↓] preview   [Enter] apply   [Esc/t] cancel").style(Style::default().fg(theme.inactive_fg)));
            let popup = Paragraph::new(Text::from(lines))
                .block(Block::default()
                    .title(accent_title("theme", &theme))
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.box_color))
                    .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        InputMode::MenuModal => {
            let menu_hints = vec![
                ("Space", "ping now"),
                ("a", "add host"),
                ("d", "delete host"),
                ("e", "edit host"),
                ("h", "history"),
                ("c", "clear stats"),
                ("i", "import"),
                ("E", "export"),
                ("g", "group"),
                ("f", "filter group"),
                ("s", "view/sort"),
                ("/", "search"),
                ("?", "keys"),
                ("Enter", "collapse group"),
                ("!", "mute 1h"),
                ("v", "compact"),
                ("t", "theme"),
                ("u", "update"),
                ("q", "quit"),
            ];
            let rows = (menu_hints.len() + 1) / 2;
            let popup_height = ((rows + 5).min(area.height as usize).max(8)) as u16;
            let popup_area = centered_rect(60, popup_height, area);
            let mut lines = vec![Line::from("")];
            for chunk in menu_hints.chunks(2) {
                let mut spans = vec![Span::raw("  ")];
                for (i, (k, l)) in chunk.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::raw("   "));
                    }
                    spans.extend(key_hint(k, l, &theme));
                }
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("[Esc/M] close").style(Style::default().fg(theme.inactive_fg)));
            let popup = Paragraph::new(Text::from(lines))
                .block(Block::default()
                    .title(accent_title("menu", &theme))
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.box_color))
                    .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        InputMode::KeysHelp => {
            let sections: Vec<(&str, Vec<(&str, &str)>)> = vec![
                ("navigate", vec![("↑/↓", "select"), ("Enter", "collapse group"), ("g", "grouped/flat"), ("v", "compact")]),
                ("hosts", vec![("a", "add"), ("e", "edit"), ("d", "delete"), ("c", "clear stats"), ("!", "mute 1h")]),
                ("inspect", vec![("h", "history"), ("Tab", "compare"), ("s", "view/sort"), ("f", "filter group"), ("/", "search")]),
                ("run", vec![("Space/p", "ping now"), ("i", "import csv"), ("E", "export csv")]),
                ("app", vec![("t", "theme"), ("u", "update"), ("M", "menu"), ("?", "this help"), ("q", "quit"), ("Esc", "reset view")]),
            ];
            let popup_height = ((sections.len() + 6).min(area.height as usize).max(8)) as u16;
            let popup_area = centered_rect(62, popup_height, area);
            let mut lines = vec![Line::from("")];
            for (title, items) in &sections {
                let mut spans = vec![
                    Span::styled(format!("  {:<9}", title), Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                ];
                for (i, (k, l)) in items.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::raw("  "));
                    }
                    spans.extend(key_hint(k, l, &theme));
                }
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("[Esc/?] close").style(Style::default().fg(theme.inactive_fg)));
            let popup = Paragraph::new(Text::from(lines))
                .block(Block::default()
                    .title(accent_title("key bindings", &theme))
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.box_color))
                    .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        InputMode::ConfirmDelete => {
            let popup_area = centered_rect(45, 14, area);
            let name = app.hosts.get(app.selected_idx).map(|h| h.name.clone()).unwrap_or_default();
            let popup = Paragraph::new(Text::from(vec![
                Line::from(""),
                Line::from("Delete this host?").style(Style::default().fg(theme.main_fg).add_modifier(Modifier::BOLD)),
                Line::from(""),
                Line::from(Span::styled(name, Style::default().fg(theme.status_danger).add_modifier(Modifier::BOLD))),
                Line::from(""),
                Line::from("[y] delete   [n] cancel").style(Style::default().fg(theme.inactive_fg)),
            ]))
            .alignment(Alignment::Center)
            .block(Block::default()
                .title(accent_title("Confirm delete", &theme))
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.status_danger))
                .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        // Search renders in the footer box; KeysHelp above.
        InputMode::Search { .. } | InputMode::Normal => {}
    }

    // First-run wizard: empty host list gets import/add/seed choices.
    if app.hosts.is_empty()
        && !app.wizard_dismissed
        && matches!(app.input_mode, InputMode::Normal)
    {
        let popup_area = centered_rect(58, 40, area);
        let popup = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::from("No hosts yet — get started:").style(
                Style::default().fg(theme.title).add_modifier(Modifier::BOLD),
            ),
            Line::from(""),
            Line::from(vec![
                Span::styled("  ▶ ", Style::default().fg(theme.hi_fg)),
                Span::styled("[i] ", Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD)),
                Span::styled("import hosts.csv", Style::default().fg(theme.main_fg)),
            ]),
            Line::from(vec![
                Span::styled("  ▶ ", Style::default().fg(theme.hi_fg)),
                Span::styled("[a] ", Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD)),
                Span::styled("add your first host", Style::default().fg(theme.main_fg)),
            ]),
            Line::from(vec![
                Span::styled("  ▶ ", Style::default().fg(theme.hi_fg)),
                Span::styled("[d] ", Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD)),
                Span::styled("seed the demo set (8.8.8.8, 1.1.1.1, …)", Style::default().fg(theme.main_fg)),
            ]),
            Line::from(""),
            Line::from("[i/a/d] choose   [Esc] dismiss").style(Style::default().fg(theme.inactive_fg)),
        ]))
        .alignment(Alignment::Left)
        .block(
            Block::default()
                .title(accent_title("welcome", &theme))
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.box_color))
                .style(Style::default().bg(theme.popup_bg)),
        );
        frame.render_widget(Clear, popup_area);
        frame.render_widget(popup, popup_area);
    }

    // Update / info overlay (shown on top of any input mode).
    if !matches!(app.update_state, UpdateState::Idle) {
        let popup_area = centered_rect(56, 34, area);
        let (title, body, color) = match &app.update_state {
            UpdateState::Idle => unreachable!(),
            UpdateState::Checking => ("Update", vec![Line::from(""), Line::from("Checking latest release...")], theme.hi_fg),
            UpdateState::Downloading { version } => ("Update", vec![Line::from(""), Line::from(format!("Downloading v{}...", version))], theme.hi_fg),
            UpdateState::Replacing { version } => ("Update", vec![Line::from(""), Line::from(format!("Installing v{}...", version))], theme.hi_fg),
            UpdateState::Error(e) => ("Notice", vec![Line::from(""), Line::from(e.clone())], theme.status_danger),
            UpdateState::Info(msg) => ("Notice", vec![Line::from(""), Line::from(msg.clone())], theme.hi_fg),
            UpdateState::Done { version, restart_required } => {
                let msg = if *restart_required {
                    format!("Updated to v{}. Please restart.", version)
                } else {
                    format!("Updated to v{}.", version)
                };
                ("Update complete", vec![Line::from(""), Line::from(msg)], theme.status_good)
            }
        };
        let mut lines = body;
        lines.push(Line::from(""));
        lines.push(Line::from("[Esc] close").style(Style::default().fg(theme.inactive_fg)));
        let popup = Paragraph::new(Text::from(lines))
            .alignment(Alignment::Center)
            .block(Block::default()
                .title(accent_title(title, &theme))
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color))
                .style(Style::default().bg(theme.popup_bg)));
        frame.render_widget(Clear, popup_area);
        frame.render_widget(popup, popup_area);
    }
}

/// Parallel check pool: 16 workers drain due hosts concurrently instead of
/// one-at-a-time. A down host still costs its full timeout, but it no longer
/// blocks every host behind it. Deliberately fork-based (no raw sockets), so
/// no root/CAP_NET_RAW is ever required.
const WORKER_POOL_SIZE: usize = 16;

struct CheckJob {
    name: String,
    port: Option<u16>,
    check_cmd: Option<String>,
    timeout_ms: u64,
    interval_secs: u64,
}

fn spawn_worker_pool(
    tx: mpsc::Sender<Message>,
    hosts: Arc<RwLock<Vec<HostSchedule>>>,
    timeout_ms: u64,
    shutdown: Arc<AtomicBool>,
) -> Vec<thread::JoinHandle<()>> {
    let (job_tx, job_rx) = mpsc::channel::<CheckJob>();
    let job_rx = Arc::new(std::sync::Mutex::new(job_rx));
    let mut handles = Vec::with_capacity(WORKER_POOL_SIZE + 1);

    // Workers: one compiled regex each, loop until the job channel closes.
    for _ in 0..WORKER_POOL_SIZE {
        let tx = tx.clone();
        let hosts = hosts.clone();
        let job_rx = job_rx.clone();
        handles.push(thread::spawn(move || {
            let re = Regex::new(r"time[<=]([\d.]+)\s*ms").unwrap();
            loop {
                let job = { job_rx.lock().unwrap().recv() };
                let job = match job {
                    Ok(j) => j,
                    Err(_) => break, // scheduler gone: shut down
                };
                let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let (up, latency_ms) =
                    check_host(&job.name, job.port, job.check_cmd.as_deref(), job.timeout_ms, &re);
                let next_ping = Instant::now()
                    + Duration::from_secs(job.interval_secs)
                    + host_jitter(&job.name, job.interval_secs);
                if let Ok(mut list) = hosts.write() {
                    if let Some(h) = list.iter_mut().find(|h| h.name == job.name) {
                        h.next_ping = next_ping;
                        h.inflight = false;
                    }
                }
                // Channel closed means the UI already quit; stop.
                if tx
                    .send(Message::Result {
                        host: job.name,
                        up,
                        latency_ms,
                        timestamp,
                        next_ping,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    // Scheduler: every 100ms, dispatch each due host exactly once.
    // The inflight flag prevents overlap when a check outlasts its interval.
    {
        let hosts = hosts.clone();
        let shutdown = shutdown.clone();
        handles.push(thread::spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                let due: Vec<CheckJob> = {
                    let mut list = hosts.write().unwrap();
                    let mut due = Vec::new();
                    let now = Instant::now();
                    for h in list.iter_mut() {
                        // Muted hosts are skipped entirely, not probed.
                        // Expiry is evaluated here so checks resume on their own.
                        let muted = h
                            .muted_until
                            .map_or(false, |until| until > crate::config::now_epoch());
                        if muted || h.inflight || h.next_ping > now {
                            continue;
                        }
                        h.inflight = true;
                        due.push(CheckJob {
                            name: h.name.clone(),
                            port: h.port,
                            check_cmd: h.check_cmd.clone(),
                            timeout_ms,
                            interval_secs: h.interval_secs,
                        });
                    }
                    due
                };
                for job in due {
                    if job_tx.send(job).is_err() {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }
        }));
    }

    handles
}

fn version_parts(v: &str) -> Vec<u32> {
    v.split('.')
        .filter_map(|p| p.parse::<u32>().ok())
        .collect()
}

fn is_newer_version(current: &str, latest: &str) -> bool {
    let cur = version_parts(current);
    let lat = version_parts(latest);
    for i in 0..cur.len().max(lat.len()) {
        let c = cur.get(i).copied().unwrap_or(0);
        let l = lat.get(i).copied().unwrap_or(0);
        if l > c { return true; }
        if l < c { return false; }
    }
    false
}

/// Check GitHub releases in the background and notify the UI if a newer version exists.
fn spawn_update_checker(tx: mpsc::Sender<Message>, current_version: String, shutdown: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Wait a few seconds so the UI starts immediately.
        thread::sleep(Duration::from_secs(3));
        let mut last_notified: Option<String> = None;
        while !shutdown.load(Ordering::Relaxed) {
            if let Some(latest) = fetch_latest_release_version() {
                if is_newer_version(&current_version, &latest) && last_notified.as_deref() != Some(&latest) {
                    last_notified = Some(latest.clone());
                    let _ = tx.send(Message::UpdateAvailable { version: latest });
                }
            }
            // Re-check every 15 minutes (4 API calls/hour, far below GitHub's
            // 60/hour unauthenticated limit) so the ↑ badge appears promptly.
            // Sleep in short chunks so shutdown stays responsive.
            for _ in 0..90 {
                if shutdown.load(Ordering::Relaxed) { break; }
                thread::sleep(Duration::from_secs(10));
            }
        }
    })
}

fn fetch_latest_release_version() -> Option<String> {
    let url = "https://api.github.com/repos/altosaxplayer/ping-uin/releases/latest";
    let response = ureq::get(url)
        .set("User-Agent", "ping-uin-update-check")
        .timeout(Duration::from_secs(10))
        .call();
    if let Ok(response) = response {
        if let Ok(body) = response.into_string() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(tag) = value.get("tag_name").and_then(|v| v.as_str()) {
                    return Some(tag.trim_start_matches('v').to_string());
                }
            }
        }
    }
    None
}

#[derive(Clone, Debug)]
struct ReleaseAsset {
    url: String,
    sha256: Option<String>,
}

fn dir_writable(dir: &std::path::Path) -> bool {
    // Probe with a temp file so we fail fast with a clear message instead of
    // downloading first and failing on replace.
    let probe = dir.join(".ping-uin-write-test");
    match fs::write(&probe, b"ok") {
        Ok(_) => { let _ = fs::remove_file(&probe); true }
        Err(_) => false,
    }
}

fn parse_sha256_text(text: &str, asset_name: &str) -> Option<String> {
    // Handles both `shasum` output ("<hash>  <file>") and bare-hash files
    // (Windows .sha256 sidecars currently contain only the hash).
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if line.contains(asset_name) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if !parts.is_empty() {
                let hash: String = parts[0].chars().filter(|c| c.is_ascii_hexdigit()).collect();
                if hash.len() == 64 {
                    return Some(hash.to_uppercase());
                }
            }
        } else if line.len() == 64 && line.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(line.to_uppercase());
        }
    }
    None
}

fn release_asset_info(expected_version: &str) -> Option<ReleaseAsset> {
    let url = "https://api.github.com/repos/altosaxplayer/ping-uin/releases/latest";
    let response = ureq::get(url)
        .set("User-Agent", "ping-uin-update")
        .timeout(Duration::from_secs(15))
        .call()
        .ok()?;
    let body = response.into_string().ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = value.get("tag_name")?.as_str()?;
    let latest_version = tag.trim_start_matches('v').to_string();
    // Tolerate skew: if a newer release landed between check and install,
    // install latest rather than failing on strict equality.
    if latest_version != expected_version
        && !is_newer_version(expected_version, &latest_version)
        && !is_newer_version(env!("CARGO_PKG_VERSION"), &latest_version)
    {
        return None;
    }

    let os = env::consts::OS;
    let asset_name = match os {
        "windows" => "ping-uin-windows-x86_64.zip".to_string(),
        "macos" => format!("ping-uin-macos-{}.tar.gz", env::consts::ARCH),
        _ => "ping-uin-linux-x86_64.tar.gz".to_string(),
    };

    let assets = value.get("assets")?.as_array()?;
    let asset = assets.iter().find(|a| {
        a.get("name").and_then(|n| n.as_str()) == Some(asset_name.as_str())
    })?;
    let url = asset.get("browser_download_url")?.as_str()?.to_string();

    // Prefer the dedicated `<asset>.sha256` sidecar uploaded by release.yml;
    // fall back to parsing the release notes body.
    let mut sha256: Option<String> = None;
    let sidecar_name = format!("{}.sha256", asset_name);
    if let Some(sidecar) = assets.iter().find(|a| {
        a.get("name").and_then(|n| n.as_str()) == Some(sidecar_name.as_str())
    }) {
        if let Some(sidecar_url) = sidecar.get("browser_download_url").and_then(|u| u.as_str()) {
            if let Ok(resp) = ureq::get(sidecar_url)
                .set("User-Agent", "ping-uin-update")
                .timeout(Duration::from_secs(15))
                .call()
            {
                if let Ok(text) = resp.into_string() {
                    sha256 = parse_sha256_text(&text, &asset_name);
                }
            }
        }
    }
    if sha256.is_none() {
        sha256 = value.get("body").and_then(|b| b.as_str()).and_then(|body| {
            parse_sha256_text(body, &asset_name)
        });
    }

    Some(ReleaseAsset { url, sha256 })
}

fn download_file(url: &str, dest: &std::path::Path) -> io::Result<()> {
    let mut file = fs::File::create(dest)?;
    let response = ureq::get(url)
        .set("User-Agent", "ping-uin-update")
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("download failed: {}", e)))?;
    let mut reader = response.into_reader();
    io::copy(&mut reader, &mut file)?;
    Ok(())
}

fn sha256_file(path: &std::path::Path) -> io::Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()).to_uppercase())
}

#[cfg(target_os = "windows")]
fn extract_windows_zip(zip_path: &std::path::Path, dest_dir: &std::path::Path) -> io::Result<PathBuf> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut zip_file = archive.by_index(i)?;
        let name = zip_file.name();
        if name.ends_with("ping-uin.exe") {
            let out_path = dest_dir.join("ping-uin.exe");
            let mut out_file = fs::File::create(&out_path)?;
            io::copy(&mut zip_file, &mut out_file)?;
            return Ok(out_path);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "ping-uin.exe not found in archive"))
}

#[cfg(not(target_os = "windows"))]
fn extract_unix_tar(tar_path: &std::path::Path, dest_dir: &std::path::Path) -> io::Result<PathBuf> {
    let file = fs::File::open(tar_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.file_name().map_or(false, |n| n == "ping-uin") {
            let out_path = dest_dir.join("ping-uin");
            entry.unpack(&out_path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&out_path)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&out_path, perms)?;
            }
            return Ok(out_path);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "ping-uin not found in archive"))
}

/// One-shot latest-version check (manual `u` press). Reports back via Message.
fn spawn_one_shot_update_check(tx: mpsc::Sender<Message>, current_version: String) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let _ = tx.send(Message::UpdateState(UpdateState::Checking));
        match fetch_latest_release_version() {
            Some(latest) if is_newer_version(&current_version, &latest) => {
                let _ = tx.send(Message::UpdateAvailable { version: latest.clone() });
                let _ = tx.send(Message::UpdateState(UpdateState::Info(format!("v{} available — press u again to install", latest))));
            }
            Some(_) => {
                let _ = tx.send(Message::UpdateState(UpdateState::Info(format!("already on latest (v{})", current_version))));
            }
            None => {
                let _ = tx.send(Message::UpdateState(UpdateState::Error("update check failed: no network or API error".to_string())));
            }
        }
    })
}

/// Homebrew update for installs managed by brew. Runs in a background thread.
fn spawn_homebrew_updater(tx: mpsc::Sender<Message>, version: String) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        fn notify(tx: &mpsc::Sender<Message>, state: UpdateState) {
            let _ = tx.send(Message::UpdateState(state));
        }

        notify(&tx, UpdateState::Checking);
        // Try short name first, then fully-qualified tap name.
        let attempts: Vec<Vec<&str>> = vec![
            vec!["upgrade", "ping-uin"],
            vec!["upgrade", "altosaxplayer/tap/ping-uin"],
        ];
        let mut last_err = String::new();
        for args in attempts {
            match Command::new("brew").args(&args).output() {
                Ok(out) if out.status.success() => {
                    notify(&tx, UpdateState::Done { version: version.clone(), restart_required: true });
                    return;
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    last_err = format!("brew {} failed:\n{}{}", args.join(" "), stdout, stderr);
                }
                Err(e) => {
                    last_err = format!("brew {} failed: {}", args.join(" "), e);
                    break;
                }
            }
        }
        notify(&tx, UpdateState::Error(last_err));
    })
}

/// Winget update for Windows installs managed by winget. Runs in a background thread.
/// Reports Done with restart_required=false: the files are already replaced,
/// so the user just quits at their convenience (no forced restart).
fn spawn_winget_updater(tx: mpsc::Sender<Message>, version: String) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        fn notify(tx: &mpsc::Sender<Message>, state: UpdateState) {
            let _ = tx.send(Message::UpdateState(state));
        }

        notify(&tx, UpdateState::Checking);
        match Command::new("winget")
            .args([
                "upgrade", "--exact", "--id", "altosaxplayer.ping-uin",
                "--silent", "--accept-package-agreements", "--accept-source-agreements",
            ])
            .output()
        {
            Ok(out) if out.status.success() => {
                notify(&tx, UpdateState::Done { version, restart_required: false });
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let stdout = String::from_utf8_lossy(&out.stdout);
                notify(&tx, UpdateState::Error(format!("winget upgrade failed:\n{}{}", stdout, stderr)));
            }
            Err(e) => {
                notify(&tx, UpdateState::Error(format!("winget upgrade failed: {}", e)));
            }
        }
    })
}

fn winget_available() -> bool {
    Command::new("winget")
        .args(["--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// In-place update for portable installs. Runs in a background thread.
fn spawn_updater(tx: mpsc::Sender<Message>, version: String) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        fn notify(tx: &mpsc::Sender<Message>, state: UpdateState) {
            let _ = tx.send(Message::UpdateState(state));
        }

        let exe_path = match env::current_exe() {
            Ok(p) => p,
            Err(e) => { notify(&tx, UpdateState::Error(format!("cannot find executable: {}", e))); return; }
        };
        let exe_dir = match exe_path.parent() {
            Some(d) => d.to_path_buf(),
            None => { notify(&tx, UpdateState::Error("cannot find executable directory".to_string())); return; }
        };

        if !dir_writable(&exe_dir) {
            notify(&tx, UpdateState::Error("install dir not writable — use brew upgrade / cargo install, or run with write permission".to_string()));
            return;
        }

        notify(&tx, UpdateState::Checking);
        let asset = match release_asset_info(&version) {
            Some(a) => a,
            None => { notify(&tx, UpdateState::Error("could not find release asset for this OS/arch".to_string())); return; }
        };

        notify(&tx, UpdateState::Downloading { version: version.clone() });
        let temp_dir = match std::env::temp_dir().join(format!("ping-uin-update-{}", version)) {
            d => { let _ = fs::create_dir_all(&d); d }
        };
        let archive_name = asset.url.rsplit('/').next().unwrap_or("archive");
        let archive_path = temp_dir.join(archive_name);
        if let Err(e) = download_file(&asset.url, &archive_path) {
            notify(&tx, UpdateState::Error(format!("download failed: {}", e))); return;
        }

        // Verify checksum if available.
        if let Some(expected) = asset.sha256 {
            match sha256_file(&archive_path) {
                Ok(actual) if actual != expected => {
                    notify(&tx, UpdateState::Error("checksum mismatch".to_string())); return;
                }
                Err(e) => { notify(&tx, UpdateState::Error(format!("checksum error: {}", e))); return; }
                _ => {}
            }
        }

        notify(&tx, UpdateState::Replacing { version: version.clone() });

        #[cfg(target_os = "windows")]
        {
            let new_exe = match extract_windows_zip(&archive_path, &temp_dir) {
                Ok(p) => p,
                Err(e) => { notify(&tx, UpdateState::Error(format!("extract failed: {}", e))); return; }
            };
            let updater_script = exe_dir.join("ping-uin-update.ps1");
            let script = format!(
                "$parentPid = (Get-CimInstance Win32_Process -Filter \"ProcessId=$PID\").ParentProcessId\n\
                $parent = Get-Process -Id $parentPid -ErrorAction SilentlyContinue\n\
                while ($parent -and -not $parent.HasExited) {{ Start-Sleep -Milliseconds 200 }}\n\
                $old = \"{old}\"\n\
                $new = \"{new}\"\n\
                $dest = \"{dest}\"\n\
                try {{\n\
                    if (Test-Path $dest) {{\n\
                        Rename-Item -Path $dest -NewName \"$dest.old\" -Force\n\
                    }}\n\
                    Move-Item -Path $new -Destination $dest -Force\n\
                    Remove-Item -Path \"$dest.old\" -Force -ErrorAction SilentlyContinue\n\
                    Remove-Item -Path \"{temp}\" -Recurse -Force -ErrorAction SilentlyContinue\n\
                    Start-Process -FilePath $dest -WorkingDirectory (Split-Path -Parent $dest)\n\
                }} catch {{\n\
                    if (Test-Path \"$dest.old\") {{\n\
                        Move-Item -Path \"$dest.old\" -Destination $dest -Force -ErrorAction SilentlyContinue\n\
                    }}\n\
                }}\n\
                Remove-Item -Path $PSCommandPath -Force -ErrorAction SilentlyContinue\n",
                old = exe_path.display(),
                new = new_exe.display(),
                dest = exe_path.display(),
                temp = temp_dir.display(),
            );
            if let Err(e) = fs::write(&updater_script, script) {
                notify(&tx, UpdateState::Error(format!("updater script failed: {}", e))); return;
            }
            let _ = Command::new("powershell")
                .args(["-WindowStyle", "Hidden", "-ExecutionPolicy", "Bypass", "-File", &updater_script.to_string_lossy()])
                .spawn();
            notify(&tx, UpdateState::Done { version, restart_required: true });
        }

        #[cfg(not(target_os = "windows"))]
        {
            let new_exe = match extract_unix_tar(&archive_path, &temp_dir) {
                Ok(p) => p,
                Err(e) => { notify(&tx, UpdateState::Error(format!("extract failed: {}", e))); return; }
            };
            // Keep the original file name (`ping-uin` has no extension, so
            // with_extension() would mangle it). Backup is `<exe>.old`.
            let backup = PathBuf::from(format!("{}.old", exe_path.display()));
            if let Err(e) = fs::rename(&exe_path, &backup) {
                notify(&tx, UpdateState::Error(format!("backup failed (check permissions): {}", e))); return;
            }
            if let Err(e) = fs::rename(&new_exe, &exe_path) {
                let _ = fs::rename(&backup, &exe_path);
                notify(&tx, UpdateState::Error(format!("replace failed, restored backup: {}", e))); return;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755));
            }
            let _ = fs::remove_file(&backup);
            let _ = fs::remove_dir_all(&temp_dir);
            notify(&tx, UpdateState::Done { version, restart_required: true });
        }
    })
}

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    shared_hosts: Arc<RwLock<Vec<HostSchedule>>>,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
    let tick_rate = Duration::from_millis(50);

    loop {
        terminal.draw(|f| ui(f, app))?;
        if event::poll(tick_rate)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // Close update/info popup with Esc regardless of input mode.
                    if !matches!(app.update_state, UpdateState::Idle) && key.code == KeyCode::Esc {
                        app.update_state = UpdateState::Idle;
                        continue;
                    }
                    match app.input_mode {
                        InputMode::Normal => match key.code {
                            KeyCode::Char('q') | KeyCode::Char('Q') => { shutdown.store(true, Ordering::Relaxed); return Ok(()); }
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => { shutdown.store(true, Ordering::Relaxed); return Ok(()); }
                            KeyCode::Char(' ') => { app.ping_selected_now(&shared_hosts); }
                            KeyCode::Char('p') | KeyCode::Char('P') => { app.ping_selected_now(&shared_hosts); }
                            KeyCode::Char('a') | KeyCode::Char('A') => {
                                app.input_mode = InputMode::AddHost(AddHostForm::default());
                            }
                            KeyCode::Char('d') | KeyCode::Char('D') => {
                                if app.hosts.is_empty() {
                                    // First-run wizard: seed the demo set.
                                    let demo = Config::default();
                                    for h in &demo.hosts {
                                        app.add_host(
                                            h.name.clone(),
                                            h.effective_interval_secs(),
                                            h.group.clone(),
                                            h.alias.clone().unwrap_or_default(),
                                            h.port,
                                            &shared_hosts,
                                        );
                                    }
                                    app.wizard_dismissed = true;
                                } else {
                                    app.input_mode = InputMode::ConfirmDelete;
                                }
                            }
                            KeyCode::Char('c') | KeyCode::Char('C') => {
                                app.clear_selected_stats();
                                app.update_state = UpdateState::Info("stats cleared for selected host".to_string());
                            }
                            KeyCode::Char('e') => {
                                if let Some(h) = app.hosts.get(app.selected_idx) {
                                    app.input_mode = InputMode::EditEntry {
                                        original: h.name.clone(),
                                        form: AddHostForm::for_host(h),
                                    };
                                }
                            }
                            KeyCode::Char('h') | KeyCode::Char('H') => {
                                if app.selected_idx < app.hosts.len() {
                                    app.input_mode = InputMode::HistoryView { host_idx: app.selected_idx, range: HistoryRange::Hours24, compare_idx: None };
                                }
                            }
                            KeyCode::Char('i') | KeyCode::Char('I') => {
                                let default_path = paths().csv.to_string_lossy().to_string();
                                app.input_mode = InputMode::ImportPath { path: default_path };
                            }
                            KeyCode::Char('E') => {
                                let default_dir = dirs::home_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| ".".to_string());
                                app.input_mode = InputMode::ExportPath { path: default_dir };
                            }
                            KeyCode::Char('u') | KeyCode::Char('U') => {
                                // No known update: manual check first. Known update: install it.
                                if let Some(ref version) = app.update_available.clone() {
                                    if matches!(app.update_state, UpdateState::Idle | UpdateState::Error(_) | UpdateState::Info(_) | UpdateState::Done { .. }) {
                                        let version = version.clone();
                                        app.update_state = UpdateState::Checking;
                                        if portable_dir().is_some() {
                                            spawn_updater(tx.clone(), version);
                                        } else if let Ok(exe) = env::current_exe() {
                                            if is_homebrew_install(&exe) {
                                                spawn_homebrew_updater(tx.clone(), version);
                                            } else if dir_writable(&exe.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))) {
                                                // Generic fallback: binary dir is writable, do in-place replace.
                                                spawn_updater(tx.clone(), version);
                                            } else if cfg!(target_os = "windows") && winget_available() {
                                                // Winget-managed install: let winget swap the files in place.
                                                spawn_winget_updater(tx.clone(), version);
                                            } else {
                                                app.update_state = UpdateState::Error("auto-update needs portable mode, Homebrew, or winget.\nUpdate with: brew upgrade ping-uin  /  winget upgrade altosaxplayer.ping-uin  /  cargo install --path .".to_string());
                                            }
                                        } else {
                                            app.update_state = UpdateState::Error("cannot determine install type".to_string());
                                        }
                                    }
                                } else if matches!(app.update_state, UpdateState::Idle | UpdateState::Error(_) | UpdateState::Info(_)) {
                                    spawn_one_shot_update_check(tx.clone(), env!("CARGO_PKG_VERSION").to_string());
                                }
                            }
                            KeyCode::Char('g') => { app.group_by = !app.group_by; app.save_prefs(); }
                            KeyCode::Char('f') | KeyCode::Char('F') => {
                                let mut groups: Vec<String> = app.hosts.iter()
                                    .map(|h| if h.group.is_empty() { "default".to_string() } else { h.group.clone() })
                                    .collect::<std::collections::BTreeSet<_>>()
                                    .into_iter()
                                    .collect();
                                groups.sort();
                                let selected = app.group_filter.as_ref()
                                    .and_then(|f| groups.iter().position(|g| g == f))
                                    .unwrap_or(0);
                                app.input_mode = InputMode::GroupFilterPicker { groups, selected };
                            }
                            KeyCode::Char('s') | KeyCode::Char('S') => {
                                app.input_mode = InputMode::SortPicker { selected: app.sort_mode.index() };
                            }
                            KeyCode::Char('t') | KeyCode::Char('T') => {
                                app.input_mode = InputMode::ThemePicker { original: app.theme_idx, selected: app.theme_idx };
                            }
                            KeyCode::Char('m') | KeyCode::Char('M') => {
                                app.input_mode = InputMode::MenuModal;
                            }
                            KeyCode::Char('?') => {
                                app.input_mode = InputMode::KeysHelp;
                            }
                            KeyCode::Char('/') => {
                                let q = app.search.clone().unwrap_or_default();
                                app.input_mode = InputMode::Search { query: q };
                            }
                            // Enter collapses/expands the selected host's group.
                            KeyCode::Enter => {
                                app.toggle_collapse_selected();
                            }
                            // ! mutes/unmutes the selected host for 1h.
                            KeyCode::Char('!') => {
                                app.toggle_mute_selected(&shared_hosts);
                            }
                            // v toggles compact table density.
                            KeyCode::Char('v') | KeyCode::Char('V') => {
                                app.compact = !app.compact;
                                app.save_prefs();
                            }
                            // Get out of any sort/filter: Esc restores the full host list.
                            KeyCode::Esc => {
                                if app.hosts.is_empty() {
                                    app.wizard_dismissed = true;
                                }
                                if app.sort_mode != SortMode::None || app.group_filter.is_some() {
                                    app.sort_mode = SortMode::None;
                                    app.group_filter = None;
                                    app.save_prefs();
                                    app.update_state = UpdateState::Info("view reset — showing all hosts".to_string());
                                }
                            }
                            KeyCode::Up => move_selection_up(app),
                            KeyCode::Down => move_selection_down(app),
                            _ => {}
                        },
                        InputMode::AddHost(ref form0) => {
                            let mut form = form0.clone();
                            match key.code {
                                KeyCode::Esc => { app.input_mode = InputMode::Normal; }
                                KeyCode::Tab | KeyCode::Down => { form.focus = (form.focus + 1) % AddHostForm::FIELDS; app.input_mode = InputMode::AddHost(form); }
                                KeyCode::BackTab | KeyCode::Up => { form.focus = (form.focus + AddHostForm::FIELDS - 1) % AddHostForm::FIELDS; app.input_mode = InputMode::AddHost(form); }
                                KeyCode::Enter => {
                                    let host = form.host.trim().to_string();
                                    if host.is_empty() {
                                        app.update_state = UpdateState::Info("host/IP is required".to_string());
                                        app.input_mode = InputMode::AddHost(form);
                                        continue;
                                    }
                                    if app.config.hosts.iter().any(|h| h.name == host) {
                                        app.update_state = UpdateState::Info("host already exists".to_string());
                                        app.input_mode = InputMode::AddHost(form);
                                        continue;
                                    }
                                    let interval_secs = parse_interval(&form.interval).unwrap_or(DEFAULT_INTERVAL_SECS);
                                    if interval_secs < config::MIN_INTERVAL_SECS {
                                        app.update_state = UpdateState::Info(format!("minimum interval is {}s", config::MIN_INTERVAL_SECS));
                                        app.input_mode = InputMode::AddHost(form);
                                        continue;
                                    }
                                    let group = form.group.trim().to_string();
                                    let alias = form.alias.trim().to_string();
                                    let port = form.port.trim().parse::<u16>().ok().filter(|p| *p > 0);
                                    if !form.port.trim().is_empty() && port.is_none() {
                                        app.update_state = UpdateState::Info("port must be 1-65535 (blank = ping)".to_string());
                                        app.input_mode = InputMode::AddHost(form);
                                        continue;
                                    }
                                    app.input_mode = InputMode::Normal;
                                    app.add_host(host, interval_secs, group, alias, port, &shared_hosts);
                                }
                                KeyCode::Backspace => {
                                    match form.focus {
                                        0 => { form.host.pop(); }
                                        1 => { form.interval.pop(); }
                                        2 => { form.group.pop(); }
                                        3 => { form.alias.pop(); }
                                        _ => { form.port.pop(); }
                                    }
                                    app.input_mode = InputMode::AddHost(form);
                                }
                                KeyCode::Char(c) => {
                                    match form.focus {
                                        0 => form.host.push(c),
                                        1 => form.interval.push(c),
                                        2 => form.group.push(c),
                                        3 => form.alias.push(c),
                                        _ => form.port.push(c),
                                    }
                                    app.input_mode = InputMode::AddHost(form);
                                }
                                _ => {}
                            }
                        }
                        InputMode::SortPicker { selected } => match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => { app.input_mode = InputMode::Normal; }
                            KeyCode::Up => {
                                let s = if selected == 0 { SortMode::ALL.len() - 1 } else { selected - 1 };
                                app.input_mode = InputMode::SortPicker { selected: s };
                            }
                            KeyCode::Down => {
                                let s = (selected + 1) % SortMode::ALL.len();
                                app.input_mode = InputMode::SortPicker { selected: s };
                            }
                            KeyCode::Enter => {
                                app.sort_mode = SortMode::from_index(selected);
                                app.save_prefs();
                                app.input_mode = InputMode::Normal;
                            }
                            // Space = show all: clears back to the unfiltered list.
                            KeyCode::Char(' ') => {
                                app.sort_mode = SortMode::None;
                                app.save_prefs();
                                app.input_mode = InputMode::Normal;
                            }
                            KeyCode::Char(c) if c.is_ascii_digit() => {
                                let idx = (c as usize) - ('1' as usize);
                                if idx < SortMode::ALL.len() {
                                    app.sort_mode = SortMode::from_index(idx);
                                    app.save_prefs();
                                    app.input_mode = InputMode::Normal;
                                }
                            }
                            _ => {}
                        },
                        InputMode::GroupFilterPicker { ref groups, selected } => {
                            let groups = groups.clone();
                            match key.code {
                                // Esc cancels without touching the active filter.
                                // Space clears it (hint in footer/popup).
                                KeyCode::Esc => {
                                    app.input_mode = InputMode::Normal;
                                }
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                    app.input_mode = InputMode::Normal;
                                }
                                KeyCode::Up => {
                                    let s = if selected == 0 { groups.len().saturating_sub(1) } else { selected - 1 };
                                    app.input_mode = InputMode::GroupFilterPicker { groups, selected: s };
                                }
                                KeyCode::Down => {
                                    let s = if groups.is_empty() { 0 } else { (selected + 1) % groups.len() };
                                    app.input_mode = InputMode::GroupFilterPicker { groups, selected: s };
                                }
                                KeyCode::Enter => {
                                    if let Some(group) = groups.get(selected) {
                                        app.group_filter = Some(group.clone());
                                    }
                                    app.input_mode = InputMode::Normal;
                                }
                                KeyCode::Char(' ') => {
                                    app.group_filter = None;
                                    app.input_mode = InputMode::Normal;
                                }
                                _ => {}
                            }
                        }
                        InputMode::ImportPath { ref path } => {
                            let mut path = path.clone();
                            match key.code {
                                KeyCode::Esc => { app.input_mode = InputMode::Normal; }
                                KeyCode::Enter => {
                                    app.input_mode = InputMode::Normal;
                                    if !path.trim().is_empty() {
                                        app.import_entries(std::path::Path::new(&path), &shared_hosts);
                                    }
                                }
                                KeyCode::Backspace => { path.pop(); app.input_mode = InputMode::ImportPath { path }; }
                                KeyCode::Char(c) => { path.push(c); app.input_mode = InputMode::ImportPath { path }; }
                                _ => {}
                            }
                        }
                        InputMode::ExportPath { ref path } => {
                            let mut path = path.clone();
                            match key.code {
                                KeyCode::Esc => { app.input_mode = InputMode::Normal; }
                                KeyCode::Enter => {
                                    app.input_mode = InputMode::Normal;
                                    if !path.trim().is_empty() {
                                        let dir = std::path::Path::new(&path).to_path_buf();
                                        let csv = app.export_entries(&dir);
                                        let html = app.export_status_page(&dir);
                                        match (csv, html) {
                                            (Ok(c), Ok(h)) => app.update_state = UpdateState::Info(format!(
                                                "exported\n{}\n{}",
                                                c.display(),
                                                h.display()
                                            )),
                                            (Err(e), _) | (_, Err(e)) => app.update_state = UpdateState::Error(format!("export failed: {}", e)),
                                        }
                                    }
                                }
                                KeyCode::Backspace => { path.pop(); app.input_mode = InputMode::ExportPath { path }; }
                                KeyCode::Char(c) => { path.push(c); app.input_mode = InputMode::ExportPath { path }; }
                                _ => {}
                            }
                        }
                        InputMode::HistoryView { host_idx, range, compare_idx } => {
                            match key.code {
                                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Char('h') | KeyCode::Char('H') => {
                                    app.input_mode = InputMode::Normal;
                                }
                                KeyCode::Left => {
                                    let idx = HistoryRange::ALL.iter().position(|&r| r == range).unwrap_or(1);
                                    let new_idx = if idx == 0 { HistoryRange::ALL.len() - 1 } else { idx - 1 };
                                    app.input_mode = InputMode::HistoryView { host_idx, range: HistoryRange::ALL[new_idx], compare_idx };
                                }
                                KeyCode::Right => {
                                    let idx = HistoryRange::ALL.iter().position(|&r| r == range).unwrap_or(1);
                                    let new_idx = (idx + 1) % HistoryRange::ALL.len();
                                    app.input_mode = InputMode::HistoryView { host_idx, range: HistoryRange::ALL[new_idx], compare_idx };
                                }
                                // Tab cycles a side-by-side compare target; a full
                                // cycle back to the host itself turns it off.
                                KeyCode::Tab => {
                                    let n = app.hosts.len();
                                    let next = match compare_idx {
                                        None if n > 1 => Some((host_idx + 1) % n),
                                        Some(c) => {
                                            let nx = (c + 1) % n.max(1);
                                            if nx == host_idx || n < 2 { None } else { Some(nx) }
                                        }
                                        _ => None,
                                    };
                                    app.input_mode = InputMode::HistoryView { host_idx, range, compare_idx: next };
                                }
                                _ => {}
                            }
                        }
                        InputMode::ThemePicker { original, selected } => {
                            match key.code {
                                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Char('t') | KeyCode::Char('T') => {
                                    app.theme_idx = original;
                                    app.input_mode = InputMode::Normal;
                                }
                                KeyCode::Up => {
                                    let s = if selected == 0 { app.themes.len() - 1 } else { selected - 1 };
                                    app.theme_idx = s;
                                    app.input_mode = InputMode::ThemePicker { original, selected: s };
                                }
                                KeyCode::Down => {
                                    let s = (selected + 1) % app.themes.len();
                                    app.theme_idx = s;
                                    app.input_mode = InputMode::ThemePicker { original, selected: s };
                                }
                                KeyCode::Enter => {
                                    app.save_prefs();
                                    app.input_mode = InputMode::Normal;
                                }
                                _ => {}
                            }
                        }
                        InputMode::MenuModal => {
                            match key.code {
                                KeyCode::Esc | KeyCode::Char('m') | KeyCode::Char('M') => {
                                    app.input_mode = InputMode::Normal;
                                }
                                // Make the "more" menu actionable, not view-only.
                                KeyCode::Char(' ') | KeyCode::Char('p') | KeyCode::Char('P') => {
                                    app.input_mode = InputMode::Normal;
                                    app.ping_selected_now(&shared_hosts);
                                }
                                KeyCode::Char('a') | KeyCode::Char('A') => {
                                    app.input_mode = InputMode::AddHost(AddHostForm::default());
                                }
                                KeyCode::Char('d') | KeyCode::Char('D') => {
                                    app.input_mode = if app.hosts.is_empty() { InputMode::Normal } else { InputMode::ConfirmDelete };
                                }
                                KeyCode::Char('c') | KeyCode::Char('C') => {
                                    app.input_mode = InputMode::Normal;
                                    app.clear_selected_stats();
                                }
                                KeyCode::Char('e') => {
                                    if let Some(h) = app.hosts.get(app.selected_idx) {
                                        app.input_mode = InputMode::EditEntry {
                                            original: h.name.clone(),
                                            form: AddHostForm::for_host(h),
                                        };
                                    } else {
                                        app.input_mode = InputMode::Normal;
                                    }
                                }
                                KeyCode::Char('h') | KeyCode::Char('H') => {
                                    app.input_mode = if app.selected_idx < app.hosts.len() {
                                        InputMode::HistoryView { host_idx: app.selected_idx, range: HistoryRange::Hours24, compare_idx: None }
                                    } else {
                                        InputMode::Normal
                                    };
                                }
                                KeyCode::Char('s') | KeyCode::Char('S') => {
                                    app.input_mode = InputMode::SortPicker { selected: app.sort_mode.index() };
                                }
                                KeyCode::Char('/') => {
                                    let q = app.search.clone().unwrap_or_default();
                                    app.input_mode = InputMode::Search { query: q };
                                }
                                KeyCode::Char('?') => {
                                    app.input_mode = InputMode::KeysHelp;
                                }
                                KeyCode::Enter => {
                                    app.input_mode = InputMode::Normal;
                                    app.toggle_collapse_selected();
                                }
                                KeyCode::Char('!') => {
                                    app.input_mode = InputMode::Normal;
                                    app.toggle_mute_selected(&shared_hosts);
                                }
                                KeyCode::Char('v') | KeyCode::Char('V') => {
                                    app.input_mode = InputMode::Normal;
                                    app.compact = !app.compact;
                                    app.save_prefs();
                                }
                                KeyCode::Char('t') | KeyCode::Char('T') => {
                                    app.input_mode = InputMode::ThemePicker { original: app.theme_idx, selected: app.theme_idx };
                                }
                                KeyCode::Char('u') | KeyCode::Char('U') => {
                                    app.input_mode = InputMode::Normal;
                                    if app.update_available.is_some() {
                                        app.update_state = UpdateState::Info("press u in the main view to install".to_string());
                                    } else {
                                        spawn_one_shot_update_check(tx.clone(), env!("CARGO_PKG_VERSION").to_string());
                                    }
                                }
                                KeyCode::Char('g') => {
                                    app.input_mode = InputMode::Normal;
                                    app.group_by = !app.group_by;
                                    app.save_prefs();
                                }
                                KeyCode::Char('q') | KeyCode::Char('Q') => {
                                    shutdown.store(true, Ordering::Relaxed);
                                    return Ok(());
                                }
                                _ => {}
                            }
                        }
                        InputMode::KeysHelp => match key.code {
                            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Char('Q') => {
                                app.input_mode = InputMode::Normal;
                            }
                            _ => {}
                        },
                        InputMode::Search { ref query } => {
                            let mut query = query.clone();
                            match key.code {
                                // Esc clears the filter entirely; Enter keeps it.
                                KeyCode::Esc => {
                                    query.clear();
                                    app.search = None;
                                    app.input_mode = InputMode::Normal;
                                }
                                KeyCode::Enter => {
                                    app.input_mode = InputMode::Normal;
                                }
                                KeyCode::Backspace => {
                                    query.pop();
                                    app.search = if query.is_empty() { None } else { Some(query.clone()) };
                                    app.input_mode = InputMode::Search { query };
                                }
                                KeyCode::Char(c) => {
                                    query.push(c);
                                    app.search = Some(query.clone());
                                    app.input_mode = InputMode::Search { query };
                                }
                                _ => {}
                            }
                        }
                        InputMode::EditEntry { ref original, ref form } => {
                            let original = original.clone();
                            let mut form = form.clone();
                            match key.code {
                                KeyCode::Esc => { app.input_mode = InputMode::Normal; }
                                KeyCode::Tab | KeyCode::Down => { form.focus = (form.focus + 1) % AddHostForm::FIELDS; app.input_mode = InputMode::EditEntry { original, form }; }
                                KeyCode::BackTab | KeyCode::Up => { form.focus = (form.focus + AddHostForm::FIELDS - 1) % AddHostForm::FIELDS; app.input_mode = InputMode::EditEntry { original, form }; }
                                KeyCode::Enter => {
                                    if form.host.trim().is_empty() {
                                        app.update_state = UpdateState::Info("host/IP is required".to_string());
                                        app.input_mode = InputMode::EditEntry { original, form };
                                        continue;
                                    }
                                    let new_name = form.host.trim().to_string();
                                    if new_name != original && app.config.hosts.iter().any(|h| h.name == new_name) {
                                        app.update_state = UpdateState::Info("another host already uses that name".to_string());
                                        app.input_mode = InputMode::EditEntry { original, form };
                                        continue;
                                    }
                                    if parse_interval(&form.interval).map_or(true, |s| s < config::MIN_INTERVAL_SECS) {
                                        app.update_state = UpdateState::Info(format!("minimum interval is {}s", config::MIN_INTERVAL_SECS));
                                        app.input_mode = InputMode::EditEntry { original, form };
                                        continue;
                                    }
                                    if !form.port.trim().is_empty() && form.port.trim().parse::<u16>().ok().filter(|p| *p > 0).is_none() {
                                        app.update_state = UpdateState::Info("port must be 1-65535 (blank = ping)".to_string());
                                        app.input_mode = InputMode::EditEntry { original, form };
                                        continue;
                                    }
                                    let form2 = form.clone();
                                    app.input_mode = InputMode::Normal;
                                    app.edit_entry(original, form2, &shared_hosts);
                                }
                                KeyCode::Backspace => {
                                    match form.focus {
                                        0 => { form.host.pop(); }
                                        1 => { form.interval.pop(); }
                                        2 => { form.group.pop(); }
                                        3 => { form.alias.pop(); }
                                        _ => { form.port.pop(); }
                                    }
                                    app.input_mode = InputMode::EditEntry { original, form };
                                }
                                KeyCode::Char(c) => {
                                    match form.focus {
                                        0 => form.host.push(c),
                                        1 => form.interval.push(c),
                                        2 => form.group.push(c),
                                        3 => form.alias.push(c),
                                        _ => form.port.push(c),
                                    }
                                    app.input_mode = InputMode::EditEntry { original, form };
                                }
                                _ => {}
                            }
                        }
                        InputMode::ConfirmDelete => match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => { app.input_mode = InputMode::Normal; app.remove_selected(&shared_hosts); }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => { app.input_mode = InputMode::Normal; }
                            _ => {}
                        },
                    }
                }
                Event::Resize(_, _) => {
                    let _ = terminal.autoresize();
                }
                Event::Mouse(m) => {
                    // Click-to-select + wheel scroll, normal view only.
                    if !matches!(app.input_mode, InputMode::Normal) {
                        continue;
                    }
                    match m.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            let rel = m.row.saturating_sub(app.table_rect.y) as usize;
                            // Skip block border + header row.
                            if rel >= 2 {
                                let (s, e) = app.table_slice;
                                let i = s + rel - 2;
                                if i < e {
                                    let target = cached_visible_rows(app)
                                        .get(i)
                                        .and_then(|r| r.host_idx);
                                    if let Some(idx) = target {
                                        app.selected_idx = idx;
                                    }
                                }
                            }
                        }
                        MouseEventKind::ScrollUp => move_selection_up(app),
                        MouseEventKind::ScrollDown => move_selection_down(app),
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        while let Ok(msg) = rx.try_recv() {
            match msg {
                Message::Result { host, up, latency_ms, timestamp, next_ping } => {
                    app.last_check = timestamp.clone();
                    app.last_result_time = Some(Instant::now());
                    // Notify on transitions only (skip the very first result per host).
                    // Suppressed (downstream-of-down-upstream) hosts stay silent.
                    let mut transition: Option<(String, String, bool, f64, String)> = None;
                    if let Some(h) = app.hosts.iter_mut().find(|h| h.name == host) {
                        let first = h.total_checks == 0;
                        let changed = !first && up != h.up;
                        h.note_result(up, Instant::now());
                        h.up = up;
                        h.latency_ms = latency_ms;
                        h.next_ping = next_ping;
                        h.total_checks += 1;
                        if up {
                            h.up_checks += 1;
                            h.down_since = None;
                            h.escalation = 0;
                        } else if h.down_since.is_none() {
                            h.down_since = Some(Instant::now());
                        }
                        let lat_u64 = if up { latency_ms.round() as u64 } else { 0 };
                        h.history.push_back(lat_u64);
                        while h.history.len() > app.config.graph_width {
                            h.history.pop_front();
                        }
                        let status = if up { "UP" } else { "DOWN" };
                        let _ = log_result(&timestamp, &h.name, status, latency_ms);
                        if changed {
                            transition = Some((h.name.clone(), h.target(), up, latency_ms, timestamp.clone()));
                        }
                    }
                    if let Some((name, target, up, latency_ms, timestamp)) = transition {
                        if !is_suppressed(&app.hosts, &name) {
                            let event = if up { "up" } else { "down" };
                            if let Some(url) = app.config.webhook_url.clone() {
                                post_webhook(url, target.clone(), up, latency_ms, timestamp, event);
                            }
                            if app.config.notify_bell && !up {
                                print!("\x07");
                                let _ = io::stdout().flush();
                            }
                        }
                    }
                }
                Message::UpdateAvailable { version } => {
                    app.update_available = Some(version);
                }
                Message::UpdateState(state) => {
                    app.update_state = state;
                    if let UpdateState::Done { restart_required: true, .. } = app.update_state {
                        app.restart_after_exit = true;
                        let _ = terminal.draw(|f| ui(f, app));
                        return Ok(());
                    }
                }
            }
        }

        // Time-based log trim (was every ~100 frames doing a full file read).
        if app.last_trim.elapsed() > Duration::from_secs(300) {
            app.last_trim = Instant::now();
            let _ = trim_log(&mut app.hosts, app.config.graph_width);
            app.history_cache.clear();
        }

        // Escalation ladder, evaluated every ~5s: still down after 5 min →
        // `still_down_5m` webhook; after 30 min → bell + `still_down_30m`.
        // Muted and suppressed (downstream-of-down) hosts stay silent.
        if app.last_esc_check.elapsed() > Duration::from_secs(5) {
            app.last_esc_check = Instant::now();
            let now_ts = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let pending: Vec<(String, String, f64, u8)> = app
                .hosts
                .iter()
                .filter(|h| !h.up && !h.muted() && h.total_checks > 0)
                .filter_map(|h| {
                    let mins = h.down_since.map(|t| t.elapsed().as_secs() / 60)?;
                    let level = if mins >= 30 { 2 } else if mins >= 5 { 1 } else { 0 };
                    if level > h.escalation && !is_suppressed(&app.hosts, &h.name) {
                        Some((h.name.clone(), h.target(), h.latency_ms, level))
                    } else {
                        None
                    }
                })
                .collect();
            for (name, target, latency_ms, level) in pending {
                if let Some(h) = app.hosts.iter_mut().find(|h| h.name == name) {
                    h.escalation = level;
                }
                let event = if level >= 2 { "still_down_30m" } else { "still_down_5m" };
                if let Some(url) = app.config.webhook_url.clone() {
                    post_webhook(url, target, false, latency_ms, now_ts.clone(), event);
                }
                if app.config.notify_bell && level >= 2 {
                    print!("\x07");
                    let _ = io::stdout().flush();
                }
            }
        }
    }
}

/// Headless single pass: check every host once, print results, exit.
/// Exit 0 = all up, 2 = any down, 1 = usage/config error. No files touched.
fn run_once(format: &str) -> io::Result<()> {
    let config = Config::load();
    if config.hosts.is_empty() {
        eprintln!("no hosts configured");
        return Ok(());
    }
    let timeout_ms = config.timeout_ms;
    let mut handles = Vec::new();
    for h in config.hosts.clone() {
        handles.push(thread::spawn(move || {
            let re = Regex::new(r"time[<=]([\d.]+)\s*ms").unwrap();
            let (up, latency_ms) = check_host(&h.name, h.port, h.check_cmd.as_deref(), timeout_ms, &re);
            (h, up, latency_ms)
        }));
    }
    let mut results = Vec::new();
    for handle in handles {
        if let Ok(r) = handle.join() {
            results.push(r);
        }
    }
    results.sort_by(|a, b| a.0.display_name().cmp(&b.0.display_name()));
    let down = results.iter().filter(|(_, up, _)| !up).count();
    if format == "json" {
        let items: Vec<serde_json::Value> = results
            .iter()
            .map(|(h, up, latency_ms)| {
                serde_json::json!({
                    "name": h.name,
                    "alias": h.alias,
                    "group": h.group,
                    "port": h.port,
                    "up": up,
                    "latency_ms": if *up { serde_json::Value::from(*latency_ms) } else { serde_json::Value::Null },
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "hosts": items, "down": down }).to_string()
        );
    } else {
        for (h, up, latency_ms) in &results {
            let status = if *up {
                format!("UP {:.0} ms", latency_ms)
            } else {
                "DOWN".to_string()
            };
            println!("{:<24} {:<16} {}", h.display_name(), h.target(), status);
        }
    }
    // Exit code doubles as the probe result for cron/systemd.
    std::process::exit(if down > 0 { 2 } else { 0 });
}

fn print_usage() {
    println!("ping-uin — btop-style TUI for monitoring hosts");
    println!();
    println!("Usage:");
    println!("  ping-uin                 run the TUI");
    println!("  ping-uin --once [--format json|text]   check once, print, exit (0=all up, 2=any down)");
    println!("  ping-uin --help          show this help");
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return Ok(());
    }
    if let Some(pos) = args.iter().position(|a| a == "--once") {
        let format = args
            .get(pos + 1)
            .and_then(|a| {
                if a == "--format" {
                    args.get(pos + 2).cloned()
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "text".to_string());
        if format != "text" && format != "json" {
            eprintln!("--format must be text or json");
            std::process::exit(1);
        }
        return run_once(&format);
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(Hide)?;
    stdout.execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let config = Config::load();
    let mut hosts: Vec<HostState> = config.hosts.iter()
        .map(HostState::new)
        .collect();
    seed_from_log(&mut hosts, config.graph_width)?;

    let shared_hosts = Arc::new(RwLock::new(schedules_from_config(&config.hosts)));
    let shutdown = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let workers = spawn_worker_pool(tx.clone(), shared_hosts.clone(), config.timeout_ms, shutdown.clone());
    let update_checker = spawn_update_checker(tx.clone(), env!("CARGO_PKG_VERSION").to_string(), shutdown.clone());

    let themes = build_themes();
    let theme_idx = themes.iter().position(|t| t.name == config.theme).unwrap_or(0);
    let collapsed: HashSet<String> = config.collapsed_groups.iter().cloned().collect();
    let mut app = App {
        themes,
        theme_idx,
        group_by: config.group_by,
        sort_mode: config.sort_mode,
        collapsed,
        collapsed_rev: 0,
        search: None,
        compact: config.compact,
        wizard_dismissed: false,
        last_esc_check: Instant::now(),
        row_cache: Vec::new(),
        row_key: RowKey::default(),
        table_rect: Rect::default(),
        table_slice: (0, 0),
        config,
        hosts,
        selected_idx: 0,
        table_state: TableState::default(),
        group_filter: None,
        input_mode: InputMode::Normal,
        update_available: None,
        update_state: UpdateState::Idle,
        last_check: "—".to_string(),
        last_result_time: None,
        restart_after_exit: false,
        history_cache: HashMap::new(),
        last_trim: Instant::now(),
    };
    // Session restore: re-select last session's host.
    if let Some(sel) = app.config.selected.clone() {
        if let Some(i) = app.hosts.iter().position(|h| h.name == sel) {
            app.selected_idx = i;
        }
    }

    let result = run_app(&mut terminal, &mut app, tx, rx, shared_hosts, shutdown.clone());
    // Persist session selection for next startup.
    app.config.selected = app.hosts.get(app.selected_idx).map(|h| h.name.clone());
    let _ = app.config.save();
    shutdown.store(true, Ordering::Relaxed);
    for w in workers {
        let _ = w.join();
    }
    let _ = update_checker.join();

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    execute!(terminal.backend_mut(), Show)?;
    terminal.show_cursor()?;

    if app.restart_after_exit {
        if let Ok(exe) = env::current_exe() {
            #[cfg(target_os = "windows")]
            {
                // Windows updater script will restart the new binary after replacement.
                // Just exit so the script can take over.
            }
            #[cfg(not(target_os = "windows"))]
            {
                let restart_exe = if is_homebrew_install(&exe) {
                    homebrew_bin_path().unwrap_or(exe)
                } else {
                    exe
                };
                let _ = Command::new(&restart_exe).spawn();
            }
        }
    }

    result
}
