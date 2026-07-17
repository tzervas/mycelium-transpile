//! The driver (M-873): parse one Rust file with `syn`, walk every top-level item exhaustively,
//! and either emit `.myc` text or record a [`Gap`] — **never both-absent** (G2).
//!
//! **Invariant** (checked by `src/tests/invariant.rs`): for every top-level item in
//! `syn::File::items`, its name/index appears in `GapReport::emitted_items` OR at least one
//! [`Gap`] in `GapReport::gaps` — never neither.

use crate::emit::{self, Emitted};
use crate::gap::{Category, Gap, GapReason, GapReport};
use crate::map::tokens_to_string;
use crate::symtab::{self, CandidateKind, SymbolTable};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use syn::spanned::Spanned;
use syn::Item;

/// Parse `path` and transpile every top-level item. Returns the best-effort `.myc` text plus the
/// structured gap report. I/O and parse failures are returned as `Err` (this is a hard failure
/// distinct from a per-item gap — the file could not be read/parsed at all).
///
/// **Single-file mode** — no batch, no siblings: every `use` (even a `crate::`-headed one) is
/// unconditionally gapped, byte-identical to pre-gap-close-2 behavior. Cross-nodule `use`
/// resolution is a *batch*-scoped capability (`batch.rs::transpile_batch`'s two-pass driver, via
/// [`transpile_file_with_ctx`]) — a lone file has no sibling to resolve against by construction.
pub fn transpile_file(path: &Path) -> Result<(String, GapReport), String> {
    transpile_file_with_ctx(path, &SymbolTable::new(), &HashSet::new())
}

/// [`transpile_file`], with a batch-wide cross-nodule [`SymbolTable`] and this file's own
/// **pub-needed** set (item names at least one sibling in the batch resolved a `use` against —
/// see `symtab.rs`/`emit.rs::EmitCtx` docs) installed for the duration of the transpile. Used by
/// `batch.rs::transpile_batch`'s final pass; [`transpile_file`] is the `symtab`-empty/`pub_needed`-
/// empty special case, so its behavior is unchanged.
pub(crate) fn transpile_file_with_ctx(
    path: &Path,
    symtab: &SymbolTable,
    pub_needed: &HashSet<String>,
) -> Result<(String, GapReport), String> {
    let source_text =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    transpile_source_with_ctx(
        &source_text,
        &path.display().to_string(),
        &derive_nodule_path(path),
        symtab,
        pub_needed,
    )
}

/// Transpile already-read source text. Split out from [`transpile_file`] so tests can exercise
/// the driver on small inline fixtures without touching the filesystem.
///
/// **Single-file mode** — see [`transpile_file`]'s doc: `symtab`/`pub_needed` are empty, so every
/// `use` gaps unconditionally, unchanged from pre-gap-close-2 behavior.
pub fn transpile_source(
    source_text: &str,
    file_label: &str,
    nodule_path: &str,
) -> Result<(String, GapReport), String> {
    transpile_source_with_ctx(
        source_text,
        file_label,
        nodule_path,
        &SymbolTable::new(),
        &HashSet::new(),
    )
}

