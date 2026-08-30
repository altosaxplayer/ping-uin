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

`ping-uin` is a lightweight TUI that pings your devices on **per-host intervals**
and shows the results in a rolling, btop-style interface:

* **Per-host intervals** — one host can ping every minute, another every 30
* **Per-host history strip** — `■` green blocks for up, red `_` for down
* **Grouped view** with collapse/expand + down-first ordering
* **"Down box"** — a red-framed panel for anything that has been down the last 3 checks
* **Multiple themes** — `btop`, `dracula`, `nord`, `gruvbox-dark`
* **CSV bulk import** — dump your host list in a spreadsheet, import in one press
* **Alias your IPs** — turn `1.1.1.1` into `Cloudflare`, `server3.internal` into `DB host`

Built for sysadmins monitoring servers, routers, VPN endpoints, IoT devices,
or anything else you'd rather not drop into a heavy dashboard for.

---

## Quick start

```bash
# clone, build, run
cargo install --path .   # builds 'ip-top' binary
cargo run --release      # or just run it straight
```

A starter `ip-top.json` is seeded the first time you launch, so it works even
before you add any config. The starter set ships with a few public IP records
(Google DNS, Cloudflare, google.com).

---

## Controls

| Key | Action |
|-----|--------|
| `↑` / `↓` | move selection |
| `a` | add a host (single form) |
| `e` | edit the selected host — name/IP/group/alias in one form |
| `d` | delete the selected host (confirmation popup) |
| `i` | **import from `hosts.csv`** — merge in bulk, new rows added, existing rows updated |
| `g` | toggle grouped/flat view |
| `s` | sort picker — `off` / down-first / up-first / by-name |
| `x` | toggle the **down box** (hosts down for the last 3 checks) |
| `t` | cycle themes |
| `q` / `Ctrl+C` | quit (cleanly!) |

---

## CSV bulk import

Press `i` and the app merges whatever is in `hosts.csv` next to the binary:

```csv
name,interval_m,group,alias
8.8.8.8,1,public-dns,Google DNS
1.1.1.1,2,public-dns,Cloudflare
server3.internal,5,router,Server 3
192.168.1.1,3,router,
```

New rows become new pings; existing rows get updated. Sync round-trips both
ways — `hosts.csv` is also rewritten on every in-TUI change, so you can always
edit it by hand and import.

---

## Data & privacy

Everything **stays local**, never sent anywhere:

| File | Purpose |
|------|---------|
| `ip-top.json` | full config (gitignored by default — your IPs are yours) |
| `hosts.csv` | bulk-import/export mirror |
| `uptime-log.csv` | rolling ping history (last ~10k events) |

The default `.gitignore` already excludes the three files above so nothing
personal hits GitHub unless you specifically force-add it.

---

## Why "ping-uin"?

Short for *ping* + *pnpm what?* honestly just because it's a fun word and the
((•O•)) face looked like a tiny penguin. Better suggestions welcome.

---

## Where this came from

Fully AI-created, evolving in chat-driven sessions:

1. Old PowerShell loop → Python + `rich` live table
2. Re-written in `Rust` + `ratatui` with real-time updates
3. Themes, grouping, alerts, CSV bulk import, etc., added iteratively

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
