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
            "systemd user service ~/.config/systemd/user/{}.service -> '{}' {} (enable --now; falls back to XDG autostart when systemd is unavailable)",
            SERVICE_NAME,
            exe,
            serve_args(port, bind)
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

#[cfg(target_os = "linux")]
fn install_linux(port: u16, bind: &str) -> io::Result<String> {
    let exe = current_exe_string()?;
    let exec = format!("{} --serve --bind {} --port {}", exe, bind, port);
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
            let _ = std::process::Command::new("systemctl").args(["--user", "daemon-reload"]).output();
            let enable = std::process::Command::new("systemctl").args(["--user", "enable", "--now", SERVICE_NAME]).output();
            match enable {
                Ok(o) if o.status.success() => return Ok(format!("installed + enabled {}", path.display())),
                Ok(o) => {
                    return Ok(format!(
                        "installed {} (systemctl said: {}; start it with: systemctl --user start {})",
                        path.display(),
                        String::from_utf8_lossy(&o.stderr).trim(),
                        SERVICE_NAME
                    ));
                }
                Err(e) => return Ok(format!("installed {} (systemctl not run: {})", path.display(), e)),
            }
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
        return Ok(format!("installed XDG autostart {}", path.display()));
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
}
