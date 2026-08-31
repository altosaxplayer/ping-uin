use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::process::Command;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Local;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table};
use ratatui::{Frame, Terminal};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Where the config/csv/log live. Resolved once at startup.
///
/// Portable mode: if the directory containing the running executable has a
/// `ping-uin.portable` marker file, or already contains one of the data files,
/// that directory is used instead of the system config dir.
///
/// Otherwise falls back to the user config dir
/// (`~/.config/ping-uin` on Linux/macOS, `%APPDATA%\ping-uin` on Windows).
struct Paths {
    config: PathBuf,
    csv: PathBuf,
    log: PathBuf,
}

static PATHS: OnceLock<Paths> = OnceLock::new();

fn portable_dir() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // Don't treat Cargo build directories as portable installs.
    if exe_dir.components().any(|c| c.as_os_str() == "target") {
        return None;
    }
    let marker = exe_dir.join("ping-uin.portable");
    let has_marker = marker.exists();
    let has_data = ["ip-top.json", "hosts.csv", "uptime-log.csv"]
        .iter()
        .any(|name| exe_dir.join(name).exists());
    if has_marker || has_data {
        Some(exe_dir)
    } else {
        None
    }
}

fn resolve_paths() -> Paths {
    let dir = portable_dir()
        .unwrap_or_else(|| dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("ping-uin"));
    let _ = fs::create_dir_all(&dir);
    Paths {
        config: dir.join("ip-top.json"),
        csv: dir.join("hosts.csv"),
        log: dir.join("uptime-log.csv"),
    }
}

fn paths() -> &'static Paths {
    PATHS.get_or_init(resolve_paths)
}

const DEFAULT_INTERVAL_M: u64 = 2;
const DEFAULT_TIMEOUT_MS: u64 = 1000;
const DEFAULT_GRAPH_WIDTH: usize = 40;
const MAX_HISTORY: usize = 10000;

#[derive(Serialize, Deserialize, Clone, Debug)]
struct HostConfig {
    name: String,
    interval_m: u64,
    group: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alias: Option<String>,
}

impl HostConfig {
    fn new(name: impl Into<String>, interval_m: u64, group: impl Into<String>, alias: Option<String>) -> Self {
        let alias = alias.filter(|a| !a.trim().is_empty());
        HostConfig {
            name: name.into(),
            interval_m,
            group: group.into(),
            alias,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Config {
    hosts: Vec<HostConfig>,
    timeout_ms: u64,
    graph_width: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hosts: vec![
                HostConfig::new("8.8.8.8", 1, "external", None),
                HostConfig::new("1.1.1.1", 2, "external", Some("Google".to_string())),
                HostConfig::new("192.168.1.1", 2, "router", None),
                HostConfig::new("google.com", 2, "external", None),
            ],
            timeout_ms: DEFAULT_TIMEOUT_MS,
            graph_width: DEFAULT_GRAPH_WIDTH,
        }
    }
}

impl Config {
    fn load() -> Self {
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
                        hosts.push(HostConfig::new(name, DEFAULT_INTERVAL_M, "default", None));
                    } else if let Some(obj) = v.as_object() {
                        let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let interval = obj.get("interval_m")
                            .and_then(|v| v.as_u64())
                            .or_else(|| obj.get("interval_s").and_then(|v| v.as_u64().map(|s| s / 60)))
                            .unwrap_or(DEFAULT_INTERVAL_M);
                        let group = obj.get("group").and_then(|v| v.as_str()).unwrap_or("default").to_string();
                        let alias = obj.get("alias").and_then(|v| v.as_str()).map(|s| s.to_string());
                        hosts.push(HostConfig::new(name, interval, group, alias));
                    }
                }
            }
            return Config {
                hosts,
                timeout_ms: value.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_TIMEOUT_MS),
                graph_width: value.get("graph_width").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_GRAPH_WIDTH as u64) as usize,
            };
        }
        Self::default()
    }

    fn save(&self) -> io::Result<()> {
        fs::write(&paths().config, serde_json::to_string_pretty(self).unwrap())
    }
}

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
    ]
}

struct HostState {
    name: String,
    alias: Option<String>,
    group: String,
    interval_m: u64,
    next_ping: Instant,
    history: VecDeque<u64>,
    up: bool,
    latency_ms: f64,
    total_checks: u64,
    up_checks: u64,
}

impl HostState {
    fn new(name: &str, interval_m: u64, group: &str, alias: Option<String>) -> Self {
        HostState {
            name: name.to_string(),
            alias: alias.filter(|a| !a.trim().is_empty()),
            group: group.to_string(),
            interval_m,
            next_ping: Instant::now(),
            history: VecDeque::with_capacity(DEFAULT_GRAPH_WIDTH),
            up: false,
            latency_ms: 0.0,
            total_checks: 0,
            up_checks: 0,
        }
    }

    /// Name shown in the UI: alias if set, else the target IP/hostname.
    fn display_name(&self) -> String {
        self.alias.clone().unwrap_or_else(|| self.name.clone())
    }