/// [`transpile_source`], with a batch-wide cross-nodule [`SymbolTable`] and this file's own
/// pub-needed set installed — see [`transpile_file_with_ctx`].
pub(crate) fn transpile_source_with_ctx(
    source_text: &str,
    file_label: &str,
    nodule_path: &str,
    symtab: &SymbolTable,
    pub_needed: &HashSet<String>,
) -> Result<(String, GapReport), String> {
    let parsed =
        syn::parse_file(source_text).map_err(|e| format!("failed to parse {file_label}: {e}"))?;

    let mut emitted_items = Vec::new();
    let mut gaps = Vec::new();

    // Gap-close-2 Phase-0 regression fix (M-1042 follow-on): a derived nodule-path segment that
    // collides with a Mycelium reserved word (`l1.fuse`, `std.runtime.colony`, …) cannot be
    // emitted verbatim — see `reserved::sanitize_nodule_path`'s doc for the full root cause. This
    // must run before `render_nodule` below is ever called, so the header itself never carries an
    // un-parseable segment.
    let derived_nodule_path = nodule_path.to_string();
    let (nodule_path, nodule_path_gap) = crate::reserved::sanitize_nodule_path(nodule_path);
    let nodule_path = nodule_path.as_str();
    if let Some(g) = nodule_path_gap {
        gaps.push(Gap {
            file: file_label.to_string(),
            line: 1,
            col: 1,
            category: g.category,
            rust_construct: g.category.as_str().to_string(),
            snippet: format!("nodule {derived_nodule_path};"),
            reason: g.reason,
            item_name: None,
        });
    }

    if !parsed.attrs.is_empty() {
        // Inner (`#![...]`) attributes live outside `syn::File::items`, so they are outside the
        // per-item invariant's scope by construction — but still recorded, never silently
        // dropped. Doc attrs (`//!`) are folded into the nodule header instead of gapped.
        let non_doc: Vec<String> = parsed
            .attrs
            .iter()
            .filter(|a| !a.path().is_ident("doc"))
            .map(tokens_to_string)
            .collect();
        if !non_doc.is_empty() {
            gaps.push(Gap {
                file: file_label.to_string(),
                line: 1,
                col: 1,
                category: Category::InnerAttr,
                rust_construct: Category::InnerAttr.as_str().to_string(),
                snippet: non_doc.join(" "),
                reason: "crate/file-level inner attributes (#![...]) are not transpiled (no \
                          nodule-header equivalent for these Rust-specific directives)"
                    .to_string(),
                item_name: None,
            });
        }
    }

    let total = parsed.items.len();
    let mut body_chunks = Vec::new();

    // M-1006 (E33-1): the per-file resolvability set gating named-field-record emission (see
    // `emit::with_resolvable`). Computed once over this file's declarations, then installed for the
    // item loop so a named-field `struct`/enum variant emits only when it introduces no unresolved
    // in-file reference (which would poison the file's `myc check` and cost its clean items).
    let resolvable = resolvable_type_names(&parsed.items);
    let layouts = struct_layouts(&parsed.items, &resolvable);
    // M-1084: this file's own `use`-resolution context — its crate-root-relative module segments
    // (`self::`/`super::` resolve relative to this) and, when derivable, its own extern-crate
    // identifier (the cross-phylum same-crate-vs-bare precedence — see `symtab.rs` module docs).
    // Derived from `file_label` (not a real `&Path` — every real caller passes an actual repo path
    // via `path.display().to_string()`; a `src`-ancestor-less label, e.g. a test fixture's
    // `"fixture.rs"`, degrades gracefully to the pre-M-1084 bare-key behavior).
    let current_module = derive_module_segments(Path::new(file_label));
    let current_crate = derive_crate_ident(Path::new(file_label));
    let use_ctx = UseCtx {
        module: &current_module,
        crate_ident: current_crate.as_deref(),
    };
    // DN-133 (M-1094) tier (ii): this file's own `use`-imported type names -> their ordered
    // cross-nodule symbol-table lookup key(s) — see `imported_type_keys`'s doc.
    let imported_type_keys =
        imported_type_keys(&parsed.items, current_crate.as_deref(), &current_module);
    crate::emit::with_emit_ctx(
        resolvable,
        layouts,
        symtab.clone(),
        pub_needed.clone(),
        imported_type_keys,
        || {
            for item in &parsed.items {
                let (line, col) = span_line_col(item);
                let name_hint = item_display_name(item);
                let snippet = tokens_to_string(item);

                match dispatch_item(item, &use_ctx) {
                    Outcome::Emitted(Emitted {
                        name,
                        myc,
                        sub_gaps,
                    }) => {
                        body_chunks.push(myc);
                        emitted_items.push(name.clone());
                        for sg in sub_gaps {
                            gaps.push(Gap {
                                file: file_label.to_string(),
                                line,
                                col,
                                category: sg.category,
                                // `rust_construct` mirrors `category` (the finer, per-failure-reason
                                // taxonomy from `gap.rs`), not the coarse `syn::Item` kind an earlier
                                // iteration used (`Impl`/`Fn`/`Struct`/...) — that coarser string
                                // collapsed e.g. every failing `impl` method to the same "Impl" label
                                // regardless of *why* it failed, hiding exactly the distinction the gap
                                // report exists to surface (G2: the report is the ground truth the
                                // surface-feature backlog is synthesized from, so its categories must be
                                // the real ones, not a re-derived approximation of them).
                                rust_construct: sg.category.as_str().to_string(),
                                snippet: snippet.clone(),
                                reason: sg.reason,
                                item_name: Some(name.clone()),
                            });
                        }
                    }
                    Outcome::Gap(reason) => {
                        gaps.push(Gap {
                            file: file_label.to_string(),
                            line,
                            col,
                            category: reason.category,
                            rust_construct: reason.category.as_str().to_string(),
                            snippet,
                            reason: reason.reason,
                            item_name: name_hint,
                        });
                    }
                    Outcome::TestExcluded => {
                        gaps.push(Gap {
                            file: file_label.to_string(),
                            line,
                            col,
                            category: Category::TestItem,
                            rust_construct: Category::TestItem.as_str().to_string(),
                            snippet,
                            reason:
                                "#[cfg(test)] item — out of scope for this PoC's transpilation \
                              surface (excluded from the expressible-fraction denominator, \
                              but recorded, never silently skipped)"
                                    .to_string(),
                            item_name: name_hint,
                        });
                    }
                }
            }
        },
    );

    let myc_text = render_nodule(nodule_path, &body_chunks, &parsed.attrs);
    let report = GapReport {
        source: file_label.to_string(),
        emitted_items,
        gaps,
        total_top_level_items: total,
    };
    Ok((myc_text, report))
}

