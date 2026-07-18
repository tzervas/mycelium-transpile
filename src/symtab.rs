//! The gap-close-2 **cross-nodule symbol table** (the `Import` gap-class lever — Phase-0 re-measure,
//! `docs/notes/DN-34-Rust-to-Mycelium-Transpiler-Strategy.md` §8.19/§8.20): a **batch-scoped** map
//! from a Rust crate-root-relative module path to the Mycelium nodule this transpiler derived for
//! it, plus the set of item names that sibling's own pass actually **emitted** — never a name that
//! merely exists in the Rust source but itself gapped. See `batch.rs::transpile_batch`'s two-pass
//! driver for how this is built and installed, and `transpile::dispatch_use` for the consumer.
//!
//! **M-1084 (Import net-close): `self::`/`super::` relative resolution + cross-PHYLUM resolution.**
//! The original gap-close-2 lever resolved only `crate::<mod>...` (crate-root-absolute) and the
//! bare crate-root form (`use <mod>::Item;` — real Rust "uniform paths": an unqualified `use` head
//! is *always* tried crate-root-relative first, in every file, not just the crate-root one). This
//! increment closes the two residuals that lever's own doc named as scoped out:
//! 1. **`self::`/`super::`** now resolve relative to the CURRENT file's own module path
//!    ([`use_candidates`]'s `current_module` parameter) — [`HeadKind::SameCrate`], always looked up
//!    in the current file's own crate, never tried as an extern-crate name. Multi-level `super::`
//!    chains (`super::super::X`, two directories up) are peeled one level at a time. A `super::`
//!    chain that would walk past the crate root is a genuine structural miss — real Rust itself
//!    rejects this — never a guess (VR-5/G2).
//! 2. **Cross-phylum**: a *bare* head (neither `crate`/`self`/`super`) is ambiguous in real Rust
//!    between "this crate's own crate-root submodule" and "an extern crate's own name" —
//!    [`HeadKind::Bare`]. **The real rule is root-file-only lexical shadowing, NOT "same-crate
//!    first everywhere"**: a bare `use foo::X;` resolves against a local item literally named
//!    `foo` in the CURRENT FILE's own lexical scope before falling back to the extern prelude, and
//!    a crate-root `mod foo;` is only ever a name in the CRATE-ROOT file's own scope — a non-root
//!    file never implicitly sees the crate root's sibling `mod` declarations. So
//!    [`SymbolTable::candidate_lookup_keys`] tries the same-crate key FIRST **only when
//!    `current_module` is empty** (this file itself is the crate root — matching real Rust's own
//!    crate-root shadowing); for every OTHER file, the same-crate key is never tried at all —
//!    only the head read literally as the named sibling PHYLUM's own extern-crate identifier
//!    (`transpile::derive_crate_ident`, the standard Cargo package-name -> crate-identifier
//!    mapping) is. A hit requires the exact crate identifier AND the exact emitted name to both be
//!    real entries in this batch's table (never a guess — VR-5); this only ever fires when the
//!    referenced phylum's own files are *in the same batch* (e.g. a multi-crate `--files`
//!    invocation) — a phylum transpiled in a wholly separate run is, honestly, still an
//!    out-of-batch miss (G2). This mirrors `crates/mycelium-l1/src/checkty.rs`'s DN-113
//!    `merge_phyla_exports`: "one added qualifier dimension" lets the SAME resolver handle both the
//!    intra-crate and cross-phylum case, no second resolver (DRY; DN-113 §9.6).
//!
//! Still scoped OUT (unchanged): a rename (the alias would need threading through every downstream
//! reference in the file body — out of this transpiler's rewrite surface) and a glob (no
//! disambiguation strategy yet — mirrors DN-113 v1's own deferral to M-982). Every miss — an
//! out-of-batch head, an in-batch sibling that itself gapped the requested name, a `self`-module-
//! binding leaf, a rename, a glob, or a `super::` with no parent to go up to — is still recorded as
//! a [`crate::gap::Category::Import`] [`crate::gap::GapReason`] naming exactly what could not
//! resolve and why (never silently dropped).
//!
//! **No bare-name collapse (the M-1060 lesson):** a resolved leaf is always emitted as a
//! nodule-qualified `use` (`use <full_nodule_path>.<Item>;`), never a bare `<Item>` — the identical
//! discipline DN-113/M-1060 use for cross-nodule visibility.
//!
//! **M-1084 net-close / the inverted-strip root cause (Empirical, 2026-07-16).** The kernel's
//! `resolve_imports` keys exports as **full** `nodule.path` + `.` + item (e.g. `l1.checkty.Width`,
//! `std.fs.error.FsErr` — see `mycelium-l1::checkty::qualify`). A live `myc check --phylum`
//! differential witnesses:
//! - `use l1.checkty.Width` / `use std.fs.error.FsErr` → **Clean**
//! - `use checkty.Width` / `use error.FsErr` → **CheckError** (`no such name … in the phylum`)
//!
//! PR #1635's same-crate crate-root **strip** (`use_emit_qualifier` → short suffix) inverted that
//! basis: imports *resolved* in the symtab but were emitted under a path the checker refuses — a
//! classification win paired with a phylum-clean regression, not net progress (the original
//! 59→57 shape, recreated by the wrong emit form). Net-close is identity emit of the resolved
//! sibling's full [`NoduleSymbols::nodule_path`] for **both** same-crate and cross-crate batch
//! hits — never a guessed rename, never a bare name, never a silent short form (VR-5/G2).
//!
//! **ONESHOT L2-B phase-2 (Empirical, post-B1/#1659):** Path **form** is closed (full nodule
//! path; never short-form collapse). The residual is **oracle visibility**: a correctly resolved
//! `use std.fs.error.FsErr` is phylum-Clean when siblings co-exist, but single-file oracle
//! (phylum-of-one) refuses `no such name … in the phylum`.
//!
//! **Phase-2 lever (type, ONESHOT L2-B):** when a resolved leaf is a **type** the sibling
//! baseline actually emitted (`pub type` / `type` line captured in [`NoduleSymbols::type_defs`]),
//! emit a **Declared co-include** of that type (plus transitive type-deps found in the same
//! batch's type surface) into the consumer instead of a phylum-of-one-refusing `use`. EXPLAIN
//! names the home nodule path (M-1084 full path preserved as provenance — never silent, never
//! short-form collapse). Dual-define under phylum is home-qualified per nodule (consumer local
//! vs sibling home) — Empirical clean on std-fs pilot; not a language import identity claim.
//!
//! **G-α Rank-2 / L2-B Import non-type:** when a resolved leaf is a **free-fn** the sibling
//! baseline actually emitted as a single-line `fn`/`pub fn` ([`NoduleSymbols::fn_defs`] via
//! [`extract_fn_defs`]), co-include that fn surface the same way (Declared + EXPLAIN home path),
//! and pull any batch-sibling **type** names referenced on the fn line into the type co-include
//! set first. Multi-line / non-fn residual (const, mangled multi-line body, etc.) still falls
//! back to full-path `use` with the DN-124 EXPLAIN (oracle may false-fail — dual-report); never a
//! silent skip (G2/VR-5).

