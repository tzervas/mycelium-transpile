//! `mycelium-transpile` — a Rust -> Mycelium transpiler PoC (M-873, kickoff `trx`).
//!
//! Reads **one** Rust crate's source (`syn::parse_file`) and produces two artifacts:
//! 1. a best-effort `.myc` rendering of every top-level item this PoC can express, and
//! 2. a structured, **never-silent** [`gap::GapReport`] naming every construct it could not
//!    (or, absent a confirmed grammar mapping, would not) express — never a silent drop (G2).
//!
//! # Design context
//!
//! The maintainer's `py2rust`/`py-rust-bridge` scaffolds have no reusable AST-walk or
//! gap-analyzer (py2rust's "analyzer" is an *allowlist* of four bad constructs — everything else
//! passes silently, the exact anti-pattern this crate avoids). This crate is instead built
//! directly on [`syn`] (the kickoff-sanctioned fallback), with an **exhaustive dispatch**
//! (`transpile::dispatch_item`) whose fallback arm always records a [`gap::Gap`].
//!
//! `lib/std/cmp.myc` (the ground-truth twin for the diff harness in
//! `src/tests/diff.rs`) is a hand-refined, structurally **distinct**, ~1/10th-scale narrower
//! surface over `crates/mycelium-std-cmp/src/lib.rs` (DN-66 §3.1) — a raw text diff would diverge
//! massively, which is *expected*. The diff harness instead characterizes each item as
//! matched / refined / absent, never asserts textual equality.
//!
//! # Guarantee tags (VR-5)
//!
//! - Every emitted `.myc` fragment is **Declared**: a heuristic `syn` -> surface-text mapping,
//!   never validated by a Mycelium parser/typechecker (this crate has none). "Emitted" means
//!   "some plausible-looking text was produced", not "this type-checks".
//! - The `.myc`-item extraction used by the diff harness (regex over `lib/std/cmp.myc`) is
//!   **Declared** (a heuristic, not a Mycelium parser).
//! - The never-silent invariant (every top-level item is emitted, gapped, or both — never
//!   neither) is checked over a fixed, hand-written corpus (`src/tests/invariant.rs`) — **not**
//!   `Proven`: `syn::Item` is `#[non_exhaustive]`, so the dispatch's exhaustiveness rests on a
//!   catch-all arm, not on an exhaustiveness check the compiler enforces. Tagged **Empirical/
//!   Declared** accordingly, per VR-5 (never upgraded past its checked basis).

pub mod batch;
pub mod emit;
pub mod gap;
pub mod map;
pub mod prim_map;
pub mod remap;
pub mod reserved;
pub(crate) mod symtab;
pub mod transpile;
pub mod type_map;
pub mod vet;
pub(crate) mod visit;

pub use batch::{
    discover_rs_files, output_rel_path, summarize, transpile_batch, BatchSummary, UnionGapReport,
};
pub use gap::{Category, Gap, GapReport};
pub use remap::{
    build_remap_manifest, render_remap_md, IdiomChoice, IdiomClass, NoduleRemap, PhylumRemap,
    RemapManifest, RemapOperation, RemapSafety, RemapSource,
};
pub use transpile::{transpile_file, transpile_source};
pub use vet::{vet_batch, MycChecker, VetClass, VetInput, VetRecord, VetReport};

#[cfg(test)]
mod tests;
