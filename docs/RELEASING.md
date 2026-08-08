# Releasing Mnemush

Manual checklist for cutting a release. Currently no automation — pin versions, build, smoke-test, tag, push.

## Publish targets

Three distribution channels, all versioned 1.0.0 (kept in lockstep):

1. **npm** — `mnemush-pi`(pi 扩展, `pi install npm:mnemush-pi`)、`mnemush-client`(共享 TS 库)、`mnemush-opencode`
2. **crates.io** — `mnemush` 二进制(`cargo install mnemush`)
3. **GitHub** — 源码仓库(release tarball + install.sh)

## npm publish checklist

```bash
# 1. 构建 TS(发布包只含 dist, 必须先 build)
for p in mnemush-client mnemush-pi mnemush-opencode; do (cd packages/$p && npm run build); done

# 2. 验证包内容
for p in mnemush-client mnemush-pi mnemush-opencode; do (cd packages/$p && npm pack --dry-run); done

# 3. 登录 + 发布(client 先于 pi/opencode, 因为它们是依赖)
npm login
(cd packages/mnemush-client && npm publish)
(cd packages/mnemush-pi && npm publish)
(cd packages/mnemush-opencode && npm publish)
```

## crates.io publish checklist

```bash
# 1. 验证打包(依赖必须在 crates.io 可解析)
cd crates/mnemush && cargo package --allow-dirty --no-verify

# 2. 登录 + 发布
cargo login
cargo publish
```

## Pre-release

1. **Bump versions.** All four must match:
   - `crates/mnemush/Cargo.toml` — `version`
   - `package.json` (root) — `version`
   - `packages/mnemush-client/package.json` — `version`
   - `packages/mnemush-pi/package.json` — `version`
   - `packages/mnemush-opencode/package.json` — `version`

2. **Move CHANGELOG.** `## Unreleased` → `## vX.Y.Z — YYYY-MM-DD`. Add a one-line summary of the release.

3. **Update ROADMAP.** Mark completed bullets (`**DONE**`) and add the release date to the version header.

4. **Run the full test suite.**
   ```bash
   cargo test --manifest-path crates/mnemush/Cargo.toml
   npm run build
   ```

## Build

```bash
cargo build --release --manifest-path crates/mnemush/Cargo.toml
npm run build --workspaces --if-present
```

## Smoke test

```bash
./scripts/install.sh --dev
mnemush --version            # confirm version string
mnemush stats                # should print counts
mnemush status               # one-line health check
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