/// Compute the set of type names that are **resolvable in this file** — the M-1006 (E33-1) gate for
/// named-field-record emission (consumed via [`crate::emit::with_resolvable`]). A declared
/// `struct`/`enum` is resolvable iff every field type (across all variants, for an enum) *maps* AND
/// every **user** type it references is itself a resolvable in-file type. A reference to a type not
/// declared in this file (e.g. a sibling-crate/kernel type such as `ContentHash`) is never
/// resolvable, so a record depending on it stays gapped rather than emitting a reference that would
/// poison the file's `myc check` (VR-5/G2). Builtins are handled by `map_type` and are not deps.
///
/// This is a **greatest** fixed point (start with every mappable declared type, then iteratively
/// drop any whose deps aren't all resolvable) so **recursive and mutually-recursive** types — a
/// self-referential `type Nat = Z | S(Nat)`, an `FsNode`/`ScopeTree` cycle — are correctly kept
/// resolvable (a least fixed point would wrongly exclude every cycle).
fn resolvable_type_names(items: &[Item]) -> HashSet<String> {
    // Each declared type -> its user-type deps, or `None` if any field is unmappable (that type can
    // then never be resolvable — consistent with `map_type` gapping the field).
    fn collect_field_deps(fields: &syn::Fields, acc: &mut Option<Vec<String>>) {
        let field_iter = match fields {
            syn::Fields::Unit => return,
            syn::Fields::Named(fs) => fs.named.iter(),
            syn::Fields::Unnamed(fs) => fs.unnamed.iter(),
        };
        for f in field_iter {
            match acc.as_mut() {
                None => return,
                Some(v) => {
                    if !crate::map::field_type_user_deps(&f.ty, v) {
                        *acc = None;
                        return;
                    }
                }
            }
        }
    }
    let mut deps: Vec<(String, Option<Vec<String>>)> = Vec::new();
    for item in items {
        match item {
            Item::Struct(s) => {
                let mut acc = Some(Vec::new());
                collect_field_deps(&s.fields, &mut acc);
                deps.push((s.ident.to_string(), acc));
            }
            Item::Enum(e) => {
                let mut acc = Some(Vec::new());
                for v in &e.variants {
                    collect_field_deps(&v.fields, &mut acc);
                    if acc.is_none() {
                        break;
                    }
                }
                deps.push((e.ident.to_string(), acc));
            }
            _ => {}
        }
    }
    // Greatest fixed point: seed with every mappable declared type, then drop any whose deps are not
    // all still in the set (an external/unmapped dep, or one already dropped — cascading out).
    let mut resolvable: HashSet<String> = deps
        .iter()
        .filter(|(_, d)| d.is_some())
        .map(|(n, _)| n.clone())
        .collect();
    loop {
        let mut changed = false;
        let mut to_drop: Vec<String> = Vec::new();
        for (name, d) in &deps {
            if !resolvable.contains(name) {
                continue;
            }
            // `d` is `Some` for every name still in `resolvable` (seeded from `is_some`).
            if let Some(ds) = d {
                if ds.iter().any(|dep| !resolvable.contains(dep)) {
                    to_drop.push(name.clone());
                }
            }
        }
        for name in to_drop {
            resolvable.remove(&name);
            changed = true;
        }
        if !changed {
            break;
        }
    }
    // DN-134 SS3 step 1 / DN-132 SS5.1 (the shared `struct_layouts` population, coordinated with
    // M-1089): `struct_layout`'s own gate (`emit::struct_layout`) requires BOTH `resolvable.
    // contains(name)` *and* a `layouts` entry for `name`, where `name` is the CTOR name (the
    // path's last segment) — for a plain struct that is already the struct's own ident, but for
    // an enum struct-variant it is the VARIANT's ident, which this fixed point never inserts (it
    // only ever tracks top-level `struct`/`enum` idents). Without this step, a variant's
    // `struct_layouts` entry (see that fn) would be permanently unreachable through
    // `struct_layout` no matter how the population changes — the resolvability gate, not the
    // layout data, would be the blocker. So: once a WHOLE enum resolves (every variant's fields
    // already had to map+resolve for the enum's own name to survive the fixed point above — a
    // strictly *smaller* dependency set than the enum's combined one), every one of its
    // `Fields::Named` variants' ctor names is *also* resolvable.
    //
    // This union is deliberately permissive (no collision bookkeeping here) — the safety
    // obligation is carried entirely by `struct_layouts`'s own population, which MUST register
    // (into its `seen`/`ambiguous` collision bookkeeping) every struct name and every enum
    // variant's ctor name **unconditionally**, regardless of the owning enum's whole-item
    // resolvability, and gate only the *insertion of a usable value into its output map* on that
    // resolvability. **Historical correction (found by the PR #1548 strict review, empirically
    // reproduced against the compiled transpiler):** an earlier version of `struct_layouts` gated
    // its collision registration on the SAME whole-enum-resolvable check this fixed point uses
    // (skipping registration entirely for an unresolvable enum's variants) and claimed that made
    // the union above safe by construction. That claim was FALSE — a bare ctor name owned by an
    // enum that fails whole-enum resolvability for a reason unrelated to that variant (e.g. a
    // sibling variant with an unmappable field) was then never entered into the collision
    // bookkeeping at all, so a same-named `struct`/other-enum-variant's layout stayed unflagged
    // and a construction site for the excluded enum's variant would resolve — bare-name only,
    // per `struct_layout`'s doc — straight into that unrelated entity's layout: a silent
    // wrong-index bind (G2). `struct_layouts` now registers unconditionally and only conditions
    // the `out`-map insertion, so this union is safe again: a name present here but
    // ambiguous/absent in `layouts` still yields `None` overall (VR-5).
    for item in items {
        if let Item::Enum(e) = item {
            if resolvable.contains(&e.ident.to_string()) {
                for v in &e.variants {
                    if matches!(v.fields, syn::Fields::Named(_)) {
                        resolvable.insert(v.ident.to_string());
                    }
                }
            }
        }
    }
    resolvable
}