use std::collections::{HashMap, HashSet};
use syn::UseTree;

/// The kind of one flattened `use`-tree leaf (see [`use_candidates`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CandidateKind {
    /// A plain imported name (`Item` in `use crate::mod::Item;`, or one member of a
    /// `use crate::mod::{A, B};` group) — the only kind [`SymbolTable::resolve`] can ever resolve.
    Name(String),
    /// `self` inside a group (`use crate::mod::{self, Item}`) — binds the **module itself** as a
    /// local name, not an item. There is no "import a nodule as a name" construct in this grammar,
    /// so this leaf is unresolvable by construction (distinct from a plain lookup miss).
    SelfModule,
    /// `use crate::mod::Item as Alias;` — a rename. Scoped OUT of this increment: emitting a
    /// renamed cross-nodule reference would need the alias threaded through every downstream
    /// reference to `Alias` in this file's body, which this transpiler does not do — flagged, not
    /// guessed (VR-5/G2).
    Rename { from: String, to: String },
    /// `use crate::mod::*;` — a cross-nodule glob. Scoped OUT, mirroring DN-113 v1's own deferral of
    /// a cross-phylum glob to M-982 rather than guessing a disambiguation.
    Glob,
}

/// Which resolution namespace a candidate leaf's HEAD names (M-1084) — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadKind {
    /// `crate::`/`self::`/`super::` — unambiguously relative to the CURRENT file's own crate; never
    /// tried as an extern-crate (cross-phylum) name.
    SameCrate,
    /// A literal head that is neither `crate`/`self`/`super` — ambiguous in real Rust between "this
    /// crate's own crate-root submodule" and "an extern crate's own name". Real Rust resolves this
    /// by ROOT-FILE-ONLY LEXICAL SHADOWING: a crate-root `mod foo;` is a name only in the
    /// crate-root file's own scope, so the same-crate interpretation is tried FIRST only when the
    /// current file itself IS the crate root (`current_module` empty); every other file's bare
    /// heads resolve via the extern prelude — the cross-phylum interpretation — EXCLUSIVELY (see
    /// [`SymbolTable::candidate_lookup_keys`]).
    Bare,
}

