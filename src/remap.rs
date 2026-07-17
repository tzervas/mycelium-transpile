//! The remap manifest (M-1044, DN-109 §5.2) — the structural + idiom provenance ledger the
//! maintainer's "every restructuring must be mapped, documented, and explained (how/where/why)"
//! hard requirement (DN-109 §4) demands, mandatory from v0 even for pure `Keep`.
//!
//! **Schema origin.** Types here follow DN-109 §5.2's proposed `remap.json` schema field-for-field
//! (`phylum`/`nodules`/`idiom_choices`, and every sub-field the note names). **Placement differs**
//! per the note's own ratified lean (§5.2 "Leaning: extend", ratified at §7-c, 2026-07-11): rather
//! than a new standalone `remap.json` sidecar, these types are folded into the existing
//! [`crate::batch::BatchSummary`] (`summary.json`) as an additive `remap` field — the gap report
//! already carries a per-file/per-category ledger, this is the same ledger with target-side plus
//! rationale columns, not a second artifact. [`render_remap_md`] renders the **`REMAP.md`** human
//! view — a **pure projection of [`RemapManifest`]**, nothing else: the same data that lands in
//! `summary.json`'s `remap` field, reformatted. That purity is what "byte-derivable from the JSON"
//! (DN-109 §5.2/M-1044 DoD) means and is exactly what `src/tests/remap.rs`'s round-trip test
//! checks — serialize, deserialize, re-render, byte-compare.
//!
//! **v0 scope (Mechanical-only auto-fire, DN-109 §7-e/ratification item 1).** Every nodule emitted
//! by a batch run gets a pure **`Keep`** entry (structure-preserving 1:1, DN-109 §5.3-B): v0 does
//! not attempt `Consolidate`/`Split`/`Relocate`/`CrateToPhylum` (those are opt-in, plan-reviewed,
//! per §4.1/§5.1 item 2 — no such plan exists yet, so none are auto-proposed here; that is the
//! transpiler staying a **structure-preserving 1:1 emitter**, DN-109 §7-d). `idiom_choices` starts
//! **honestly empty**: the `&T`-erasure idiom (D4) already fires mechanically in
//! [`crate::map`]/[`crate::emit`], but neither module yet records a *located* EXPLAIN entry for
//! each firing (that instrumentation is a separate, later change — not invented here per the task's
//! explicit KISS/YAGNI boundary). An empty `idiom_choices` vec is the honest v0 answer, not a
//! silent gap: the field exists in the schema (never-silent structurally) and is populated the
//! moment the instrumentation lands, with no schema change needed.
//!
//! **Guarantee tag: `Declared`, and it stays that way.** Per DN-109 §8 DoD / the M-1044 issue body:
//! *"no guarantee-tag upgrade from the manifest's existence alone (VR-5)"*. The manifest **records**
//! proposed/performed structural and idiom decisions; it does not certify them. A `Keep` entry
//! being present says only "this nodule's mapping was recorded", not that it was verified correct —
//! that certification is the differential's job (a separate, later gate), not this ledger's.

use crate::batch::FileResult;
use crate::transpile::derive_nodule_path;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The restructuring operation recorded for a target nodule (DN-109 §4.1). v0 only ever emits
/// `Keep` (see module docs); the other variants exist in the schema now so a later, human-reviewed
/// restructuring pass can populate them without a schema break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemapOperation {
    Keep,
    Consolidate,
    Split,
    Relocate,
    CrateToPhylum,
}

/// DN-109 §4.2's safe/risky classification for a restructuring entry. v0's `Keep` entries are
/// always `Safe` (structure-preserving 1:1 changes neither the public API surface nor elaborated
/// identity, §4.3/ADR-003); any future non-`Keep` operation that changes the API surface must be
/// `Review`, never auto-applied (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemapSafety {
    Safe,
    Review,
}

/// The L4 idiom-decision class (DN-109 §3.1) an [`IdiomChoice`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdiomClass {
    Mechanical,
    Heuristic,
    Judgment,
}

