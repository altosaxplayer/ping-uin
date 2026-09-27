# apt repo (maintainer notes)

Public install: https://altosaxplayer.github.io/ping-uin/apt (landing page
is `packaging/apt/index.html`, deployed from the `gh-pages` branch).

## How it works

- `.github/workflows/apt.yml` runs on every published GitHub release (or
  manually via workflow dispatch with an optional tag): it builds the .deb
  with `cargo-deb` (config in `Cargo.toml` `[package.metadata.deb]`),
  attaches it to the release, then regenerates the repo under
  `apt/` on the `gh-pages` branch and pushes.
- The pool keeps every version (`dpkg-scanpackages` handles multi-version),
  so users can `apt install ping-uin=<version>` to downgrade.
- Before pushing, the workflow proves installability: `apt-get update` +
  `--download-only` against a `file://` copy with the real key, then an
  actual `dpkg` unpack into a throwaway root.

## Signing key

- Archive key: `ping-uin apt archive`, RSA 3072, fingerprint
  `EFA64F6E2FC2DA6F7C699A1CBB911DE3400FE84E`, expires ~Sept 2028.
- Private half: repository secret `APT_GPG_PRIVATE_KEY` (armored, no
  passphrase). Rotate by generating a new key, updating the secret, and
  replacing `packaging/apt/key.asc` — then re-run the workflow (dispatch,
  latest tag) to re-sign the metadata.
- Public half: `packaging/apt/key.asc` (committed; copied to `apt/key.asc`
  on publish for `curl ... | gpg --dearmor` installs).

## gh-pages branch

Created automatically on the first workflow run (orphan branch holding only
the site). GitHub Pages must serve it: Settings → Pages → Deploy from
branch → `gh-pages` → `/ (root)`. Only needed once.
