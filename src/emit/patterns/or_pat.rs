//! M-823 (or-pattern) — DN-136/P1-a row. Moved verbatim (no behavior change) from
//! `map_pattern_inner`'s `Pat::Or` arm.

use super::PatternHandler;
use crate::emit::map_pattern;
use crate::gap::{Category, GapReason};
use syn::Pat;

fn recognizes(pat: &Pat) -> bool {
    matches!(pat, Pat::Or(_))
}

fn emit(pat: &Pat, self_ty: Option<&str>) -> Result<String, GapReason> {
    let Pat::Or(po) = pat else {
        // Unreachable given `recognizes` gates every call through `lookup` (DN-136 §2's
        // recognizer/emit pairing) — a defensive, never-silent gap rather than a panic (G2).
        return Err(GapReason::new(
            Category::Other,
            "or_pat: recognizer/emit mismatch (internal invariant violation)",
        ));
    };
    let mut alts = Vec::with_capacity(po.cases.len());
    for c in &po.cases {
        alts.push(map_pattern(c, self_ty)?);
    }
    Ok(alts.join(" | "))
}

pub const ROW: PatternHandler = PatternHandler {
    recognizes,
    emit,
    slug: "M-823",
    citation: "M-823 (or-pattern); DN-136 P1-a migration (moved verbatim from \
               map_pattern_inner's Pat::Or arm)",
};
