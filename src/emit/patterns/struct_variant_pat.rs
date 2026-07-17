//! M-1089/DN-132 (struct-variant pattern P1) — DN-136/P1-a row. Moved verbatim (no behavior
//! change) from `map_pattern_inner`'s `Pat::Struct` arm + the (former, free-standing)
//! `map_struct_pattern` helper.
//!
//! **Consumes the shared `struct_layouts` interface (DN-136 §4.1) via
//! [`crate::emit::struct_layout`] — never its own layout map** (DN-134's collision-safe
//! population is owned by `transpile.rs`, read-only here).

use super::PatternHandler;
use crate::emit::{map_pattern, struct_layout};
use crate::gap::{Category, GapReason};
use std::collections::HashSet;
use syn::Pat;

fn recognizes(pat: &Pat) -> bool {
    matches!(pat, Pat::Struct(ps) if ps.qself.is_none())
}

/// DN-132 §5.2 — the `Pat::Struct` lowering: resolve the pattern's constructor name (the path's
/// last segment, ignoring any `Self::`/enum-qualifying prefix — the identical convention
/// `map_pattern_inner`'s `Pat::Path`/`Pat::TupleStruct` arms use), fetch its
/// [`crate::emit::struct_layout`], resolve each named field to its declaration index, and emit a
/// positional `Ctor(subs...)` with a wildcard `_` at every unmentioned index.
///
/// **Never-silent gaps (VR-5/G2, DN-132 §5.2/§7):**
/// - No confirmed layout for the ctor name (`struct_layout` returns `None`) — a foreign/
///   unresolved type, a cross-nodule variant not present in this file, or an ambiguous same-name
///   struct/variant collision refused at the population (DN-134 §3 step 1(b)).
/// - A field member that is **positional** (`Foo { 0: a }`, syntactically legal on a `Pat::Struct`
///   even though every DN-132 P1 target is `Fields::Named`) — out of this cluster's scope, gapped.
/// - A field **name not present** in the resolved layout — never silently dropped or wildcarded
///   (OQ-5's field-order canonicalization only reorders *known* names; an unknown name is refused).
/// - A **duplicate** field name within one pattern (defensive: `syn` does not itself reject this,
///   unlike `rustc`) — refused rather than resolved to a guessed/last-wins index (OQ-4c).
///
/// **`..`-rest arity (OQ-4):** every index the pattern does not name is a wildcard `_`, regardless
/// of whether the pattern actually carries `..` — DN-132 §5.2 point 4: `rustc` already requires
/// `..` for a genuinely partial pattern, so by the time syntactically-valid Rust reaches here an
/// absent `..` never leaves a real field unmentioned; the lowered positional form is identical
/// either way, so `ps.rest` is deliberately not inspected.
///
/// **Field-order canonicalization (OQ-5, inherited from DN-123 OQ-1):** sub-patterns are placed at
/// their **declaration** index regardless of the order they appear in the source pattern, so
/// `Foo { y, x }` and `Foo { x, y }` emit identically.
///
/// **DN-104 seal (OQ-3(a)):** this lowering constructs nothing — it emits the same positional
/// `Ctor` pattern any other pattern-position match already does, so a sealed (`priv Mk`)
/// constructor's pattern-position matching stays allowed, unchanged from DN-104's semantics.
///
/// **`Self { .. }` resolution:** a bare (unqualified) `Self` path resolves to `self_ty` — the
/// pattern-side counterpart of `crate::emit`'s `known_struct_literal_ty` expression-side
/// `Self { .. }` resolution, gated identically (a resolvable name only, never guessed). A
/// **qualified** `Self::Variant { .. }` is untouched by this (the path's last segment is already
/// `Variant`, exactly the convention `Pat::Path`/`Pat::TupleStruct` use) — only the
/// single-segment bare form needs it.
fn emit(pat: &Pat, self_ty: Option<&str>) -> Result<String, GapReason> {
    let Pat::Struct(ps) = pat else {
        // Unreachable given `recognizes` gates every call through `lookup` — a defensive,
        // never-silent gap rather than a panic (G2).
        return Err(GapReason::new(
            Category::Other,
            "struct_variant_pat: recognizer/emit mismatch (internal invariant violation)",
        ));
    };
    let seg = ps
        .path
        .segments
        .last()
        .ok_or_else(|| GapReason::new(Category::Other, "empty struct pattern path"))?;
    let raw = seg.ident.to_string();
    let name = if raw == "Self" {
        self_ty
            .ok_or_else(|| {
                GapReason::new(
                    Category::Other,
                    "struct pattern `Self { .. }` used where the enclosing type is not known \
                     (outside an `impl` body, or the impl's own `Self` type could not be \
                     resolved) -- `Self` is never guessed (VR-5/G2)",
                )
            })?
            .to_string()
    } else {
        raw
    };
    let name = crate::reserved::valid_ident(&name).text;
    let layout = struct_layout(&name).ok_or_else(|| {
        GapReason::new(
            Category::Other,
            format!(
                "struct-variant pattern `{name} {{ .. }}` names a constructor with no confirmed \
                 in-file layout -- resolved only for an emitted, in-file struct (or, once \
                 DN-132's SS5.1 variant-aware `StructLayout` population lands, an in-file enum's \
                 named-field variant); never a guessed field-index arity (DN-132 SS5.1/OQ-4, \
                 VR-5/G2)"
            ),
        )
    })?;
    let mut subs: Vec<Option<String>> = vec![None; layout.len()];
    let mut seen_names: HashSet<String> = HashSet::new();
    for f in &ps.fields {
        let fname = match &f.member {
            syn::Member::Named(ident) => ident.to_string(),
            syn::Member::Unnamed(_) => {
                return Err(GapReason::new(
                    Category::Other,
                    format!(
                        "struct pattern `{name} {{ .. }}` uses a positional field-index member \
                         (`N: pat`) -- only named-field struct-pattern members are in DN-132 P1's \
                         scope"
                    ),
                ));
            }
        };
        if !seen_names.insert(fname.clone()) {
            return Err(GapReason::new(
                Category::Other,
                format!(
                    "struct pattern `{name} {{ .. }}` names field `{fname}` more than once -- a \
                     duplicate field-pattern binding is never resolved to a guessed index \
                     (DN-132 OQ-4c, VR-5/G2)"
                ),
            ));
        }
        let idx = layout
            .iter()
            .position(|slot| slot.as_deref() == Some(fname.as_str()))
            .ok_or_else(|| {
                GapReason::new(
                    Category::Other,
                    format!(
                        "struct pattern `{name} {{ .. }}` names field `{fname}`, which is not a \
                         declared field of `{name}`'s confirmed in-file layout (DN-132 OQ-5)"
                    ),
                )
            })?;
        subs[idx] = Some(map_pattern(&f.pat, self_ty)?);
    }
    let positional: Vec<String> = subs
        .into_iter()
        .map(|s| s.unwrap_or_else(|| "_".to_string()))
        .collect();
    Ok(format!("{}({})", name, positional.join(", ")))
}

pub const ROW: PatternHandler = PatternHandler {
    recognizes,
    emit,
    slug: "M-1089/DN-132",
    citation: "M-1089/DN-132 (struct-variant pattern P1); DN-134 (collision-safe \
               struct_layouts); DN-136 P1-a migration (moved verbatim from the former \
               map_struct_pattern)",
};
