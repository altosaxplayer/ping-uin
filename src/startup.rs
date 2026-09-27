//! Start-on-boot wiring so `ping-uin --serve` reliably restarts.
//!
//! - macOS: `~/Library/LaunchAgents/com.ping-uin.plist` (RunAtLoad+KeepAlive)
//! - Linux: systemd user service `ping-uin.service`, fallback to XDG autostart
//! - Windows: Scheduled Task `ping-uin` (on logon)
//!
//! All targets run the same headless command:
//! `<exe> --serve --bind <bind> --port <port>` (bind defaults to 0.0.0.0
//! so the read-only page is visible anywhere on the local network).

use std::fs;
use std::io;
use std::path::PathBuf;

pub const DEFAULT_WEB_PORT: u16 = 8080;
pub const DEFAULT_WEB_BIND: &str = "0.0.0.0";
#[allow(dead_code)]
pub const LAUNCH_LABEL: &str = "com.ping-uin";
#[allow(dead_code)]
pub const SERVICE_NAME: &str = "ping-uin";
#[allow(dead_code)]
pub const TASK_NAME: &str = "ping-uin";

fn current_exe_string() -> io::Result<String> {
    let exe = std::env::current_exe()?;
    Ok(exe.to_string_lossy().to_string())
}

fn serve_args(port: u16, bind: &str) -> String {
    format!("--serve --bind {} --port {}", bind, port)
}

/// One-time command that lets the binary bind privileged ports (<1024) as a
/// normal user. File capabilities ride the binary, so they must be
/// re-applied after every update replaces it.
#[allow(dead_code)] // Linux-only callers; kept portable so tests run anywhere.
fn setcap_command(exe: &str) -> String {
    format!("sudo setcap 'cap_net_bind_service=+ep' {}", exe)
}

/// Install/describe suffix for privileged ports; empty for high ports.
/// `granted` is the outcome of the automatic setcap attempt (None = not
/// attempted yet, e.g. the `describe` plan).
#[allow(dead_code)] // Linux-only callers; kept portable so tests run anywhere.
fn low_port_note(port: u16, exe: &str, granted: Option<bool>) -> String {
    if port >= 1024 {
        return String::new();
    }
    match granted {
        Some(true) => format!(
            " (cap_net_bind_service granted for port {}; re-apply after each update: {})",
            port,
            setcap_command(exe)
        ),
        Some(false) => format!(
            " — NOTE: port {} needs privilege; run once: {} (re-apply after each update), or system-wide: sudo sysctl net.ipv4.ip_unprivileged_port_start={}",
            port,
            setcap_command(exe),
            port
        ),
        None => format!(
            " — port {} is privileged: install tries to grant cap_net_bind_service (root or passwordless sudo), else prints the manual command",
            port
        ),
    }
}

