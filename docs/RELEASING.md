# Releasing Mneme

Manual checklist for cutting a release. Currently no automation — pin versions, build, smoke-test, tag, push.

## Pre-release

1. **Bump versions.** All four must match:
   - `crates/mneme/Cargo.toml` — `version`
   - `package.json` (root) — `version`
   - `packages/mneme-client/package.json` — `version`
   - `packages/mneme-pi/package.json` — `version`
   - `packages/mneme-opencode/package.json` — `version`

2. **Move CHANGELOG.** `## Unreleased` → `## vX.Y.Z — YYYY-MM-DD`. Add a one-line summary of the release.

3. **Update ROADMAP.** Mark completed bullets (`**DONE**`) and add the release date to the version header.

4. **Run the full test suite.**
   ```bash
   cargo test --manifest-path crates/mneme/Cargo.toml
   npm run build
   ```

## Build

```bash
cargo build --release --manifest-path crates/mneme/Cargo.toml
npm run build --workspaces --if-present
```

## Smoke test

```bash
./scripts/install.sh --dev
mneme --version            # confirm version string
mneme stats                # should print counts
mneme status               # one-line health check
```

## Tag & push

```bash
git tag -a vX.Y.Z -m "vX.Y.Z: <one-line summary>"
git push origin main --tags
```

## Post-release

- Move the binary to a release artifact (not automated yet — see ROADMAP v0.4 "Publish to crates.io, npm, Homebrew").
- Announce in commit / release notes.

## Hotfix patches

Same as above but on a `hotfix/X.Y.Z` branch. Bump patch version, ship immediately, then merge back to `main`.
