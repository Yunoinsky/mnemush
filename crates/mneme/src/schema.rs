//! Core data model: Memory, Edge, enums, and supporting structs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Tier (scope) of a memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Visible across all projects and sessions.
    Global,
    /// Scoped to a specific project.
    Project,
    /// A skill (procedural knowledge).
    Skill,
    /// Session-scoped (transient).
    Session,
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Global => "global",
            Tier::Project => "project",
            Tier::Skill => "skill",
            Tier::Session => "session",
        }
    }
}

/// Type of memory (brain-inspired categorization).
///
/// - **Identity**: user profile / agent persona. Special: never decays.
/// - **Procedural**: skills (how to do X). Slow decay.
/// - **Semantic**: facts, decisions, preferences, knowledge. Normal decay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    Identity,
    Procedural,
    Semantic,
}

impl MemoryType {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryType::Identity => "identity",
            MemoryType::Procedural => "procedural",
            MemoryType::Semantic => "semantic",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "identity" => MemoryType::Identity,
            "procedural" => MemoryType::Procedural,
            "semantic" => MemoryType::Semantic,
            _ => return None,
        })
    }
}

/// Category (sub-classification within memory type).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Decision,
    Lesson,
    Failure,
    Correction,
    Insight,
    Preference,
    Convention,
    ToolQuirk,
    Note,
    /// Time-stamped event / session highlight.
    Episodic,
    /// Skill / how-to.
    Skill,
    /// Identity-related (user profile / persona).
    Identity,
}

impl Category {
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Decision => "decision",
            Category::Lesson => "lesson",
            Category::Failure => "failure",
            Category::Correction => "correction",
            Category::Insight => "insight",
            Category::Preference => "preference",
            Category::Convention => "convention",
            Category::ToolQuirk => "tool_quirk",
            Category::Note => "note",
            Category::Episodic => "episodic",
            Category::Skill => "skill",
            Category::Identity => "identity",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "decision" => Category::Decision,
            "lesson" => Category::Lesson,
            "failure" => Category::Failure,
            "correction" => Category::Correction,
            "insight" => Category::Insight,
            "preference" => Category::Preference,
            "convention" => Category::Convention,
            "tool_quirk" | "toolquirk" => Category::ToolQuirk,
            "episodic" => Category::Episodic,
            "skill" => Category::Skill,
            "identity" => Category::Identity,
            "note" => Category::Note,
            _ => return None,
        })
    }
}

/// Source of a memory (how it was created).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Manual,
    /// Auto-captured by heuristic trigger.
    AutoHeuristic,
    /// Auto-captured by periodic LLM review.
    AutoReview,
    /// User correction.
    Correction,
    /// Skill (procedural) creation.
    Skill,
    /// Imported from session log.
    SessionImport,
    /// Saved web search result.
    SearchResult,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Manual => "manual",
            Source::AutoHeuristic => "auto_heuristic",
            Source::AutoReview => "auto_review",
            Source::Correction => "correction",
            Source::Skill => "skill",
            Source::SessionImport => "session_import",
            Source::SearchResult => "search_result",
        }
    }
}

/// Edge type in the memory graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    /// Generic association, bidirectional.
    Related,
    /// A provides evidence for B, bidirectional.
    Supports,
    /// A and B are in conflict, bidirectional.
    Contradicts,
    /// A replaces B, unidirectional.
    Supersedes,
}

impl EdgeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeType::Related => "related",
            EdgeType::Supports => "supports",
            EdgeType::Contradicts => "contradicts",
            EdgeType::Supersedes => "supersedes",
        }
    }

    pub fn default_bidirectional(&self) -> bool {
        match self {
            EdgeType::Supersedes => false,
            EdgeType::Related | EdgeType::Supports | EdgeType::Contradicts => true,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "related" => EdgeType::Related,
            "supports" => EdgeType::Supports,
            "contradicts" => EdgeType::Contradicts,
            "supersedes" => EdgeType::Supersedes,
            _ => return None,
        })
    }
}

