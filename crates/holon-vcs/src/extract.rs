//! Symbol extraction: real source files split into holon-vcs symbols, and back,
//! byte for byte (ADR-0099, *Extraction*).
//!
//! # The lossless rule
//!
//! [`extract`] cuts a file into pieces whose concatenation, in order, IS the
//! file — every byte (comments, blank lines, attributes, doc comments, `//!`
//! headers, a BOM, a missing final newline, CRLF) belongs to exactly one symbol.
//! Cuts are only ever made just after a `\n`, so every symbol but the last ends
//! in one, which is what `snapshot-export`'s layout needs to reproduce the file
//! exactly (it inserts a `\n` only between two symbols when the first lacks one).
//! [`extract`] checks both properties on its own output and, if a splitter ever
//! got it wrong, falls back to one `kind: file` symbol rather than lose a byte.
//!
//! Where the bytes between items go:
//!
//! * **Leading trivia** — comments, blank lines, `///` docs and `#[attributes]`
//!   above an item — belongs to the item below it.
//! * **The rest of an item's last line** — `} // why` — belongs to the item.
//! * **Two items on one line** (`fn a() {} fn b() {}`) are one symbol, named after
//!   the first: a cut there would not end in `\n`.
//! * **The file header** — BOM, shebang, inner attributes and `//!` docs, and the
//!   `use` / `extern crate` declarations before the first other item (in WIT: the
//!   `package` line and top-level `use`s) — is one symbol, `(header)`, kind
//!   `module`.
//! * **A later run of `use` declarations** is one symbol, `(use)`.
//! * **Trailing trivia** after the last item: whitespace joins the last item;
//!   anything with a comment in it is a `(tail)` symbol.
//!
//! # Granularity (Rust, via `syn`)
//!
//! One symbol per top-level item: `fn` (function), `struct`/`union`
//! (struct-item), `enum`, `trait` / trait alias (trait-item), `impl` (impl-block,
//! named `impl Order` / `impl Display for Order`), `const`, `static`, `type`
//! (type-alias), `macro_rules! m` (macro-item `m`) and other item macros
//! (macro-item `path!`), `mod m;` (module), `extern "C" { .. }` (module).
//!
//! * **impl blocks are the unit**, not their methods. A method is not
//!   independently meaningful to the store (its `Self` and its sibling methods are
//!   in the block). Two agents editing two methods of one impl therefore
//!   conflict; splitting methods out (an impl as head + methods + `(end)`, like an
//!   inline module) is the next refinement.
//! * **Inline modules recurse**: `mod tests { .. }` becomes a `tests` symbol (the
//!   attributes, the `mod tests {` line and the module's leading `use`s), one
//!   symbol per inner item (`tests::adds_up`), and `tests::(end)` (the closing
//!   brace and any trivia before it) — test modules are where agents collide most.
//!   A module whose braces share a line with its items is kept whole.
//! * Names repeat (two `impl Order` blocks, `#[cfg]`-twinned `fn`s): the second is
//!   `impl Order~2`, and so on, in file order.
//!
//! A file `syn` cannot parse is one `kind: file` symbol named by its path, as is
//! a file with no items (only comments, or empty), and any file that is neither
//! `.rs` nor `.wit`.
//!
//! # WIT
//!
//! A brace-matching splitter that knows `//`, `///` and nested `/* */` comments:
//! one symbol per top-level `interface` (wit-interface) and `world` (wit-world),
//! a nested `package x:y { .. }` block as one module, and the `package` / `use`
//! header as `(header)`. `wit-parser` is not used: its spans are for diagnostics
//! and it resolves more than a splitter needs. Unbalanced braces fall back to a
//! `kind: file` symbol.
//!
//! # Edges, best effort
//!
//! * `depends_on`: identifiers an item mentions that another item of the SAME FILE
//!   defines (by simple name — `Foo::new` makes a dependency on a local `new` too).
//! * `implements`: `impl Trait for X` where `Trait` is defined in the same file.
//! * `wit_binding`: an `impl exports::ns::pkg::iface::Guest for C` is bound to
//!   `ns:pkg/iface`; a bare `impl Guest for C` to `Guest` (the world's export — the
//!   world is in a `generate!` elsewhere); a WIT interface or world to
//!   `ns:pkg/name` (its package, without the version). The two spellings meet.
//!
//! # Ingest
//!
//! [`ingest_file`] and [`ingest_tree`] turn an edited copy of a file into patches
//! against the store as it was at the agent's read point (`read_at`): a symbol
//! whose bytes did not change produces no patch; a changed one is a `replace` whose
//! parent is its tip AT THE READ POINT (so an edit another agent landed since then
//! comes back as a conflict, never a silent overwrite); a new one is a `create`
//! placed right after the item before it in the file (or `first`); one that
//! changed place is a `move`; a vanished one a `delete`. Every patch carries the
//! read point as `read-at`, so `commuted` is exact. Every item of the file is its
//! own symbol wherever it stands. See [`ingest_tree`] for the details and
//! renames.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::engine::{symbol_pointer, Engine};
use crate::error::{Result, VcsError};
use crate::graph::Graph;
use crate::model::{
    Agent, CommitResult, ConflictId, Content, Hash, OpId, PatchOutcome, PatchRequest, Placement,
    SymbolId, SymbolKind, Transformation,
};
use crate::oplog::OpLog;
use crate::order;
use crate::store::{BlobStore, PointerStore};

/// Name of the file-header symbol.
pub const HEADER: &str = "(header)";
/// Name of a run of `use` declarations after the header.
pub const USES: &str = "(use)";
/// Name of the trailing-comments symbol.
pub const TAIL: &str = "(tail)";
/// Suffix of an inline module's closing piece (`tests::(end)`).
pub const END: &str = "(end)";

/// One symbol cut from a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSymbol {
    pub id: SymbolId,
    /// The symbol's exact bytes. Every symbol but a file's last ends in `\n`.
    pub content: String,
    /// Other symbols of the same file this one mentions (best effort).
    pub depends_on: Vec<SymbolId>,
    /// Traits of the same file this `impl` implements.
    pub implements: Vec<SymbolId>,
    /// The WIT name this symbol implements or is (best effort, see the module docs).
    pub wit_binding: Option<String>,
}

/// Which splitter a path gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Wit,
    Other,
}

pub fn language_of(path: &str) -> Language {
    if path.ends_with(".rs") {
        Language::Rust
    } else if path.ends_with(".wit") {
        Language::Wit
    } else {
        Language::Other
    }
}

/// Split `bytes` into symbols, in file order. The concatenation of their
/// `content` is `bytes`. Non-UTF-8 input is refused: it is not source code, and
/// [`ingest_file`] stores it as one `kind: file` blob instead.
pub fn extract(
    component: &str,
    path: &str,
    bytes: &[u8],
) -> std::result::Result<Vec<ExtractedSymbol>, std::str::Utf8Error> {
    Ok(extract_str(component, path, std::str::from_utf8(bytes)?))
}

/// [`extract`] over text.
pub fn extract_str(component: &str, path: &str, text: &str) -> Vec<ExtractedSymbol> {
    let units = match language_of(path) {
        Language::Rust => rust::units(text),
        Language::Wit => wit::units(text),
        Language::Other => None,
    };
    let pieces = units
        .and_then(|u| layout(text, &u))
        .filter(|p| well_formed(text, p))
        .unwrap_or_else(|| whole_file(text, path));
    finish(component, path, text, pieces)
}

