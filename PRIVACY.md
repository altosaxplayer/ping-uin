# ping-uin Privacy Policy

**Effective:** September 27, 2026
**Applies to:** ping-uin (all releases; see [Version scope](#version-scope) for
feature differences across versions)

ping-uin is a local-first network monitoring tool. It has no accounts, no
analytics, no telemetry, and no crash reporting. It sends nothing to the
developer or to any third party except where this policy says so — and every
such case is either the monitoring you configured or an update check you can
see in the source.

## Data stored on your device

Everything ping-uin knows lives in files on your own machine. Nothing is
uploaded anywhere by default.

| Data | Purpose | File |
|------|---------|------|
| Monitored targets: hostnames, IP addresses, aliases, groups, check intervals, TCP ports, latency thresholds, custom check commands, upstream dependencies, mute windows | The host list you asked to monitor | `ping-uin.json` |
| Bulk import/export mirror of the host list | Editing hosts as CSV | `hosts.csv` |
| Rolling availability history (timestamp, host, UP/DOWN, latency) | Uptime stats, SLA, history views | `uptime-log.csv` |
| UI preferences (theme, grouping, sort, collapsed groups, compact mode, selected host) | Restoring your workspace | `ping-uin.json` |
| Notification settings: webhook URL, bell preference, SMTP relay settings **including the username/password you entered** | Sending the alerts you configured | `ping-uin.json` |
| Outage-mail state (which hosts already got a DOWN mail, escalation level) | Not re-mailing the same outage after a restart | `ping-uin.json` |
| Sync pairing (secret token, paired device addresses/tokens/hostnames, deletion tombstones) | Keeping paired devices in sync (only if you pair them) | `ping-uin.json` |

### Where the files live

- **Linux:** `~/.config/ping-uin/`
- **macOS:** `~/Library/Application Support/ping-uin/`
- **Windows:** `%APPDATA%\ping-uin\`
- **Portable mode:** next to the executable, when a `ping-uin.portable` marker
  file (or existing data files) sits beside the binary

## Retention and deletion

- The availability log is capped at roughly the last 10,000 events; older
  entries are discarded automatically.
- Sync deletion records expire after 30 days.
- Host entries, preferences, and notification settings persist until you
  change them: delete a host (`d` in the TUI), clear its stats (`c`), edit
  `ping-uin.json` directly, or delete the data files outright.
- Uninstalling the app does **not** delete the data directory. To remove
  everything, delete the directory listed above after uninstalling.

## Network activity

ping-uin talks to the network only for the job you gave it, plus one
automatic check:

1. **Monitoring probes (you configure the targets).** ICMP ping, TCP
   connects, or shell commands against the hosts in your list, on your
   per-host schedule. Traffic goes only to those targets.
2. **Release update check (automatic).** On startup and every 15 minutes the
   app queries `api.github.com` for the latest ping-uin release tag. Only
   the app version is compared; no personal data is sent. There is no
   opt-out in the UI — block the binary's network access at your firewall
   if you want it fully silent.
3. **Alert webhooks (only if you configure `webhook_url`).** A JSON POST
   (`app`, `host`, `status`, `event`, `latency_ms`, `timestamp`) goes to
   your endpoint on up/down transitions and long-outage escalations.
4. **Alert email (only if you configure SMTP).** Mail is delivered through
   the relay **you** entered, using the credentials **you** entered, to the
   recipients **you** entered. Store an app-specific password, not your main
   one.
5. **LAN status page (only when you turn it on).** Pressing `W` or running
   `--serve` serves a read-only page on your local network. It is off unless
   you enable it.
6. **Device sync (only with devices you pair).** Sync pushes your host list
   to paired devices about once a minute over your LAN, authenticated with
   the pairing token from your join code. Nothing syncs without pairing.

## What leaves your device — summary

- Probes reach only the hosts you configured.
- The GitHub release check reaches only `api.github.com` (version comparison).
- Webhook/email content reaches only the endpoints and recipients you configured.
- Sync content reaches only devices you explicitly paired.
- **Nothing else leaves the device.** No analytics, telemetry, advertising,
  tracking, or account data of any kind.

## Version scope

This policy describes the current product. Older releases collect a subset
of the above: releases in the 0.1.x series store host entries, aliases,
imported host data, availability history, and preferences locally, and
perform monitoring probes plus the GitHub release check. SMTP alert settings
and outage-mail state, the LAN status page, device sync pairing data, and
startup-service entries were introduced in later releases and do not exist
in 0.1.x data files.

## Contact

Questions about this policy: open an issue at
https://github.com/altosaxplayer/ping-uin/issues