    /// True only if the host currently reads down AND the last `streak` checks
    /// (or all available history) are all down. Used for the alert box.
    fn down_streak(&self, streak: usize) -> bool {
        if self.up || self.history.is_empty() { return false; }
        self.history.iter().rev().take(streak).all(|&lat| lat == 0)
    }
}

/// All-in-one add-host form state (focused field editable).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct AddHostForm {
    host: String,
    interval: String,
    group: String,
    alias: String,
    focus: usize,
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum InputMode {
    Normal,
    AddHost(AddHostForm),
    EditEntry { original: String, form: AddHostForm },
    SortPicker { selected: usize },
    GroupFilterPicker { groups: Vec<String>, selected: usize },
    ImportPath { path: String },
    ConfirmDelete,
}

/// Sort applied within the host list (flat view) and inside each group (grouped view).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SortMode {
    None,
    DownFirst,
    UpFirst,
    Name,
}

impl SortMode {
    const ALL: [SortMode; 4] = [SortMode::None, SortMode::DownFirst, SortMode::UpFirst, SortMode::Name];

    fn index(&self) -> usize {
        match self {
            SortMode::None => 0,
            SortMode::DownFirst => 1,
            SortMode::UpFirst => 2,
            SortMode::Name => 3,
        }
    }

    fn from_index(i: usize) -> Self {
        match i {
            1 => SortMode::DownFirst,
            2 => SortMode::UpFirst,
            3 => SortMode::Name,
            _ => SortMode::None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            SortMode::None => "off",
            SortMode::DownFirst => "down first",
            SortMode::UpFirst => "up first",
            SortMode::Name => "name",
        }
    }
}

struct App {
    themes: Vec<Theme>,
    theme_idx: usize,
    config: Config,
    hosts: Vec<HostState>,
    selected_idx: usize,
    group_by: bool,
    group_filter: Option<String>,
    sort_mode: SortMode,
    alerts: bool,
    input_mode: InputMode,
    update_available: Option<String>,
    last_check: String,
    last_result_time: Option<Instant>,
}

impl App {
    fn theme(&self) -> &Theme { &self.themes[self.theme_idx] }

    fn next_theme(&mut self) {
        self.theme_idx = (self.theme_idx + 1) % self.themes.len();
    }

    fn add_host(&mut self, name: String, interval_m: u64, group: String, alias: String, shared_hosts: &Arc<RwLock<Vec<HostSchedule>>>) {
        let name = name.trim().to_string();
        if name.is_empty() || self.config.hosts.iter().any(|h| h.name == name) { return; }
        let interval_m = if interval_m == 0 { DEFAULT_INTERVAL_M } else { interval_m };
        let group = if group.trim().is_empty() { "default".to_string() } else { group.trim().to_string() };
        let alias = if alias.trim().is_empty() { None } else { Some(alias.trim().to_string()) };
        self.config.hosts.push(HostConfig::new(&name, interval_m, &group, alias.clone()));
        self.persist();
        self.hosts.push(HostState::new(&name, interval_m, &group, alias));
        if let Ok(mut h) = shared_hosts.write() {
            *h = schedules_from_config(&self.config.hosts);
        }
    }

    fn remove_selected(&mut self, shared_hosts: &Arc<RwLock<Vec<HostSchedule>>>) {
        if self.hosts.len() <= 1 { return; }
        if self.selected_idx < self.hosts.len() {
            self.hosts.remove(self.selected_idx);
            self.config.hosts.remove(self.selected_idx);
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

    /// Write all entries to hosts.csv for bulk editing/import.
    fn write_entries_csv(&self) -> io::Result<()> {
        let mut wtr = csv::Writer::from_path(&paths().csv)?;
        wtr.write_record(["name", "interval_m", "group", "alias"])?;
        for h in &self.config.hosts {
            wtr.write_record([
                h.name.clone(),
                h.interval_m.to_string(),
                h.group.clone(),
                h.alias.clone().unwrap_or_default(),
            ])?;
        }
        wtr.flush()?;
        Ok(())
    }

    /// Apply one all-fields edit (from the EditEntry form) by original name.
    fn edit_entry(&mut self, original: String, form: AddHostForm, shared_hosts: &Arc<RwLock<Vec<HostSchedule>>>) {
        let new_name = form.host.trim().to_string();
        let interval: u64 = form.interval.trim().parse().unwrap_or(DEFAULT_INTERVAL_M).max(1);
        let group = if form.group.trim().is_empty() { "default".to_string() } else { form.group.trim().to_string() };
        let alias = if form.alias.trim().is_empty() { None } else { Some(form.alias.trim().to_string()) };
        if let Some(idx) = self.config.hosts.iter().position(|h| h.name == original) {
            let renamed = !new_name.is_empty() && new_name != self.config.hosts[idx].name;
            if renamed && self.config.hosts.iter().any(|h| h.name == new_name) { return; }
            if !new_name.is_empty() {
                self.config.hosts[idx].name = new_name.clone();
                if let Some(h) = self.hosts.get_mut(idx) { h.name = new_name.clone(); }
            }
            self.config.hosts[idx].interval_m = interval;
            self.config.hosts[idx].group  = group.clone();
            self.config.hosts[idx].alias  = alias.clone();
            if let Some(h) = self.hosts.get_mut(idx) {
                h.interval_m = interval;
                h.group      = group;
                h.alias      = alias;
            }
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
                            h.interval_m = entry.interval_m;
                            h.group      = entry.group.clone();
                            h.alias      = entry.alias.clone();
                        }
                    }
                    None => {
                        self.config.hosts.push(entry.clone());
                        self.hosts.push(HostState::new(&entry.name, entry.interval_m, &entry.group, entry.alias.clone()));
                    }
                }
            }
            self.persist();
            if let Ok(mut h) = shared_hosts.write() {
                *h = schedules_from_config(&self.config.hosts);
            }
        }
    }
}