// ---- the language-neutral layout ------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Shebang, inner attribute, WIT `package x;` — only ever part of `(header)`.
    Header,
    /// `use`, `extern crate` — part of `(header)` when leading, else `(use)`.
    Use,
    Item,
}

#[derive(Debug, Clone, Default)]
struct Meta {
    defines: Vec<String>,
    idents: BTreeSet<String>,
    implements: Vec<String>,
    binding: Option<String>,
}

impl Meta {
    fn absorb(&mut self, o: &Meta) {
        self.defines.extend(o.defines.iter().cloned());
        self.idents.extend(o.idents.iter().cloned());
        self.implements.extend(o.implements.iter().cloned());
        if self.binding.is_none() {
            self.binding = o.binding.clone();
        }
    }
}

/// One item as a splitter sees it. Only its END matters for the cuts: where it
/// starts is decided by the lossless rule (leading trivia is its own).
#[derive(Debug, Clone)]
struct Unit {
    /// Byte just past the item's last token.
    end: usize,
    name: String,
    kind: SymbolKind,
    role: Role,
    meta: Meta,
    body: Option<Body>,
}

/// An inline module's braces and items.
#[derive(Debug, Clone)]
struct Body {
    /// Just past `{`.
    open_end: usize,
    /// At `}`.
    close_start: usize,
    units: Vec<Unit>,
}

#[derive(Debug, Clone)]
struct Piece {
    start: usize,
    end: usize,
    name: String,
    kind: SymbolKind,
    meta: Meta,
}

/// Where the line an item ends on ends: skip blanks and comments from `pos`; the
/// cut is just after the first `\n` outside a block comment. `None` if another
/// token comes first. At `limit`, `Some(limit)` only if `eof_ok`.
fn line_end(text: &str, pos: usize, limit: usize, eof_ok: bool) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = pos;
    loop {
        if i >= limit {
            return eof_ok.then_some(limit);
        }
        match b[i] {
            b'\n' => return Some(i + 1),
            b' ' | b'\t' | b'\r' | 0x0b | 0x0c => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < limit && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = skip_block_comment(b, i)?;
                if i > limit {
                    return None;
                }
            }
            _ => return None,
        }
    }
}