/// One flattened leaf of a (possibly grouped/nested) `use` tree, with the crate-root-relative Rust
/// module path it was found under (e.g. `["checkty"]`, `["foo", "bar"]`) and the [`HeadKind`] that
/// determines how [`SymbolTable::candidate_lookup_keys`] resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UseCandidate {
    pub module_segs: Vec<String>,
    pub kind: CandidateKind,
    pub head_kind: HeadKind,
}

/// Extract every candidate leaf from a `use` item's tree, given the CURRENT file's own
/// crate-root-relative module path (`current_module` — e.g. `[]` for a crate-root `lib.rs`/`mod.rs`,
/// `["foo", "bar"]` for `foo/bar.rs`; see `transpile::derive_module_segments`). Returns `None` when
/// the tree has no module-path segment at all (a bare `use Item;` naming nothing to resolve
/// against), or when the head is `super` and `current_module` is already empty (no parent to go up
/// to at the crate root — a genuine structural miss, not attempted; VR-5/G2).
///
/// - A `crate::...` head is peeled — the rest becomes the crate-root-relative module path
///   ([`HeadKind::SameCrate`]).
/// - A `self::...` head resolves relative to `current_module` itself ([`HeadKind::SameCrate`]; M-1084).
/// - A `super::...` head resolves relative to `current_module`'s PARENT ([`HeadKind::SameCrate`];
///   M-1084).
/// - Any other head is tried **as-is** as a crate-root-relative candidate too (the bare-sibling-
///   module form, e.g. `use error::FsErr;` — real Rust "uniform paths": an unqualified `use` head is
///   *always* crate-root-relative-or-extern-prelude, in every file, not just the crate-root one) —
///   [`HeadKind::Bare`]. This never mis-fires on a genuine external crate (`use serde::Serialize;`):
///   [`SymbolTable::resolve`] only ever returns a hit when the head concretely matches a batch
///   sibling's own module key (same-crate) or a batch sibling PHYLUM's own extern-crate identifier
///   (cross-phylum), so an unrelated crate sharing neither is simply never a key in the table (a
///   miss, not a guess — VR-5).
pub(crate) fn use_candidates(
    tree: &UseTree,
    current_module: &[String],
) -> Option<Vec<UseCandidate>> {
    let UseTree::Path(p) = tree else {
        return None;
    };
    let head = p.ident.to_string();
    let (mut prefix, head_kind, mut rest): (Vec<String>, HeadKind, &UseTree) = match head.as_str() {
        "crate" => (Vec::new(), HeadKind::SameCrate, &p.tree),
        "self" => (current_module.to_vec(), HeadKind::SameCrate, &p.tree),
        "super" => {
            if current_module.is_empty() {
                return None;
            }
            (
                current_module[..current_module.len() - 1].to_vec(),
                HeadKind::SameCrate,
                &p.tree,
            )
        }
        _ => (vec![head], HeadKind::Bare, &p.tree),
    };
    // Peel any further CONSECUTIVE leading `super::` segments (multi-level, e.g.
    // `super::super::X` — two directories up) — each one more level up from the current parent. A
    // chain that would walk past the crate root is a genuine structural miss, never a guess. Only
    // meaningful after a `crate`/`self`/`super` head (`HeadKind::SameCrate`) — a `Bare` head never
    // enters this loop, so a literal `foo::super::bar` (nonsensical Rust, never real code) is left
    // untouched rather than mis-peeled.
    if head_kind == HeadKind::SameCrate {
        while let UseTree::Path(next) = rest {
            if next.ident != "super" {
                break;
            }
            if prefix.is_empty() {
                return None;
            }
            prefix.pop();
            rest = &next.tree;
        }
    }
    let mut out = Vec::new();
    flatten(rest, &mut prefix, head_kind, &mut out);
    Some(out)
}

