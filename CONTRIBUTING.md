# Contributing to Mneme

## Development setup

```bash
# Rust toolchain (1.75+ recommended)
rustup install stable
rustup default stable

# Node 20+ (for TS packages)
nvm install 20
nvm use 20

# Clone
git clone https://github.com/Yunoinsky/mneme.git
cd mneme

# Build
cargo build

# Test
cargo test
```

## Project layout

```
mneme/
├── crates/mneme/        # Rust core + MCP + CLI
├── packages/
│   ├── mneme-client/    # Shared TS library
│   ├── mneme-pi/        # Pi extension
│   └── mneme-opencode/  # OpenCode plugin
├── docs/                # User-facing docs
└── scripts/             # Build / release scripts
```

## Coding conventions

### Rust

- `cargo fmt` before commit
- `cargo clippy --all-targets` clean
- Public API documented with `///` rustdoc
- Errors via `thiserror` for libraries, `anyhow` for binaries
- No `unwrap()` outside tests

## Commit message format

We follow [gitmoji](https://gitmoji.dev/specification): `<emoji> [scope?] <description>`. Scope is optional and lowercase. Subject line ≤72 chars, sentence case, no trailing period.

```text
<emoji> [scope?] <description>
```

Examples (drawn from real history):

- `📝 docs(README): fix placeholder URL`
- `♻️ refactor(config): delete 8 dead config fields`
- `🔧 chore(gitignore): ignore .codegraph/ tooling cache`
- `🐛 fix(mcp): handle empty result set in memory_search`

Common emojis (full set at gitmoji.dev):

| Emoji | Use for |
|---|---|
| ✨ | new feature |
| 🐛 | bug fix |
| 🩹 | trivial / non-critical fix |
| 🚑 | critical hotfix |
| 📝 | docs |
| ♻️ | refactor |
| 🎨 | code formatting / structure |
| ⚡ | performance |
| 🔥 / ⚰️ | remove code or files / dead code |
| ✅ | tests |
| 🔧 | config files |
| 🔨 | dev scripts |
| 📦 | package / build |
| 🚀 | deploy / release |
| 🚧 | WIP |
| 🙈 | .gitignore |
| 💚 | CI fix |
| 🔒 | security |

## Testing

- Unit tests colocated with code (`#[cfg(test)] mod tests`)
- TS builds verified via `tsc --noEmit`
- Run all: `cargo test --manifest-path crates/mneme/Cargo.toml && npm run build`

## Release process

See [docs/RELEASING.md](docs/RELEASING.md) (TBD).

## License

By contributing, you agree that your contributions will be licensed under MulanPSL-2.0.