/// `b[i..]` starts with `/*`; the index just past its (nesting) `*/`.
fn skip_block_comment(b: &[u8], mut i: usize) -> Option<usize> {
    let mut depth = 0usize;
    while i + 1 < b.len() {
        if b[i] == b'/' && b[i + 1] == b'*' {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b[i + 1] == b'/' {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return Some(i);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Units grouped into cuttable runs: a group closes at the first unit whose line
/// ends cleanly. `None` if the last group cannot close.
fn groups<'u>(
    text: &str,
    units: &'u [Unit],
    limit: usize,
    eof_ok: bool,
) -> Option<Vec<(Vec<&'u Unit>, usize)>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for u in units {
        if u.end > limit {
            return None;
        }
        cur.push(u);
        if let Some(cut) = line_end(text, u.end, limit, eof_ok) {
            out.push((std::mem::take(&mut cur), cut));
        }
    }
    cur.is_empty().then_some(out)
}

struct Region {
    /// End of the leading header/use groups, and what they carry.
    header: Option<(usize, Meta)>,
    pieces: Vec<Piece>,
    /// End of the last group (the region's trailing trivia starts here).
    end: usize,
}

fn region(
    text: &str,
    lo: usize,
    hi: usize,
    units: &[Unit],
    prefix: &str,
    eof_ok: bool,
) -> Option<Region> {
    let gs = groups(text, units, hi, eof_ok)?;
    let mut start = lo;
    let mut i = 0;
    let mut header = None;
    let mut hmeta = Meta::default();
    while i < gs.len() && gs[i].0.iter().all(|u| u.role != Role::Item) {
        for u in &gs[i].0 {
            hmeta.absorb(&u.meta);
        }
        start = gs[i].1;
        i += 1;
    }
    if i > 0 {
        header = Some((start, hmeta));
    }
    let mut pieces = Vec::new();
    while i < gs.len() {
        let (members, cut) = &gs[i];
        if members.iter().all(|u| u.role != Role::Item) {
            // A run of `use` groups.
            let mut meta = Meta::default();
            let mut end = *cut;
            while i < gs.len() && gs[i].0.iter().all(|u| u.role != Role::Item) {
                for u in &gs[i].0 {
                    meta.absorb(&u.meta);
                }
                end = gs[i].1;
                i += 1;
            }
            pieces.push(Piece {
                start,
                end,
                name: format!("{prefix}{USES}"),
                kind: SymbolKind::Module,
                meta,
            });
            start = end;
            continue;
        }
        let first = members[0];
        let name = format!("{prefix}{}", first.name);
        let recursed = match (&first.body, members.len()) {
            (Some(body), 1) => module_pieces(text, start, *cut, &name, &first.meta, body),
            _ => None,
        };
        match recursed {
            Some(ps) => pieces.extend(ps),
            None => {
                let mut meta = Meta::default();
                for u in members {
                    meta.absorb(&u.meta);
                }
                pieces.push(Piece { start, end: *cut, name, kind: first.kind, meta });
            }
        }
        start = *cut;
        i += 1;
    }
    Some(Region { header, pieces, end: start })
}

/// An inline module as head + inner items + `(end)`, if its body can be cut on
/// line boundaries and has at least one item.
fn module_pieces(
    text: &str,
    start: usize,
    end: usize,
    name: &str,
    meta: &Meta,
    body: &Body,
) -> Option<Vec<Piece>> {
    let inner_lo = line_end(text, body.open_end, body.close_start, false)?;
    let r = region(text, inner_lo, body.close_start, &body.units, &format!("{name}::"), false)?;
    if r.pieces.is_empty() {
        return None;
    }
    let head_end = r.header.as_ref().map(|(e, _)| *e).unwrap_or(inner_lo);
    let mut head_meta = Meta { defines: meta.defines.clone(), ..Meta::default() };
    if let Some((_, m)) = &r.header {
        head_meta.idents.extend(m.idents.iter().cloned());
    }
    let mut out = vec![Piece {
        start,
        end: head_end,
        name: name.to_string(),
        kind: SymbolKind::Module,
        meta: head_meta,
    }];
    let inner_end = r.end;
    out.extend(r.pieces);
    out.push(Piece {
        start: inner_end,
        end,
        name: format!("{name}::{END}"),
        kind: SymbolKind::Module,
        meta: Meta::default(),
    });
    Some(out)
}

/// The top level: header, items, tail. `None` if there is nothing to split.
fn layout(text: &str, units: &[Unit]) -> Option<Vec<Piece>> {
    let r = region(text, 0, text.len(), units, "", true)?;
    let mut pieces = Vec::new();
    if let Some((end, meta)) = r.header {
        pieces.push(Piece { start: 0, end, name: HEADER.into(), kind: SymbolKind::Module, meta });
    }
    pieces.extend(r.pieces);
    if pieces.is_empty() {
        return None;
    }
    if r.end < text.len() {
        let tail = &text[r.end..];
        if tail.trim().is_empty() {
            pieces.last_mut().expect("non-empty").end = text.len();
        } else {
            pieces.push(Piece {
                start: r.end,
                end: text.len(),
                name: TAIL.into(),
                kind: SymbolKind::Module,
                meta: Meta::default(),
            });
        }
    }
    Some(pieces)
}

fn whole_file(text: &str, path: &str) -> Vec<Piece> {
    vec![Piece {
        start: 0,
        end: text.len(),
        name: path.to_string(),
        kind: SymbolKind::File,
        meta: Meta::default(),
    }]
}

/// The lossless rule, checked: contiguous, covering, non-empty (unless the file
/// is), every piece but the last ending in `\n`, and on char boundaries.
fn well_formed(text: &str, pieces: &[Piece]) -> bool {
    let mut at = 0;
    for (i, p) in pieces.iter().enumerate() {
        if p.start != at || p.end <= p.start || p.end > text.len() {
            return false;
        }
        if !text.is_char_boundary(p.start) || !text.is_char_boundary(p.end) {
            return false;
        }
        if i + 1 < pieces.len() && text.as_bytes()[p.end - 1] != b'\n' {
            return false;
        }
        at = p.end;
    }
    at == text.len()
}

fn finish(component: &str, path: &str, text: &str, pieces: Vec<Piece>) -> Vec<ExtractedSymbol> {
    // Unique names, in file order.
    let mut seen: HashMap<(String, SymbolKind), u32> = HashMap::new();
    let ids: Vec<SymbolId> = pieces
        .iter()
        .map(|p| {
            let n = seen.entry((p.name.clone(), p.kind)).or_insert(0);
            *n += 1;
            let name = if *n == 1 { p.name.clone() } else { format!("{}~{n}", p.name) };
            SymbolId::new(component, path, &name, p.kind)
        })
        .collect();
    let mut definers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, p) in pieces.iter().enumerate() {
        for d in &p.meta.defines {
            definers.entry(d.as_str()).or_default().push(i);
        }
    }
    let refer = |i: usize, names: &mut dyn Iterator<Item = &String>| -> Vec<SymbolId> {
        let mut to = BTreeSet::new();
        for n in names {
            for &j in definers.get(n.as_str()).map(Vec::as_slice).unwrap_or(&[]) {
                if j != i {
                    to.insert(j);
                }
            }
        }
        to.into_iter().map(|j| ids[j].clone()).collect()
    };
    pieces
        .iter()
        .enumerate()
        .map(|(i, p)| ExtractedSymbol {
            id: ids[i].clone(),
            content: text[p.start..p.end].to_string(),
            depends_on: refer(i, &mut p.meta.idents.iter().filter(|n| !p.meta.defines.contains(n))),
            implements: refer(i, &mut p.meta.implements.iter()),
            wit_binding: p.meta.binding.clone(),
        })
        .collect()
}

/// Collapse whitespace runs to one space (for names built from source text).
fn compact(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---- Rust -------------------------------------------------------------------------

mod rust {
    use std::collections::BTreeSet;

    use proc_macro2::{TokenStream, TokenTree};
    use quote::ToTokens;
    use syn::spanned::Spanned;
    use syn::{AttrStyle, Item};

    use super::{compact, Body, Meta, Role, Unit};
    use crate::model::SymbolKind;

    /// `None` when `syn` cannot parse the file.
    pub(super) fn units(text: &str) -> Option<Vec<Unit>> {
        // What `syn::parse_file` strips, stripped here so offsets can be restored.
        let mut base = 0;
        if text.starts_with('\u{feff}') {
            base = '\u{feff}'.len_utf8();
        }
        let mut shebang_end = None;
        let rest = &text[base..];
        if rest.starts_with("#!") && !rest[2..].trim_start().starts_with('[') {
            let n = rest.find('\n').unwrap_or(rest.len());
            base += n;
            shebang_end = Some(base);
        }
        let src = &text[base..];
        let out = syn::parse_str::<syn::File>(src).ok().map(|file| {
            let cx = Cx { src, base };
            let mut units = Vec::new();
            if let Some(end) = shebang_end {
                units.push(header_unit(end));
            }
            units.extend(inner_attrs(&cx, &file.attrs));
            units.extend(file.items.iter().map(|i| cx.unit(i)));
            units
        });
        // The spans above live in a thread-local source map that only grows;
        // nothing of them survives this call.
        proc_macro2::extra::invalidate_current_thread_spans();
        let units = out?;
        // Offsets must only move forward, or the layout is not ours to trust.
        let mut last = 0;
        for u in &units {
            if u.end < last || u.end > text.len() {
                return None;
            }
            last = u.end;
        }
        Some(units)
    }

    fn header_unit(end: usize) -> Unit {
        Unit {
            end,
            name: String::new(),
            kind: SymbolKind::Module,
            role: Role::Header,
            meta: Meta::default(),
            body: None,
        }
    }

    fn inner_attrs(cx: &Cx, attrs: &[syn::Attribute]) -> Vec<Unit> {
        attrs
            .iter()
            .filter(|a| matches!(a.style, AttrStyle::Inner(_)))
            .map(|a| header_unit(cx.end(a)))
            .collect()
    }

    struct Cx<'a> {
        src: &'a str,
        base: usize,
    }

    impl Cx<'_> {
        fn end(&self, t: &impl Spanned) -> usize {
            t.span().byte_range().end + self.base
        }
        fn text(&self, t: &impl Spanned) -> String {
            let r = t.span().byte_range();
            compact(self.src.get(r).unwrap_or_default())
        }

        fn unit(&self, item: &Item) -> Unit {
            let mut meta = Meta::default();
            collect_idents(item.to_token_stream(), &mut meta.idents);
            let mut body = None;
            let (name, kind, role) = match item {
                Item::Fn(f) => (ident(&f.sig.ident), SymbolKind::Function, Role::Item),
                Item::Struct(s) => (ident(&s.ident), SymbolKind::StructItem, Role::Item),
                Item::Union(u) => (ident(&u.ident), SymbolKind::StructItem, Role::Item),
                Item::Enum(e) => (ident(&e.ident), SymbolKind::EnumItem, Role::Item),
                Item::Trait(t) => (ident(&t.ident), SymbolKind::TraitItem, Role::Item),
                Item::TraitAlias(t) => (ident(&t.ident), SymbolKind::TraitItem, Role::Item),
                Item::Const(c) => (ident(&c.ident), SymbolKind::Constant, Role::Item),
                Item::Static(s) => (ident(&s.ident), SymbolKind::StaticItem, Role::Item),
                Item::Type(t) => (ident(&t.ident), SymbolKind::TypeAlias, Role::Item),
                Item::Mod(m) => {
                    if let Some((brace, items)) = &m.content {
                        let mut units = inner_attrs(self, &m.attrs);
                        units.extend(items.iter().map(|i| self.unit(i)));
                        body = Some(Body {
                            open_end: brace.span.open().byte_range().end + self.base,
                            close_start: brace.span.close().byte_range().start + self.base,
                            units,
                        });
                    }
                    (ident(&m.ident), SymbolKind::Module, Role::Item)
                }
                Item::Macro(m) => match &m.ident {
                    Some(i) => (ident(i), SymbolKind::MacroItem, Role::Item),
                    None => {
                        (format!("{}!", self.text(&m.mac.path)), SymbolKind::MacroItem, Role::Item)
                    }
                },
                Item::Impl(i) => {
                    let self_ty = self.text(&*i.self_ty);
                    let name = match &i.trait_ {
                        Some((path, _)) => {
                            if let Some(last) = path.segments.last() {
                                meta.implements.push(ident(&last.ident));
                            }
                            meta.binding = binding(path);
                            let neg = if i.modifiers.polarity.is_some() { "!" } else { "" };
                            format!("impl {neg}{} for {self_ty}", self.text(path))
                        }
                        None => format!("impl {self_ty}"),
                    };
                    (name, SymbolKind::ImplBlock, Role::Item)
                }
                Item::ForeignMod(f) => (
                    format!("extern {}", self.text(&f.abi.name)).trim().to_string(),
                    SymbolKind::Module,
                    Role::Item,
                ),
                Item::Use(_) | Item::ExternCrate(_) => {
                    (String::new(), SymbolKind::Module, Role::Use)
                }
                _ => ("(item)".to_string(), SymbolKind::Module, Role::Item),
            };
            let defines = !matches!(item, Item::Impl(_) | Item::ForeignMod(_))
                && !matches!(item, Item::Macro(m) if m.ident.is_none());
            if role == Role::Item && defines {
                // Only the NAME a definition introduces; other items refer to it.
                let n = name.as_str();
                if !n.is_empty() && !n.contains(' ') && !n.starts_with('(') {
                    meta.defines.push(n.to_string());
                }
            }
            Unit { end: self.end(item), name, kind, role, meta, body }
        }
    }

    fn ident(i: &proc_macro2::Ident) -> String {
        let s = i.to_string();
        s.strip_prefix("r#").map(str::to_string).unwrap_or(s)
    }

    fn collect_idents(ts: TokenStream, out: &mut BTreeSet<String>) {
        for tt in ts {
            match tt {
                TokenTree::Ident(i) => {
                    out.insert(ident(&i));
                }
                TokenTree::Group(g) => collect_idents(g.stream(), out),
                _ => {}
            }
        }
    }

    /// `exports::ns::pkg::iface::Guest` → `ns:pkg/iface`; a bare `…Guest` → the
    /// path as written.
    fn binding(path: &syn::Path) -> Option<String> {
        let segs: Vec<String> = path.segments.iter().map(|s| ident(&s.ident)).collect();
        let last = segs.last()?;
        if let Some(at) = segs.iter().position(|s| s == "exports") {
            let rest = &segs[at + 1..segs.len() - 1];
            if rest.len() >= 3 {
                let kebab = |s: &String| s.replace('_', "-");
                let ifaces: Vec<String> = rest[2..].iter().map(kebab).collect();
                return Some(format!(
                    "{}:{}/{}",
                    kebab(&rest[0]),
                    kebab(&rest[1]),
                    ifaces.join("/")
                ));
            }
        }
        (last == "Guest").then(|| segs.join("::"))
    }
}

