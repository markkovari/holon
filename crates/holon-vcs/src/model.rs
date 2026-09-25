//! The records `holon:vcs/types` declares, one Rust type per WIT type, field for
//! field. Serde names follow the WIT (kebab-case), so a JSON rendering of any of
//! these reads like the contract.
//!
//! Nothing in here has behaviour. The decisions live in [`crate::patch`] and
//! [`crate::engine`]; the records the *graph* keeps (which carry more than the
//! contract shows) live in [`crate::graph`].

use serde::{Deserialize, Serialize};

/// Lower-case hex SHA-256 — of a blob's bytes, or of a patch's canonical form.
pub type Hash = String;
/// A conflict is named by the hash of its two sides.
pub type ConflictId = String;
/// Position in a workspace's operation log. Strictly increasing per workspace,
/// starting at 1.
pub type OpId = u64;
/// A mutable line of work.
pub type WorkspaceId = String;

/// Who made a change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub goal: Option<String>,
    pub model: Option<String>,
}

impl Agent {
    /// An agent with only an id — what tests and simple callers need.
    pub fn named(id: impl Into<String>) -> Self {
        Agent { id: id.into(), goal: None, model: None }
    }
}

/// What kind of thing a symbol is.
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
    /// A whole file.
    File,
}

impl SymbolKind {
    /// The WIT spelling (`struct-item`), used in canonical encodings — stable
    /// because the contract is.
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

/// A symbol's identity: the file within the component, and the item path inside it.
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

    /// The same symbol under another name (what `rename` produces).
    pub fn renamed(&self, name: &str) -> Self {
        SymbolId { name: name.to_string(), ..self.clone() }
    }
}

impl std::fmt::Display for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}#{} ({})", self.component, self.path, self.name, self.kind.as_str())
    }
}

/// Content, inline or already in the object store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Content {
    Inline(String),
    Blob(Hash),
}

/// Where a symbol sits in its file, relative to the file's other live symbols.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Placement {
    /// Before every other symbol of the file.
    First,
    /// After every other symbol of the file.
    Last,
    /// Immediately after this symbol (same component and path, live).
    After(SymbolId),
    /// Immediately before this symbol (same component and path, live).
    Before(SymbolId),
}

/// What an edit does to its symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transformation {
    Create(Content),
    Replace(Content),
    Delete,
    Rename(String),
    /// Same content, new place in the file.
    Move(Placement),
}

/// One edit, as an agent submits it.
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
    /// The oplog position the agent's view reflects (a `symbol-view`'s `as-of`,
    /// or `oplog-head`). `None`: unknown, and `commuted` is measured from the
    /// parent's op instead (an over-approximation).
    #[serde(default)]
    pub read_at: Option<OpId>,
    /// Where a `create` goes in its file. `None` appends. Only `create` takes
    /// one; a `move` carries its own.
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
    /// The settled oplog head read before this view: every op at or below it
    /// is reflected. Pass it back as `patch-request.read-at`.
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

/// The pointer and the two values a compare-and-set disagreed on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CasFailure {
    pub pointer: String,
    pub expected: Option<Hash>,
    pub actual: Option<Hash>,
}

// ---- verify / repair ----------------------------------------------------------

/// A pointer, what it holds, and the op its value names.
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
    /// Whether its pointer write happened (repair rolls it forward) or not
    /// (repair aborts it).
    pub landed: bool,
}

/// A record the graph should have and does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingRecord {
    /// The pointer (or conflict id) that names it.
    pub by: String,
    pub hash: Hash,
}

/// One way the three stores can disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Inconsistency {
    /// A pointer whose value no committed op explains: it names no op, a pending
    /// or aborted one, or one whose logged move wrote something else.
    UnexplainedPointer(PointerState),
    /// An op still pending past the lease.
    StalePending(StaleOp),
    /// A tip whose patch record the graph lacks.
    MissingPatch(MissingRecord),
    /// A symbol whose graph index entry is missing, or behind its pointer.
    StaleMirror(String),
    /// A conflict naming a patch the graph lacks.
    DanglingConflict(MissingRecord),
    /// An open conflict nothing committed opened, or whose `left` is no longer
    /// its symbol's tip.
    OrphanConflict(ConflictId),
    /// A name reservation held for a symbol no longer called that, or a live
    /// symbol whose name is not reserved for it.
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
    /// Pending ops whose pointer write had happened: finished.
    pub rolled_forward: Vec<OpId>,
    /// Pending ops whose pointer write had not (and now cannot) happen.
    pub aborted: Vec<OpId>,
    /// Issues the first verify found that the second did not.
    pub fixed: Vec<Inconsistency>,
    /// Issues left: none, unless something outside the engine wrote a store.
    pub remaining: Vec<Inconsistency>,
}