/// Read hosts.csv rows into HostConfig entries.
fn read_entries_csv(path: &std::path::Path) -> io::Result<Vec<HostConfig>> {
    let mut rdr = csv::Reader::from_path(path)?;
    let mut out = Vec::new();
    for record in rdr.records() {
        let r = record?;
        let name = r.get(0).unwrap_or("").trim().to_string();
        if name.is_empty() { continue; }
        let interval = r.get(1).and_then(|s| s.trim().parse().ok()).unwrap_or(DEFAULT_INTERVAL_M);
        let group = r.get(2).unwrap_or("").trim();
        let group = if group.is_empty() { "default".to_string() } else { group.to_string() };
        let alias = r.get(3).map(|s| s.trim().to_string());
        out.push(HostConfig::new(name, interval, group, alias));
    }
    Ok(out)
}

#[derive(Clone)]
struct HostSchedule {
    name: String,
    interval_m: u64,
    next_ping: Instant,
}

fn schedules_from_config(hosts: &[HostConfig]) -> Vec<HostSchedule> {
    hosts.iter().map(|h| HostSchedule {
        name: h.name.clone(),
        interval_m: h.interval_m,
        next_ping: Instant::now(),
    }).collect()
}

enum Message {
    Result { host: String, up: bool, latency_ms: f64, timestamp: String, next_ping: Instant },
    UpdateAvailable { version: String },
}

fn ping_host(host: &str, timeout_ms: u64, re: &Regex) -> (bool, f64) {
    let os = env::consts::OS;
    let output = match os {
        "windows" => Command::new("ping").args(["-n", "1", "-w", &timeout_ms.to_string(), host]).output(),
        "macos" => Command::new("ping").args(["-c", "1", "-W", &timeout_ms.to_string(), host]).output(),
        _ => Command::new("ping").args(["-c", "1", "-W", &timeout_ms.div_ceil(1000).to_string(), host]).output(),
    };
    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(cap) = re.captures(&text) {
                if let Ok(lat) = cap[1].parse::<f64>() { return (true, lat); }
            }
            (true, 0.0)
        }
        _ => (false, 0.0),
    }
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

fn seed_from_log(hosts: &mut [HostState]) -> io::Result<()> {
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
        while h.history.len() > h.history.capacity() { h.history.pop_front(); }
    }
    Ok(())
}

fn trim_log(hosts: &mut [HostState]) -> io::Result<()> {
    let contents = fs::read_to_string(&paths().log)?;
    let mut lines: Vec<&str> = contents.lines().collect();
    if lines.len() > MAX_HISTORY + 1 {
        let kept: Vec<String> = std::iter::once(lines[0].to_string())
            .chain(lines.drain(lines.len() - MAX_HISTORY..).map(|s| s.to_string()))
            .collect();
        fs::write(&paths().log, kept.join("\n") + "\n")?;
        for h in hosts.iter_mut() { h.total_checks = 0; h.up_checks = 0; h.history.clear(); }
        seed_from_log(hosts)?;
    }
    Ok(())
}

fn render_graph(history: &VecDeque<u64>, theme: &Theme, width: usize) -> Text<'static> {
    // btop-disks style: each ping is a block; green ■ if up, red bottom line _ if down.
    // One space between blocks. Show the newest `width/2` samples.
    let max_show = width / 2;
    let start = history.len().saturating_sub(max_show);
    let shown = history.len() - start;
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, &lat) in history.iter().skip(start).enumerate() {
        if i > 0 { spans.push(Span::raw(" ")); }
        if lat > 0 {
            spans.push(Span::styled("■", Style::default().fg(theme.graph_start)));
        } else {
            // Down ping: a thin red bottom line underscores where the gap is.
            spans.push(Span::styled("_", Style::default().fg(theme.status_danger)));
        }
    }
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
fn key_hint(key: &'static str, label: &'static str, theme: &Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled("[", Style::default().fg(theme.divider)),
        Span::styled(key, Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD)),
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

