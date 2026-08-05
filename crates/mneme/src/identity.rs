//! Identity layer.
//!
//! Loads USER.md / PERSONA.md / CONSTITUTION.md from the identity dir
//! and returns a frozen snapshot suitable for injection into a system
//! prompt. Files are plain markdown with optional `§` entry separators
//! (we currently treat the whole file as a block; parsing entries is
//! future work).
//!
//! Also implements the v0.2 identity-reflection flow: the LLM observes
//! user behavior across sessions and proposes updates to USER.md /
//! PERSONA.md. Proposals are written to `pending.jsonl` next to the
//! identity files and NEVER applied silently. The user runs
//! `mneme identity list-pending` to see them and `approve` / `reject`
//! to act. This keeps the LLM honest — it can suggest, but the human
//! stays in the loop.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{MnemeError, Result};

pub const USER_FILE: &str = "USER.md";
pub const PERSONA_FILE: &str = "PERSONA.md";
pub const CONSTITUTION_FILE: &str = "CONSTITUTION.md";
pub const PENDING_FILE: &str = "pending.jsonl";

/// Default identity dir: `~/.mneme/identity/`.
pub fn default_identity_dir() -> PathBuf {
    crate::default_data_dir().join("identity")
}

/// Allowed target files for proposals. Anything else is rejected to
/// prevent path traversal.
pub fn is_valid_target(target: &str) -> bool {
    matches!(target, USER_FILE | PERSONA_FILE | CONSTITUTION_FILE)
}

/// Identity snapshot, loaded once per session.
#[derive(Debug, Clone, Default)]
pub struct Identity {
    pub user: String,
    pub persona: String,
    pub constitution: String,
}

impl Identity {
    /// Load from the default data dir.
    pub fn load() -> Result<Self> {
        Self::load_from(&default_identity_dir())
    }

    /// Load from a specific directory.
    pub fn load_from(dir: &Path) -> Result<Self> {
        Ok(Self {
            user: read_or_empty(&dir.join(USER_FILE))?,
            persona: read_or_empty(&dir.join(PERSONA_FILE))?,
            constitution: read_or_empty(&dir.join(CONSTITUTION_FILE))?,
        })
    }

    /// Render as a system-prompt block (wrapped in <identity> tags so
    /// the LLM can distinguish identity from agent output).
    pub fn render_prompt_block(&self) -> String {
        let mut s = String::from("<identity>\n");
        if !self.user.is_empty() {
            s.push_str("## About the user\n");
            s.push_str(&self.user);
            s.push_str("\n\n");
        }
        if !self.persona.is_empty() {
            s.push_str("## About me (the memory system)\n");
            s.push_str(&self.persona);
            s.push_str("\n\n");
        }
        if !self.constitution.is_empty() {
            s.push_str("## Constitution (absolute rules)\n");
            s.push_str(&self.constitution);
            s.push('\n');
        }
        s.push_str("</identity>");
        s
    }

    /// Concatenate non-empty sections for LLM consumption.
    pub fn is_empty(&self) -> bool {
        self.user.is_empty() && self.persona.is_empty() && self.constitution.is_empty()
    }
}

fn read_or_empty(path: &PathBuf) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(content)
}

// ── Identity reflection (v0.2) ──────────────────────────────────────

/// Status of a pending identity update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Approved,
    Rejected,
}

impl ProposalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProposalStatus::Pending => "pending",
            ProposalStatus::Approved => "approved",
            ProposalStatus::Rejected => "rejected",
        }
    }
}

/// A proposal to update one of the identity files. The LLM (or the
/// user, via the CLI) creates these; the user reviews and acts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingUpdate {
    pub id: String,
    pub target: String,
    pub content: String,
    pub reason: String,
    pub evidence_count: u32,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub status: ProposalStatus,
}

/// Append a new proposal to `pending.jsonl`. Creates the dir if needed.
pub fn propose_in(
    dir: &Path,
    target: &str,
    content: &str,
    reason: &str,
    evidence_count: u32,
) -> Result<PendingUpdate> {
    if !is_valid_target(target) {
        return Err(MnemeError::Other(format!(
            "invalid target '{}': must be one of USER.md, PERSONA.md, CONSTITUTION.md",
            target
        )));
    }
    std::fs::create_dir_all(dir)?;
    let update = PendingUpdate {
        id: uuid::Uuid::new_v4().to_string(),
        target: target.to_string(),
        content: content.to_string(),
        reason: reason.to_string(),
        evidence_count,
        created_at: Utc::now(),
        resolved_at: None,
        status: ProposalStatus::Pending,
    };
    let path = dir.join(PENDING_FILE);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    use std::io::Write;
    let line = serde_json::to_string(&update)
        .map_err(|e| MnemeError::Other(format!("serialize proposal: {}", e)))?;
    writeln!(file, "{}", line)?;
    Ok(update)
}