// ---- WIT --------------------------------------------------------------------------

mod wit {
    use super::{skip_block_comment, Meta, Role, Unit};
    use crate::model::SymbolKind;

    fn is_word(c: u8) -> bool {
        c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'%'
    }

    /// Past whitespace and comments. `None` on an unterminated block comment.
    fn skip_trivia(b: &[u8], mut i: usize) -> Option<usize> {
        loop {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if b[i..].starts_with(b"//") {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            } else if b[i..].starts_with(b"/*") {
                i = skip_block_comment(b, i)?;
            } else {
                return Some(i);
            }
        }
    }

    /// `None` when the braces do not balance (the caller keeps the file whole).
    pub(super) fn units(text: &str) -> Option<Vec<Unit>> {
        let b = text.as_bytes();
        let mut i = if text.starts_with('\u{feff}') { 3 } else { 0 };
        let mut units: Vec<Unit> = Vec::new();
        let mut package: Option<String> = None;
        loop {
            i = skip_trivia(b, i)?;
            if i >= b.len() {
                break;
            }
            // One top-level item: to `;` at depth 0, or the `}` that closes its body.
            let mut depth = 0usize;
            let mut parens = 0usize;
            let mut opened = false;
            let mut after_at = false;
            // (start, end) of each word before the body, outside `@gate(...)`.
            let mut head: Vec<(usize, usize)> = Vec::new();
            let mut head_end = None;
            let mut meta = Meta::default();
            let end = loop {
                i = skip_trivia(b, i)?;
                if i >= b.len() {
                    return None;
                }
                let c = b[i];
                if is_word(c) {
                    let s = i;
                    while i < b.len() && is_word(b[i]) {
                        i += 1;
                    }
                    let w = &text[s..i];
                    meta.idents.insert(w.trim_start_matches('%').to_string());
                    if !opened && parens == 0 && !after_at {
                        head.push((s, i));
                    }
                    after_at = false;
                    continue;
                }
                after_at = c == b'@';
                match c {
                    b'(' => parens += 1,
                    b')' => parens = parens.saturating_sub(1),
                    b'{' => {
                        if !opened {
                            head_end = Some(i);
                        }
                        depth += 1;
                        opened = true;
                    }
                    b'}' => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            break i + 1;
                        }
                    }
                    b';' if depth == 0 => {
                        head_end.get_or_insert(i);
                        break i + 1;
                    }
                    b'"' => {
                        i += 1;
                        while i < b.len() && b[i] != b'"' {
                            i += if b[i] == b'\\' { 2 } else { 1 };
                        }
                    }
                    _ => {}
                }
                i += 1;
            };
            i = end;
            let word = |k: usize| head.get(k).map(|&(s, e)| &text[s..e]);
            let kw = word(0).unwrap_or_default();
            let rest = || {
                let from = head.first().map(|h| h.1).unwrap_or(end);
                super::compact(&text[from..head_end.unwrap_or(end)])
            };
            let qualified = |n: &str| {
                package.as_ref().map(|p| format!("{}/{}", p.split('@').next().unwrap_or(p), n))
            };
            let (name, kind, role) = match kw {
                "package" if !opened => {
                    package = Some(rest());
                    (String::new(), SymbolKind::Module, Role::Header)
                }
                "package" => (format!("package {}", rest()), SymbolKind::Module, Role::Item),
                "use" => (String::new(), SymbolKind::Module, Role::Use),
                "interface" | "world" => {
                    let n = word(1).unwrap_or("?").to_string();
                    meta.binding = qualified(n.trim_start_matches('%'));
                    meta.defines.push(n.trim_start_matches('%').to_string());
                    let kind =
                        if kw == "world" { SymbolKind::WitWorld } else { SymbolKind::WitInterface };
                    (n, kind, Role::Item)
                }
                other => (
                    super::compact(&format!("{other} {}", word(1).unwrap_or_default())),
                    SymbolKind::WitType,
                    Role::Item,
                ),
            };
            if let Some(d) = meta.defines.first().cloned() {
                meta.idents.remove(&d);
            }
            units.push(Unit { end, name, kind, role, meta, body: None });
        }
        Some(units)
    }
}

// ---- ingest -----------------------------------------------------------------------

