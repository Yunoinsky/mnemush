//! Regression tests for past production incidents.
//!
//! These tests scan the source code (and where feasible, the compiled
//! artifact) for known-bad patterns. If a future commit reintroduces a
//! dangerous code path, the test fails at `cargo test` time, before
//! the binary ever ships.
//!
//! Each test is named for the incident it prevents. The doc-comment
//! explains what the pattern caused and what to do if you need to
//! re-introduce it.

use std::path::Path;

/// **`edef25b` regression (2026-08-26): automatic sync wiped all edges.**
///
/// The pre-fix `upsert_memory_tx` did `DELETE FROM memory WHERE id = ?1`
/// then `INSERT INTO memory ...`. With the FK CASCADE on
/// `memory_edge.source_id` / `target_id`, every memory upsert
/// silently destroyed all edges pointing to that memory. The fix
/// (`edef25b`) replaced DELETE+INSERT with `UPDATE`, which doesn't
/// trigger CASCADE. The audit revealed this when `cargo test` spawned
/// a fire-and-forget `mnemush sync webdav-push` via the stale PATH
/// binary, which still had the pre-fix code.
///
/// To re-introduce raw `DELETE FROM memory WHERE id` outside the
/// `forget.rs` hard-delete path, you must:
///   1. Confirm there's no FK CASCADE on `memory_edge.*_id` to
///      `memory(id)` (currently there is).
///   2. Cascade-delete edges explicitly in the same transaction
///      before the memory DELETE.
///   3. Add the new call site to the count below and document why.
#[test]
fn edef25b_no_unsafe_delete_from_memory() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits: Vec<String> = Vec::new();
    for entry in walk_rs(&src_dir) {
        let body = std::fs::read_to_string(&entry).unwrap_or_default();
        for (lineno, line) in body.lines().enumerate() {
            // Match the SQL string. The string `"DELETE FROM memory WHERE id = ?1"`
            // is the canonical edef25b pattern; we don't tolerate any other
            // "DELETE FROM memory" except in forget.rs.
            if line.contains("DELETE FROM memory WHERE id") {
                let rel = entry.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap().display().to_string();
                hits.push(format!("{rel}:{}", lineno + 1));
            }
        }
    }
    // The ONLY allowed site is forget.rs (hard-delete with FK CASCADE-aware
    // call ordering). If you add a second, the test fails.
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one `DELETE FROM memory WHERE id` site (forget.rs); found {}:\n  {}",
        hits.len(),
        hits.join("\n  ")
    );
    assert!(
        hits[0].contains("forget.rs"),
        "the lone `DELETE FROM memory WHERE id` must live in forget.rs, found at {}",
        hits[0]
    );
}

/// Walk a directory recursively for `*.rs` files.
fn walk_rs(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_rs(&p));
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
    out
}

/// **`edef25b` runtime check (2026-08-26): catch the bug at the binary.**
///
/// The source-side test above prevents reintroducing the bad pattern
/// in future commits. This test catches it in the SHIPPED binary, in
/// case someone bypasses the test (e.g. `cargo install --path` from a
/// stale checkout, or an old `mnemush` already on PATH from a
/// pre-fix release).
///
/// Counts the `DELETE FROM memory WHERE id` pattern in the user's
/// installed binary at `~/.cargo/bin/mnemush`. We expect 1 (the
/// legitimate `forget.rs` hard-delete). If the count is higher,
/// someone is running a pre-`edef25b` binary, and the next
/// `webdav-push` will wipe their edges. Refusing to pass the test
/// gives CI / pre-commit a chance to surface this.
///
/// The test is ignored-by-default because not every developer has the
/// installed binary at the canonical path. Run explicitly with:
///   `cargo test --test regression -- --ignored --nocapture`
/// or via `cargo test -- --include-ignored`.
#[test]
#[ignore = "requires ~/.cargo/bin/mnemush on disk; run with --ignored"]
fn edef25b_installed_binary_is_safe() {
    use std::io::Read;
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .expect("HOME / USERPROFILE not set");
    let candidates = [
        std::path::PathBuf::from(&home).join(".cargo").join("bin").join("mnemush"),
        std::path::PathBuf::from(&home).join(".cargo").join("bin").join("mnemush.exe"),
    ];
    let p = candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone());
    if !p.exists() {
        eprintln!(
            "SKIP: installed binary not at {} (or .exe). Run `cargo install --path crates/mnemush --force` to populate it.",
            p.display()
        );
        return;
    }
    let mut buf = Vec::new();
    std::fs::File::open(&p)
        .expect("open installed binary")
        .read_to_end(&mut buf)
        .expect("read installed binary");
    // Count ASCII substrings (case-sensitive; mnemush uses uppercase SQL).
    let needle = b"DELETE FROM memory WHERE id";
    let count = buf.windows(needle.len()).filter(|w| *w == needle).count();
    assert!(
        count <= 1,
        "installed binary at {} contains {} occurrences of `{}` (expected ≤1). \
         Reinstall with `cargo install --path crates/mnemush --force` to replace the stale binary.",
        p.display(),
        count,
        std::str::from_utf8(needle).unwrap(),
    );
}

/// **`ae08f9d` regression (2026-08-25): version drift across packages.**
///
/// 5 `package.json` and 1 `Cargo.toml` need to share a single version.
/// If they drift, npm install pulls a different version than
/// `cargo install`, leading to the edef25b bug pattern (PATH binary
/// from npm != binary the dev was testing). All six files must
/// carry the same `version` string.
#[test]
fn ae08f9d_package_versions_in_sync() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .parent()
        .expect("repo root");
    let mut versions: Vec<(String, String)> = Vec::new();
    // Cargo.toml
    let cargo = std::fs::read_to_string(workspace_root.join("crates/mnemush/Cargo.toml")).unwrap();
    let cargo_v = extract_version(&cargo).expect("crates/mnemush/Cargo.toml has version");
    versions.push(("crates/mnemush/Cargo.toml".into(), cargo_v));
    // Top-level + 4 packages
    for rel in [
        "package.json",
        "packages/mnemush-client/package.json",
        "packages/mnemush-dsh/package.json",
        "packages/mnemush-opencode/package.json",
        "packages/mnemush-pi/package.json",
    ] {
        let p = workspace_root.join(rel);
        let body = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        let v = extract_version(&body).unwrap_or_else(|| panic!("{rel} has version"));
        versions.push((rel.into(), v));
    }
    let first = &versions[0].1;
    for (file, v) in &versions {
        assert_eq!(v, first, "version drift: {file} = {v}, expected {first}");
    }
}

fn extract_version(body: &str) -> Option<String> {
    for line in body.lines() {
        let l = line.trim().trim_matches('"');
        // Accept "version ...", 'version ...', or just "version" (Cargo / JSON / TOML).
        let rest = l
            .strip_prefix("\"version\"")
            .or_else(|| l.strip_prefix("version"))
            .or_else(|| l.strip_prefix("'version'"));
        let rest = match rest {
            Some(r) => r,
            None => continue,
        };
        // Cargo.toml: "version = \"1.6.1\""
        if let Some(eq) = rest.find('=') {
            let v = rest[eq + 1..]
                .trim()
                .trim_matches(',')
                .trim_matches('"')
                .trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
        // package.json: "version": "1.6.1",
        if let Some(colon) = rest.find(':') {
            let v = rest[colon + 1..]
                .trim()
                .trim_matches(',')
                .trim_matches('"')
                .trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}