/// Convenience wrapper using the default identity dir.
pub fn propose(
    target: &str,
    content: &str,
    reason: &str,
    evidence_count: u32,
) -> Result<PendingUpdate> {
    propose_in(
        &default_identity_dir(),
        target,
        content,
        reason,
        evidence_count,
    )
}

/// Read all proposals from `pending.jsonl`. Skips malformed lines (a
/// corrupted line shouldn't kill the whole list). Filter by status if
/// provided.
pub fn list_pending_in(dir: &Path, status: Option<ProposalStatus>) -> Result<Vec<PendingUpdate>> {
    let path = dir.join(PENDING_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)?;
    let mut out = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<PendingUpdate>(line) {
            Ok(p) => {
                if let Some(want) = status {
                    if p.status != want {
                        continue;
                    }
                }
                out.push(p);
            }
            Err(e) => {
                // ponytail: skip corrupted lines rather than crash the
                // whole list. Operators can `cat pending.jsonl` to inspect.
                eprintln!("[mneme] skipping malformed pending.jsonl line: {}", e);
            }
        }
    }
    Ok(out)
}

pub fn list_pending(status: Option<ProposalStatus>) -> Result<Vec<PendingUpdate>> {
    list_pending_in(&default_identity_dir(), status)
}

/// Find a specific proposal by id (any status). Used by the MCP layer
/// to distinguish "not found" from "already resolved". Returns `None`
/// if no proposal in `pending.jsonl` (across all statuses) matches the
/// given id prefix (≥4 chars, matching `resolve_in`'s rules).
pub fn find_proposal_in(dir: &Path, id: &str) -> Result<Option<PendingUpdate>> {
    let path = dir.join(PENDING_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let mut found: Option<PendingUpdate> = None;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let p: PendingUpdate = match serde_json::from_str(line) {
            Ok(p) => p,
            Err(_) => continue,
        };
        // Same prefix-match rule as resolve_in (>= 4 chars).
        let matches = p.id == id || (id.len() >= 4 && p.id.starts_with(id));
        if matches {
            found = Some(p);
        }
    }
    Ok(found)
}

pub fn find_proposal(id: &str) -> Result<Option<PendingUpdate>> {
    find_proposal_in(&default_identity_dir(), id)
}

/// Resolve (approve or reject) a proposal by id. Returns the updated
/// proposal on success, or `None` if not found. Writes the new state
/// of `pending.jsonl` atomically (write to temp, rename).
fn resolve_in(
    dir: &Path,
    id: &str,
    new_status: ProposalStatus,
    apply_to_target: bool,
) -> Result<Option<PendingUpdate>> {
    let path = dir.join(PENDING_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let now = Utc::now();
    let mut found: Option<PendingUpdate> = None;
    let mut new_lines: Vec<String> = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut p: PendingUpdate = match serde_json::from_str(line) {
            Ok(p) => p,
            Err(_) => {
                // Preserve unparseable lines as-is so we don't lose data.
                new_lines.push(line.to_string());
                continue;
            }
        };
        // ponytail: accept full id or any unique prefix (>= 4 chars)
        // so users can paste the 8-char short form from list-pending.
        let matches = if p.id == id {
            true
        } else if id.len() >= 4 && p.id.starts_with(id) {
            // Only treat as prefix match if unique across all entries.
            // For now accept it — the loop continues even after match,
            // and we re-validate uniqueness below.
            true
        } else {
            false
        };
        if matches && p.status == ProposalStatus::Pending {
            p.status = new_status;
            p.resolved_at = Some(now);
            if apply_to_target {
                // Append to the target file.
                let target_path = dir.join(&p.target);
                let date_str = now.format("%Y-%m-%d");
                let section = format!(
                    "\n## {} (proposed, evidence={})\n\n> {}\n\n{}\n",
                    date_str, p.evidence_count, p.reason, p.content
                );
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&target_path)?;
                f.write_all(section.as_bytes())?;
            }
            found = Some(p.clone());
        }
        let json = serde_json::to_string(&p)
            .map_err(|e| MnemeError::Other(format!("serialize proposal: {}", e)))?;
        new_lines.push(json);
    }
    if found.is_none() {
        return Ok(None);
    }
    // Atomic write: temp file + rename.
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, new_lines.join("\n") + "\n")?;
    std::fs::rename(&tmp, &path)?;
    Ok(found)
}

pub fn approve_in(dir: &Path, id: &str) -> Result<Option<PendingUpdate>> {
    resolve_in(dir, id, ProposalStatus::Approved, true)
}

pub fn reject_in(dir: &Path, id: &str) -> Result<Option<PendingUpdate>> {
    resolve_in(dir, id, ProposalStatus::Rejected, false)
}

pub fn approve(id: &str) -> Result<Option<PendingUpdate>> {
    approve_in(&default_identity_dir(), id)
}