/// Try to give the binary CAP_NET_BIND_SERVICE so a privileged port binds
/// as a normal user. Root runs setcap directly; otherwise only passwordless
/// sudo can help (a detached installer can't prompt for a password).
#[cfg(target_os = "linux")]
fn grant_bind_capability(exe: &str) -> bool {
    const CAP: &str = "cap_net_bind_service=+ep";
    let direct = std::process::Command::new("setcap")
        .args([CAP, exe])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if direct {
        return true;
    }
    std::process::Command::new("sudo")
        .args(["-n", "setcap", CAP, exe])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Human-readable description of what install will do on this OS.
pub fn describe(port: u16, bind: &str) -> String {
    let exe = current_exe_string().unwrap_or_else(|_| "ping-uin".to_string());
    #[cfg(target_os = "macos")]
    {
        format!(
            "LaunchAgent ~/Library/LaunchAgents/{}.plist -> '{}' {} (starts at login, kept alive)",
            LAUNCH_LABEL,
            exe,
            serve_args(port, bind)
        )
    }
    #[cfg(target_os = "linux")]
    {
        format!(
            "systemd user service ~/.config/systemd/user/{}.service -> '{}' {} (enable --now; falls back to XDG autostart when systemd is unavailable){}",
            SERVICE_NAME,
            exe,
            serve_args(port, bind),
            low_port_note(port, &exe, None)
        )
    }
    #[cfg(target_os = "windows")]
    {
        format!(
            "Scheduled Task '{}' (on logon) -> \"{}\" {}",
            TASK_NAME,
            exe,
            serve_args(port, bind)
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (port, bind, exe);
        "unsupported OS for startup install".to_string()
    }
}

/// Where the startup entry lives (primary location), for status messages.
fn primary_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{}.plist", LAUNCH_LABEL)))
    }
    #[cfg(target_os = "linux")]
    {
        dirs::config_dir().map(|c| c.join("systemd/user").join(format!("{}.service", SERVICE_NAME)))
    }
    #[cfg(target_os = "windows")]
    {
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

pub fn is_installed() -> bool {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("schtasks")
            .args(["/query", "/tn", TASK_NAME])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let primary = primary_path().map(|p| p.exists()).unwrap_or(false);
        if primary {
            return true;
        }
        // Linux fallback location.
        #[cfg(target_os = "linux")]
        {
            if let Some(home) = dirs::home_dir() {
                if home.join(".config/autostart").join(format!("{}.desktop", SERVICE_NAME)).exists() {
                    return true;
                }
            }
            false
        }
        #[cfg(not(target_os = "linux"))]
        {
            primary
        }
    }
}

pub fn status_line() -> String {
    if is_installed() {
        #[cfg(target_os = "linux")]
        {
            // Installed on disk is half the story: say whether it runs.
            let running = if service_active() { ", running" } else { ", NOT running" };
            match primary_path() {
                Some(p) => format!("startup: installed ({}){}", p.display(), running),
                None => format!("startup: installed{}", running),
            }
        }
        #[cfg(not(target_os = "linux"))]
        match primary_path() {
            Some(p) => format!("startup: installed ({})", p.display()),
            None => {
                #[cfg(target_os = "windows")]
                {
                    format!("startup: installed (Scheduled Task '{}')", TASK_NAME)
                }
                #[cfg(not(target_os = "windows"))]
                {
                    "startup: installed".to_string()
                }
            }
        }
    } else {
        "startup: not installed".to_string()
    }
}

pub fn install(port: u16, bind: &str) -> io::Result<String> {
    #[cfg(target_os = "macos")]
    {
        return install_macos(port, bind);
    }
    #[cfg(target_os = "linux")]
    {
        return install_linux(port, bind);
    }
    #[cfg(target_os = "windows")]
    {
        return install_windows(port, bind);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (port, bind);
        Err(io::Error::new(io::ErrorKind::Unsupported, "startup install not supported on this OS"))
    }
}

pub fn uninstall() -> io::Result<String> {
    #[cfg(target_os = "macos")]
    {
        return uninstall_macos();
    }
    #[cfg(target_os = "linux")]
    {
        return uninstall_linux();
    }
    #[cfg(target_os = "windows")]
    {
        return uninstall_windows();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(io::Error::new(io::ErrorKind::Unsupported, "startup uninstall not supported on this OS"))
    }
}

// ── macOS ──

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> io::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot find home dir"))?;
    Ok(home.join("Library/LaunchAgents").join(format!("{}.plist", LAUNCH_LABEL)))
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(target_os = "macos")]
fn install_macos(port: u16, bind: &str) -> io::Result<String> {
    let exe = current_exe_string()?;
    let plist = launchd_plist_path()?;
    if let Some(parent) = plist.parent() {
        fs::create_dir_all(parent)?;
    }
    let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string()).unwrap_or_else(|| "/tmp".to_string());
    let contents = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
        <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
        <plist version=\"1.0\"><dict>\n\
        <key>Label</key><string>{}</string>\n\
        <key>ProgramArguments</key><array>\n\
        <string>{}</string>\n\
        <string>--serve</string>\n\
        <string>--bind</string><string>{}</string>\n\
        <string>--port</string><string>{}</string>\n\
        </array>\n\
        <key>RunAtLoad</key><true/>\n\
        <key>KeepAlive</key><true/>\n\
        <key>StandardOutPath</key><string>{}/Library/Logs/ping-uin.log</string>\n\
        <key>StandardErrorPath</key><string>{}/Library/Logs/ping-uin.log</string>\n\
        </dict></plist>\n",
        LAUNCH_LABEL,
        xml_escape(&exe),
        xml_escape(bind),
        port,
        home,
        home,
    );
    fs::write(&plist, contents)?;
    // Best-effort (re)load; failure still leaves a valid plist for next login.
    let _ = std::process::Command::new("launchctl").args(["unload", "-w", &plist.to_string_lossy()]).output();
    match std::process::Command::new("launchctl").args(["load", "-w", &plist.to_string_lossy()]).output() {
        Ok(o) if o.status.success() => Ok(format!("installed + loaded {}", plist.display())),
        Ok(o) => Ok(format!(
            "installed {} (launchctl load said: {})",
            plist.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Ok(format!("installed {} (launchctl not run: {})", plist.display(), e)),
    }
}

#[cfg(target_os = "macos")]
fn uninstall_macos() -> io::Result<String> {
    let plist = launchd_plist_path()?;
    let _ = std::process::Command::new("launchctl").args(["unload", "-w", &plist.to_string_lossy()]).output();
    if plist.exists() {
        fs::remove_file(&plist)?;
        Ok(format!("removed {}", plist.display()))
    } else {
        Ok("nothing installed".to_string())
    }
}

// ── Linux ──

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> Option<PathBuf> {
    dirs::config_dir().map(|c| c.join("systemd/user").join(format!("{}.service", SERVICE_NAME)))
}

#[cfg(target_os = "linux")]
fn autostart_desktop_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/autostart").join(format!("{}.desktop", SERVICE_NAME)))
}

