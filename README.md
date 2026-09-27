# ping-uin

A btop-style terminal UI for monitoring hosts. Built for sysadmins who live in a terminal.

![status](https://img.shields.io/badge/status-active-brightgreen)
![built with](https://img.shields.io/badge/built%20with-Rust%20%2B%20ratatui-orange)
![ai created](https://img.shields.io/badge/creation-stripped--warmed--fully%20AI-blue)
![license](https://img.shields.io/badge/license-MIT-blue)

> **This project is 100% AI-created** — from the original PS/PowerShell ping loop
> to the Rust + ratatui TUI, every refactor, feature, theme, and bug fix was
> written by an AI coding assistant. Treat it accordingly: it works, but it
> has not been through a human review process.

A pink little penguin face ((•O•)) watches over your network.

---

## What it is

`ping-uin` is a lightweight TUI that pings (or TCP-checks) your devices on
**per-host intervals** and shows the results in a rolling, btop-style interface:

* **Per-host intervals** — `30s`, `5m`, or `2h` per host, down to 5 seconds
* **Ping, TCP, or custom commands** — ICMP by default, a port (`db:5432`) for connect checks, or any shell command (exit 0 = up)
* **WARN state** — per-host latency thresholds turn slow-but-up hosts amber
* **Per-host history strip** — `■` green blocks for up, red `_` for down, newest on the left
* **Flap + flash** — flapping hosts read `FLAP`, fresh transitions flash underlined, outages ticker `↓ 14m`
* **Grouped view** with collapsible groups (`Enter`) and down-first ordering
* **View picker** — order by down-first / up-first / name / group, or filter to **down only**
* **`/` search** across names, aliases, and groups
* **24h SLA column** straight from the ping log
* **Maintenance mutes** (`!` = 1h) and **upstream dependencies** that silence downstream noise
* **Escalating alerts** — `still_down_5m` / `still_down_30m` webhooks plus bell
* **Notifications** — generic webhook POST plus optional terminal bell on transitions
* **Headless `--once` mode** — one pass over all hosts as text or JSON, exit code doubles as the probe result
* **HTML status export** — one keypress renders a shareable status page
* **Read-only LAN web page** — `W` serves the live table as a website (grouped by label, themed like the TUI), `--serve` runs it headless, `--install-startup` starts it on boot
* **Device sync** — `Y` pairs instances with join codes; adds, edits, and removals converge both ways
* **Mouse support**, compact density (`v`), session restore, first-run wizard
* **Multiple themes** — `btop`, `dracula`, `nord`, `gruvbox-dark`, `ayu-light`, `archwave`
* **CSV bulk import** — dump your host list in a spreadsheet, import in one press
* **Alias your IPs** — turn `1.1.1.1` into `Cloudflare`, `server3.internal` into `DB host`

Built for sysadmins monitoring servers, routers, VPN endpoints, IoT devices,
or anything else you'd rather not drop into a heavy dashboard for.

---

## Quick start

```bash
# Debian/Ubuntu (amd64) — signed apt repo
curl -fsSL https://altosaxplayer.github.io/ping-uin/apt/key.asc | sudo gpg --dearmor -o /usr/share/keyrings/ping-uin-archive-keyring.gpg
echo "deb [signed-by=/usr/share/keyrings/ping-uin-archive-keyring.gpg] https://altosaxplayer.github.io/ping-uin/apt stable main" | sudo tee /etc/apt/sources.list.d/ping-uin.list
sudo apt update && sudo apt install ping-uin
```

```bash
# anywhere else: clone, build, run
cargo install --path .   # builds the 'ping-uin' binary
cargo run --release      # or just run it straight
```
Single `.deb` files are also attached to every
[GitHub release](https://github.com/altosaxplayer/ping-uin/releases) for
`dpkg -i`. Note: ICMP checks shell out to the system `ping` binary — on
minimal distros install it first (`sudo apt install iputils-ping`).

A starter host set is built in on first launch, so it works even before you
add any config: Google DNS (`8.8.8.8`), Cloudflare (`1.1.1.1`), your local
gateway (`192.168.1.1`), and `google.com`. Starting truly empty instead?
A welcome popup offers CSV import, manual add, or the demo set — and the
app re-selects last session's host on startup.

---

## Controls

| Key | Action |
|-----|--------|
| `↑` / `↓` | move selection |
| `Space` / `p` | ping selected host now |
| `a` | add a host (single form) |
| `e` | edit the selected host — name/IP/interval/group/alias/port in one form |
| `d` | delete the selected host (confirmation popup) |
| `h` | per-host history (8h / 24h / 7d, `Tab` compares a second host) |
| `c` | clear stats for selected host |
| `!` | mute/unmute selected host for 1h (maintenance) |
| `v` | compact table density (hides IP + Group columns) |
| `i` | **import from `hosts.csv`** — merge in bulk, new rows added, existing rows updated |
| `E` | export timestamped CSV **plus** an HTML status page |
| `W` | serve read-only LAN web page on/off — `http://<LAN-IP>:8080/`, grouped + themed |
| `Y` | device sync menu — join code, paired hostnames, join/last-sync times |
| `B` | open the served page in the default browser (starts serving first if off) |
| `g` | toggle grouped/flat view |
| `f` | filter by group (`Space` = show all, `Esc` = cancel) |
| `s` | view picker — `off` / down-first / up-first / name / group / **down only** (`1-6` quick-pick, `Space` = show all; group sort hides the per-group up/down tallies) |
| `Esc` | reset view — clear any sort/group filter and show all hosts |
| `Enter` | collapse/expand the selected host's group |
| `/` | search names, aliases, groups (`Enter` keeps, `Esc` clears) |
| `?` | full key-binding cheat sheet |
| mouse | click to select, wheel to scroll |
| `t` | theme picker (live preview, persists) |
| `o` | email (SMTP) alert settings — threshold, escalations, themed like the TUI |
| `u` | check for updates / install when available |
| `M` | full menu (actionable) |
| `q` / `Ctrl+C` | quit (cleanly!) |

> Intervals accept `30s`, `5m`, `2h`, or bare minutes (`2` = 2m), minimum `5s`.
> Set a TCP port per host to check `host:port` connects instead of pinging.

> The bottom menu lives in its own bordered box with a fixed height — it
> never resizes or shifts the table. On narrow windows labels abbreviate,
> and anything left over collapses into a `+N more [M]` marker.
>
> History strips read newest-first: the most recent ping is always the
> leftmost block, and the strip grows left-to-right.

---

## CSV bulk import

Press `i` and the app prompts for a CSV path (defaulting to the `hosts.csv`
in your config dir) and merges it — new rows added, existing rows updated:

```csv
name,interval,group,alias,port,warn_ms,check_cmd,depends_on
8.8.8.8,1m,public-dns,Google DNS,,,,
1.1.1.1,2m,public-dns,Cloudflare,,,,,
server3.internal,5m,router,Server 3,,,,
db.internal,30s,databases,Primary DB,5432,200,,
web.internal,30s,web,Web,,500,,db.internal
192.168.1.1,3m,router,,,,,
```

Intervals accept `30s`/`5m`/`2h` (bare numbers mean minutes); a `port`
turns the row into a TCP connect check, `warn_ms` flags slow-but-up hosts
amber, `check_cmd` runs a shell command instead (exit 0 = up), and
`depends_on` names an upstream whose outage silences this host's alerts.
Shorter legacy files still import fine — missing columns stay unset.

New rows become new pings; existing rows get updated. Sync round-trips both
ways — `hosts.csv` is also rewritten on every in-TUI change, so you can always
edit it by hand and import.

---

## Data & privacy

Everything **stays local** by default. The app talks to the network for
monitoring probes against your own targets, the release checker (GitHub API,
every 15 min), and — only if you configure them — alert webhooks, SMTP email,
the opt-in LAN page, and paired device sync. See [PRIVACY.md](./PRIVACY.md)
for the full accounting of what is stored and what leaves the device.

### Default location

On Linux: `~/.config/ping-uin/`
On macOS: `~/Library/Application Support/ping-uin/`
On Windows: `%APPDATA%\ping-uin\` (usually `C:\Users\<you>\AppData\Roaming\ping-uin\`)

Full details live in [PRIVACY.md](./PRIVACY.md).

| File | Purpose |
|------|---------|
| `ping-uin.json` | full config (gitignored by default — your IPs are yours; an old `ip-top.json` is migrated automatically) |
| `hosts.csv` | bulk-import/export mirror |
| `uptime-log.csv` | rolling ping history (last ~10k events) |

### Portable mode

To keep the data files next to the executable (e.g., on a USB drive or in a
self-contained folder), create an empty file named `ping-uin.portable` in the
same directory as `ping-uin`/`ping-uin.exe`:

```bash
# Linux/macOS
touch ping-uin.portable

# Windows (PowerShell)
New-Item -ItemType File -Name ping-uin.portable
```

On the next launch, `ping-uin.json`, `hosts.csv`, and `uptime-log.csv` will be
read from and written to that same directory instead of the system config path.

### In-place updates

The app checks GitHub releases at startup and then **every 15 minutes**
(failed checks — e.g. launching before the VPN connects — retry every
minute until one succeeds), so the `↑ vX.Y.Z ready — u to update` pill
appears in the **top-right corner** (plus a badge in the menu box) without
restarting. Press **`u`** any time to check manually — press **`u`** again
to install:

- **Portable mode** (marker file or data files next to the binary): the
  running binary is replaced in place (checksum-verified, backup + restore
  on failure). macOS/Linux restart automatically; Windows exits and a small
  PowerShell updater swaps `ping-uin.exe`.
- **Homebrew** (`/Cellar/ping-uin/`): runs `brew upgrade ping-uin`
  (falls back to `altosaxplayer/tap/ping-uin`) in the background.
- **Winget** (Windows): runs `winget upgrade altosaxplayer.ping-uin`
  in place — just quit and relaunch when it completes.
- **Anywhere else writable** (e.g. `~/.cargo/bin` you own): same in-place
  flow as portable. If the install dir isn't writable you'll get a clear
  message instead of a late failure — update with
  `brew upgrade ping-uin` / `winget upgrade altosaxplayer.ping-uin` /
  `cargo install --path .`.

Theme, group, sort/view, and collapsed-group prefs persist in `ping-uin.json`.
A corrupt config is backed up to `ping-uin.json.corrupt` instead of being
discarded.

### Notifications

`ping-uin` can shout when hosts change state. Webhook + bell live in
`ping-uin.json` (edit the file directly); email has a TUI form — press `o`:

```json
{
  "webhook_url": "https://hooks.slack.com/services/…",
  "notify_bell": true,
  "smtp": {
    "enabled": true,
    "host": "smtp.gmail.com",
    "port": 587,
    "username": "you@gmail.com",
    "password": "app-password",
    "from": "ping-uin@gmail.com",
    "to": "ops@example.com, noc@example.com",
    "use_tls": true,
    "down_threshold": 3,
    "escalations": false
  }
}
```

- `webhook_url` — POSTs `{app, host, status, event, latency_ms, timestamp}`
  JSON on every up/down transition (Slack-compatible shape), plus
  `still_down_5m` / `still_down_30m` escalation events for long outages.
- `notify_bell` — rings the terminal bell on down-transitions (and again
  at the 30-minute escalation).
- `smtp` — sends **one DOWN email after `down_threshold` consecutive failures**
  (default 3, 1–100, one mail per outage), plus a **recovery UP email** when
  the host comes back. Outage-mail state is persisted in `ping-uin.json`, so
  quitting and reopening never resends DOWN mail for an already-mailed
  outage (and a still-owed recovery still goes out). Set `"escalations": true` to also mail the
  `still_down_5m` / `still_down_30m` reminders. Port 465 uses
  SMTPS, other ports use STARTTLS; set `use_tls: false` only for local
  plaintext relays. `to` accepts a comma-separated list. Delivery is
  fire-and-forget so a slow relay never stalls pinging.

Emails are styled with the **currently active theme**: page background,
card, and text colors come from the theme, and the status is unmissable —
a full-width banner in the theme's danger color (`● DOWN`) or good color
(`● UP`), with host, target, group, timestamp, latency, and streak.

Downstream hosts with `depends_on` pointing at a down upstream read `DEP`
and stay silent — fix the upstream, not the noise. `!` mutes a host for an
hour of maintenance (countdown in the Latency column, excluded from tallies).

### Headless mode

For cron, systemd timers, or quick checks without the TUI:

```bash
ping-uin --once                  # text table, exit 0 = all up, 2 = any down
ping-uin --once --format json    # {"hosts": […], "down": 0} for scripts
```

Checks run in parallel (ICMP and TCP alike), touch no files, and the exit
code doubles as the probe result.

### Web status page (read-only, LAN-visible, opt-in)

Nothing is served unless you ask. Press **`W`** in the TUI to start serving
the live table as a website, press **`W`** again to stop it — or run it
headless (no terminal needed) with `--serve`. Press **`B`** to open the
page in your default browser (starts serving first when off). The `M` menu
always shows whether the page is currently being served.

```bash
ping-uin --serve --bind 0.0.0.0 --port 8080
# HTML:  http://<this-host>:8080/          (auto-refreshes every 15s)
# Health: http://<this-host>:8080/health   ("ok", for supervisors/monitors)
```

* **Read-only by design** — `GET /` serves HTML and nothing else; there is
  no ping-now, mute, or config surface, so no auth is needed on a trusted
  LAN. Unknown paths get `404`, non-GET gets `405`.
* **Bind `0.0.0.0`** (the default) to make it visible anywhere on the local
  network; use `--bind 127.0.0.1` for local-only. The startup banner and the
  `W` popup print the real LAN IP + port (e.g. `http://192.168.1.42:8080/`),
  so you know exactly what to type from other devices.
* **Grouped by label** — like the TUI grouped view: collapsible per-group
  cards (no JS, native `<details>`), groups and hosts down-first, per-group
  up/down tallies, plus one-click group filter chips. `?group=<label>`
  filters to one group.
* **Modern card UI, zero JS** — status summary pills, per-status badges,
  sticky table headers, row hover, system fonts with monospace numerals,
  and a responsive layout that stacks on phones. Auto-refresh keeps it live;
  sorting, filtering, and collapsing are all plain links.
* **Sorting** — click any table header (Host, Status, Latency, Group, Uptime,
  SLA 24h) for a flat sorted table; clicking the active header toggles
  asc/desc. Same thing directly: `/?sort=status&order=desc` accepts
  `name|status|group|latency|uptime|sla` + `asc|desc` (`status` puts problems
  first, like down-first view).
* **Themed like the desktop** — the page uses the serving instance's active
  TUI theme (or its configured theme headless), so the website visibly
  matches the machine serving it. Switch themes in the TUI and the page
  follows within seconds.
* **Zero new dependencies** — plain-stdlib HTTP baked into the same single
  binary. In the TUI, `W` serves the exact live session; `--serve` runs its
  own probing loop with the same intervals, logging, and webhooks.

### Device sync (bidirectional, join codes, opt-in)

Keep two devices in two places on the same host list — no discovery, no
accounts, no cloud. One side shows a join code, the other types it in:

```bash
# on device A (or press Y → g in its TUI)
ping-uin --sync-code --port 8080
# PUIN-192.168.1.42-8080-abcd-efgh-jklm

# on device B (or press Y → j in its TUI and paste it)
ping-uin --sync-join PUIN-192.168.1.42-8080-abcd-efgh-jklm --port 8080
```

* **Bidirectional** — adds, edits, *and* removals propagate both ways about
  once a minute while both instances run (TUI or `--serve`). Last-write-wins
  per host; deletes travel as tombstones so they can't be resurrected by a
  stale peer. Probing, history, and logs stay local — only the host list
  converges.
* **Y menu** — press **`Y`** in the TUI: shows this device's hostname and
  join code, plus every paired device with its hostname, join date/time, and
  last sync time (`[g]` new code, `[j]` join, `[1-9]` forget a peer).
* **CLI twins** — `--sync-peers` lists pairs, `--sync-forget <ip:port>`
  unpairs (hosts stay, pushes stop). CLI commands edit the config file, so
  quit the TUI on that device first if it's running.
* **Joins failing?** Both devices must be on the same network with ping-uin
  running (TUI or `--serve`) and listening — the Y menu shows the listener
  address, and a busy port is reported instead of failing silently. Same
  Wi-Fi (no client isolation), allow ping-uin through the device firewall,
  and make sure the IP in the join code is reachable (VPNs/Docker can put a
  wrong one in — join errors say which of these it looks like). If the
  code's IP is wrong, keep the code and append the right address when
  joining: `Y → j`, paste `PUIN-…-…`, add ` @ 192.168.1.42`, Enter
  (CLI: `--sync-join "CODE @ 192.168.1.42"` or `--sync-join CODE 192.168.1.42`).
  Even without that: if the code's address is unreachable, joining
  automatically scans your subnet for the peer, so a stale IP usually
  just works (wrong tokens and outdated peers are never scanned).

### Start on boot (opt-in)

Nothing is installed automatically. Only `--install-startup` creates a boot
entry, and `--uninstall-startup` removes it again. To start the status page
each time the machine restarts:

```bash
ping-uin --install-startup --bind 0.0.0.0 --port 8080
ping-uin --startup-status      # show whether it is installed
ping-uin --uninstall-startup   # remove it again
```

* **macOS**: LaunchAgent `~/Library/LaunchAgents/com.ping-uin.plist`
  (starts at login, kept alive, logs to `~/Library/Logs/ping-uin.log`).
* **Linux**: systemd user service (`systemctl --user enable --now ping-uin`),
  falling back to XDG autostart (`~/.config/autostart/ping-uin.desktop`)
  where systemd isn't available.
* **Windows**: Scheduled Task `ping-uin` (on logon).

---

## Why "ping-uin"?

Short for *ping* + *pnpm what?* honestly just because it's a fun word and the
((•O•)) face looked like a tiny penguin. Better suggestions welcome.

---

## Where this came from

Fully AI-created, evolving in chat-driven sessions:

1. Old PowerShell loop → Python + `rich` live table
2. Re-written in `Rust` + `ratatui` with real-time updates
3. Themes, grouping, view picker, CSV bulk import, self-updates, etc., added iteratively

No human review was involved. If anything weird happens, open an issue and
the next AI watch-commander will fix it. Probably.

---

## License

[MIT](./LICENSE). Permissive as it gets — fork it, remix it, ship it in your
company tooling. Just keep the copyright header.

---

> **Pro tip for sysadmins:** point the CSV at your asset inventory export
> every morning and keep `ping-uin` running in a tmux pane. Ships as a
> single binary — no metrics endpoint, no mutable web UI. Just pinging,
> plus an optional read-only status page (`W` / `--serve`) for the LAN.