/// Positional field layouts of every in-file `struct` **and** every in-file `enum`
/// `Fields::Named` struct-variant — the M-1006 field-projection input (Lever 1) plus, since
/// DN-134/DN-132 (M-1093, coordinated with M-1089's `map_pattern_inner` `Pat::Struct` arm), the
/// shared variant-aware population both the construction arm (`emit::EmitVisitor::visit_struct`)
/// and the pattern arm consume via [`crate::emit::with_emit_ctx`]/[`crate::emit::struct_layout`].
/// Each entity maps to its field slots in declaration order (`Some(name)` named, `None` unnamed
/// — only ever `Some` for an enum variant, since only `Fields::Named` variants are walked); the
/// emitted constructor's name is the struct's own type name, or the variant's own ctor name (see
/// `emit::emit_struct`/`emit::emit_enum`'s struct-variant lowering, `emit.rs:3113` at the time of
/// writing).
///
/// **Collision safety (mandatory, G2 — DN-134 SS3 step 1(b), the cross-leaf finding from the
/// M-1089 pattern-emit review).** [`crate::emit::struct_layout`] resolves by **bare ctor name
/// only** (the path's last segment; no qualifier is threaded through resolution — see that fn's
/// doc). So this population must never let a variant's bare ctor name silently shadow (or be
/// shadowed by) an unrelated struct's — or another variant's — SAME bare name: a file with both
/// `struct A { foo, bar }` and `enum E { A { foo } }` must never let `E::A { foo }` resolve
/// against struct `A`'s layout (a wrong-index bind), nor may `struct A`'s own literal silently
/// keep resolving under the same ambiguous name once a second, distinct declaration claims it —
/// `struct_layout` has no way to tell "meant the struct" from "meant the variant" once two
/// different declarations share one bare name (the resolution side deliberately gets no qualifier
/// threading, discipline (b)'s whole point). So on any collision this population **refuses**
/// (removes) **every** contending entry for that name and marks it permanently ambiguous for the
/// rest of the pass — a partial refusal that left one interpretation reachable would still be a
/// silent wrong bind for a caller that meant the OTHER one.
fn struct_layouts(
    items: &[Item],
    resolvable: &HashSet<String>,
) -> HashMap<String, Vec<Option<String>>> {
    fn named_field_layout(fs: &syn::FieldsNamed) -> Vec<Option<String>> {
        fs.named
            .iter()
            .map(|f| f.ident.as_ref().map(ToString::to_string))
            .collect()
    }

    let mut out: HashMap<String, Vec<Option<String>>> = HashMap::new();
    for item in items {
        if let Item::Struct(s) = item {
            let fields: Vec<Option<String>> = match &s.fields {
                syn::Fields::Named(fs) => named_field_layout(fs),
                syn::Fields::Unnamed(fs) => fs.unnamed.iter().map(|_| None).collect(),
                syn::Fields::Unit => Vec::new(),
            };
            // Pre-existing behavior, unchanged: a duplicate top-level `struct` name (invalid Rust,
            // but not itself validated here) is last-wins — out of DN-134's scope, which is only
            // the NEW struct-vs-variant / variant-vs-variant collision this population introduces.
            out.insert(s.ident.to_string(), fields);
        }
    }

    // `seen`/`ambiguous` track every bare name this population has ever contributed (from a
    // struct OR an accepted variant) so a later collision can be detected and refused — see the
    // collision-safety doc above. Seeded from the structs just inserted.
    let mut seen: HashSet<String> = out.keys().cloned().collect();
    let mut ambiguous: HashSet<String> = HashSet::new();
    for item in items {
        let Item::Enum(e) = item else { continue };
        // **CRITICAL, fixed by the PR #1548 strict review (empirically reproduced against the
        // compiled transpiler — the exact #1535/DN-134 build-blocking hazard).** This whole-enum
        // resolvability check must NOT gate collision *registration* — only whether a usable
        // layout gets *inserted* into `out` below. `struct_layout`'s runtime lookup gate
        // (`emit.rs::struct_layout`) is keyed purely on the bare ctor name, with NO per-enum
        // scoping — so if an unresolvable enum's variant name is skipped here entirely, that bare
        // name never gets marked `seen`/`ambiguous`, and a later resolvable struct or enum that
        // happens to share the SAME bare ctor name goes unflagged, un-poisoned, and reachable —
        // while a construction site for the (unrelated-reason) unresolvable enum's excluded
        // variant would still resolve, bare-name only, straight into that other entity's layout: a
        // silent wrong-index bind (G2). So: register every struct name and every `Fields::Named`
        // variant's ctor name into `seen`/`ambiguous` UNCONDITIONALLY (a name claimed by an
        // unresolvable enum's variant still POISONS it for any other contender), and gate ONLY the
        // `out` insertion on this enum's own resolvability.
        let enum_resolvable = resolvable.contains(&e.ident.to_string());
        for v in &e.variants {
            let syn::Fields::Named(fs) = &v.fields else {
                continue;
            };
            let key = v.ident.to_string();
            if ambiguous.contains(&key) {
                continue;
            }
            if seen.contains(&key) {
                // Collides with an existing struct's name or an earlier-accepted/-registered
                // variant's ctor name — never silently shadow either interpretation (G2). Refuse
                // BOTH: remove whatever is currently keyed under `key` (a struct's or a prior
                // variant's real layout, if any) and mark it ambiguous so no later variant can
                // re-claim it either. This fires regardless of `enum_resolvable` — collision
                // registration is unconditional (see above).
                out.remove(&key);
                ambiguous.insert(key);
                continue;
            }
            // Always mark the name claimed (collision bookkeeping), independent of whether this
            // enum is itself whole-resolvable.
            seen.insert(key.clone());
            // Only a resolvable enum contributes a USABLE layout — an unresolvable enum's variant
            // still occupies (poisons) the name above, but inserts nothing here, matching
            // `struct_layout`'s own gate (`resolvable.contains(name) && layouts.get(name)`): the
            // name won't even be in `resolvable` for an unresolvable enum's variant (see
            // `resolvable_type_names`'s union step), so `out` never needs an entry for it.
            if enum_resolvable {
                out.insert(key, named_field_layout(fs));
            }
        }
    }
    out
}