fn flatten(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    head_kind: HeadKind,
    out: &mut Vec<UseCandidate>,
) {
    match tree {
        UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            flatten(&p.tree, prefix, head_kind, out);
            prefix.pop();
        }
        UseTree::Name(n) => {
            let kind = if n.ident == "self" {
                CandidateKind::SelfModule
            } else {
                CandidateKind::Name(n.ident.to_string())
            };
            out.push(UseCandidate {
                module_segs: prefix.clone(),
                kind,
                head_kind,
            });
        }
        UseTree::Rename(r) => out.push(UseCandidate {
            module_segs: prefix.clone(),
            kind: CandidateKind::Rename {
                from: r.ident.to_string(),
                to: r.rename.to_string(),
            },
            head_kind,
        }),
        UseTree::Glob(_) => out.push(UseCandidate {
            module_segs: prefix.clone(),
            kind: CandidateKind::Glob,
            head_kind,
        }),
        UseTree::Group(g) => {
            for t in &g.items {
                flatten(t, prefix, head_kind, out);
            }
        }
    }
}

/// One sibling nodule's cross-nodule-visible surface, as seen by this batch's own baseline pass.
#[derive(Debug, Clone)]
pub(crate) struct NoduleSymbols {
    /// The Mycelium nodule path this file transpiles to (`transpile::derive_nodule_path`'s output)
    /// — the qualifier a resolved `use` is emitted against (`use <nodule_path>.<Item>;`), and the
    /// provenance string on an L2-B type co-include EXPLAIN comment.
    pub nodule_path: String,
    /// Every top-level item name this batch's baseline pass actually **emitted** for this file
    /// (`GapReport::emitted_items`) — the only names a cross-nodule `use` may ever resolve to.
    pub emitted: HashSet<String>,
    /// L2-B: bare type name → full single-line `type`/`pub type` emission from the sibling's
    /// baseline `.myc` ([`extract_type_defs`]). Empty when the sibling emitted no type items.
    /// Only names also present in [`Self::emitted`] are stored (never a gapped/unemitted type).
    pub type_defs: HashMap<String, String>,
    /// G-α L2-B non-type: bare free-fn name → full single-line `fn`/`pub fn` emission from the
    /// sibling's baseline `.myc` ([`extract_fn_defs`]). Empty when the sibling emitted no free-fns
    /// (or only multi-line forms). Only names also present in [`Self::emitted`] are stored.
    pub fn_defs: HashMap<String, String>,
}

/// Extract single-line `type Name = …;` / `pub type Name = …;` definitions from baseline `.myc`
/// text (L2-B co-include surface). Multi-line type bodies are **not** captured (Declared residual —
/// today's emitter writes sum/product types on one line; a multi-line form would fall back to
/// full-path `use` and remain oracle-false-fail until the extractor widens — G2, never guessed).
pub(crate) fn extract_type_defs(myc: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in myc.lines() {
        let trimmed = line.trim();
        let rest = if let Some(r) = trimmed.strip_prefix("pub type ") {
            r
        } else if let Some(r) = trimmed.strip_prefix("type ") {
            r
        } else {
            continue;
        };
        // `Name = …;` — name is the first whitespace-delimited token before `=`.
        let Some(eq) = rest.find('=') else {
            continue;
        };
        let name = rest[..eq].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        // Prefer the first definition if a name repeats (should not happen in one nodule).
        out.entry(name.to_string())
            .or_insert_with(|| trimmed.to_string());
    }
    out
}

