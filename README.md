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
* **Mouse support**, compact density (`v`), session restore, first-run wizard
* **Multiple themes** — `btop`, `dracula`, `nord`, `gruvbox-dark`, `ayu-light`, `archwave`
* **CSV bulk import** — dump your host list in a spreadsheet, import in one press
* **Alias your IPs** — turn `1.1.1.1` into `Cloudflare`, `server3.internal` into `DB host`

Built for sysadmins monitoring servers, routers, VPN endpoints, IoT devices,
or anything else you'd rather not drop into a heavy dashboard for.

---

## Quick start

```bash
# clone, build, run
cargo install --path .   # builds the 'ping-uin' binary
cargo run --release      # or just run it straight
```

A starter host set is built in on first launch, so it works even before you
add any config: Google DNS (`8.8.8.8`), Cloudflare (`1.1.1.1`), your local
gateway (`192.168.1.1`), and `google.com`.

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
| `g` | toggle grouped/flat view |
| `f` | filter by group (`Space` = show all, `Esc` = cancel) |
| `s` | view picker — `off` / down-first / up-first / name / group / **down only** (`1-6` quick-pick, `Space` = show all) |
| `Esc` | reset view — clear any sort/group filter and show all hosts |
| `Enter` | collapse/expand the selected host's group |
| `/` | search names, aliases, groups (`Enter` keeps, `Esc` clears) |
| `?` | full key-binding cheat sheet |
| mouse | click to select, wheel to scroll |
| `t` | theme picker (live preview, persists) |
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

Everything **stays local** by default. Two features intentionally talk to
the network: the release checker (GitHub API, every 15 min) and the optional
`webhook_url` you configure yourself for transition alerts.

### Default location

On Linux/macOS: `~/.config/ping-uin/`
On Windows: `%APPDATA%\ping-uin\` (usually `C:\Users\<you>\AppData\Roaming\ping-uin\`)

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

The app checks GitHub releases at startup and then **every 15 minutes**,
so the `↑ vX.Y.Z ready — u to update` pill appears in the **top-right corner**
(plus a badge in the menu box) without restarting. Press **`u`** any time to
check manually — press **`u`** again to install:

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

`ping-uin` can shout when hosts change state. Both live in `ping-uin.json`
(next to the other settings — no UI yet, edit the file directly):

```json
{
  "webhook_url": "https://hooks.slack.com/services/…",
  "notify_bell": true
}
```

- `webhook_url` — POSTs `{app, host, status, event, latency_ms, timestamp}`
  JSON on every up/down transition (Slack-compatible shape), plus
  `still_down_5m` / `still_down_30m` escalation events for long outages.
- `notify_bell` — rings the terminal bell on down-transitions (and again
  at the 30-minute escalation).

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
> single binary — no server, no web UI, no metrics endpoint. Just pinging.