/// DN-133 (M-1094) tier (ii): for every locally `use`-imported type NAME in this file, the
/// ordered cross-nodule symbol-table lookup key(s)
/// ([`SymbolTable::candidate_lookup_keys`]) that head would resolve through — consumed by
/// [`crate::emit::cross_nodule_resolve_mangled`] so a qualified/associated-fn call site
/// (`Type::method(...)`) can try a batch sibling's own emitted-name set for the mangled
/// `{Type}__{method}` decl (see `emit.rs::EmitCtx::imported_type_keys`'s doc for this tier's
/// currently-honest scope). Reuses [`symtab::use_candidates`]/
/// [`SymbolTable::candidate_lookup_keys`] — the SAME resolution policy [`dispatch_use`] already
/// applies to a plain `use` (DRY, one policy, not two divergent copies). Only a
/// [`CandidateKind::Name`] leaf names a single importable item (a rename/glob/self-module leaf
/// carries no plain name to key on, so it is simply not entered here — consistent with
/// [`dispatch_use`]'s own handling of those kinds elsewhere). Empty in single-file/non-batch mode
/// (no sibling table will ever hit — byte-identical no-op).
fn imported_type_keys(
    items: &[Item],
    current_crate: Option<&str>,
    current_module: &[String],
) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for item in items {
        let Item::Use(u) = item else { continue };
        let Some(candidates) = symtab::use_candidates(&u.tree, current_module) else {
            continue;
        };
        for c in &candidates {
            if let CandidateKind::Name(name) = &c.kind {
                let keys = SymbolTable::candidate_lookup_keys(current_crate, current_module, c);
                out.insert(name.clone(), keys);
            }
        }
    }
    out
}

enum Outcome {
    Emitted(Emitted),
    Gap(GapReason),
    TestExcluded,
}

/// Per-file `use`-resolution context (M-1084) — see the item-loop construction site's doc comment.
/// Threaded through [`dispatch_item`] to [`dispatch_use`] only; every other dispatch arm ignores it,
/// so this signature change stays local to the `use`-dispatch path (never touches `emit.rs`).
struct UseCtx<'a> {
    /// This file's own crate-root-relative module segments (`transpile::derive_module_segments`).
    module: &'a [String],
    /// This file's own extern-crate identifier, when derivable (`transpile::derive_crate_ident`).
    crate_ident: Option<&'a str>,
}

/// Exhaustive dispatch over `syn::Item` (itself `#[non_exhaustive]`). Every arm either calls into
/// `emit.rs` or produces an explicit [`GapReason`] — the trailing `_` arm is the
/// forward-compatibility catch-all, itself a gap, never a silent no-op.
fn dispatch_item(item: &Item, use_ctx: &UseCtx) -> Outcome {
    match item {
        Item::Enum(e) => emit::emit_enum(e).map_or_else(Outcome::Gap, Outcome::Emitted),
        Item::Struct(s) => emit::emit_struct(s).map_or_else(Outcome::Gap, Outcome::Emitted),
        Item::Fn(f) => emit::emit_fn(f).map_or_else(Outcome::Gap, Outcome::Emitted),
        Item::Trait(t) => emit::emit_trait(t).map_or_else(Outcome::Gap, Outcome::Emitted),
        Item::Impl(i) => emit::emit_impl(i).map_or_else(Outcome::Gap, Outcome::Emitted),
        Item::Use(u) => dispatch_use(u, use_ctx),
        Item::Mod(m) => {
            if emit::is_cfg_test(&m.attrs) {
                Outcome::TestExcluded
            } else if m.content.is_none() {
                // Bodyless `mod foo;` / `pub mod foo;` — file-linkage, not translatable library
                // surface. The module tree is implicit in Mycelium's nodule-per-file layout; the
                // sibling `foo.rs` transpiles as its own nodule. Recorded but excluded from the
                // denominator (like a test item), never silently dropped (G2/VR-5; M-1006 Phase-2).
                Outcome::Gap(GapReason::new(
                    Category::ModuleDecl,
                    "external `mod foo;` declaration — file-linkage, not translatable library \
                     surface (Mycelium's nodule-per-file model makes the module tree implicit in \
                     the file layout; the sibling file transpiles as its own nodule). Excluded from \
                     the expressible-fraction denominator, recorded not dropped",
                ))
            } else {
                // Inline `mod foo { … }` — its body is real dropped content, so this stays a counted
                // coverage gap (flatten-able in a later ladder phase), distinct from the bodyless
                // file-linkage case above.
                Outcome::Gap(GapReason::new(
                    Category::Other,
                    "inline `mod foo { … }` — Mycelium's nodule-per-file model has no nested-module \
                     construct in this grammar fragment; the module body is dropped (a real coverage \
                     gap, not file-linkage — its inner items could be flattened in a later phase)",
                ))
            }
        }
        Item::Macro(m) => {
            if m.mac.path.is_ident("macro_rules") {
                Outcome::Gap(GapReason::new(
                    Category::MacroDef,
                    "`macro_rules!` definition — no macro system in this grammar",
                ))
            } else {
                Outcome::Gap(GapReason::new(
                    Category::MacroInvocation,
                    "item-position macro invocation — no macro system in this grammar",
                ))
            }
        }
        Item::Const(c) => Outcome::Gap(GapReason::new(
            Category::Other,
            format!(
                "top-level `const {}` — no const item production in the grammar (`item` covers \
                 use/default/type/trait/impl/fn/object/lower/derive only)",
                c.ident
            ),
        )),
        Item::Static(s) => Outcome::Gap(GapReason::new(
            Category::Other,
            format!(
                "top-level `static {}` — no static item production in the grammar",
                s.ident
            ),
        )),
        Item::Type(t) => Outcome::Gap(GapReason::new(
            Category::Other,
            format!(
                "`type {} = ...` alias — Mycelium's `type_item` always introduces a new nominal \
                 sum type via `'=' constructor ('|' constructor)*`; a bare alias to an existing \
                 type would fabricate a sum type where none exists semantically",
                t.ident
            ),
        )),
        Item::Union(u) => Outcome::Gap(GapReason::new(
            Category::Struct,
            format!("`union {}` — no union construct in the grammar", u.ident),
        )),
        Item::ExternCrate(e) => Outcome::Gap(GapReason::new(
            Category::Other,
            format!(
                "`extern crate {}` — no equivalent (phylum/nodule import model differs)",
                e.ident
            ),
        )),
        Item::ForeignMod(_) => Outcome::Gap(GapReason::new(
            Category::Other,
            "foreign/FFI block — Mycelium's FFI escape is `wild`, legal only inside an \
             `@std-sys` nodule with a declared `!{ffi}` effect; not auto-mapped",
        )),
        Item::TraitAlias(t) => Outcome::Gap(GapReason::new(
            Category::Trait,
            format!("`trait {} = ...` alias — no trait-alias construct", t.ident),
        )),
        Item::Verbatim(_) => Outcome::Gap(GapReason::new(
            Category::Other,
            "unparsed/verbatim item (syn could not fully parse this construct)",
        )),
        _ => Outcome::Gap(GapReason::new(
            Category::Other,
            "unrecognized syn::Item variant (Item is #[non_exhaustive] — forward-compatibility \
             catch-all)",
        )),
    }
}

