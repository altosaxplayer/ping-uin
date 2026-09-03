//! Probing: ICMP ping via the system `ping` binary, TCP connect checks,
//! per-host schedules, and transition webhooks.

use std::env;
use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::config::HostConfig;

#[derive(Clone)]
pub struct HostSchedule {
    pub name: String,
    pub port: Option<u16>,
    pub interval_secs: u64,
    pub next_ping: Instant,
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

/// Route a check: TCP when the host has a port, ICMP ping otherwise.
pub fn check_host(host: &str, port: Option<u16>, timeout_ms: u64, re: &Regex) -> (bool, f64) {
    match port {
        Some(p) => tcp_check(host, p, timeout_ms),
        None => ping_host(host, timeout_ms, re),
    }
}

/// Fire-and-forget webhook POST on up/down transitions. Never blocks the UI.
pub fn post_webhook(url: String, host: String, up: bool, latency_ms: f64, timestamp: String) {
    std::thread::spawn(move || {
        let status = if up { "up" } else { "down" };
        let body = serde_json::json!({
            "app": "ping-uin",
            "host": host,
            "status": status,
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
    fn tcp_check_closed_port_is_down_fast() {
        // Port 1 on localhost is (almost) certainly closed; must fail fast.
        let start = Instant::now();
        let (up, _) = tcp_check("127.0.0.1", 1, 500);
        assert!(!up);
        assert!(start.elapsed() < Duration::from_secs(10));
    }
}
