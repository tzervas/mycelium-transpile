//! DN-136/P1-a — the pattern-kind emit-hook axis (Alt B: a static per-axis handler table
//! generalizing the already-landed `prim_map::TABLE` registry, `prim_map.rs:140`).
//!
//! Each row pairs a **pure** recognizer (`fn(&syn::Pat) -> bool` — no ctx, no emission) with the
//! lowering it owns; [`crate::emit::map_pattern_inner`] (the unchanged driver) consults
//! [`lookup`] FIRST, threading the same `self_ty` it always did, falling through to its own
//! base-kernel-pattern match for anything the table doesn't cover, then the driver's own final
//! explicit gap — identical fallback shape to the pre-refactor inline `match`'s `_` arm (G2: a
//! table miss never silently drops).
//!
//! **Scope (DN-136 §1's own framing — "M-823/M-826/M-1089 serialized here"):** exactly the three
//! landed "gap-closing leaf" pattern kinds that used to serialize on `map_pattern_inner`'s shared
//! body move here. The base-kernel forms (`Pat::Wild`/`Ident`/`Path`/`TupleStruct`/`Lit`/`Paren`/
//! `Reference`) are foundational grammar primitives, not additive leaf targets — the driver keeps
//! them (a lower-risk, narrower move than hook-ifying every arm, and the one DN-136 §1's table
//! actually names as the collision surface). A future pattern leaf (e.g. a range/`@`-subpattern
//! row) adds ONE new file here + ONE append-only [`TABLE`] row — never touches
//! `map_pattern_inner` (DN-136's stated objective).
//!
//! **Ordered-pass-preservation (DN-136 §3/§7):** a recognizer never inspects anything beyond the
//! `Pat` node itself and never mutates state; `emit` threads only the `self_ty` parameter the
//! driver already threads, recursing back through the public, recursion-guarded
//! [`crate::emit::map_pattern`] for any sub-pattern — identical to the pre-refactor inline arms'
//! own recursion shape. The table changes *which* function handles a `Pat` shape, never *when* or
//! *how many* times pattern lowering runs.

use crate::gap::GapReason;
use syn::Pat;

mod or_pat;
mod struct_variant_pat;
mod tuple_pat;

/// One pattern-kind handler row (DN-136 §2's `PatternHandler` shape).
pub struct PatternHandler {
    /// Pure recognizer — does this row own this `Pat` shape? No emission, no ctx mutation.
    pub recognizes: fn(&Pat) -> bool,
    /// The lowering — the native `.myc` pattern text, or an explicit gap.
    pub emit: fn(&Pat, self_ty: Option<&str>) -> Result<String, GapReason>,
    /// For `EXPLAIN`/diagnostics (G2).
    #[allow(dead_code)] // read by future EXPLAIN tooling, not yet consumed (DN-136 §2)
    pub slug: &'static str,
    /// The DN/M-id grounding this row (VR-5).
    #[allow(dead_code)] // read by future EXPLAIN tooling, not yet consumed (DN-136 §2)
    pub citation: &'static str,
}

/// The table. Order is insertion order; [`lookup`] does a linear scan (small, fixed table, same
/// shape as [`crate::prim_map::TABLE`]). The three rows recognize **mutually exclusive**
/// `syn::Pat` variants (`Or`/`Tuple`/`Struct`), so scan order carries no ambiguity today — a
/// future row must keep that property (a `recognizes` overlapping an existing row would make
/// first-match-wins order-significant, which the ordered-pass invariant does not forbid but a
/// reviewer must notice, per DN-136 §8 point 2).
pub const TABLE: &[PatternHandler] = &[or_pat::ROW, tuple_pat::ROW, struct_variant_pat::ROW];

/// First-match-wins linear scan over [`TABLE`]. `None` for every `Pat` shape this axis doesn't
/// (yet) own — the caller's own base match / final gap covers it, unchanged.
#[must_use]
pub fn lookup(pat: &Pat) -> Option<&'static PatternHandler> {
    TABLE.iter().find(|row| (row.recognizes)(pat))
}