#[derive(Clone)]
enum RowKind {
    GroupHeader(String),
    Host,
}

#[derive(Clone)]
struct VisibleRow {
    kind: RowKind,
    host_idx: Option<usize>,
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
    }
}

fn build_visible_rows(hosts: &[HostState], group_by: bool, group_filter: Option<&str>, sort_mode: SortMode) -> Vec<VisibleRow> {
    let in_group = |h: &HostState| {
        let g = if h.group.is_empty() { "default" } else { &h.group };
        group_filter.map_or(true, |f| g == f)
    };
    if !group_by {
        let mut indices: Vec<usize> = hosts.iter().enumerate()
            .filter(|(_, h)| in_group(h))
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
                    rows.push(VisibleRow { kind: RowKind::GroupHeader(label), host_idx: None });
                    last_up = Some(up);
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
        if !in_group(h) { continue; }
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
        rows.push(VisibleRow { kind: RowKind::GroupHeader(group.clone()), host_idx: None });
        let mut indices = groups[&group].clone();
        // Default grouped behavior is down-first per group; explicit sort overrides.
        let mode = if sort_mode == SortMode::None { SortMode::DownFirst } else { sort_mode };
        sort_host_indices(&mut indices, hosts, mode);
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
    let rows = build_visible_rows(&app.hosts, app.group_by, app.group_filter.as_deref(), app.sort_mode);
    if let Some(pos) = selected_visible_position(&rows, app.selected_idx) {
        for i in (0..pos).rev() {
            if let Some(idx) = rows[i].host_idx {
                app.selected_idx = idx;
                return;
            }
        }
    }
}

fn move_selection_down(app: &mut App) {
    let rows = build_visible_rows(&app.hosts, app.group_by, app.group_filter.as_deref(), app.sort_mode);
    if let Some(pos) = selected_visible_position(&rows, app.selected_idx) {
        for i in pos + 1..rows.len() {
            if let Some(idx) = rows[i].host_idx {
                app.selected_idx = idx;
                return;
            }
        }
    }
}

fn render_host_row(h: &HostState, is_selected: bool, theme: &Theme, graph_width: usize) -> Row<'static> {
    let status_color = if h.up { theme.status_good } else { theme.status_danger };
    let status_label = if h.up { "UP" } else { "DOWN" };
    let latency_str = if h.up { format!("{:.0} ms", h.latency_ms) } else { "—".to_string() };
    let uptime = if h.total_checks > 0 { h.up_checks as f64 / h.total_checks as f64 * 100.0 } else { 0.0 };
    let next_str = {
        let remaining = h.next_ping.saturating_duration_since(Instant::now());
        if remaining.is_zero() { "now".to_string() } else { format!("{}s", remaining.as_secs()) }
    };
    let row_style = if is_selected {
        Style::default().bg(theme.selected_bg).fg(theme.selected_fg)
    } else {
        Style::default().bg(theme.main_bg).fg(theme.main_fg)
    };
    // Name cell: alias (title color) or the raw target.
    let name_line = match &h.alias {
        Some(alias) => Span::styled(alias.clone(), Style::default().fg(theme.title)),
        None => Span::styled(h.name.clone(), Style::default().fg(theme.title)),
    };
    // IP column shows the target only when an alias is set, otherwise empty.
    let ip_line = if h.alias.is_some() {
        Span::styled(h.name.clone(), Style::default().fg(theme.inactive_fg))
    } else {
        Span::styled("", Style::default())
    };

    Row::new(vec![
        Cell::from(name_line),
        Cell::from(ip_line),
        Cell::from(Line::from(vec![
            Span::styled("● ", Style::default().fg(status_color)),
            Span::styled(status_label, Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
        ])),
        Cell::from(Span::styled(latency_str, Style::default().fg(theme.main_fg))),
        Cell::from(Span::styled(format!("{}m", h.interval_m), Style::default().fg(theme.inactive_fg))),
        Cell::from(Span::styled(next_str, Style::default().fg(theme.inactive_fg))),
        Cell::from(Span::styled(format!("{:.1}%", uptime), Style::default().fg(theme.graph_text))),
        Cell::from(Span::styled(h.group.clone(), Style::default().fg(theme.inactive_fg))),
        Cell::from(render_graph(&h.history, theme, graph_width)),
    ]).style(row_style)
}

fn render_group_header(group: &str, hosts: &[HostState], theme: &Theme) -> Row<'static> {
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
    if !is_status_label {
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
    spans.push(Span::styled("──────────", Style::default().fg(theme.divider)));
    Row::new(vec![
        Cell::from(Line::from(spans)),
        Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""),
    ]).style(Style::default().bg(theme.main_bg))
}

