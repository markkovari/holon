//! The contract's records, between this crate's generated bindings and
//! `holon_vcs::model` (the serde types the wire is made of) — both directions.
//!
//! Shared by `vcs-store` and `vcs-gateway` as a SOURCE file (`#[path]`), not a
//! library: each crate's `bindings.rs` makes its own Rust types for the same
//! WIT types (ADR-0095), so this is compiled once per crate against
//! `crate::wit::t` (the `holon:vcs/types` bindings) and `crate::wit::files`
//! (the `holon:vcs/files` ones), which each crate points at its own.

use holon_vcs::model as m;
use holon_vcs::wire;
use holon_vcs::VcsError;

use crate::wit::files as f;
use crate::wit::t;

pub trait ToWit {
    type Out;
    fn to_wit(self) -> Self::Out;
}

pub trait ToModel {
    type Out;
    fn to_model(self) -> Self::Out;
}

macro_rules! identity {
    ($($ty:ty),*) => {$(
        impl ToWit for $ty { type Out = $ty; fn to_wit(self) -> $ty { self } }
        impl ToModel for $ty { type Out = $ty; fn to_model(self) -> $ty { self } }
    )*};
}
identity!(String, u64, u32, bool);

impl<T: ToWit> ToWit for Option<T> {
    type Out = Option<T::Out>;
    fn to_wit(self) -> Self::Out {
        self.map(ToWit::to_wit)
    }
}
impl<T: ToModel> ToModel for Option<T> {
    type Out = Option<T::Out>;
    fn to_model(self) -> Self::Out {
        self.map(ToModel::to_model)
    }
}
impl<T: ToWit> ToWit for Vec<T> {
    type Out = Vec<T::Out>;
    fn to_wit(self) -> Self::Out {
        self.into_iter().map(ToWit::to_wit).collect()
    }
}
impl<T: ToModel> ToModel for Vec<T> {
    type Out = Vec<T::Out>;
    fn to_model(self) -> Self::Out {
        self.into_iter().map(ToModel::to_model).collect()
    }
}

/// A record with the same name and fields on both sides.
macro_rules! record {
    ($name:ident { $($field:ident),* $(,)? }) => {
        impl ToWit for m::$name {
            type Out = t::$name;
            fn to_wit(self) -> t::$name { t::$name { $($field: self.$field.to_wit()),* } }
        }
        impl ToModel for t::$name {
            type Out = m::$name;
            fn to_model(self) -> m::$name { m::$name { $($field: self.$field.to_model()),* } }
        }
    };
}

/// A variant or enum with the same name and cases on both sides; a case has
/// no payload or one.
macro_rules! variant {
    ($name:ident { $($case:ident $(($x:ident))?),* $(,)? }) => {
        impl ToWit for m::$name {
            type Out = t::$name;
            fn to_wit(self) -> t::$name {
                match self { $(m::$name::$case $(($x))? => t::$name::$case $(($x.to_wit()))?),* }
            }
        }
        impl ToModel for t::$name {
            type Out = m::$name;
            fn to_model(self) -> m::$name {
                match self { $(t::$name::$case $(($x))? => m::$name::$case $(($x.to_model()))?),* }
            }
        }
    };
}

record!(Agent { id, goal, model });
variant!(SymbolKind {
    Module, Function, Method, StructItem, EnumItem, TraitItem, ImplBlock, Constant,
    StaticItem, TypeAlias, MacroItem, WitInterface, WitWorld, WitType, File,
});
record!(SymbolId { component, path, name, kind });
variant!(Content { Inline(x), Blob(x) });
variant!(Placement { First, Last, After(x), Before(x) });
variant!(Transformation { Create(x), Replace(x), Delete, Rename(x), Move(x) });
record!(PatchRequest {
    workspace, symbol, parent, change, agent, message, depends_on, implements, wit_binding,
    read_at, position,
});
variant!(PatchOutcome { Applied, Commuted, Conflicted, Duplicate });
record!(CommitResult { patch, op, outcome, tip, commuted_with, conflict });
variant!(ConflictState { Open, Resolved, Abandoned });
record!(ConflictSide { patch, agent, content });
record!(Conflict { id, workspace, symbol, base, left, right, state, opened_at, resolved_by });
record!(ResolutionRequest { workspace, conflict, resolution, agent, message });
record!(PointerMove { pointer, before, after });
variant!(OpKind { Apply(x), Resolve(x), Revert(x) });
record!(OpEntry { id, workspace, at, agent, kind, moves });
variant!(SymbolQuery { Symbol(x), Component(x) });
record!(SymbolView {
    id, tip, content, author, wit_binding, depends_on, dependents, implements, open_conflicts,
    as_of,
});
record!(TreeEntry { path, blob, executable });
record!(Snapshot { workspace, component, at, entries, git_tree });
record!(CasFailure { pointer, expected, actual });
record!(PointerState { pointer, value, op });
record!(StaleOp { op, age_ms, landed });
record!(MissingRecord { by, hash });
variant!(Inconsistency {
    UnexplainedPointer(x), StalePending(x), MissingPatch(x), StaleMirror(x),
    DanglingConflict(x), OrphanConflict(x), StaleName(x),
});
record!(ConsistencyReport { workspace, ops, in_flight, issues });
record!(RepairReport { workspace, rolled_forward, aborted, fixed, remaining });

