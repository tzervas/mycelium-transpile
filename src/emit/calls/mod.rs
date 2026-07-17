//! DN-136/P1-a — the call-shape emit-hook axis (Alt B: a static per-axis handler table
//! generalizing the already-landed `prim_map::TABLE` registry, `prim_map.rs:140`).
//!
//! **The call-shape *recognition* (bare vs qualified/associated) moves to a row; the *resolution
//! against `local_mangled`* stays effectively driver-owned (DN-136 §3 item 3 / §7).** In this
//! crate that resolution is read through the thread-local `EmitCtx` via free functions
//! (`crate::emit::local_mangled_assoc_fn_known`/`crate::emit::cross_nodule_resolve_mangled`), not
//! an explicit `&mut EmitCtx` parameter — so [`qualified_assoc::resolve`] calls those SAME
//! functions, unchanged, rather than re-deriving the lookup. Nothing about **who populates**
//! `local_mangled` moves: `record_local_mangled_assoc_fn` is still called only from
//! `crate::emit::emit_impl`'s success path, in the SAME single left-to-right item pass — a row
//! here only ever *reads* that state, never advances it, so the DN-133 ordering invariant holds
//! by construction (the row cannot perturb *when* an earlier item's mangled name becomes known).
//!
//! **The other two call-target shapes (a 3+-segment qualified path, and a non-path call target)
//! never resolve** (there is no row for them, by design — DN-133 §2 sub-kind 3 explicitly routes
//! the former through the Import/symtab resolver instead) — [`crate::emit::EmitVisitor::visit_call`]
//! (the driver) keeps their explicit gaps unchanged, reached only when [`lookup`] misses (G2: a
//! table miss falls through to the existing explicit gap, never a silent drop).

use crate::gap::GapReason;
use syn::ExprCall;

mod bare;
mod qualified_assoc;

/// One call-shape handler row.
pub struct CallHandler {
    /// Pure recognizer — does this row own this call-target shape? No emission, no ctx read.
    pub recognizes: fn(&ExprCall) -> bool,
    /// Resolve the call TARGET NAME text (reads, never mutates, the driver's thread-local
    /// resolution state — see module docs), given the driver's threaded `self_ty`.
    pub resolve: fn(&ExprCall, self_ty: Option<&str>) -> Result<String, GapReason>,
    /// For `EXPLAIN`/diagnostics (G2).
    #[allow(dead_code)] // read by future EXPLAIN tooling, not yet consumed (DN-136 §2)
    pub slug: &'static str,
    /// The DN/M-id grounding this row (VR-5).
    #[allow(dead_code)] // read by future EXPLAIN tooling, not yet consumed (DN-136 §2)
    pub citation: &'static str,
}

/// The table. The two rows recognize **mutually exclusive** shapes (1-segment vs 2-segment
/// path), so scan order carries no ambiguity.
pub const TABLE: &[CallHandler] = &[bare::ROW, qualified_assoc::ROW];

/// First-match-wins linear scan over [`TABLE`]. `None` for a 3+-segment qualified path or a
/// non-path call target — the driver's own remaining match arms cover those, unchanged.
#[must_use]
pub fn lookup(c: &ExprCall) -> Option<&'static CallHandler> {
    TABLE.iter().find(|row| (row.recognizes)(c))
}