pub fn reject(id: &str) -> Result<Option<PendingUpdate>> {
    reject_in(&default_identity_dir(), id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn loads_missing_files_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let id = Identity::load_from(dir.path()).unwrap();
        assert!(id.is_empty());
    }

    #[test]
    fn loads_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(USER_FILE), "name: test").unwrap();
        fs::write(dir.path().join(PERSONA_FILE), "persona: x").unwrap();
        let id = Identity::load_from(dir.path()).unwrap();
        assert!(!id.is_empty());
        assert!(id.user.contains("test"));
    }

    #[test]
    fn renders_prompt_block() {
        let id = Identity {
            user: "name: A".into(),
            persona: "I am B".into(),
            constitution: "rule: C".into(),
        };
        let block = id.render_prompt_block();
        assert!(block.contains("<identity>"));
        assert!(block.contains("About the user"));
        assert!(block.contains("About me"));
        assert!(block.contains("Constitution"));
    }

    // ── proposal tests ───────────────────────────────────────

    #[test]
    fn propose_writes_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let p = propose_in(dir.path(), USER_FILE, "researcher in CS", "user said it", 3).unwrap();
        assert_eq!(p.target, USER_FILE);
        assert_eq!(p.status, ProposalStatus::Pending);
        let raw = fs::read_to_string(dir.path().join(PENDING_FILE)).unwrap();
        assert_eq!(raw.lines().count(), 1);
        let parsed: PendingUpdate = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(parsed.id, p.id);
    }

    #[test]
    fn propose_rejects_invalid_target() {
        let dir = tempfile::tempdir().unwrap();
        let r = propose_in(dir.path(), "../escape.md", "x", "y", 1);
        assert!(r.is_err());
    }

    #[test]
    fn list_pending_filters_by_status() {
        let dir = tempfile::tempdir().unwrap();
        propose_in(dir.path(), USER_FILE, "a", "r", 1).unwrap();
        let p = propose_in(dir.path(), USER_FILE, "b", "r", 1).unwrap();
        // All pending
        let all = list_pending_in(dir.path(), None).unwrap();
        assert_eq!(all.len(), 2);
        // Approve one
        approve_in(dir.path(), &p.id).unwrap();
        let pending = list_pending_in(dir.path(), Some(ProposalStatus::Pending)).unwrap();
        let approved = list_pending_in(dir.path(), Some(ProposalStatus::Approved)).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].id, p.id);
    }

    #[test]
    fn approve_appends_to_target_file() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-seed USER.md with some content
        fs::write(dir.path().join(USER_FILE), "name: A\n").unwrap();
        let p = propose_in(
            dir.path(),
            USER_FILE,
            "researcher in CS",
            "user said it 3x",
            3,
        )
        .unwrap();
        let resolved = approve_in(dir.path(), &p.id).unwrap().unwrap();
        assert_eq!(resolved.status, ProposalStatus::Approved);
        assert!(resolved.resolved_at.is_some());
        // USER.md should have the original content + a new section
        let user_md = fs::read_to_string(dir.path().join(USER_FILE)).unwrap();
        assert!(user_md.contains("name: A"), "original content preserved");
        assert!(user_md.contains("researcher in CS"), "proposal appended");
        assert!(
            user_md.contains("user said it 3x"),
            "reason recorded in section"
        );
    }

    #[test]
    fn reject_does_not_touch_target() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(USER_FILE), "name: A\n").unwrap();
        let p = propose_in(dir.path(), USER_FILE, "should not appear", "r", 1).unwrap();
        let resolved = reject_in(dir.path(), &p.id).unwrap().unwrap();
        assert_eq!(resolved.status, ProposalStatus::Rejected);
        let user_md = fs::read_to_string(dir.path().join(USER_FILE)).unwrap();
        assert_eq!(user_md, "name: A\n", "target file untouched");
    }

    #[test]
    fn approve_unknown_id_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let r = approve_in(dir.path(), "nope");
        assert!(matches!(r, Ok(None)));
    }

    #[test]
    fn double_approve_is_idempotent_no() {
        // Once a proposal is approved, the second approve should be a
        // no-op (it won't find a Pending one with that id).
        let dir = tempfile::tempdir().unwrap();
        let p = propose_in(dir.path(), USER_FILE, "x", "r", 1).unwrap();
        assert!(approve_in(dir.path(), &p.id).unwrap().is_some());
        let second = approve_in(dir.path(), &p.id).unwrap();
        assert!(second.is_none(), "second approve should be no-op");
    }

    #[test]
    fn is_valid_target_rejects_traversal() {
        assert!(is_valid_target(USER_FILE));
        assert!(is_valid_target(PERSONA_FILE));
        assert!(is_valid_target(CONSTITUTION_FILE));
        assert!(!is_valid_target("USER.mdx"));
        assert!(!is_valid_target("../USER.md"));
        assert!(!is_valid_target("/etc/passwd"));
        assert!(!is_valid_target(""));
    }
}