/// Extract single-line free-fn definitions from baseline `.myc` (G-α L2-B non-type co-include
/// surface). Matches `fn name(…) => … = …;` / `pub fn name(…) => … = …;` (and the type-param form
/// `fn name[T](…) => …`). Multi-line fn bodies are **not** captured (Declared residual — today's
/// emitter writes free-fns on one line after any leading `//` doc lines; a multi-line form falls
/// back to full-path `use` — G2, never guessed). Does **not** capture trait/`impl` method
/// signatures that lack a body `=`.
pub(crate) fn extract_fn_defs(myc: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in myc.lines() {
        let trimmed = line.trim();
        // Skip pure comments / empty.
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let rest = if let Some(r) = trimmed.strip_prefix("pub fn ") {
            r
        } else if let Some(r) = trimmed.strip_prefix("fn ") {
            r
        } else {
            continue;
        };
        // Require a complete single-line def: has `=>` and ends with `;`.
        if !rest.contains("=>") || !trimmed.ends_with(';') {
            continue;
        }
        // Name is the identifier before `(` or `[` (type params).
        let name_end = rest.find(['(', '[']).unwrap_or(rest.len());
        let name = rest[..name_end].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        // Prefer first def if a name repeats (should not happen in one nodule).
        out.entry(name.to_string())
            .or_insert_with(|| trimmed.to_string());
    }
    out
}

/// The batch-wide cross-nodule symbol table: a lookup key -> that sibling's [`NoduleSymbols`]. The
/// key is EITHER a bare, dot-joined Rust crate-root-relative module path (`"checkty"`, `"foo.bar"`
/// — when the inserting file has no derivable crate identity, e.g. a `src`-ancestor-less test
/// fixture: byte-identical to pre-M-1084 behavior) OR that same module path qualified under the
/// file's own crate identity via [`Self::qualify_key`] (`"mycelium_std_rand"`,
/// `"mycelium_std_rand.rng"` — M-1084's cross-phylum extension). Built once per batch (see
/// `batch.rs::build_symbol_table`) from every file's baseline-pass [`crate::gap::GapReport`]; never
/// hand-merged from two sources with a colliding key (each real file has exactly one derived key).
#[derive(Debug, Clone, Default)]
pub(crate) struct SymbolTable {
    modules: HashMap<String, NoduleSymbols>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert one file's cross-nodule-visible surface under `module_key`. The struct doc asserts
    /// `module_key` uniqueness (each real batch file derives exactly one key — `batch.rs`'s
    /// `discover_rs_files` walks a real, deduplicated filesystem tree, so two *distinct* files
    /// legitimately collide here only if their derived crate-identity + module path happen to
    /// coincide, e.g. two same-named crate directories reachable from one batch root) — that basis
    /// is `Declared`, not `Proven` (no static guarantee two distinct discovered paths can never
    /// derive the same key), so a silent last-write-wins `HashMap::insert` would violate G2 if it
    /// were ever actually violated. `debug_assert!` catches a real collision in dev/test builds
    /// (never-silent, VR-5) without paying a release-build cost for a check whose triggering case
    /// is currently unobserved in this crate's own test corpus.
    pub fn insert(
        &mut self,
        module_key: String,
        nodule_path: String,
        emitted: HashSet<String>,
        type_defs: HashMap<String, String>,
        fn_defs: HashMap<String, String>,
    ) {
        debug_assert!(
            !self.modules.contains_key(&module_key),
            "SymbolTable::insert: module_key {module_key:?} already present (nodule_path \
             {nodule_path:?}) -- two distinct batch files derived the SAME lookup key, so this \
             insert would silently overwrite the first file's emitted-surface entry. This \
             violates the struct doc's uniqueness invariant; investigate the colliding files' \
             derived crate-identity + module path rather than silently proceeding (G2)."
        );
        // Only retain defs for names that actually emitted (never a gapped/unemitted item).
        let type_defs: HashMap<String, String> = type_defs
            .into_iter()
            .filter(|(n, _)| emitted.contains(n))
            .collect();
        let fn_defs: HashMap<String, String> = fn_defs
            .into_iter()
            .filter(|(n, _)| emitted.contains(n))
            .collect();
        self.modules.insert(
            module_key,
            NoduleSymbols {
                nodule_path,
                emitted,
                type_defs,
                fn_defs,
            },
        );
    }