/// What one patch of an ingest did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Create,
    Replace,
    Delete,
    Rename,
    /// Same content, new place in the file.
    Move,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestPatch {
    pub symbol: SymbolId,
    pub edit: EditKind,
    /// A store refusal (`unresolved-conflict`, `name-taken`, …) is recorded here
    /// and the ingest goes on with the other symbols — they commute. Only a
    /// storage outage aborts.
    pub result: std::result::Result<CommitResult, VcsError>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestReport {
    pub path: String,
    /// The read point the file was diffed against, and the `read-at` every
    /// patch carried.
    pub read_at: OpId,
    /// Symbols whose bytes and place did not change: no patch.
    pub unchanged: Vec<SymbolId>,
    pub patches: Vec<IngestPatch>,
}

impl IngestReport {
    /// Conflicts this ingest opened (or re-found).
    pub fn conflicts(&self) -> Vec<ConflictId> {
        self.patches.iter().filter_map(|p| p.result.as_ref().ok()?.conflict.clone()).collect()
    }
    pub fn errors(&self) -> Vec<&VcsError> {
        self.patches.iter().filter_map(|p| p.result.as_ref().err()).collect()
    }
    /// Every patch landed (applied, commuted or duplicate).
    pub fn is_clean(&self) -> bool {
        self.patches
            .iter()
            .all(|p| matches!(&p.result, Ok(r) if r.outcome != PatchOutcome::Conflicted))
    }
}

/// A live symbol as of a read point.
#[derive(Debug, Clone)]
struct Stored {
    key: String,
    id: SymbolId,
    tip: Hash,
    content: Vec<u8>,
    /// Its order key at the read point ([`crate::order`]).
    order: String,
}

/// The component as the agent read it.
struct Base {
    /// The read point.
    at: OpId,
    /// Live symbols at the read point, by path, in file order: (order key,
    /// symbol key), as `snapshot-export` lays them out.
    live: BTreeMap<String, Vec<Stored>>,
    /// Every symbol id the component's index has, live or dead: a `create` of
    /// one of these is a recreate, which keeps the old place unless placed.
    known: BTreeSet<SymbolId>,
}

/// The settled oplog head of `ws` (`oplog-head`): what an agent should
/// remember as its read point when it reads (snapshots, queries) before editing
/// — every op at or below it is reflected in what it read. `0` for an empty log.
pub async fn read_point<B, P, G, L>(engine: &Engine<B, P, G, L>, ws: &str) -> Result<OpId>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    engine.oplog_head(ws).await
}

/// The component's live symbols as of `read_at` (`None`: the settled head now):
/// tips replayed from the committed ops at or below it. The writers of a
/// pointer form a chain of increasing op ids, so the last committed op, in id
/// order, that moved a pointer is the value it held at the read point.
async fn base_at<B, P, G, L>(
    engine: &Engine<B, P, G, L>,
    ws: &str,
    component: &str,
    read_at: Option<OpId>,
) -> Result<Base>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let at = match read_at {
        Some(r) => r,
        None => engine.oplog_head(ws).await?,
    };
    let records = engine.graph().symbols_in_component(ws, component).await?;
    let mut tips: HashMap<String, Option<Hash>> = HashMap::new();
    let mut after = None;
    'pages: loop {
        let page = engine.oplog(ws, after, 256).await?;
        if page.is_empty() {
            break;
        }
        for e in page {
            if e.id > at {
                break 'pages;
            }
            after = Some(e.id);
            for m in e.moves {
                tips.insert(m.pointer, m.after);
            }
        }
    }
    let mut live: BTreeMap<String, Vec<Stored>> = BTreeMap::new();
    for rec in &records {
        let Some(tip) = tips.get(&symbol_pointer(ws, &rec.key)).cloned().flatten() else {
            continue;
        };
        let Some(p) = engine.graph().patch(ws, &tip).await? else {
            return Err(VcsError::Storage(format!(
                "a committed op names patch {tip}, which the graph does not have"
            )));
        };
        let Some(blob) = &p.content else { continue };
        let content = engine.blobs().get(blob).await?.ok_or_else(|| {
            VcsError::Storage(format!("blob {blob} is missing from the object store"))
        })?;
        let position = match rec.position {
            Some(p) => Some(p),
            // The listing raced the op that created it: read it again.
            None => engine.graph().symbol(ws, &rec.key).await?.and_then(|s| s.position),
        };
        let order =
            p.order.clone().unwrap_or_else(|| order::derived(position.unwrap_or(OpId::MAX)));
        live.entry(rec.path.clone()).or_default().push(Stored {
            key: rec.key.clone(),
            id: p.symbol.clone(),
            tip,
            content,
            order,
        });
    }
    for v in live.values_mut() {
        v.sort_by(|a, b| (&a.order, &a.key).cmp(&(&b.order, &b.key)));
    }
    let known =
        records.iter().map(|r| SymbolId::new(&r.component, &r.path, &r.name, r.kind)).collect();
    Ok(Base { at, live, known })
}

/// Where a planned symbol goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// It was in the file at the read point, and its place relative to the
    /// other kept symbols did not change: no move.
    Kept,
    /// It was in the file, somewhere else: a `move`.
    Moved,
    /// It is new: a placed `create`.
    New,
}

/// One symbol as it will be stored.
#[derive(Debug, Clone)]
struct Planned {
    id: SymbolId,
    content: Vec<u8>,
    depends_on: Vec<SymbolId>,
    implements: Vec<SymbolId>,
    wit_binding: Option<String>,
    /// The base symbol it is (index into the path's base list), if any.
    from: Option<usize>,
    /// `from` is a vanished symbol this one is a rename of.
    renamed: bool,
    place: Place,
}

/// How alike a vanished symbol and a new one must be to be taken as a rename.
pub const RENAME_SIMILARITY: f64 = 0.5;

/// Dice similarity of the two symbols' non-blank lines, after the old name is
/// replaced by the new one in the old content (so a pure rename scores 1).
fn similarity(old: &Stored, new: &ExtractedSymbol) -> f64 {
    let leaf = |n: &str| n.rsplit("::").next().unwrap_or(n).to_string();
    let (from, to) = (leaf(&old.id.name), leaf(&new.id.name));
    let old_text = String::from_utf8_lossy(&old.content).replace(&from, &to);
    let lines = |t: &str| -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for l in t.lines().map(str::trim).filter(|l| !l.is_empty()) {
            *m.entry(l.to_string()).or_insert(0) += 1;
        }
        m
    };
    let (a, b) = (lines(&old_text), lines(&new.content));
    let total: usize = a.values().sum::<usize>() + b.values().sum::<usize>();
    if total == 0 {
        return 0.0;
    }
    let common: usize = a.iter().map(|(l, n)| (*n).min(b.get(l).copied().unwrap_or(0))).sum();
    2.0 * common as f64 / total as f64
}

/// Longest strictly increasing subsequence of `(index, base index)`, as indexes.
fn lis(seq: &[(usize, usize)]) -> BTreeSet<usize> {
    let n = seq.len();
    let mut len = vec![1usize; n];
    let mut prev = vec![usize::MAX; n];
    for i in 0..n {
        for j in 0..i {
            if seq[j].1 < seq[i].1 && len[j] + 1 > len[i] {
                len[i] = len[j] + 1;
                prev[i] = j;
            }
        }
    }
    let mut out = BTreeSet::new();
    let Some(mut k) = (0..n).max_by_key(|&i| (len[i], std::cmp::Reverse(i))) else { return out };
    loop {
        out.insert(seq[k].0);
        if prev[k] == usize::MAX {
            break;
        }
        k = prev[k];
    }
    out
}