fn ui(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = frame.area();

    frame.render_widget(Block::default().style(Style::default().bg(theme.main_bg)), area);

    // Layout: title / stats / optional alerts box / table / footer.
    let up_count = app.hosts.iter().filter(|h| h.up).count();
    let total = app.hosts.len();
    let down_count = total.saturating_sub(up_count);
    let pct_up = if total > 0 { (up_count as f64 / total as f64 * 100.0).round() as u64 } else { 0 };
    let now = Local::now().format("%H:%M:%S").to_string();
    let downs: Vec<&HostState> = app.hosts.iter().filter(|h| h.down_streak(3)).collect();
    let show_alerts = app.alerts;
    let alert_rows = if show_alerts { (downs.len().min(6).max(1) + if downs.len() > 6 { 1 } else { 0 }) as u16 } else { 0 };

    let mut constraints = vec![Constraint::Length(5), Constraint::Length(3)];
    if show_alerts {
        constraints.push(Constraint::Length(alert_rows + 3));
    }
    constraints.push(Constraint::Min(5));
    constraints.push(Constraint::Length(1));

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(constraints)
        .split(area);

    let (stats_area, alert_area, table_area, footer_area) = if show_alerts {
        (main_layout[1], Some(main_layout[2]), main_layout[3], main_layout[4])
    } else {
        (main_layout[1], None, main_layout[2], main_layout[3])
    };
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
    let stats_line = Line::from(vec![
        Span::styled("● ", Style::default().fg(theme.status_good)),
        Span::styled(format!("{} up", up_count), Style::default().fg(theme.status_good).add_modifier(Modifier::BOLD)),
        Span::styled("   ", Style::default()),
        Span::styled("● ", Style::default().fg(theme.status_danger)),
        Span::styled(format!("{} down", down_count), Style::default().fg(theme.status_danger).add_modifier(Modifier::BOLD)),
        Span::styled("   ", Style::default()),
        Span::styled("◐ ", Style::default().fg(theme.hi_fg)),
        Span::styled(format!("{}% up", pct_up), Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  ·  {} hosts", total), Style::default().fg(theme.inactive_fg)),
    ]);
    let stats_right = Line::from(vec![
        Span::styled(now, Style::default().fg(theme.inactive_fg)),
        Span::styled(" · ", Style::default().fg(theme.divider)),
        Span::styled(theme.name, Style::default().fg(theme.hi_fg)),
    ]);
    let stats_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(30), Constraint::Length(22)])
        .split(stats_inner);
    frame.render_widget(Paragraph::new(Text::from(stats_line)), stats_layout[0]);
    frame.render_widget(Paragraph::new(Text::from(stats_right)).alignment(Alignment::Right), stats_layout[1]);

    // Down-hosts alert box (x to toggle): red-bordered, group-agnostic.
    if let Some(alerts_rect) = alert_area {
        let alert_title = Line::from(vec![
            Span::styled(" ▐ ", Style::default().fg(theme.status_danger)),
            Span::styled("down now", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
            Span::styled(" ▌ ", Style::default().fg(theme.status_danger)),
            Span::styled(format!("{} host(s) ", downs.len()), Style::default().fg(theme.status_danger)),
        ]);
        let alert_block = Block::default()
            .title(alert_title)
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.status_danger))
            .style(Style::default().bg(theme.main_bg));

        let mut lines: Vec<Line> = Vec::new();
        if downs.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(theme.status_good)),
                Span::styled("all hosts up", Style::default().fg(theme.inactive_fg)),
            ]));
        } else {
            let shown = downs.len().min(6);
            for h in downs.iter().take(shown) {
                let name_display = h.alias.clone().unwrap_or_else(|| h.name.clone());
                let ip_display = if h.alias.is_some() { h.name.clone() } else { "".to_string() };
                let uptime = if h.total_checks > 0 { h.up_checks as f64 / h.total_checks as f64 * 100.0 } else { 0.0 };
                let mut spans = vec![
                    Span::styled("● ", Style::default().fg(theme.status_danger)),
                    Span::styled(name_display, Style::default().fg(theme.title)),
                ];
                if !ip_display.is_empty() {
                    spans.push(Span::styled(format!(" ({})", ip_display), Style::default().fg(theme.inactive_fg)));
                }
                spans.push(Span::styled("   ", Style::default()));
                spans.push(Span::styled(h.group.clone(), Style::default().fg(theme.inactive_fg)));
                spans.push(Span::styled(" · ", Style::default().fg(theme.divider)));
                spans.push(Span::styled(format!("{}m", h.interval_m), Style::default().fg(theme.inactive_fg)));
                spans.push(Span::styled(" · ", Style::default().fg(theme.divider)));
                spans.push(Span::styled(format!("{:.1}% up", uptime), Style::default().fg(theme.graph_text)));
                lines.push(Line::from(spans));
            }
            let extra = downs.len().saturating_sub(6);
            if extra > 0 {
                lines.push(Line::from(Span::styled(format!("… {} more down", extra), Style::default().fg(theme.inactive_fg))));
            }
        }

        let alert_box = Paragraph::new(Text::from(lines)).block(alert_block);
        frame.render_widget(alert_box, alerts_rect);
    }

    let mut title_spans = accent_title("last check", theme).spans;
    title_spans.push(Span::styled(if app.last_check.is_empty() { "—".to_string() } else { app.last_check.clone() }, Style::default().fg(theme.inactive_fg)));
    let table_block = Block::default()
        .title(Line::from(title_spans))
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.box_color))
        .style(Style::default().bg(theme.main_bg));

    let header = Row::new(vec!["Host", "IP", "Status", "Latency", "Int", "Next", "Uptime", "Group", "History"])
        .style(Style::default().fg(theme.inactive_fg).add_modifier(Modifier::BOLD))
        .height(1);

    let mut rows = Vec::new();
    if app.hosts.is_empty() {
        rows.push(Row::new(vec![
            Cell::from(Span::styled("No hosts — press 'a' to add one", Style::default().fg(theme.inactive_fg))),
            Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""),
        ]));
    } else {
        let visible_rows = build_visible_rows(&app.hosts, app.group_by, app.group_filter.as_deref(), app.sort_mode);
        for row in visible_rows {
            match row.kind {
                RowKind::GroupHeader(group) => rows.push(render_group_header(&group, &app.hosts, theme)),
                RowKind::Host => {
                    let idx = row.host_idx.unwrap();
                    rows.push(render_host_row(&app.hosts[idx], idx == app.selected_idx, theme, app.config.graph_width));
                }
            }
        }
    }

    let table = Table::new(rows, [
        Constraint::Length(18),
        Constraint::Length(18),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Min(app.config.graph_width as u16),
    ])
    .header(header)
    .block(table_block);
    frame.render_widget(table, table_area);

    let footer_text = match app.input_mode {
        InputMode::Normal => {
            let mut spans = vec![Span::styled(" ↑/↓ select ", Style::default().fg(theme.inactive_fg))];
            spans.extend(key_hint("a", "add", theme));
            spans.extend(key_hint("d", "delete", theme));
            spans.extend(key_hint("e", "edit", theme));
            spans.extend(key_hint("i", "import csv", theme));
            spans.extend(key_hint("g", "group", theme));
            spans.extend(key_hint("f", "filter group", theme));
            spans.extend(key_hint("s", "sort", theme));
            spans.extend(key_hint("x", "down box", theme));
            spans.extend(key_hint("t", "theme", theme));
            spans.extend(key_hint("q", "quit", theme));
            let mut status_parts = vec![format!("sort: {}", app.sort_mode.label())];
            if let Some(ref filter) = app.group_filter {
                status_parts.push(format!("group: {}", filter));
            }
            spans.push(Span::styled(format!("  {}", status_parts.join("  ·  ")), Style::default().fg(theme.divider)));
            if let Some(ref version) = app.update_available {
                spans.push(Span::styled(
                    format!("  ↑ v{} available", version),
                    Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD),
                ));
            }
            Text::from(Line::from(spans))
        }
        InputMode::AddHost(_) => Text::from(Line::from(vec![
            Span::styled("Add host", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
            Span::raw("   "),
            Span::raw("[Tab]/[↑↓] move field   [Enter] add   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)),
        ])),
        InputMode::SortPicker { .. } => Text::from(Line::from(vec![
            Span::styled("Sort hosts", Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
            Span::raw("   "),
            Span::raw("[↑↓] pick   [Enter] apply   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)),
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
    };
    frame.render_widget(footer_text, footer_area);

    match app.input_mode {
        InputMode::AddHost(ref form) | InputMode::EditEntry { ref form, .. } => {
            let title_text = if matches!(app.input_mode, InputMode::AddHost(_)) { "Add host" } else { "Edit host" };
            let popup_area = centered_rect(56, 40, area);
            let labels = ["host (IP/name)", "interval (min)", "group", "display name"];
            let values = [&form.host, &form.interval, &form.group, &form.alias];
            let placeholder = ["e.g. 8.8.8.8", "2", "default", "optional"];
            let mut lines: Vec<Line> = Vec::new();
            for i in 0..4 {
                let focused = form.focus == i;
                let marker = if focused { "▶ " } else { "  " };
                let value = if values[i].is_empty() { placeholder[i].to_string() } else { values[i].clone() };
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
                    .title(accent_title(title_text, theme))
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.box_color))
                    .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        InputMode::SortPicker { selected } => {
            let popup_area = centered_rect(30, 38, area);
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
            lines.push(Line::from("[Enter] apply   [Esc] cancel").style(Style::default().fg(theme.inactive_fg)));
            let popup = Paragraph::new(Text::from(lines))
                .block(Block::default()
                    .title(accent_title("Sort hosts", theme))
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
                    .title(accent_title("Filter by group", theme))
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
                .title(accent_title("Import CSV", theme))
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
                .title(accent_title("Confirm delete", theme))
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.status_danger))
                .style(Style::default().bg(theme.popup_bg)));
            frame.render_widget(Clear, popup_area);
            frame.render_widget(popup, popup_area);
        }
        InputMode::Normal => {}
    }
}

fn spawn_worker(tx: mpsc::Sender<Message>, hosts: Arc<RwLock<Vec<HostSchedule>>>, timeout_ms: u64, shutdown: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let re = Regex::new(r"time[<=]([\d.]+)\s*ms").unwrap();
        while !shutdown.load(Ordering::Relaxed) {
            let now = Instant::now();
            let due = { hosts.read().unwrap().iter().min_by_key(|h| h.next_ping).cloned() };
            if let Some(host) = due {
                let wait = host.next_ping.saturating_duration_since(now);
                if wait.is_zero() {
                    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let (up, latency_ms) = ping_host(&host.name, timeout_ms, &re);
                    let next_ping = Instant::now() + Duration::from_secs(host.interval_m * 60);
                    if let Ok(mut list) = hosts.write() {
                        if let Some(h) = list.iter_mut().find(|h| h.name == host.name) {
                            h.next_ping = next_ping;
                        }
                    }
                    // Channel closed means the UI already quit; stop.
                    if tx.send(Message::Result { host: host.name, up, latency_ms, timestamp, next_ping }).is_err() {
                        break;
                    }
                } else {
                    thread::sleep(wait.min(Duration::from_millis(100)));
                }
            } else {
                thread::sleep(Duration::from_millis(100));
            }
        }
    })
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
fn spawn_update_checker(tx: mpsc::Sender<Message>, current_version: String) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Wait a few seconds so the UI starts immediately.
        thread::sleep(Duration::from_secs(3));
        let url = "https://api.github.com/repos/altosaxplayer/ping-uin/releases/latest";
        let response = ureq::get(url)
            .set("User-Agent", "ping-uin-update-check")
            .timeout(Duration::from_secs(10))
            .call();
        if let Ok(response) = response {
            if let Ok(body) = response.into_string() {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(tag) = value.get("tag_name").and_then(|v| v.as_str()) {
                        let latest = tag.trim_start_matches('v').to_string();
                        if is_newer_version(&current_version, &latest) {
                            let _ = tx.send(Message::UpdateAvailable { version: latest });
                        }
                    }
                }
            }
        }
    })
}

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    rx: mpsc::Receiver<Message>,
    shared_hosts: Arc<RwLock<Vec<HostSchedule>>>,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
    let tick_rate = Duration::from_millis(50);
    let mut frames_since_trim = 0;

    loop {
        terminal.draw(|f| ui(f, app))?;
        if event::poll(tick_rate)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match app.input_mode {
                        InputMode::Normal => match key.code {
                            KeyCode::Char('q') | KeyCode::Char('Q') => { shutdown.store(true, Ordering::Relaxed); return Ok(()); }
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => { shutdown.store(true, Ordering::Relaxed); return Ok(()); }
                            KeyCode::Char('a') | KeyCode::Char('A') => {
                                app.input_mode = InputMode::AddHost(AddHostForm::default());
                            }
                            KeyCode::Char('d') | KeyCode::Char('D') => { if !app.hosts.is_empty() { app.input_mode = InputMode::ConfirmDelete; } }
                            KeyCode::Char('e') | KeyCode::Char('E') => {
                                if let Some(h) = app.hosts.get(app.selected_idx) {
                                    app.input_mode = InputMode::EditEntry {
                                        original: h.name.clone(),
                                        form: AddHostForm {
                                            host: h.name.clone(),
                                            interval: h.interval_m.to_string(),
                                            group: h.group.clone(),
                                            alias: h.alias.clone().unwrap_or_default(),
                                            focus: 0,
                                        },
                                    };
                                }
                            }
                            KeyCode::Char('i') | KeyCode::Char('I') => {
                                let default_path = paths().csv.to_string_lossy().to_string();
                                app.input_mode = InputMode::ImportPath { path: default_path };
                            }
                            KeyCode::Char('g') => app.group_by = !app.group_by,
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
                            KeyCode::Char('x') | KeyCode::Char('X') => { app.alerts = !app.alerts; }
                            KeyCode::Char('t') | KeyCode::Char('T') => app.next_theme(),
                            KeyCode::Up => move_selection_up(app),
                            KeyCode::Down => move_selection_down(app),
                            _ => {}
                        },
                        InputMode::AddHost(ref form0) => {
                            let mut form = form0.clone();
                            match key.code {
                                KeyCode::Esc => { app.input_mode = InputMode::Normal; }
                                KeyCode::Tab | KeyCode::Down => { form.focus = (form.focus + 1) % 4; app.input_mode = InputMode::AddHost(form); }
                                KeyCode::BackTab | KeyCode::Up => { form.focus = (form.focus + 3) % 4; app.input_mode = InputMode::AddHost(form); }
                                KeyCode::Enter => {
                                    let host = form.host.trim().to_string();
                                    let interval = form.interval.trim().parse().unwrap_or(DEFAULT_INTERVAL_M);
                                    let group = form.group.trim().to_string();
                                    let alias = form.alias.trim().to_string();
                                    app.input_mode = InputMode::Normal;
                                    if !host.is_empty() { app.add_host(host, interval, group, alias, &shared_hosts); }
                                }
                                KeyCode::Backspace => {
                                    match form.focus {
                                        0 => { form.host.pop(); }
                                        1 => { form.interval.pop(); }
                                        2 => { form.group.pop(); }
                                        _ => { form.alias.pop(); }
                                    }
                                    app.input_mode = InputMode::AddHost(form);
                                }
                                KeyCode::Char(c) => {
                                    match form.focus {
                                        0 => form.host.push(c),
                                        1 => form.interval.push(c),
                                        2 => form.group.push(c),
                                        _ => form.alias.push(c),
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
                                app.input_mode = InputMode::Normal;
                            }
                            KeyCode::Char(c) if c.is_ascii_digit() => {
                                let idx = (c as usize) - ('1' as usize);
                                if idx < SortMode::ALL.len() {
                                    app.sort_mode = SortMode::from_index(idx);
                                    app.input_mode = InputMode::Normal;
                                }
                            }
                            _ => {}
                        },
                        InputMode::GroupFilterPicker { ref groups, selected } => {
                            let groups = groups.clone();
                            match key.code {
                                KeyCode::Esc => {
                                    app.group_filter = None;
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
                        InputMode::EditEntry { ref original, ref form } => {
                            let original = original.clone();
                            let mut form = form.clone();
                            match key.code {
                                KeyCode::Esc => { app.input_mode = InputMode::Normal; }
                                KeyCode::Tab | KeyCode::Down => { form.focus = (form.focus + 1) % 4; app.input_mode = InputMode::EditEntry { original, form }; }
                                KeyCode::BackTab | KeyCode::Up => { form.focus = (form.focus + 3) % 4; app.input_mode = InputMode::EditEntry { original, form }; }
                                KeyCode::Enter => {
                                    let form2 = form.clone();
                                    app.input_mode = InputMode::Normal;
                                    app.edit_entry(original, form2, &shared_hosts);
                                }
                                KeyCode::Backspace => {
                                    match form.focus {
                                        0 => { form.host.pop(); }
                                        1 => { form.interval.pop(); }
                                        2 => { form.group.pop(); }
                                        _ => { form.alias.pop(); }
                                    }
                                    app.input_mode = InputMode::EditEntry { original, form };
                                }
                                KeyCode::Char(c) => {
                                    match form.focus {
                                        0 => form.host.push(c),
                                        1 => form.interval.push(c),
                                        2 => form.group.push(c),
                                        _ => form.alias.push(c),
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
            }
        }

        while let Ok(msg) = rx.try_recv() {
            match msg {
                Message::Result { host, up, latency_ms, timestamp, next_ping } => {
                    app.last_check = timestamp.clone();
                    app.last_result_time = Some(Instant::now());
                    if let Some(h) = app.hosts.iter_mut().find(|h| h.name == host) {
                        h.up = up;
                        h.latency_ms = latency_ms;
                        h.next_ping = next_ping;
                        h.total_checks += 1;
                        if up { h.up_checks += 1; }
                        let lat_u64 = if up { latency_ms.round() as u64 } else { 0 };
                        h.history.push_back(lat_u64);
                        let status = if up { "UP" } else { "DOWN" };
                        let _ = log_result(&timestamp, &h.name, status, latency_ms);
                    }
                }
                Message::UpdateAvailable { version } => {
                    app.update_available = Some(version);
                }
            }
        }

        frames_since_trim += 1;
        if frames_since_trim >= 100 {
            frames_since_trim = 0;
            let _ = trim_log(&mut app.hosts);
        }
    }
}

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let config = Config::load();
    let mut hosts: Vec<HostState> = config.hosts.iter()
        .map(|h| HostState::new(&h.name, h.interval_m, &h.group, h.alias.clone()))
        .collect();
    seed_from_log(&mut hosts)?;

    let shared_hosts = Arc::new(RwLock::new(schedules_from_config(&config.hosts)));
    let shutdown = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let worker = spawn_worker(tx.clone(), shared_hosts.clone(), config.timeout_ms, shutdown.clone());
    let update_checker = spawn_update_checker(tx, env!("CARGO_PKG_VERSION").to_string());

    let mut app = App {
        themes: build_themes(),
        theme_idx: 0,
        config,
        hosts,
        selected_idx: 0,
        group_by: false,
        group_filter: None,
        sort_mode: SortMode::None,
        alerts: false,
        input_mode: InputMode::Normal,
        update_available: None,
        last_check: "—".to_string(),
        last_result_time: None,
    };

    let result = run_app(&mut terminal, &mut app, rx, shared_hosts, shutdown.clone());
    shutdown.store(true, Ordering::Relaxed);
    let _ = worker.join();
    let _ = update_checker.join();

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    execute!(terminal.backend_mut(), Show)?;
    terminal.show_cursor()?;
    result
}