impl ToWit for VcsError {
    type Out = t::VcsError;
    fn to_wit(self) -> t::VcsError {
        match self {
            VcsError::Storage(s) => t::VcsError::StorageError(s),
            VcsError::ConcurrentModification(c) => t::VcsError::ConcurrentModification(c.to_wit()),
            VcsError::UnresolvedConflict(ids) => t::VcsError::UnresolvedConflict(ids),
            VcsError::SymbolNotFound(id) => t::VcsError::SymbolNotFound(id.to_wit()),
            VcsError::NameTaken(id) => t::VcsError::NameTaken(id.to_wit()),
            VcsError::NotFound(s) => t::VcsError::NotFound(s),
            VcsError::Invalid(s) => t::VcsError::Invalid(s),
        }
    }
}

impl ToModel for t::VcsError {
    type Out = VcsError;
    fn to_model(self) -> VcsError {
        match self {
            t::VcsError::StorageError(s) => VcsError::Storage(s),
            t::VcsError::ConcurrentModification(c) => VcsError::ConcurrentModification(c.to_model()),
            t::VcsError::UnresolvedConflict(ids) => VcsError::UnresolvedConflict(ids),
            t::VcsError::SymbolNotFound(id) => VcsError::SymbolNotFound(id.to_model()),
            t::VcsError::NameTaken(id) => VcsError::NameTaken(id.to_model()),
            t::VcsError::NotFound(s) => VcsError::NotFound(s),
            t::VcsError::Invalid(s) => VcsError::Invalid(s),
        }
    }
}

// ---- holon:vcs/files, against the wire's records ----------------------------------

impl ToWit for wire::EditKind {
    type Out = f::EditKind;
    fn to_wit(self) -> f::EditKind {
        match self {
            wire::EditKind::Create => f::EditKind::Create,
            wire::EditKind::Replace => f::EditKind::Replace,
            wire::EditKind::Delete => f::EditKind::Delete,
            wire::EditKind::Rename => f::EditKind::Rename,
            wire::EditKind::Move => f::EditKind::Move,
        }
    }
}

impl ToModel for f::EditKind {
    type Out = wire::EditKind;
    fn to_model(self) -> wire::EditKind {
        match self {
            f::EditKind::Create => wire::EditKind::Create,
            f::EditKind::Replace => wire::EditKind::Replace,
            f::EditKind::Delete => wire::EditKind::Delete,
            f::EditKind::Rename => wire::EditKind::Rename,
            f::EditKind::Move => wire::EditKind::Move,
        }
    }
}

impl ToWit for wire::IngestPatch {
    type Out = f::IngestPatch;
    fn to_wit(self) -> f::IngestPatch {
        f::IngestPatch {
            symbol: self.symbol.to_wit(),
            edit: self.edit.to_wit(),
            outcome: match self.outcome {
                wire::Outcome::Ok(c) => Ok(c.to_wit()),
                wire::Outcome::Err(e) => Err(e.into_error().to_wit()),
            },
        }
    }
}

impl ToModel for f::IngestPatch {
    type Out = wire::IngestPatch;
    fn to_model(self) -> wire::IngestPatch {
        wire::IngestPatch {
            symbol: self.symbol.to_model(),
            edit: self.edit.to_model(),
            outcome: match self.outcome {
                Ok(c) => wire::Outcome::Ok(c.to_model()),
                Err(e) => wire::Outcome::Err(wire::ErrorBody::from(&e.to_model())),
            },
        }
    }
}

impl ToWit for wire::IngestReport {
    type Out = f::IngestReport;
    fn to_wit(self) -> f::IngestReport {
        f::IngestReport {
            path: self.path,
            read_at: self.read_at,
            unchanged: self.unchanged.to_wit(),
            patches: self.patches.to_wit(),
        }
    }
}

impl ToModel for f::IngestReport {
    type Out = wire::IngestReport;
    fn to_model(self) -> wire::IngestReport {
        wire::IngestReport {
            path: self.path,
            read_at: self.read_at,
            unchanged: self.unchanged.to_model(),
            patches: self.patches.to_model(),
        }
    }
}

impl ToWit for wire::SourceFile {
    type Out = f::SourceFile;
    fn to_wit(self) -> f::SourceFile {
        f::SourceFile { path: self.path, content: self.content.0 }
    }
}

impl ToModel for f::SourceFile {
    type Out = wire::SourceFile;
    fn to_model(self) -> wire::SourceFile {
        wire::SourceFile { path: self.path, content: wire::Bytes(self.content) }
    }
}

impl ToWit for wire::Materialized {
    type Out = f::Materialized;
    fn to_wit(self) -> f::Materialized {
        f::Materialized {
            dir: self.dir,
            snapshot: self.snapshot.to_wit(),
            files: self.files,
            bytes: self.bytes,
        }
    }
}

impl ToModel for f::Materialized {
    type Out = wire::Materialized;
    fn to_model(self) -> wire::Materialized {
        wire::Materialized {
            dir: self.dir,
            snapshot: self.snapshot.to_model(),
            files: self.files,
            bytes: self.bytes,
        }
    }
}
