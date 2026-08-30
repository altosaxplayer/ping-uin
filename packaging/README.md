# Packaging

We (the project maintainers) handle distribution. Users don't need to do
anything except `brew` / `winget`. This sibling doc keeps internal notes for
those maintainers — it can stay public, it's just how we operate.

## Homebrew — we're the tap

The formula `packaging/homebrew/ping-uin.rb` lives in this repo **until** we
reach critical mass, and in our own tap `altosaxplayer/homebrew-tap` once we
set it up. End users:

```bash
brew tap altosaxplayer/tap
brew install ping-uin
```

Once codebase is ready, retag and our CI release (`.github/workflows/release.yml`)
provides the checksums that the `url` in the formula expects — then the
formula can be moved into `homebrew-tap` for clear UX.

The formula builds **from source** with `cargo`, so Homebrew's audit passes
even before binaries exist.

## winget — PR to microsoft/winget-pkgs

Maintainers run one command once we have a Windows release asset
(produced automatically on tag push):

```powershell
wingetcreate new \
    https://github.com/altosaxplayer/ping-uin/releases/download/v0.1.0/ping-uin-windows-x86_64.zip
```

Then open a PR with the generated manifest on `microsoft/winget-pkgs`.
Project-owned manifest: we ship, users just `winget install ping-uin`.

## Why is this shared publicly?

Transparency — it documents how we (maintainers) ship without making anyone
else a packaging consumer. Users never need to think about taps/formulas; we
handle all of it on ingest.