/// Match the extracted symbols against the file as the agent read it, and
/// decide where each goes.
///
/// A symbol whose id was in the file is that symbol. A new one standing where a
/// vanished one of the same kind stood (between the same matched neighbours),
/// with at least [`RENAME_SIMILARITY`] of its lines, is a rename of it. Of the
/// matched symbols, the longest run whose read-point order the file keeps
/// stays where it is ([`Place::Kept`]); every other matched symbol is
/// [`Place::Moved`], and the rest are [`Place::New`]. Every symbol of the file
/// is its own symbol: nothing is folded into a neighbour.
fn plan(extracted: Vec<ExtractedSymbol>, base: &[Stored]) -> Vec<Planned> {
    let index: HashMap<&SymbolId, usize> =
        base.iter().enumerate().map(|(i, s)| (&s.id, i)).collect();
    let mut from: Vec<Option<usize>> =
        extracted.iter().map(|e| index.get(&e.id).copied()).collect();
    let matched: BTreeSet<usize> = from.iter().flatten().copied().collect();
    let vanished: Vec<usize> = (0..base.len()).filter(|i| !matched.contains(i)).collect();
    let mut renamed = vec![false; extracted.len()];
    let mut used = BTreeSet::new();
    for i in 0..extracted.len() {
        if from[i].is_some() {
            continue;
        }
        let lo = (0..i).rev().find_map(|j| from[j]);
        let hi = (i + 1..extracted.len()).find_map(|j| from[j]);
        let pick = vanished
            .iter()
            .copied()
            .filter(|&v| {
                !used.contains(&v)
                    && base[v].id.kind == extracted[i].id.kind
                    && lo.is_none_or(|l| l < v)
                    && hi.is_none_or(|h| v < h)
            })
            .map(|v| (similarity(&base[v], &extracted[i]), v))
            .filter(|(score, _)| *score >= RENAME_SIMILARITY)
            .max_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)))
            .map(|(_, v)| v);
        if let Some(v) = pick {
            used.insert(v);
            from[i] = Some(v);
            renamed[i] = true;
        }
    }
    let seq: Vec<(usize, usize)> =
        from.iter().enumerate().filter_map(|(i, b)| b.map(|b| (i, b))).collect();
    let kept = lis(&seq);
    extracted
        .into_iter()
        .enumerate()
        .map(|(i, e)| Planned {
            id: e.id,
            content: e.content.into_bytes(),
            depends_on: e.depends_on,
            implements: e.implements,
            wit_binding: e.wit_binding,
            from: from[i],
            renamed: renamed[i],
            place: match from[i] {
                Some(_) if kept.contains(&i) => Place::Kept,
                Some(_) => Place::Moved,
                None => Place::New,
            },
        })
        .collect()
}

/// Ingest an edited copy of one file (see [`ingest_tree`]).
#[allow(clippy::too_many_arguments)]
pub async fn ingest_file<B, P, G, L>(
    engine: &Engine<B, P, G, L>,
    ws: &str,
    component: &str,
    path: &str,
    bytes: &[u8],
    agent: &Agent,
    read_at: Option<OpId>,
) -> Result<IngestReport>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let base = base_at(engine, ws, component, read_at).await?;
    ingest_one(engine, ws, component, path, bytes, agent, &base).await
}

/// Ingest a set of files (component-relative path, bytes) against the store as
/// of `read_at` (`None`: the settled head now, [`read_point`]). For each file,
/// only symbols whose bytes or place changed produce patches, every one
/// carrying `read-at`:
///
/// * `delete` for what the file no longer has (first);
/// * then in file order: `replace` (parent = the tip at the read point),
///   `rename` when a new item stands where a vanished one of the same kind
///   stood, `move` when an existing item changed place, and `create` for a new
///   one — placed `after(<the item before it in the file>)`, or `first`.
///
/// A new item with no existing item after it in the file is created unplaced
/// (`position: none`, which appends): the same place, and resolving an
/// explicit placement reads the component's other symbols, which would make
/// importing a whole file quadratic.
///
/// With `prune`, the set is the whole component: a file the store has and
/// `files` does not is deleted, symbol by symbol.
///
/// No filesystem here: [`read_tree`] is the host-side helper that reads one.
#[allow(clippy::too_many_arguments)]
pub async fn ingest_tree<B, P, G, L>(
    engine: &Engine<B, P, G, L>,
    ws: &str,
    component: &str,
    files: &[(String, Vec<u8>)],
    agent: &Agent,
    read_at: Option<OpId>,
    prune: bool,
) -> Result<Vec<IngestReport>>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let base = base_at(engine, ws, component, read_at).await?;
    let mut out = Vec::with_capacity(files.len());
    for (path, bytes) in files {
        out.push(ingest_one(engine, ws, component, path, bytes, agent, &base).await?);
    }
    if prune {
        let given: BTreeSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        for (path, syms) in &base.live {
            if given.contains(path.as_str()) {
                continue;
            }
            let cx = Submit { engine, ws, agent, path, read_at: base.at };
            let mut report =
                IngestReport { path: path.clone(), read_at: base.at, ..Default::default() };
            for s in syms {
                let r =
                    cx.send(&s.id, Some(s.tip.clone()), Transformation::Delete, None, None).await?;
                report.patches.push(IngestPatch {
                    symbol: s.id.clone(),
                    edit: EditKind::Delete,
                    result: r,
                });
            }
            out.push(report);
        }
    }
    Ok(out)
}

/// The patch a landed result names — what the next patch of the same symbol
/// builds on. `None` if it did not land (a conflict, a refusal).
fn landed(r: &std::result::Result<CommitResult, VcsError>) -> Option<Hash> {
    match r {
        Ok(c) if c.outcome != PatchOutcome::Conflicted => Some(c.patch.clone()),
        _ => None,
    }
}

async fn ingest_one<B, P, G, L>(
    engine: &Engine<B, P, G, L>,
    ws: &str,
    component: &str,
    path: &str,
    bytes: &[u8],
    agent: &Agent,
    base: &Base,
) -> Result<IngestReport>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    crate::git::validate_path(path)?;
    let cx = Submit { engine, ws, agent, path, read_at: base.at };
    let empty = Vec::new();
    let stored = base.live.get(path).unwrap_or(&empty);
    let planned = match extract(component, path, bytes) {
        Ok(extracted) => plan(extracted, stored),
        // Not UTF-8: not source; stored as one `kind: file` blob.
        Err(_) => plan(
            vec![ExtractedSymbol {
                id: SymbolId::new(component, path, path, SymbolKind::File),
                content: String::new(),
                depends_on: vec![],
                implements: vec![],
                wit_binding: None,
            }],
            stored,
        )
        .into_iter()
        .map(|p| Planned { content: bytes.to_vec(), ..p })
        .collect(),
    };
    let mut report =
        IngestReport { path: path.to_string(), read_at: base.at, ..Default::default() };
    let survivors: BTreeSet<usize> = planned.iter().filter_map(|p| p.from).collect();

    // Deletes first: what the file no longer has.
    for (i, s) in stored.iter().enumerate() {
        if survivors.contains(&i) {
            continue;
        }
        let r = cx.send(&s.id, Some(s.tip.clone()), Transformation::Delete, None, None).await?;
        report.patches.push(IngestPatch {
            symbol: s.id.clone(),
            edit: EditKind::Delete,
            result: r,
        });
    }

    // Then everything else in file order, each new or moved symbol placed right
    // after the last symbol before it that is live under its planned name.
    let last_existing = planned.iter().rposition(|p| p.from.is_some());
    let mut anchor: Option<SymbolId> = None;
    for (i, p) in planned.iter().enumerate() {
        let here = || anchor.clone().map_or(Placement::First, Placement::After);
        let Some(b) = p.from else {
            // Past the last existing symbol: append (see `ingest_tree`) — unless
            // it is a recreate, which would go back to its old place.
            let append = last_existing.is_none_or(|l| i > l) && !base.known.contains(&p.id);
            let position = if append { None } else { Some(here()) };
            let change = Transformation::Create(content(engine, &p.content).await?);
            let r = cx.send(&p.id, None, change, Some(p), position).await?;
            // A conflicted create is the same symbol created by somebody else:
            // live under this name all the same.
            if r.is_ok() {
                anchor = Some(p.id.clone());
            }
            report.patches.push(IngestPatch {
                symbol: p.id.clone(),
                edit: EditKind::Create,
                result: r,
            });
            continue;
        };
        let s = &stored[b];
        let mut parent = Some(s.tip.clone());
        let mut live_as = s.id.clone();
        if p.renamed {
            let change = Transformation::Rename(p.id.name.clone());
            let r = cx.send(&s.id, parent.clone(), change, None, None).await?;
            parent = landed(&r);
            if parent.is_some() {
                live_as = p.id.clone();
            }
            report.patches.push(IngestPatch {
                symbol: s.id.clone(),
                edit: EditKind::Rename,
                result: r,
            });
        }
        if parent.is_some() && s.content != p.content {
            let change = Transformation::Replace(content(engine, &p.content).await?);
            let r = cx.send(&live_as, parent.clone(), change, Some(p), None).await?;
            parent = landed(&r);
            report.patches.push(IngestPatch {
                symbol: live_as.clone(),
                edit: EditKind::Replace,
                result: r,
            });
        }
        if parent.is_some() && p.place == Place::Moved {
            let change = Transformation::Move(here());
            let r = cx.send(&live_as, parent.clone(), change, None, None).await?;
            report.patches.push(IngestPatch {
                symbol: live_as.clone(),
                edit: EditKind::Move,
                result: r,
            });
        }
        if !p.renamed && s.content == p.content && p.place == Place::Kept {
            report.unchanged.push(p.id.clone());
        }
        anchor = Some(live_as);
    }
    Ok(report)
}