/// Memory node in the LTM graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub memory_type: MemoryType,
    pub tier: Tier,
    pub category: Category,
    pub title: String,
    pub content: String,
    pub context: Option<String>,
    pub topic_key: Option<String>,
    pub tags: Vec<String>,
    pub project: Option<String>,
    pub source: Source,

    /// Confidence at creation.
    pub initial_confidence: f32,
    /// Current confidence (decays over time, boosts on access).
    pub confidence: f32,
    /// 0.0-1.0. Higher = more important (decays slower).
    pub importance: f32,

    pub access_count: u32,
    pub last_accessed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,

    /// Per-memory override for half-life (None = use config).
    pub override_half_life: Option<f32>,
    /// If true, never pruned.
    pub never_prune: bool,
    /// If true, never decays.
    pub never_decay: bool,

    /// SHA-256 of normalized content, for dedup.
    pub content_hash: String,
    /// Set when soft-deleted (30-day recovery window).
    pub deleted_at: Option<DateTime<Utc>>,

    /// Pending LLM review at session-end.
    pub needs_review: bool,

    /// Lifecycle state for agent-self memories (commitments, TODOs,
    /// follow-ups, action items). Decoupled from `category` so any
    /// memory can transition active→completed as the agent works
    /// through it. See decisions.md D14.
    pub status: ActionStatus,
    /// Optional deadline (unix seconds). `memory_next` sorts by this
    /// first; null means no specific deadline.
    pub due_at: Option<DateTime<Utc>>,
    /// Optional agent id for multi-agent lease / claim. When set, only
    /// that agent should update this memory until released.
    pub claimed_by: Option<String>,
    /// Optional parent memory id (for sub-actions / decomposition).
    pub parent_id: Option<String>,
    /// When `status` became Completed (or Abandoned). Set by the
    /// layer that performs the transition, not by direct assignment.
    pub completed_at: Option<DateTime<Utc>>,
}

/// Lifecycle state for a memory. Distinct from `category`: any memory
/// can be a commitment, observation, decision, etc. AND be in any of
/// these states. The transition is driven by the agent as work
/// progresses; not by external observers. See decisions.md D14.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    /// Open / pending work. Default for new memories.
    Active,
    /// Agent has completed the work this memory represents.
    Completed,
    /// Work was abandoned (out of scope, wrong premise, etc.).
    Abandoned,
}

impl ActionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionStatus::Active => "active",
            ActionStatus::Completed => "completed",
            ActionStatus::Abandoned => "abandoned",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "active" => ActionStatus::Active,
            "completed" => ActionStatus::Completed,
            "abandoned" => ActionStatus::Abandoned,
            _ => return None,
        })
    }
}

/// Input for creating a new memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMemory {
    pub content: String,
    pub title: String,
    pub category: Category,
    pub memory_type: MemoryType,
    pub tier: Tier,
    pub context: Option<String>,
    pub tags: Vec<String>,
    pub project: Option<String>,
    pub source: Source,
    pub importance: f32,
    pub override_half_life: Option<f32>,
    pub never_prune: bool,
    pub never_decay: bool,
    pub needs_review: bool,
}

impl NewMemory {
    /// Construct with sensible defaults for category=Note, type=Semantic, tier=Global.
    pub fn note(content: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            title: title.into(),
            category: Category::Note,
            memory_type: MemoryType::Semantic,
            tier: Tier::Global,
            context: None,
            tags: vec![],
            project: None,
            source: Source::Manual,
            importance: 0.5,
            override_half_life: None,
            never_prune: false,
            never_decay: false,
            needs_review: false,
        }
    }
}

/// Options for search.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchOpts {
    pub category: Option<Category>,
    pub memory_type: Option<MemoryType>,
    pub project: Option<String>,
    pub limit: Option<usize>,
    /// Min confidence (post-decay) for inclusion.
    pub min_confidence: Option<f32>,
}

/// A search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub memory: Memory,
    /// Combined score.
    pub score: f32,
    /// Raw BM25 from FTS5.
    pub bm25: f32,
    /// Retrievability factor [0..1].
    pub retrievability: f32,
}

/// Edge in the memory graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    pub source_id: String,
    pub target_id: String,
    pub edge_type: EdgeType,
    pub strength: f32,
    pub initial_strength: f32,
    pub bidirectional: bool,
    pub provenance: Option<String>,
    pub evidence: Option<String>,
    pub context: Option<String>,
    pub access_count: u32,
    pub last_activated: Option<DateTime<Utc>>,
    pub stability: f32,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}