/// `use` imports (M-1001 `Category::Import`; gap-close-2 DN-34 §8.19/§8.20 batch-scoped
/// cross-nodule resolution — the Import gap-class lever, `symtab.rs`; M-1084 extends it with
/// `self::`/`super::` relative resolution and cross-phylum resolution — see `symtab.rs` module docs).
///
/// **Batch mode (a cross-nodule [`SymbolTable`] is installed):** a `use crate::<mod>::Item`, a
/// `self::`/`super::`-relative form, or a bare `use <mod>::Item;`/`pub use <mod>::Item;` (crate-root
/// -relative OR, when `<mod>` names a sibling PHYLUM in this same batch, cross-phylum) is resolved
/// leaf-by-leaf against [`SymbolTable::candidate_lookup_keys`]' precedence-ordered key(s), each tried
/// against the batch's own sibling files' actually-**emitted** surface (never a name that merely
/// exists in the Rust source but itself gapped). Every leaf that resolves emits a real
/// `use <nodule_path>.<Item>;` line, home-qualified against the sibling's own derived nodule path
/// (never a bare name — the same no-bare-name-collapse discipline DN-113/M-1060's
/// `qualify_cross_phylum` uses for the kernel's cross-phylum case). A leaf that does **not**
/// resolve — an out-of-batch head (`std::`, an external/workspace crate, a phylum not in this batch),
/// an in-batch sibling that itself gapped the requested name, a `self`-module-binding group member, a
/// rename, or a glob — is still recorded as a precise, never-silent [`Category::Import`] gap
/// (VR-5/G2): when **at least one** leaf resolved, the unresolved leaves ride the item's `sub_gaps`
/// (the item is simultaneously "emitted" and "honestly flagged" — see [`Emitted`]'s doc); when
/// **none** resolve, the whole item is one ordinary [`Gap`], as before.
///
/// **Single-file mode (no `SymbolTable` installed — `symtab.rs`'s `cross_nodule_resolve` always
/// misses):** every leaf fails to resolve, so this degenerates to exactly the pre-gap-close-2
/// behavior — a single flagged gap, nothing emitted (byte-identical for every existing single-file
/// caller).
fn dispatch_use(u: &syn::ItemUse, ctx: &UseCtx) -> Outcome {
    let Some(candidates) = symtab::use_candidates(&u.tree, ctx.module) else {
        // A tree with no module-path segment at all (a bare `use Item;` naming nothing), or a
        // `super::` head with no parent to go up to (this file is already at the crate root — a
        // genuine structural miss, real Rust itself rejects this). Unchanged from pre-M-1084
        // behavior for these two residual cases.
        let detail = describe_use_tree(&u.tree);
        return Outcome::Gap(GapReason::new(
            Category::Import,
            format!(
                "`use` import ({detail}) — either a module-path-less head (naming nothing to \
                 resolve against) or a `super::` with no parent to go up to at this file's own \
                 module root. Flagged, not guessed (VR-5/G2)"
            ),
        ));
    };

    let mut emitted_lines = Vec::new();
    let mut resolved_names = Vec::new();
    let mut leaf_gaps = Vec::new();
    for c in &candidates {
        let keys = SymbolTable::candidate_lookup_keys(ctx.crate_ident, ctx.module, c);
        let module_dotted = c.module_segs.join(".");
        match &c.kind {
            CandidateKind::Name(name) => {
                let hit = keys.iter().find_map(|k| {
                    emit::cross_nodule_resolve(k, name).map(|nodule_path| (k, nodule_path))
                });
                match hit {
                    Some((_, nodule_path)) => {
                        emitted_lines.push(format!("use {nodule_path}.{name};"));
                        resolved_names.push(name.clone());
                    }
                    None => leaf_gaps.push(GapReason::new(
                        Category::Import,
                        if keys.iter().any(|k| emit::cross_nodule_has_module(k)) {
                            format!(
                                "`use {module_dotted}::{name}` — `{name}` is not among sibling \
                                 `{module_dotted}`'s successfully-emitted surface in this batch \
                                 (the sibling file itself gapped it rather than emitting it). \
                                 Flagged, not guessed (VR-5/G2)"
                            )
                        } else {
                            format!(
                                "`use {module_dotted}::{name}` — `{module_dotted}` is not a \
                                 sibling module OR sibling phylum transpiled in this same batch \
                                 (an external crate, `std`, or simply out of this batch's target \
                                 set). Flagged, not guessed (VR-5/G2)"
                            )
                        },
                    )),
                }
            }
            CandidateKind::SelfModule => leaf_gaps.push(GapReason::new(
                Category::Import,
                format!(
                    "`use {module_dotted}::{{self, ..}}` — `self` binds the module ITSELF as a \
                     local name; there is no \"import a nodule as a name\" construct in this \
                     grammar, so this leaf cannot resolve (distinct from an ordinary lookup miss)"
                ),
            )),
            CandidateKind::Rename { from, to } => leaf_gaps.push(GapReason::new(
                Category::Import,
                format!(
                    "`use {module_dotted}::{from} as {to}` — a renamed cross-nodule import is out \
                     of this increment's scope (the alias would need threading through every \
                     downstream reference to `{to}` in this file's body); flagged, not guessed \
                     (VR-5/G2)"
                ),
            )),
            CandidateKind::Glob => leaf_gaps.push(GapReason::new(
                Category::Import,
                format!(
                    "`use {module_dotted}::*` — a cross-nodule glob is out of this increment's \
                     scope (mirrors DN-113 v1's own deferral of a cross-phylum glob to M-982 \
                     rather than guessing a disambiguation); flagged, not guessed (VR-5/G2)"
                ),
            )),
        }
    }

    if emitted_lines.is_empty() {
        // Nothing resolved: fold every leaf's precise reason into one gap covering the whole item
        // (mirrors the self/super-headed early-return above — a wholly-unresolved `use` is still a
        // single, never-silent gap, not a vacuous "emission" of zero lines).
        let joined = leaf_gaps
            .into_iter()
            .map(|g| g.reason)
            .collect::<Vec<_>>()
            .join("; ");
        Outcome::Gap(GapReason::new(Category::Import, joined))
    } else {
        Outcome::Emitted(Emitted {
            name: format!("use:{}", resolved_names.join(",")),
            myc: emitted_lines.join("\n"),
            sub_gaps: leaf_gaps,
        })
    }
}