    /// Resolve `name` in `module_key` (dot-joined Rust module-path segments, bare OR
    /// crate-qualified — see struct docs). Returns the **home-qualified** Mycelium nodule path to
    /// emit against — never the bare `module_key`, never a guessed rename (VR-5; mirrors DN-113
    /// `qualify_cross_phylum`'s never-bare discipline).
    pub fn resolve(&self, module_key: &str, name: &str) -> Option<&str> {
        self.modules
            .get(module_key)
            .filter(|m| m.emitted.contains(name))
            .map(|m| m.nodule_path.as_str())
    }

    /// L2-B: the baseline `type`/`pub type` line for `name` in `module_key`, when that sibling
    /// emitted a type of that name. `None` when the module misses, the name was not emitted, or
    /// the emission was not a single-line type def (fn / other surface — falls back to free-fn
    /// co-include or full-path `use`).
    pub fn type_def(&self, module_key: &str, name: &str) -> Option<&str> {
        self.modules
            .get(module_key)
            .and_then(|m| m.type_defs.get(name))
            .map(String::as_str)
    }

    /// G-α L2-B non-type: the baseline single-line `fn`/`pub fn` line for `name` in `module_key`,
    /// when that sibling emitted a free-fn of that name. `None` when missing / not emitted / not a
    /// single-line free-fn (falls back to full-path `use` with DN-124 EXPLAIN).
    pub fn fn_def(&self, module_key: &str, name: &str) -> Option<&str> {
        self.modules
            .get(module_key)
            .and_then(|m| m.fn_defs.get(name))
            .map(String::as_str)
    }

    /// G-α L2-B non-type: for seed `(module_key, name)` free-fn pairs, return
    /// `(home_nodule_path, def_line)` in seed order (stable, never bare-name first-wins across
    /// modules). Seeds without a fn_def are omitted (caller gaps them honestly).
    pub fn fn_def_lines(&self, seeds: &[(String, String)]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (module_key, name) in seeds {
            let Some(m) = self.modules.get(module_key.as_str()) else {
                continue;
            };
            let Some(def) = m.fn_defs.get(name) else {
                continue;
            };
            out.push((m.nodule_path.clone(), def.clone()));
        }
        out
    }