/// One `sources` entry under a [`NoduleRemap`] (DN-109 §5.2): the Rust-side origin of the material
/// folded into `target_nodule`. `moved_items` is empty for a pure `Keep` (nothing was moved between
/// nodules — the whole file maps to the whole nodule).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemapSource {
    pub rust_path: String,
    pub rust_span: Option<String>,
    pub moved_items: Vec<String>,
}

/// One target nodule's restructuring record (DN-109 §5.2 `nodules[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoduleRemap {
    pub target_nodule: String,
    pub operation: RemapOperation,
    pub sources: Vec<RemapSource>,
    pub rationale: String,
    pub safety: RemapSafety,
    pub api_surface_changed: bool,
    pub identity_neutral: bool,
    pub guarantee: String,
}

/// One `idiom_choices[]` entry (DN-109 §5.2): the L4 EXPLAIN trail for a non-default idiom decision
/// (§3.2 clause 4 — "recorded EXPLAIN-ably" is one of the four conjunctive conditions an automatic
/// idiom transformation must satisfy to fire at all).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdiomChoice {
    pub target_span: String,
    pub rust_span: String,
    pub decision: String,
    pub class: IdiomClass,
    pub chose: String,
    pub alternatives: Vec<String>,
    pub reason: String,
}

/// The `phylum` header (DN-109 §5.2): which Rust crate this manifest's batch run transpiled, and
/// the Mycelium phylum name it targets. Derived from the batch root's directory name via the same
/// `mycelium-` prefix-strip / `-`-to-`.` convention [`derive_nodule_path`] already uses for the
/// crate-name segment of a nodule path (`crates::transpile`'s existing, documented heuristic — not
/// a new one minted here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhylumRemap {
    pub source_crate: String,
    pub target_phylum: String,
}

/// The full remap manifest for one batch run (DN-109 §5.2), folded into
/// [`crate::batch::BatchSummary`] per the §7-c "extend, don't mint a new artifact" ratification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemapManifest {
    pub phylum: PhylumRemap,
    pub nodules: Vec<NoduleRemap>,
    pub idiom_choices: Vec<IdiomChoice>,
}

/// Derive the `(source_crate, target_phylum)` pair from a batch run's root directory. Mirrors
/// [`derive_nodule_path`]'s crate-prefix convention: if `root`'s own last component is `src` (the
/// common single-crate invocation, `<crate>/src`), the crate name is `root`'s parent directory;
/// otherwise `root`'s own name is used directly (e.g. a whole-corpus run rooted at `crates/`).
/// Falls back to `"unknown"` rather than panicking or fabricating a name — never-silent (G2) on the
/// degenerate case of a root with no usable file name.
fn derive_phylum(root: &Path) -> PhylumRemap {
    let crate_dir_name = if root.file_name().and_then(|n| n.to_str()) == Some("src") {
        root.parent().and_then(Path::file_name)
    } else {
        root.file_name()
    };
    let source_crate = crate_dir_name
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let target_phylum = {
        let stripped = source_crate
            .strip_prefix("mycelium-")
            .unwrap_or(&source_crate);
        stripped.replace('-', ".")
    };
    PhylumRemap {
        source_crate,
        target_phylum,
    }
}

/// Build the v0 [`RemapManifest`] for a batch run: the `phylum` header derived from `root`, one
/// pure-`Keep` [`NoduleRemap`] per transpiled file (structure-preserving 1:1, DN-109 §5.3-B), and
/// an honestly empty `idiom_choices` (see module docs — no per-item idiom instrumentation exists
/// yet to populate it from). Never fabricates an entry it cannot derive mechanically (DN-109
/// §7-e's Mechanical-only v0 boundary).
pub fn build_remap_manifest(results: &[FileResult], root: &Path) -> RemapManifest {
    let phylum = derive_phylum(root);
    let nodules = results
        .iter()
        .map(|r| NoduleRemap {
            target_nodule: derive_nodule_path(&r.path),
            operation: RemapOperation::Keep,
            sources: vec![RemapSource {
                rust_path: r.path.display().to_string(),
                rust_span: None,
                moved_items: Vec::new(),
            }],
            rationale: "structure-preserving 1:1 (DN-109 §5.3-B v0 default: every emitted nodule \
                        keeps its source file's mod-tree position, no restructuring proposed)"
                .to_string(),
            safety: RemapSafety::Safe,
            api_surface_changed: false,
            identity_neutral: true,
            guarantee: "Declared".to_string(),
        })
        .collect();
    RemapManifest {
        phylum,
        nodules,
        idiom_choices: Vec::new(),
    }
}