/// A short human description of a `use` tree's shape, for the gap reason.
fn describe_use_tree(tree: &syn::UseTree) -> String {
    match tree {
        syn::UseTree::Path(p) => describe_use_tree(&p.tree),
        syn::UseTree::Name(n) => format!("single path ending `{}`", n.ident),
        syn::UseTree::Glob(_) => "glob `::*`".to_string(),
        syn::UseTree::Rename(r) => format!("rename `{} as {}`", r.ident, r.rename),
        syn::UseTree::Group(_) => "grouped `{{a, b}}`".to_string(),
    }
}

fn item_display_name(item: &Item) -> Option<String> {
    match item {
        Item::Const(i) => Some(i.ident.to_string()),
        Item::Enum(i) => Some(i.ident.to_string()),
        Item::ExternCrate(i) => Some(i.ident.to_string()),
        Item::Fn(i) => Some(i.sig.ident.to_string()),
        Item::ForeignMod(_) => None,
        Item::Impl(i) => Some(tokens_to_string(&*i.self_ty)),
        Item::Macro(i) => i.ident.as_ref().map(|id| id.to_string()),
        Item::Mod(i) => Some(i.ident.to_string()),
        Item::Static(i) => Some(i.ident.to_string()),
        Item::Struct(i) => Some(i.ident.to_string()),
        Item::Trait(i) => Some(i.ident.to_string()),
        Item::TraitAlias(i) => Some(i.ident.to_string()),
        Item::Type(i) => Some(i.ident.to_string()),
        Item::Union(i) => Some(i.ident.to_string()),
        Item::Use(_) => None,
        _ => None,
    }
}

fn span_line_col(item: &Item) -> (usize, usize) {
    let start = item.span().start();
    (start.line, start.column + 1)
}

/// Best-effort nodule-path derivation (Declared heuristic): `crates/mycelium-std-cmp/src/lib.rs`
/// -> `std.cmp`, matching `lib/std/cmp.myc`'s actual header for the crate this PoC targets.
///
/// **DN-109 section 5.1 item 1 (M-1042).** Also incorporates the file's **intra-crate module
/// path** — the path components between `src/` and the leaf file — so two same-stem files in
/// different subdirectories of the same crate (two `mod.rs`, say) get distinct dotted nodule
/// names instead of colliding on the crate-level prefix alone: `crates/mycelium-std-cmp/src/
/// foo/mod.rs` -> `std.cmp.foo`, `crates/mycelium-std-cmp/src/foo/bar.rs` -> `std.cmp.foo.bar`.
/// A `mod.rs`/`lib.rs` leaf contributes no segment of its own (it names the *enclosing*
/// directory, not a new submodule); every other path component becomes a dotted segment. This
/// generalizes the prior top-level-only derivation without changing it for the common case (a
/// file directly under a crate's `src/`, e.g. `lib.rs`) — same crate-prefix logic, just anchored
/// on the last `src` path component (robust to whatever root a batch run walks: M-1006 Phase-2's
/// whole-corpus run, or the per-crate `<crate>/src` root every real invocation
/// (`scripts/checks/transpile-vet.sh`, `gen/myc-drafts/regenerate.sh`) actually uses today — no
/// extra root parameter needs threading through `batch.rs`/the CLI for this).
///
/// Not guaranteed to be meaningful for an arbitrary input path outside this `<crate>/src/...`
/// convention — the CLI documents this; a path with no `src` ancestor falls back to the bare
/// file stem (never a silent mis-derivation, G2).
pub(crate) fn derive_nodule_path(path: &Path) -> String {
    let Some((crate_prefix, segments)) = crate_prefix_and_segments(path) else {
        return fallback_stem(path);
    };
    if segments.is_empty() {
        crate_prefix
    } else {
        format!("{crate_prefix}.{}", segments.join("."))
    }
}

