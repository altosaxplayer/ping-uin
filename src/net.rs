//! Probing: ICMP ping via the system `ping` binary, TCP connect checks,
//! per-host schedules, and transition webhooks.

use std::env;
use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::config::{HostConfig, SmtpConfig};

#[derive(Clone)]
pub struct HostSchedule {
    pub name: String,
    pub port: Option<u16>,
    pub interval_secs: u64,
    pub next_ping: Instant,
    /// Set while a worker owns this host; prevents overlapping checks.
    pub inflight: bool,
    /// Mute window end (unix secs). Evaluated at dispatch so expiry
    /// resumes checks without any rebuild.
    pub muted_until: Option<i64>,
    pub check_cmd: Option<String>,
}

pub fn host_jitter(name: &str, interval_secs: u64) -> Duration {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut s = DefaultHasher::new();
    name.hash(&mut s);
    let hash = s.finish();
    let max_jitter = (interval_secs / 2).clamp(1, 30);
    Duration::from_secs(hash % max_jitter)
}

pub fn schedules_from_config(hosts: &[HostConfig]) -> Vec<HostSchedule> {
    let now = Instant::now();
    hosts
        .iter()
        .map(|h| {
            let interval_secs = h.effective_interval_secs();
            HostSchedule {
                name: h.name.clone(),
                port: h.port,
                interval_secs,
                next_ping: now + host_jitter(&h.name, interval_secs),
                inflight: false,
                muted_until: h.muted_until,
                check_cmd: h.check_cmd.clone(),
            }
        })
        .collect()
}

pub fn ping_host(host: &str, timeout_ms: u64, re: &Regex) -> (bool, f64) {
    let os = env::consts::OS;
    let output = match os {
        "windows" => Command::new("ping")
            .args(["-n", "1", "-w", &timeout_ms.to_string(), host])
            .output(),
        "macos" => Command::new("ping")
            .args(["-c", "1", "-W", &timeout_ms.to_string(), host])
            .output(),
        _ => Command::new("ping")
            .args(["-c", "1", "-W", &timeout_ms.div_ceil(1000).to_string(), host])
            .output(),
    };
    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(cap) = re.captures(&text) {
                if let Ok(lat) = cap[1].parse::<f64>() {
                    return (true, lat);
                }
            }
            (true, 0.0)
        }
        _ => (false, 0.0),
    }
}

/// TCP connect check. Returns (up, connect latency ms).
pub fn tcp_check(host: &str, port: u16, timeout_ms: u64) -> (bool, f64) {
    let timeout = Duration::from_millis(timeout_ms.max(100));
    let addrs: Vec<SocketAddr> = match (host, port).to_socket_addrs() {
        Ok(it) => it.collect(),
        Err(_) => return (false, 0.0),
    };
    let start = Instant::now();
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(_) => return (true, start.elapsed().as_secs_f64() * 1000.0),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(_) => continue,
        }
    }
    (false, 0.0)
}

/// Custom shell check: exit 0 = up, wall-clock time = latency.
/// Killed at `timeout_ms`; timeout counts as down.
pub fn cmd_check(cmd: &str, timeout_ms: u64) -> (bool, f64) {
    let timeout = Duration::from_millis(timeout_ms.max(500));
    let start = Instant::now();
    let mut child = match shell_command(cmd).spawn() {
        Ok(c) => c,
        Err(_) => return (false, 0.0),
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                return (status.success(), ms);
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (false, 0.0);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return (false, 0.0),
        }
    }
}

fn shell_command(cmd: &str) -> Command {
    if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", cmd]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", cmd]);
        c
    }
}

/// Route a check: custom command first, then TCP when a port is set,
/// otherwise ICMP ping.
pub fn check_host(
    host: &str,
    port: Option<u16>,
    check_cmd: Option<&str>,
    timeout_ms: u64,
    re: &Regex,
) -> (bool, f64) {
    if let Some(cmd) = check_cmd.filter(|c| !c.trim().is_empty()) {
        return cmd_check(cmd, timeout_ms);
    }
    match port {
        Some(p) => tcp_check(host, p, timeout_ms),
        None => ping_host(host, timeout_ms, re),
    }
}

/// Fire-and-forget SMTP email. Never blocks the UI; failures are silent
/// (logged to stderr in debug builds only) so a bad mail relay can't stall
/// pinging. `subject`, `text_body`, and `html_body` should already be built
/// by the caller (see `build_alert_email` in main.rs for theme styling).
pub fn send_smtp_email(cfg: SmtpConfig, subject: String, text_body: String, html_body: String) {
    if !cfg.is_configured() {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = send_smtp_email_blocking(&cfg, &subject, &text_body, &html_body) {
            #[cfg(debug_assertions)]
            eprintln!("smtp send failed: {}", e);
        }
    });
}

