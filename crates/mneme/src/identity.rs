//! Identity layer.
//!
//! Loads USER.md / PERSONA.md / CONSTITUTION.md from the identity dir
//! and returns a frozen snapshot suitable for injection into a system
//! prompt. Files are plain markdown with optional `§` entry separators
//! (we currently treat the whole file as a block; parsing entries is
//! future work).

use std::path::{Path, PathBuf};

use crate::error::Result;

pub const USER_FILE: &str = "USER.md";
pub const PERSONA_FILE: &str = "PERSONA.md";
pub const CONSTITUTION_FILE: &str = "CONSTITUTION.md";

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
        let dir = crate::default_data_dir().join("identity");
        Self::load_from(&dir)
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
}