/// UTF-8 inline; anything else stored as a blob first.
async fn content<B, P, G, L>(engine: &Engine<B, P, G, L>, bytes: &[u8]) -> Result<Content>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    Ok(match std::str::from_utf8(bytes) {
        Ok(s) => Content::Inline(s.to_string()),
        Err(_) => Content::Blob(engine.blobs().put(bytes.to_vec()).await?),
    })
}

/// What every patch of one file's ingest shares.
struct Submit<'a, B, P, G, L> {
    engine: &'a Engine<B, P, G, L>,
    ws: &'a str,
    agent: &'a Agent,
    path: &'a str,
    read_at: OpId,
}

impl<B, P, G, L> Submit<'_, B, P, G, L>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    /// One patch. A refusal is the patch's result; a storage outage is the
    /// ingest's error.
    async fn send(
        &self,
        symbol: &SymbolId,
        parent: Option<Hash>,
        change: Transformation,
        meta: Option<&Planned>,
        position: Option<Placement>,
    ) -> Result<std::result::Result<CommitResult, VcsError>> {
        let req = PatchRequest {
            workspace: self.ws.to_string(),
            symbol: symbol.clone(),
            parent,
            change,
            agent: self.agent.clone(),
            message: Some(format!("ingest {}", self.path)),
            depends_on: meta.map(|m| m.depends_on.clone()).unwrap_or_default(),
            implements: meta.map(|m| m.implements.clone()).unwrap_or_default(),
            wit_binding: meta.and_then(|m| m.wit_binding.clone()),
            read_at: Some(self.read_at),
            position,
        };
        match self.engine.apply_patch(req).await {
            Ok(r) => Ok(Ok(r)),
            Err(e @ VcsError::Storage(_)) => Err(e),
            Err(e) => Ok(Err(e)),
        }
    }
}