#[cfg(target_os = "linux")]
fn systemctl_available() -> bool {
    std::process::Command::new("systemctl").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// True when this session has a user bus (plain SSH without lingering or a
/// `su` shell may not). Without it every `systemctl --user` call fails, so
/// install reports that plainly instead of a cryptic bus error.
#[cfg(target_os = "linux")]
fn user_bus_alive() -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "show", "-p", "Version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Linger state for `user`: `Some(true)` on, `Some(false)` off, `None` when
/// loginctl can't tell us. Without lingering the user manager — and our
/// service — stops at logout, which is the classic "startup install does
/// nothing" on headless servers.
#[cfg(target_os = "linux")]
fn linger_state(user: &str) -> Option<bool> {
    let out = std::process::Command::new("loginctl")
        .args(["show-user", user, "-p", "Linger", "--value"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_linger_value(String::from_utf8_lossy(&out.stdout).trim())
}

/// Parse `loginctl ... Linger` output (`yes`/`no`, anything else unknown).
#[cfg(target_os = "linux")]
fn parse_linger_value(s: &str) -> Option<bool> {
    match s {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn current_user() -> Option<String> {
    std::env::var("USER")
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .or_else(|| {
            dirs::home_dir()?
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
}

/// Best-effort: enable lingering so the service survives logout. Works when
/// already privileged (e.g. root); ordinary users get guidance instead.
#[cfg(target_os = "linux")]
fn try_enable_linger(user: &str) -> bool {
    std::process::Command::new("loginctl")
        .args(["enable-linger", user])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Parse `systemctl --user is-active` output.
#[cfg(target_os = "linux")]
fn parse_active_state(out: &str) -> bool {
    out.trim() == "active"
}

#[cfg(target_os = "linux")]
fn service_active() -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "is-active", SERVICE_NAME])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or(false, |s| parse_active_state(&s))
}

#[cfg(target_os = "linux")]
fn install_linux(port: u16, bind: &str) -> io::Result<String> {
    let exe = current_exe_string()?;
    let exec = format!("{} --serve --bind {} --port {}", exe, bind, port);
    // Ports below 1024 need CAP_NET_BIND_SERVICE on the binary: try to
    // grant it up front (root or passwordless sudo) so the service's first
    // start can actually bind, and ride the outcome note on every message.
    let cap_note = if port < 1024 {
        low_port_note(port, &exe, Some(grant_bind_capability(&exe)))
    } else {
        String::new()
    };
    if systemctl_available() {
        if let Some(path) = systemd_unit_path() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let unit = format!(
                "[Unit]\nDescription=ping-uin network monitor (read-only LAN status page)\nAfter=network-online.target\nWants=network-online.target\n\n\
                [Service]\nType=simple\nExecStart={}\nRestart=always\nRestartSec=5\n\n\
                [Install]\nWantedBy=default.target\n",
                exec
            );
            fs::write(&path, unit)?;
            if !user_bus_alive() {
                return Ok(format!(
                    "installed {} but no user bus here (plain SSH/su shell?) — start it with: systemctl --user start {} (needs a systemd user session){}",
                    path.display(),
                    SERVICE_NAME,
                    cap_note
                ));
            }
            let _ = std::process::Command::new("systemctl").args(["--user", "daemon-reload"]).output();
            let _ = std::process::Command::new("systemctl").args(["--user", "enable", SERVICE_NAME]).output();
            // Restart, not just enable --now: a re-install with new flags
            // (e.g. a different --port) must pick them up instead of
            // leaving the old unit running.
            let restarted = std::process::Command::new("systemctl")
                .args(["--user", "restart", SERVICE_NAME])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            let mut msg = if restarted && service_active() {
                format!("installed + running {}", path.display())
            } else {
                format!(
                    "installed {} but the service didn't start — check: journalctl --user -u {}",
                    path.display(),
                    SERVICE_NAME
                )
            };
            // Without lingering the user manager (and our service) stops at
            // logout — the classic headless "startup does nothing".
            match current_user().and_then(|u| linger_state(&u).map(|l| (u, l))) {
                Some((_, true)) => {}
                Some((user, false)) => {
                    if try_enable_linger(&user) {
                        msg.push_str(" (lingering enabled so it survives logout)");
                    } else {
                        msg.push_str(&format!(
                            " — NOTE: lingering is off, so it stops at logout; run once as admin: sudo loginctl enable-linger {}",
                            user
                        ));
                    }
                }
                None => {
                    msg.push_str(" (couldn't check lingering; if it stops at logout run: sudo loginctl enable-linger $USER)");
                }
            }
            msg.push_str(&cap_note);
            return Ok(msg);
        }
    }
    // Fallback: XDG autostart (works without systemd, e.g. WSL/desktop sessions).
    if let Some(path) = autostart_desktop_path() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let desktop = format!(
            "[Desktop Entry]\nType=Application\nName=ping-uin\nComment=Network monitor status page\nExec={}\nX-GNOME-Autostart-enabled=true\n",
            exec
        );
        fs::write(&path, desktop)?;
        return Ok(format!("installed XDG autostart {} (starts at graphical login){}", path.display(), cap_note));
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "cannot determine config dir"))
}

#[cfg(target_os = "linux")]
fn uninstall_linux() -> io::Result<String> {
    let mut removed = Vec::new();
    let _ = std::process::Command::new("systemctl").args(["--user", "disable", "--now", SERVICE_NAME]).output();
    if let Some(path) = systemd_unit_path() {
        if path.exists() {
            fs::remove_file(&path)?;
            removed.push(path.display().to_string());
        }
    }
    if let Some(path) = autostart_desktop_path() {
        if path.exists() {
            fs::remove_file(&path)?;
            removed.push(path.display().to_string());
        }
    }
    let _ = std::process::Command::new("systemctl").args(["--user", "daemon-reload"]).output();
    if removed.is_empty() {
        Ok("nothing installed".to_string())
    } else {
        Ok(format!("removed {}", removed.join(", ")))
    }
}

// ── Windows ──

#[cfg(target_os = "windows")]
fn install_windows(port: u16, bind: &str) -> io::Result<String> {
    let exe = current_exe_string()?;
    let tr = format!("\"{}\" --serve --bind {} --port {}", exe, bind, port);
    let out = std::process::Command::new("schtasks")
        .args(["/create", "/tn", TASK_NAME, "/tr", &tr, "/sc", "onlogon", "/rl", "limited", "/f"])
        .output()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("schtasks failed: {}", e)))?;
    if out.status.success() {
        Ok(format!("installed Scheduled Task '{}' (on logon)", TASK_NAME))
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("schtasks said: {}", String::from_utf8_lossy(&out.stderr).trim()),
        ))
    }
}

