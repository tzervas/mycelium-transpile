//! M-826 (tuple-pattern, len >= 2) — DN-136/P1-a row. Moved verbatim (no behavior change) from
//! `map_pattern_inner`'s `Pat::Tuple` arm. A tuple pattern with 0 or 1 elements is deliberately
//! NOT recognized by this row (matches the pre-refactor gate exactly) — it falls through to the
//! driver's final "unsupported match pattern form" gap, unchanged.

use super::PatternHandler;
use crate::emit::map_pattern;
use crate::gap::{Category, GapReason};
use syn::Pat;

fn recognizes(pat: &Pat) -> bool {
    matches!(pat, Pat::Tuple(pt) if pt.elems.len() >= 2)
}

fn emit(pat: &Pat, self_ty: Option<&str>) -> Result<String, GapReason> {
    let Pat::Tuple(pt) = pat else {
        // Unreachable given `recognizes` gates every call through `lookup` — a defensive,
        // never-silent gap rather than a panic (G2).
        return Err(GapReason::new(
            Category::Other,
            "tuple_pat: recognizer/emit mismatch (internal invariant violation)",
        ));
    };
    let mut elems = Vec::with_capacity(pt.elems.len());
    for e in &pt.elems {
        elems.push(map_pattern(e, self_ty)?);
    }
    Ok(format!("({})", elems.join(", ")))
}

pub const ROW: PatternHandler = PatternHandler {
    recognizes,
    emit,
    slug: "M-826",
    citation: "M-826 (tuple-pattern, len >= 2); DN-136 P1-a migration (moved verbatim from \
               map_pattern_inner's Pat::Tuple arm)",
};