/// Host-side helper: every file under `root` that `keep` accepts, as
/// (`/`-separated relative path, bytes), sorted by path. Not for components,
/// which have no filesystem (ADR-0023).
pub fn read_tree(
    root: &std::path::Path,
    keep: impl Fn(&str) -> bool,
) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        keep: &dyn Fn(&str) -> bool,
        out: &mut Vec<(String, Vec<u8>)>,
    ) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let p = entry.path();
            if entry.file_type()?.is_dir() {
                walk(root, &p, keep, out)?;
            } else {
                let rel = p.strip_prefix(root).expect("under root");
                let rel: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                let rel = rel.join("/");
                if keep(&rel) {
                    out.push((rel, std::fs::read(&p)?));
                }
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &keep, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(path: &str, text: &str) -> Vec<ExtractedSymbol> {
        let s = extract_str("c", path, text);
        let joined: String = s.iter().map(|s| s.content.as_str()).collect();
        assert_eq!(joined, text, "lossless for {path}");
        for w in s.windows(2) {
            assert!(w[0].content.ends_with('\n'), "{:?} must end in a newline", w[0].id);
        }
        s
    }

    fn names(s: &[ExtractedSymbol]) -> Vec<(&str, SymbolKind)> {
        s.iter().map(|s| (s.id.name.as_str(), s.id.kind)).collect()
    }

    const SAMPLE: &str = "//! Orders.\n#![allow(dead_code)]\n\nuse std::fmt;\nuse std::io;\n\n/// An order.\n#[derive(Debug)]\npub struct Order { n: u32 }\n\nimpl Order {\n    fn new() -> Self { Order { n: helper() } }\n}\n\nimpl fmt::Display for Order {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, \"{}\", self.n) }\n}\n\nfn helper() -> u32 { 1 } // trailing\n\nconst MAX: u32 = 3;\nstatic S: u32 = 4;\ntype T = Order;\nmacro_rules! m { () => {} }\nm!();\nmod decl;\n";

    #[test]
    fn splits_items_and_keeps_every_byte() {
        let s = cut("src/lib.rs", SAMPLE);
        assert_eq!(
            names(&s),
            vec![
                (HEADER, SymbolKind::Module),
                ("Order", SymbolKind::StructItem),
                ("impl Order", SymbolKind::ImplBlock),
                ("impl fmt::Display for Order", SymbolKind::ImplBlock),
                ("helper", SymbolKind::Function),
                ("MAX", SymbolKind::Constant),
                ("S", SymbolKind::StaticItem),
                ("T", SymbolKind::TypeAlias),
                ("m", SymbolKind::MacroItem),
                ("m!", SymbolKind::MacroItem),
                ("decl", SymbolKind::Module),
            ]
        );
        assert!(s[0].content.ends_with("use std::io;\n"));
        assert!(s[1].content.starts_with("\n/// An order.\n#[derive(Debug)]\n"));
        assert!(s[4].content.ends_with("// trailing\n"));
        // `impl Order` uses `helper` and `Order`; the Display impl uses `Order`.
        let dep_names =
            |i: usize| s[i].depends_on.iter().map(|d| d.name.clone()).collect::<Vec<_>>();
        assert_eq!(dep_names(2), vec!["Order".to_string(), "helper".to_string()]);
        assert_eq!(dep_names(7), vec!["Order".to_string()]);
        assert_eq!(dep_names(9), vec!["m".to_string()]);
    }

    #[test]
    fn crlf_bom_no_trailing_newline() {
        let crlf = SAMPLE.replace('\n', "\r\n");
        let s = cut("a.rs", &crlf);
        assert_eq!(s.len(), 11);
        let bom = format!("\u{feff}{SAMPLE}");
        let s = cut("a.rs", &bom);
        assert_eq!(s[0].id.name, HEADER);
        assert!(s[0].content.starts_with('\u{feff}'));
        let s = cut("a.rs", "fn a() {}\n\nfn b() {}");
        assert_eq!(names(&s), vec![("a", SymbolKind::Function), ("b", SymbolKind::Function)]);
        assert_eq!(s[1].content, "\nfn b() {}");
        // Only a BOM before the first item, no header: the BOM leads the item.
        let s = cut("a.rs", "\u{feff}fn a() {}\n");
        assert_eq!(names(&s), vec![("a", SymbolKind::Function)]);
    }

    #[test]
    fn shebang_is_header() {
        let s = cut("main.rs", "#!/usr/bin/env run-cargo-script\nfn main() {}\n");
        assert_eq!(names(&s), vec![(HEADER, SymbolKind::Module), ("main", SymbolKind::Function)]);
    }

    #[test]
    fn comments_only_empty_and_unparsable_are_one_file() {
        for text in ["// just a comment\n/* and a block */\n", "", "\n\n", "fn broken( {\n"] {
            let s = cut("x.rs", text);
            assert_eq!(names(&s), vec![("x.rs", SymbolKind::File)], "{text:?}");
        }
        let s = cut("README.md", "# hi\n");
        assert_eq!(names(&s), vec![("README.md", SymbolKind::File)]);
    }

    #[test]
    fn inner_docs_only_is_a_header() {
        let s = cut("x.rs", "//! Docs.\n//! More.\n");
        assert_eq!(names(&s), vec![(HEADER, SymbolKind::Module)]);
    }

    #[test]
    fn tail_comments_and_whitespace() {
        let s = cut("x.rs", "fn a() {}\n\n\n");
        assert_eq!(s.len(), 1);
        let s = cut("x.rs", "fn a() {}\n\n// the end\n");
        assert_eq!(names(&s), vec![("a", SymbolKind::Function), (TAIL, SymbolKind::Module)]);
    }

    #[test]
    fn same_line_items_merge() {
        let s = cut("x.rs", "fn a() {} fn b() {}\nfn c() {}\n");
        assert_eq!(names(&s), vec![("a", SymbolKind::Function), ("c", SymbolKind::Function)]);
        let s = cut("x.rs", "fn a() {} /* multi\nline */ fn b() {}\nfn c() {}\n");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn nested_modules_recurse() {
        let text = "fn top() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn one() { top(); }\n\n    mod deep {\n        fn two() {}\n    }\n    // trailing in tests\n}\n";
        let s = cut("x.rs", text);
        assert_eq!(
            names(&s),
            vec![
                ("top", SymbolKind::Function),
                ("tests", SymbolKind::Module),
                ("tests::one", SymbolKind::Function),
                ("tests::deep", SymbolKind::Module),
                ("tests::deep::two", SymbolKind::Function),
                ("tests::deep::(end)", SymbolKind::Module),
                ("tests::(end)", SymbolKind::Module),
            ]
        );
        assert!(s[1].content.ends_with("use super::*;\n"));
        assert_eq!(s[6].content, "    // trailing in tests\n}\n");
        assert_eq!(s[2].depends_on[0].name, "top");
        // Braces sharing a line with the items: kept whole.
        let s = cut("x.rs", "mod m { fn a() {} }\nmod n {\n    fn b() {} }\n");
        assert_eq!(names(&s), vec![("m", SymbolKind::Module), ("n", SymbolKind::Module)]);
    }

    #[test]
    fn duplicates_get_suffixes() {
        let s = cut(
            "x.rs",
            "#[cfg(unix)]\nfn f() {}\n#[cfg(not(unix))]\nfn f() {}\nimpl A {}\nimpl A {}\n",
        );
        assert_eq!(
            names(&s),
            vec![
                ("f", SymbolKind::Function),
                ("f~2", SymbolKind::Function),
                ("impl A", SymbolKind::ImplBlock),
                ("impl A~2", SymbolKind::ImplBlock),
            ]
        );
    }

    #[test]
    fn later_uses_group_and_bindings() {
        let text = "mod a;\nmod b;\npub use a::X;\npub use b::Y;\n\ntrait Guest {}\nstruct Component;\nimpl Guest for Component {}\nimpl exports::holon::orders::order_api::Guest for Component {}\n";
        let s = cut("lib.rs", text);
        assert_eq!(s[2].id.name, USES);
        assert!(s[2].content.contains("a::X") && s[2].content.contains("b::Y"));
        let g = s.iter().find(|s| s.id.name == "impl Guest for Component").unwrap();
        assert_eq!(g.wit_binding.as_deref(), Some("Guest"));
        assert_eq!(g.implements[0].name, "Guest");
        let e = s.iter().find(|s| s.id.name.starts_with("impl exports")).unwrap();
        assert_eq!(e.wit_binding.as_deref(), Some("holon:orders/order-api"));
    }

    #[test]
    fn unicode_offsets() {
        let s = cut("x.rs", "/// Größe — ünïcödé\nfn a() { let _ = \"ß\"; }\nfn b() {}\n");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn wit_split() {
        let text = "// header comment\npackage holon:orders@0.1.0;\n\n/// Types.\ninterface types {\n    /// A { brace in a comment\n    record order { id: string }\n}\n\n@since(version = 0.2.0)\ninterface api {\n    use types.{order};\n    get: func() -> order;\n}\n\nworld orders {\n    export api;\n} // done\n/* trailing */\n";
        let s = cut("wit/orders.wit", text);
        assert_eq!(
            names(&s),
            vec![
                (HEADER, SymbolKind::Module),
                ("types", SymbolKind::WitInterface),
                ("api", SymbolKind::WitInterface),
                ("orders", SymbolKind::WitWorld),
                (TAIL, SymbolKind::Module),
            ]
        );
        assert_eq!(s[2].wit_binding.as_deref(), Some("holon:orders/api"));
        assert!(s[2].content.starts_with("\n@since"));
        assert_eq!(s[2].depends_on[0].name, "types");
        assert_eq!(s[3].depends_on[0].name, "api");
        // Unbalanced: whole file.
        let s = cut("x.wit", "interface a {\n");
        assert_eq!(names(&s), vec![("x.wit", SymbolKind::File)]);
    }

    #[test]
    fn plan_places_a_middle_insert_moves_and_renames() {
        let stored: Vec<Stored> = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(i, n)| Stored {
                key: format!("k-{n}"),
                id: SymbolId::new("c", "x.rs", n, SymbolKind::Function),
                tip: "0".repeat(64),
                content: format!("fn {n}() {{}}\n").into_bytes(),
                order: crate::order::derived(i as OpId + 1),
            })
            .collect();
        let places = |text: &str| -> Vec<(String, Place, bool)> {
            plan(extract_str("c", "x.rs", text), &stored)
                .into_iter()
                .map(|p| (p.id.name, p.place, p.renamed))
                .collect()
        };
        let own = |n: &str, p: Place| (n.to_string(), p, false);
        // x inserted between a and b, z appended: both their own symbols.
        assert_eq!(
            places("fn a() {}\nfn x() {}\nfn b() {}\nfn c() {}\nfn z() {}\n"),
            vec![
                own("a", Place::Kept),
                own("x", Place::New),
                own("b", Place::Kept),
                own("c", Place::Kept),
                own("z", Place::New),
            ]
        );
        // c moved to the top: one move, the rest stay.
        assert_eq!(
            places("fn c() {}\nfn a() {}\nfn b() {}\n"),
            vec![own("c", Place::Moved), own("a", Place::Kept), own("b", Place::Kept)]
        );
        // b renamed to bb in place.
        let p = plan(extract_str("c", "x.rs", "fn a() {}\nfn bb() {}\nfn c() {}\n"), &stored);
        assert_eq!((p[1].from, p[1].renamed, p[1].place), (Some(1), true, Place::Kept));
    }
}