#[cfg(target_os = "windows")]
fn uninstall_windows() -> io::Result<String> {
    let out = std::process::Command::new("schtasks")
        .args(["/delete", "/tn", TASK_NAME, "/f"])
        .output()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("schtasks failed: {}", e)))?;
    if out.status.success() {
        Ok(format!("removed Scheduled Task '{}'", TASK_NAME))
    } else {
        Ok("nothing installed".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_args_format() {
        assert_eq!(serve_args(8080, "0.0.0.0"), "--serve --bind 0.0.0.0 --port 8080");
    }

    #[test]
    fn describe_mentions_serve() {
        let d = describe(8080, "0.0.0.0");
        assert!(d.contains("--serve"));
        assert!(d.contains("8080"));
    }

    #[test]
    fn low_port_note_guides_privilege() {
        // High ports: no note at all, whatever the grant outcome.
        assert!(low_port_note(8080, "/usr/bin/ping-uin", None).is_empty());
        assert!(low_port_note(8080, "/usr/bin/ping-uin", Some(false)).is_empty());
        // Plan (not yet attempted): says a grant will be tried.
        let plan = low_port_note(80, "/usr/bin/ping-uin", None);
        assert!(plan.contains("cap_net_bind_service"));
        // Granted: confirms, with the re-apply-after-update reminder.
        let ok = low_port_note(80, "/usr/bin/ping-uin", Some(true));
        assert!(ok.contains("granted"));
        assert!(ok.contains("setcap"));
        // Failed: exact manual command + the sysctl alternative.
        let failed = low_port_note(80, "/usr/bin/ping-uin", Some(false));
        assert!(failed.contains("sudo setcap 'cap_net_bind_service=+ep' /usr/bin/ping-uin"));
        assert!(failed.contains("ip_unprivileged_port_start=80"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linger_and_active_parsing() {
        assert_eq!(parse_linger_value("yes"), Some(true));
        assert_eq!(parse_linger_value("no"), Some(false));
        assert_eq!(parse_linger_value("maybe"), None);
        assert_eq!(parse_linger_value(""), None);
        assert!(parse_active_state("active\n"));
        assert!(!parse_active_state("inactive\n"));
        assert!(!parse_active_state("failed\n"));
        assert!(!parse_active_state(""));
    }
}