    /// L2-B: for seed `(module_key, name)` pairs that resolved in this batch, collect the
    /// co-include set — each seed's **module-keyed** type def (never a bare-name first-wins
    /// across crates — two crates can both emit `Foo`) plus transitive type deps preferred from
    /// the same home nodule, then any other batch module. Returns `(home_nodule_path, def_line)`
    /// in dependency-before-user order. Seeds without a type_def are omitted.
    pub fn type_def_closure(&self, seeds: &[(String, String)]) -> Vec<(String, String)> {
        if seeds.is_empty() {
            return Vec::new();
        }
        // name -> (home, def) for the chosen definition of that bare name.
        let mut chosen: HashMap<String, (String, String)> = HashMap::new();
        for (module_key, name) in seeds {
            let Some(m) = self.modules.get(module_key.as_str()) else {
                continue;
            };
            let Some(def) = m.type_defs.get(name) else {
                continue;
            };
            chosen.insert(name.clone(), (m.nodule_path.clone(), def.clone()));
        }
        if chosen.is_empty() {
            return Vec::new();
        }
        // Transitive deps: prefer same-home nodule, else any module that defines the name.
        let mut changed = true;
        while changed {
            changed = false;
            let snapshot: Vec<(String, String)> = chosen
                .iter()
                .map(|(n, (h, d))| (n.clone(), format!("{h}\0{d}")))
                .collect();
            for (n, home_and_def) in snapshot {
                let Some((home, def)) = home_and_def.split_once('\0') else {
                    continue;
                };
                let _ = n;
                for dep in type_names_referenced_in_def_line(def) {
                    if chosen.contains_key(&dep) {
                        continue;
                    }
                    // Prefer a type_def from a module with the same home nodule path as the parent.
                    let mut found: Option<(String, String)> = None;
                    for m in self.modules.values() {
                        if let Some(d) = m.type_defs.get(&dep) {
                            if m.nodule_path == home {
                                found = Some((m.nodule_path.clone(), d.clone()));
                                break;
                            }
                            if found.is_none() {
                                found = Some((m.nodule_path.clone(), d.clone()));
                            }
                        }
                    }
                    if let Some((h, d)) = found {
                        chosen.insert(dep, (h, d));
                        changed = true;
                    }
                }
            }
        }
        // Topological-ish order.
        let needed: HashSet<String> = chosen.keys().cloned().collect();
        let mut order: Vec<String> = Vec::new();
        let mut placed: HashSet<String> = HashSet::new();
        let mut guard = 0;
        while placed.len() < needed.len() && guard < needed.len() + 1 {
            guard += 1;
            let mut progress = false;
            let mut candidates: Vec<String> = needed.difference(&placed).cloned().collect();
            candidates.sort();
            for n in candidates {
                let Some((_, def)) = chosen.get(&n) else {
                    continue;
                };
                let deps_ready = type_names_referenced_in_def_line(def)
                    .into_iter()
                    .filter(|d| needed.contains(d))
                    .all(|d| placed.contains(&d));
                if deps_ready {
                    order.push(n.clone());
                    placed.insert(n);
                    progress = true;
                }
            }
            if !progress {
                let mut rest: Vec<String> = needed.difference(&placed).cloned().collect();
                rest.sort();
                order.extend(rest);
                break;
            }
        }
        order
            .into_iter()
            .filter_map(|n| chosen.get(&n).cloned())
            .collect()
    }

    /// Is `module_key` a batch sibling at all (regardless of whether a particular name resolves in
    /// it)? Used to word an honest, distinct reason for "this head isn't even a batch sibling" vs
    /// "it is a sibling, but it gapped this particular name".
    pub fn has_module(&self, module_key: &str) -> bool {
        self.modules.contains_key(module_key)
    }

    /// Every `(module_key, name)` pair this table can resolve, for a candidate leaf's module path
    /// segments joined with `.`. A thin convenience over [`Self::resolve`] used by
    /// `batch.rs`'s pub-needed scan.
    pub fn module_key(module_segs: &[String]) -> String {
        module_segs.join(".")
    }

    /// Qualify `module_key` under `crate_ident`'s own namespace — `crate_ident` alone when
    /// `module_key` is empty (the crate-root case), else `"{crate_ident}.{module_key}"` — never a
    /// bare, unqualified collapse across crates (M-1084: mirrors DN-113 `qualify_cross_phylum`'s own
    /// never-bare discipline, one dot-joined dimension instead of `::`, matching this table's own
    /// `module_key` convention).
    pub fn qualify_key(crate_ident: &str, module_key: &str) -> String {
        if module_key.is_empty() {
            crate_ident.to_string()
        } else {
            format!("{crate_ident}.{module_key}")
        }
    }