fn send_smtp_email_blocking(
    cfg: &SmtpConfig,
    subject: &str,
    text_body: &str,
    html_body: &str,
) -> Result<(), String> {
    use lettre::message::{header::ContentType, Mailbox, Message, MultiPart, SinglePart};
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{SmtpTransport, Transport};

    let from: Mailbox = cfg
        .from
        .trim()
        .parse()
        .map_err(|e| format!("bad from address: {}", e))?;
    let recipients = cfg.recipients();
    if recipients.is_empty() {
        return Err("no recipients".to_string());
    }
    let mut builder = Message::builder().from(from).subject(subject);
    for r in &recipients {
        let mb: Mailbox = r.parse().map_err(|e| format!("bad to address '{}': {}", r, e))?;
        builder = builder.to(mb);
    }
    let msg = builder
        .multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::builder().header(ContentType::TEXT_PLAIN).body(text_body.to_string()))
                .singlepart(SinglePart::builder().header(ContentType::TEXT_HTML).body(html_body.to_string())),
        )
        .map_err(|e| format!("build message: {}", e))?;

    let base = if cfg.use_tls {
        if cfg.port == 465 {
            SmtpTransport::relay(&cfg.host).map_err(|e| format!("smtp relay: {}", e))?
        } else {
            SmtpTransport::starttls_relay(&cfg.host).map_err(|e| format!("smtp starttls: {}", e))?
        }
    } else {
        SmtpTransport::builder_dangerous(&cfg.host)
    };
    let with_port = base.port(cfg.port).timeout(Some(Duration::from_secs(15)));
    let transport = match (cfg.username.clone(), cfg.password.clone()) {
        (Some(user), Some(pass)) if !user.trim().is_empty() => with_port
            .credentials(Credentials::new(user, pass))
            .build(),
        _ => with_port.build(),
    };
    transport.send(&msg).map(|_| ()).map_err(|e| format!("send: {}", e))
}

/// Fire-and-forget webhook POST on transitions and escalations.
/// `event` is e.g. "down", "up", "still_down_5m", "still_down_30m".
/// Never blocks the UI.
pub fn post_webhook(
    url: String,
    host: String,
    up: bool,
    latency_ms: f64,
    timestamp: String,
    event: &str,
) {
    let event = event.to_string();
    std::thread::spawn(move || {
        let status = if up { "up" } else { "down" };
        let body = serde_json::json!({
            "app": "ping-uin",
            "host": host,
            "status": status,
            "event": event,
            "latency_ms": latency_ms,
            "timestamp": timestamp,
        })
        .to_string();
        let _ = ureq::post(&url)
            .set("Content-Type", "application/json")
            .set("User-Agent", "ping-uin-notify")
            .timeout(Duration::from_secs(10))
            .send_string(&body);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_is_deterministic_and_bounded() {
        let a = host_jitter("db.internal", 120);
        let b = host_jitter("db.internal", 120);
        assert_eq!(a, b);
        assert!(a <= Duration::from_secs(30));
        // Short intervals still get some spread without panicking.
        let _ = host_jitter("x", 5);
        let _ = host_jitter("x", 0);
    }

    #[test]
    fn schedules_use_effective_interval() {
        let hosts = vec![HostConfig::new("a", 30, "g", None, Some(5432))];
        let sched = schedules_from_config(&hosts);
        assert_eq!(sched.len(), 1);
        assert_eq!(sched[0].interval_secs, 30);
        assert_eq!(sched[0].port, Some(5432));
    }

    #[test]
    fn cmd_check_true_is_up_false_is_down() {
        let (up, ms) = cmd_check("true", 2000);
        assert!(up);
        assert!(ms < 2000.0);
        let (up, _) = cmd_check("false", 2000);
        assert!(!up);
        let (up, _) = cmd_check("exit 3", 2000);
        assert!(!up);
    }

    #[test]
    fn tcp_check_closed_port_is_down_fast() {
        // Port 1 on localhost is (almost) certainly closed; must fail fast.
        let start = Instant::now();
        let (up, _) = tcp_check("127.0.0.1", 1, 500);
        assert!(!up);
        assert!(start.elapsed() < Duration::from_secs(10));
    }
}
