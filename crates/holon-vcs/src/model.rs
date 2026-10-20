//! The records `holon:vcs/types` declares, one Rust type per WIT type.

use serde::{Deserialize, Serialize};

/// Lower-case hex SHA-256 — of a blob's bytes, or of a patch's canonical form.
pub type Hash = String;
/// A conflict is named by the hash of its two sides.
pub type ConflictId = String;
/// Strictly increasing per workspace, starting at 1.
pub type OpId = u64;
pub type WorkspaceId = String;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub goal: Option<String>,
    pub model: Option<String>,
}

impl Agent {
    pub fn named(id: impl Into<String>) -> Self {
        Agent { id: id.into(), goal: None, model: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolKind {
    Module,
    Function,
    Method,
    StructItem,
    EnumItem,
    TraitItem,
    ImplBlock,
    Constant,
    StaticItem,
    TypeAlias,
    MacroItem,
    WitInterface,
    WitWorld,
    WitType,
    File,
}

impl SymbolKind {
    /// The WIT spelling; canonical encodings use it, so it must stay stable.
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Module => "module",
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::StructItem => "struct-item",
            SymbolKind::EnumItem => "enum-item",
            SymbolKind::TraitItem => "trait-item",
            SymbolKind::ImplBlock => "impl-block",
            SymbolKind::Constant => "constant",
            SymbolKind::StaticItem => "static-item",
            SymbolKind::TypeAlias => "type-alias",
            SymbolKind::MacroItem => "macro-item",
            SymbolKind::WitInterface => "wit-interface",
            SymbolKind::WitWorld => "wit-world",
            SymbolKind::WitType => "wit-type",
            SymbolKind::File => "file",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SymbolId {
    pub component: String,
    pub path: String,
    pub name: String,
    pub kind: SymbolKind,
}

impl SymbolId {
    pub fn new(component: &str, path: &str, name: &str, kind: SymbolKind) -> Self {
        SymbolId {
            component: component.to_string(),
            path: path.to_string(),
            name: name.to_string(),
            kind,
        }
    }

    pub fn renamed(&self, name: &str) -> Self {
        SymbolId { name: name.to_string(), ..self.clone() }
    }
}

impl std::fmt::Display for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}#{} ({})", self.component, self.path, self.name, self.kind.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Content {
    Inline(String),
    Blob(Hash),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Placement {
    First,
    Last,
    /// Immediately after this symbol (same component and path, live).
    After(SymbolId),
    /// Immediately before this symbol (same component and path, live).
    Before(SymbolId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transformation {
    Create(Content),
    Replace(Content),
    Delete,
    Rename(String),
    Move(Placement),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PatchRequest {
    pub workspace: WorkspaceId,
    pub symbol: SymbolId,
    pub parent: Option<Hash>,
    pub change: Transformation,
    pub agent: Agent,
    pub message: Option<String>,
    pub depends_on: Vec<SymbolId>,
    pub implements: Vec<SymbolId>,
    pub wit_binding: Option<String>,
    /// A `symbol-view`'s `as-of`; `None` measures `commuted` from the parent's op (over-approximates).
    #[serde(default)]
    pub read_at: Option<OpId>,
    /// `create` only (`None` appends); a `move` carries its own.
    #[serde(default)]
    pub position: Option<Placement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PatchOutcome {
    Applied,
    Commuted,
    Conflicted,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CommitResult {
    pub patch: Hash,
    pub op: OpId,
    pub outcome: PatchOutcome,
    pub tip: Option<Hash>,
    pub commuted_with: Vec<Hash>,
    pub conflict: Option<ConflictId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictState {
    Open,
    Resolved,
    Abandoned,
}

impl ConflictState {
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictState::Open => "open",
            ConflictState::Resolved => "resolved",
            ConflictState::Abandoned => "abandoned",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictSide {
    pub patch: Hash,
    pub agent: Agent,
    pub content: Option<Content>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Conflict {
    pub id: ConflictId,
    pub workspace: WorkspaceId,
    pub symbol: SymbolId,
    pub base: Option<Hash>,
    pub left: ConflictSide,
    pub right: ConflictSide,
    pub state: ConflictState,
    pub opened_at: OpId,
    pub resolved_by: Option<Hash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionRequest {
    pub workspace: WorkspaceId,
    pub conflict: ConflictId,
    pub resolution: Transformation,
    pub agent: Agent,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerMove {
    pub pointer: String,
    pub before: Option<Hash>,
    pub after: Option<Hash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpKind {
    Apply(Hash),
    Resolve(ConflictId),
    Revert(OpId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpEntry {
    pub id: OpId,
    pub workspace: WorkspaceId,
    pub at: u64,
    pub agent: Agent,
    pub kind: OpKind,
    pub moves: Vec<PointerMove>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolQuery {
    Symbol(SymbolId),
    Component(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SymbolView {
    pub id: SymbolId,
    pub tip: Hash,
    pub content: Content,
    pub author: Agent,
    pub wit_binding: Option<String>,
    pub depends_on: Vec<SymbolId>,
    pub dependents: Vec<SymbolId>,
    pub implements: Vec<SymbolId>,
    pub open_conflicts: Vec<ConflictId>,
    /// Every op at or below it is reflected; pass it back as `patch-request.read-at`.
    pub as_of: OpId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeEntry {
    pub path: String,
    pub blob: Hash,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Snapshot {
    pub workspace: WorkspaceId,
    pub component: String,
    pub at: OpId,
    pub entries: Vec<TreeEntry>,
    pub git_tree: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CasFailure {
    pub pointer: String,
    pub expected: Option<Hash>,
    pub actual: Option<Hash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerState {
    pub pointer: String,
    pub value: Option<Hash>,
    pub op: Option<OpId>,
}

/// An op still `pending` after the lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StaleOp {
    pub op: OpId,
    pub age_ms: u64,
    /// Its pointer write happened: repair rolls it forward, else aborts it.
    pub landed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingRecord {
    /// The pointer (or conflict id) that names it.
    pub by: String,
    pub hash: Hash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Inconsistency {
    /// Names no op, a pending or aborted one, or one whose logged move wrote something else.
    UnexplainedPointer(PointerState),
    StalePending(StaleOp),
    MissingPatch(MissingRecord),
    /// A symbol whose graph index entry is missing, or behind its pointer.
    StaleMirror(String),
    DanglingConflict(MissingRecord),
    /// Nothing committed opened it, or its `left` is no longer its symbol's tip.
    OrphanConflict(ConflictId),
    /// A name reserved for a symbol no longer called that, or a live symbol's name not reserved.
    StaleName(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ConsistencyReport {
    pub workspace: WorkspaceId,
    /// Ops read (every id in the log, whatever its state).
    pub ops: u64,
    /// Ops pending inside the lease — in flight, not inconsistent.
    pub in_flight: Vec<OpId>,
    pub issues: Vec<Inconsistency>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RepairReport {
    pub workspace: WorkspaceId,
    pub rolled_forward: Vec<OpId>,
    pub aborted: Vec<OpId>,
    /// Issues the first verify found that the second did not.
    pub fixed: Vec<Inconsistency>,
    /// Issues left: none, unless something outside the engine wrote a store.
    pub remaining: Vec<Inconsistency>,
}