    /// The lookup key(s) to try, IN PRECEDENCE ORDER, for one candidate leaf — the single policy
    /// both `transpile::dispatch_use` (via `emit::cross_nodule_resolve`, one key at a time — the
    /// `EmitCtx` thread-local mediates `emit.rs` access) and `batch.rs::scan_pub_needed` (direct
    /// `&SymbolTable` access) consult, so the "which key(s), in what order" policy lives in exactly
    /// one place (DRY).
    ///
    /// `current_module` is the CALLING file's own crate-root-relative module segments (empty for a
    /// crate-root `lib.rs`/`mod.rs` — see `transpile::derive_module_segments`); it is what gates
    /// the [`HeadKind::Bare`] precedence below (real Rust's root-file-only lexical shadowing — see
    /// the module docs and [`HeadKind::Bare`]'s own doc).
    ///
    /// [`HeadKind::SameCrate`] (`crate::`/`self::`/`super::`) yields exactly one key, qualified
    /// under `current_crate` when derivable, else the bare `module_key` (no real crate context —
    /// e.g. a `src`-ancestor-less test fixture; byte-identical to pre-M-1084 behavior).
    /// [`HeadKind::Bare`]'s keys depend on WHERE the `use` is written: when `current_module` is
    /// empty (this file IS the crate root), it yields up to two, the same-crate interpretation
    /// FIRST (matching real Rust's crate-root shadowing), then the cross-phylum interpretation
    /// (the head segment itself read AS the named phylum's own extern-crate identifier); for any
    /// OTHER (non-root) file it yields exactly ONE key — the cross-phylum interpretation only — a
    /// non-root file's local scope never implicitly contains the crate root's sibling `mod`
    /// declarations, so trying the same-crate key there would silently mis-bind a genuine
    /// cross-phylum reference to an unrelated same-named submodule (the CRITICAL fix this doc
    /// records; see `src/tests/symtab.rs` + `src/tests/batch.rs`'s non-root regressions).
    pub fn candidate_lookup_keys(
        current_crate: Option<&str>,
        current_module: &[String],
        candidate: &UseCandidate,
    ) -> Vec<String> {
        let module_key = Self::module_key(&candidate.module_segs);
        let same_crate_key = match current_crate {
            Some(c) => Self::qualify_key(c, &module_key),
            None => module_key,
        };
        if candidate.head_kind != HeadKind::Bare {
            return vec![same_crate_key];
        }
        let mut keys = Vec::new();
        if current_module.is_empty() {
            keys.push(same_crate_key);
        }
        if let Some((head, rest)) = candidate.module_segs.split_first() {
            keys.push(Self::qualify_key(head, &Self::module_key(rest)));
        }
        keys
    }

    /// The dotted path prefix for an emitted `use <prefix>.<Item>;` line — see the module docs
    /// (M-1084 net-close). Always the resolved sibling's **full** [`NoduleSymbols::nodule_path`]:
    /// the kernel's export registry keys by that full path (Empirical vs PR #1635's inverted strip).
    ///
    /// `importer_crate` / `resolved_via_key` are retained so call sites stay stable and so a future
    /// cross-phylum `use dep::…` emit form (DN-113 `::` boundary) can branch without rewiring every
    /// consumer; today both same-crate and same-batch multi-crate hits share one combined phylum
    /// and therefore the same full-path form (never a silent short-form collapse — G2).
    pub fn use_emit_qualifier(
        _importer_crate: Option<&str>,
        resolved_nodule_path: &str,
        _resolved_via_key: &str,
    ) -> String {
        resolved_nodule_path.to_string()
    }
}

/// UpperCamel identifiers on a surface line (type def, free-fn sig/body). Crude token scan —
/// sufficient for the emitter's single-line forms; not a full Mycelium parser (Declared).
/// Callers filter against known type_defs in the table. `exclude` drops the defined type name
/// (or free-fn name when it happens to be UpperCamel).
pub(crate) fn upper_camel_names_in_line(line: &str, exclude: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, found: &mut Vec<String>| {
        if !cur.is_empty() {
            if cur != exclude
                && cur.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && !found.iter().any(|x| x == cur)
            {
                // Heuristic: type names are UpperCamel (ErrnoClass, FsErr, Source); skip
                // lowercase keywords. Binary-like width tokens are still Upper — Binary is a
                // prim, not a batch type_def, so the table lookup filters it out.
                found.push(cur.clone());
            }
            cur.clear();
        }
    };
    for ch in line.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            flush(&mut cur, &mut found);
        }
    }
    flush(&mut cur, &mut found);
    found
}

/// Identifiers on a type-def RHS (excluding the defined name). Crude token scan — sufficient
/// for the emitter's single-line sum/product forms; not a full Mycelium parser (Declared).
/// Callers filter against known type_defs in the table.
fn type_names_referenced_in_def_line(def_line: &str) -> Vec<String> {
    let defined = def_line
        .trim()
        .strip_prefix("pub type ")
        .or_else(|| def_line.trim().strip_prefix("type "))
        .and_then(|r| r.split('=').next())
        .map(str::trim)
        .unwrap_or("");
    upper_camel_names_in_line(def_line, defined)
}