/// The Rust **extern-crate identifier** (M-1084 cross-phylum lever — see `symtab.rs` module docs):
/// the raw crate-directory name, hyphens replaced with underscores (`mycelium-std-rand` ->
/// `mycelium_std_rand`) — the standard Cargo package-name -> crate-identifier mapping, and exactly
/// the token a sibling PHYLUM's own `use mycelium_std_rand::...;` names. `None` when `path` has no
/// `src` ancestor to anchor the derivation on (the same degenerate case [`derive_nodule_path`]/
/// [`derive_module_segments`] fall back for) — never a guessed identity for a path this transpiler
/// cannot really place in a real crate (VR-5/G2); every existing `src`-ancestor-less caller (this
/// crate's own `src/tests/batch.rs` temp fixtures) is unaffected: cross-phylum qualification never
/// applies to them, byte-identical to pre-M-1084 behavior.
pub(crate) fn derive_crate_ident(path: &Path) -> Option<String> {
    let (raw_prefix, _segments) = raw_crate_dir_and_segments(path)?;
    Some(raw_prefix.replace('-', "_"))
}

/// The Rust crate-root-relative **module-path segments** for `path` — e.g. `checkty.rs` ->
/// `["checkty"]`, `foo/bar.rs` -> `["foo", "bar"]`, `foo/mod.rs` -> `["foo"]`, a crate-root
/// `lib.rs`/`mod.rs` -> `[]` (empty — it names no submodule of itself). This is the SAME
/// derivation [`derive_nodule_path`] further crate-prefixes + `.`-joins for the **emitted** `.myc`
/// nodule header; the gap-close-2 cross-nodule [`crate::symtab::SymbolTable`] (`batch.rs`) instead
/// dot-joins these bare (no crate prefix) to match a `use crate::<segs>::Item` / crate-root bare
/// `use <segs>::Item` path's own module-path segments — one derivation, not two divergent copies
/// (DRY). Falls back to `[<bare file stem>]` for a path with no `src` ancestor to anchor on (the
/// same degenerate case [`derive_nodule_path`]/[`fallback_stem`] handle) — never a silent empty
/// module key that could spuriously collide with a genuine crate-root file.
pub(crate) fn derive_module_segments(path: &Path) -> Vec<String> {
    match crate_prefix_and_segments(path) {
        Some((_, segments)) => segments,
        None => vec![fallback_stem(path)],
    }
}

/// Shared by [`derive_nodule_path`]/[`derive_module_segments`]: the crate-prefix string (dotted,
/// `mycelium-` stripped) and the intra-crate module-path segments, or `None` when `path` has no
/// `src` ancestor to anchor the derivation on.
fn crate_prefix_and_segments(path: &Path) -> Option<(String, Vec<String>)> {
    let (raw_prefix, segments) = raw_crate_dir_and_segments(path)?;
    let crate_prefix = {
        let stripped = raw_prefix.strip_prefix("mycelium-").unwrap_or(raw_prefix);
        stripped.replace('-', ".")
    };
    Some((crate_prefix, segments))
}

/// Shared by [`crate_prefix_and_segments`]/[`derive_crate_ident`] (M-1084; DRY — one derivation, not
/// two divergent copies): the RAW crate-directory name (verbatim, e.g. `mycelium-std-rand` —
/// un-stripped, un-dotted) and the intra-crate module-path segments, anchored on the last `src`
/// path component. `None` when `path` has no `src` ancestor (the degenerate case every caller falls
/// back from).
fn raw_crate_dir_and_segments(path: &Path) -> Option<(&str, Vec<String>)> {
    let components: Vec<&std::ffi::OsStr> = path.components().map(|c| c.as_os_str()).collect();
    let src_idx = components.iter().rposition(|c| *c == "src")?;
    let raw_prefix = (src_idx > 0)
        .then(|| components[src_idx - 1].to_str())
        .flatten()?;

    let after_src = &components[src_idx + 1..];
    let mut segments = Vec::with_capacity(after_src.len());
    for (i, comp) in after_src.iter().enumerate() {
        let name = comp.to_str().unwrap_or("");
        if i + 1 == after_src.len() {
            // Leaf file: strip `.rs`; a `mod`/`lib` stem names the enclosing directory, not a
            // new segment.
            let stem = Path::new(name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(name);
            if stem != "mod" && stem != "lib" {
                segments.push(stem.to_string());
            }
        } else {
            segments.push(name.to_string());
        }
    }
    Some((raw_prefix, segments))
}

/// Fallback nodule-path derivation for a path with no `src` ancestor to anchor on — the bare
/// file stem, never a silent panic/empty string (G2).
fn fallback_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn render_nodule(nodule_path: &str, chunks: &[String], file_attrs: &[syn::Attribute]) -> String {
    let mut out = String::new();
    out.push_str(&format!("// nodule: {nodule_path}\n"));
    for d in emit::doc_lines(file_attrs) {
        out.push_str(&d);
        out.push('\n');
    }
    out.push_str(
        "// @summary: best-effort transpilation via mycelium-transpile (M-873). Declared,\n\
         // unvalidated — no Mycelium parser/typechecker confirms this output; see the\n\
         // accompanying .gap.json for every construct this pass could not express.\n",
    );
    out.push_str(&format!("nodule {nodule_path};\n\n"));
    out.push_str(&chunks.join("\n\n"));
    out.push('\n');
    out
}
