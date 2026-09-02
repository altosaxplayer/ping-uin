# Packaging

We (the project maintainers) handle distribution. Users don't need to do
anything except `brew` / `winget`. This sibling doc keeps internal notes for
those maintainers — it can stay public, it's just how we operate.

## Homebrew — tap is live

The official formula lives in [`altosaxplayer/homebrew-tap`](https://github.com/altosaxplayer/homebrew-tap/blob/main/Formula/ping-uin.rb).
A copy is kept here under `packaging/homebrew/` for reference only.

End users:

```bash
brew tap altosaxplayer/tap
brew install ping-uin
```

Or directly:

```bash
brew install altosaxplayer/tap/ping-uin
```

The formula builds **from source** with `cargo`, so Homebrew's audit passes
without signed binaries.

## winget — manifest ready for submission

Windows binaries are built automatically by `.github/workflows/release.yml`
on every `v*` tag push and attached to the GitHub release.

To submit or update the manifest in `microsoft/winget-pkgs`:

```powershell
wingetcreate new \
    https://github.com/altosaxplayer/ping-uin/releases/download/v0.1.9/ping-uin-windows-x86_64.zip
```

Or use the pre-generated manifest files in `packaging/winget/manifests/`
and open a PR against `microsoft/winget-pkgs`.

Once accepted, users install with:

```powershell
winget install ping-uin
```

## Release workflow

`.github/workflows/release.yml` builds and uploads:

- `ping-uin-macos-aarch64.tar.gz`
- `ping-uin-macos-x86_64.tar.gz`
- `ping-uin-linux-x86_64.tar.gz`
- `ping-uin-windows-x86_64.zip`

Plus a `.sha256` checksum file for each asset.

## Why is this shared publicly?

Transparency — it documents how we (maintainers) ship without making anyone
else a packaging consumer. Users never need to think about taps/formulas; we
handle all of it on ingest.
