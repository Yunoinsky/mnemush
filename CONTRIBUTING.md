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

### TypeScript

- Strict mode (`"strict": true` in tsconfig)
- `noUncheckedIndexedAccess: true`
- No `any` (use `unknown` and narrow)
- Public API: TypeBox schemas for tool parameters

## Commit message format

```
<type>(<scope>): <description>

[body]

[footer]
```

Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`

Examples:
- `feat(rust): add edge decay with Ebbinghaus curve`
- `fix(mcp): handle empty result set in memory_search`
- `docs: clarify auto_inject_before_turn semantics`

## Testing

- Unit tests colocated with code (`#[cfg(test)] mod tests`)
- Integration tests in `crates/mneme/tests/`
- TS tests with vitest
- Run all tests: `cargo test && npm test`

## Release process

See [docs/RELEASING.md](docs/RELEASING.md) (TBD).

## License

By contributing, you agree that your contributions will be licensed under MulanPSL-2.0.