/// Escape a value before it lands inside a Markdown table cell: a literal `|` would otherwise
/// terminate the cell early (splitting the row) and a literal newline would break the row outright.
/// v0's own `rationale` text is a fixed, pipe-free constant, so this never fires today — but
/// `rationale`/`reason`/`chose` are free-text `String` fields in the schema (DN-109 §5.2), and a
/// later human-authored `Consolidate`/`Split`/`Relocate` rationale or an `idiom_choices` entry
/// could legitimately contain either character. Escaping unconditionally keeps the renderer correct
/// over the *whole* schema, not just the fields v0 happens to populate — never a silently-corrupted
/// table row (G2).
fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Render the human-readable **`REMAP.md`** view of a [`RemapManifest`] — a **pure projection**: it
/// reads only the fields of `manifest`, so `render_remap_md` applied to a manifest deserialized
/// from `remap.json`/`summary.json` produces byte-identical output to applying it directly (checked
/// by `src/tests/remap.rs::remap_md_is_byte_derivable_from_json`). Deterministic — no timestamps,
/// no non-manifest state — so re-rendering the same manifest twice always yields the same bytes.
pub fn render_remap_md(manifest: &RemapManifest) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Remap Manifest — {} \u{2192} {}\n\n",
        manifest.phylum.source_crate, manifest.phylum.target_phylum
    ));
    out.push_str(
        "_DN-109 §5.2 provenance ledger. Guarantee: **Declared** — this records proposed/performed \
         structural and idiom choices; it does not certify them (no guarantee-tag upgrade from the \
         manifest's existence alone, VR-5). Rendered from `remap` in `summary.json` — this file is a \
         pure projection, never a second source of truth._\n\n",
    );

    out.push_str(&format!("## Nodules ({})\n\n", manifest.nodules.len()));
    if manifest.nodules.is_empty() {
        out.push_str("_(none — no files transpiled in this batch run)_\n\n");
    } else {
        out.push_str(
            "| Target nodule | Operation | Safety | API surface changed | Identity neutral | Sources | Rationale |\n\
             |---|---|---|---|---|---|---|\n",
        );
        for n in &manifest.nodules {
            let sources = n
                .sources
                .iter()
                .map(|s| format!("`{}`", md_cell(&s.rust_path)))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "| `{}` | {:?} | {:?} | {} | {} | {} | {} |\n",
                md_cell(&n.target_nodule),
                n.operation,
                n.safety,
                n.api_surface_changed,
                n.identity_neutral,
                sources,
                md_cell(&n.rationale),
            ));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "## Idiom choices ({})\n\n",
        manifest.idiom_choices.len()
    ));
    if manifest.idiom_choices.is_empty() {
        out.push_str(
            "_(none in this run — v0 Mechanical-only auto-fire with no located idiom-choice \
             instrumentation yet; see DN-109 §7-e and this module's doc comment)_\n",
        );
    } else {
        out.push_str(
            "| Target span | Rust span | Decision | Class | Chose | Alternatives | Reason |\n\
             |---|---|---|---|---|---|---|\n",
        );
        for c in &manifest.idiom_choices {
            let alternatives = c
                .alternatives
                .iter()
                .map(|a| md_cell(a))
                .collect::<Vec<_>>()
                .join("; ");
            out.push_str(&format!(
                "| `{}` | `{}` | {} | {:?} | {} | {} | {} |\n",
                md_cell(&c.target_span),
                md_cell(&c.rust_span),
                md_cell(&c.decision),
                c.class,
                md_cell(&c.chose),
                alternatives,
                md_cell(&c.reason),
            ));
        }
    }

    out
}
