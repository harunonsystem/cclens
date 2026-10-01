//! Multi-agent domain model and shared analysis. Raw formats stay in adapters.
use serde::{Deserialize, Serialize};

pub mod cli;
pub mod store;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Tokens {
    /// Total input, including both cache components.
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub reasoning: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Evidence {
    pub path: String,
    pub line: Option<usize>,
    pub table: Option<String>,
    pub row_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// Stable agent-wide identity, including copied fork history.
    pub id: String,
    pub session_id: String,
    pub timestamp_ms: Option<i64>,
    /// usage | failure | edit | prompt | skill | compaction
    pub kind: String,
    pub model: Option<String>,
    pub tool: Option<String>,
    pub target: Option<String>,
    pub category: Option<String>,
    /// measured | estimated | unavailable
    pub measurement: String,
    pub tokens: Tokens,
    pub api_cost_usd: Option<f64>,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub id: String,
    pub project: Option<String>,
    pub parent_id: Option<String>,
    pub started_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Batch {
    pub sessions: Vec<Session>,
    pub events: Vec<Event>,
    pub warnings: Vec<String>,
    pub unknown_records: usize,
}
