# Packaging notes

## Homebrew (via your own tap — recommended)

1. Create a public repo named `homebrew-tap` on GitHub.
2. Add `tap/Formula/ping-uin.rb` from `packaging/homebrew/ping-uin.rb` here,
   replacing `<your-username>` with your GitHub user.
3. Users can then:

```bash
brew tap <your-username>/tap \
    https://github.com/<your-username>/homebrew-tap
brew install ping-uin
```

The formula builds **from source** with `cargo`, so Homebrew's audit is
satisfied even before the first release (no binary URLs needed).

## winget

Once you have a GitHub Release with a Windows zip (`ping-uin-windows-x86_64.zip`)
— produced automatically on tag push by `.github/workflows/release.yml` —
generate a manifest with:

```powershell
wingetcreate new `
    https://github.com/<your-username>/ping-uin/releases/download/v0.1.0/ping-uin-windows-x86_64.zip
```

Then submit the generated manifest as a PR to `microsoft/winget-pkgs`.

Alternatively, keep `packaging/winget/manifest.yaml` on your own repo and have
users install from a private index.
