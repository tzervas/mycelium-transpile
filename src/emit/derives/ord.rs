//! DN-128 (M-1086) `derive(PartialOrd)`/`derive(Ord)` -> a lexicographic fold over the landed
//! `Ord3` prelude trait (DN-122 §13 / M-1080) — DN-136 Phase-2 (DERIVE-COMPLETION) additive row
//! over the frozen `emit/derives` axis (DN-136 P1-a). DN-128 §2: "PartialOrd/Ord = lexicographic
//! `Ord3` fold" over the "landed `Ord3` (M-1080)" prelude trait.
//!
//! **Recognizes ONLY `"PartialOrd"`, never `"Ord"` — the exact same verified, disclosed choice
//! [`super::eq`] makes for `PartialEq`/`Eq`, for the analogous root cause.** `impl Ord3[T] for T`
//! is keyed **globally** per `(trait, type-head)` (RFC-0019 §4.5 coherence); if this row recognized
//! BOTH `"Ord"` and `"PartialOrd"`, the driver's per-derive-list-entry dispatch would invoke `emit`
//! TWICE for one struct under the common real Rust `#[derive(PartialOrd, Ord)]`, composing the
//! IDENTICAL `impl Ord3[T] for T {...}` text twice — refused by the real `myc-check` oracle as an
//! "overlapping instance" coherence violation (empirically confirmed against a scratch probe
//! during this leaf's development; mirrors [`super::eq`]'s duplicate-function finding for the same
//! underlying reason: no per-row dedup state across driver calls). `PartialOrd` is the reliable
//! signal: Rust's own `Ord: Eq + PartialOrd` supertrait bound means valid Rust source never
//! derives bare `Ord` without `PartialOrd` also present, so a solo `#[derive(Ord)]` (invalid Rust,
//! syntactically representable but never rustc-accepted) falls through to the driver's honest
//! `unrecognized` bucket rather than composing twice.
//!
//! **Unlike [`super::eq`]/[`super::hash`], this row composes the literal `impl Ord3[T] for T`
//! shape** the DN-136 Phase-2 worklist sketches — `Ord3` genuinely IS a landed `PRELUDE_TRAIT_SEEDS`
//! entry (`crates/mycelium-l1/src/checkty.rs:2282`; `crates/mycelium-l1/src/ord3.rs`), so no
//! self-declared trait (and thus no duplicate-declaration risk) is needed — only the coherence-key
//! (not-name) uniqueness above applies, and recognizing a single derive name already avoids it.
//!
//! **The three-way sentinel convention** (`Lt = 0b00000000`, `Eq = 0b00000001`, `Gt =
//! 0b00000010`) mirrors the landed `Ord3` regression fixture's own worked example
//! (`crates/mycelium-l1/src/tests/ord3.rs`'s
//! `ord3_prelude_trait_is_builtin_and_an_instance_checks_with_no_local_declaration`) — `Ord3`
//! itself carries **no fixed law** (its own module doc: "an instance's `cmp` may encode whatever
//! three-way order... the implementer intends", DN-122 §13's explicit YAGNI on a law checker), so
//! this is THIS derive's own, disclosed, self-consistent convention. Composability across a nested
//! derived field's own `Ord3` instance holds **by construction** as long as every participating
//! type's instance was ALSO produced by this same row (a heuristic boundary [`field_derive_kind`]
//! already shares with [`super::show`]/[`super::init`], VR-5).
//!
//! **The ADR-040 Float/NaN refusal** fires FIRST, ahead of the general [`field_derive_kind`]
//! classification, for the identical documented reason [`super::eq`] gives.
//!
//! Guarantee: `Empirical` (live-oracle-verified, `src/tests/emit.rs`); the field-eligibility
//! heuristic stays `Declared` (same VR-5 boundary as every other row in this axis).

use std::collections::BTreeMap;

use super::{
    field_derive_kind, is_seeded_scalar_width, mangle_ty, scalar_binary_width, vec_element,
    zero_bin_literal, DeriveCtx, DeriveHandler, DeriveOutcome, FieldDeriveKind,
};
use crate::gap::{Category, GapReason};

/// The `Ord3.cmp` "equal" sentinel this derive's own convention uses (see the module doc's
/// three-way-sentinel paragraph). Only `EQ` needs to be a named constant — `Lt`/`Gt` are never
/// tested against by generated code (any *non*-`EQ` result short-circuits the fold as-is,
/// regardless of whether it happens to be the `Lt` or `Gt` value), so they stay documentation-only
/// (no unused-constant lint).
const ORD3_EQ: &str = "0b00000001";

fn recognizes(name: &str) -> bool {
    name == "PartialOrd"
}

/// **DN-138 WU-4 — the LEAF `cmp` expression for a value pair of kind `ft`** (a field, or a `Vec`
/// element recursed into by [`vec_ord_aux`]) — mirrors [`super::show::leaf_show_expr`]'s shape for
/// `Ord3`: a `Binary{64}` `ScalarBinary` dispatches the seeded instance directly; a NARROWER width
/// is `width_cast` up to `Binary{64}` first (DN-41, WU-4 unblock). **A width WIDER than 64
/// (`u128`/`i128` -> `Binary{128}`) is an honest, disclosed GAP, never composed** (post-landing
/// review fix — mirrors [`super::show::leaf_show_expr`]'s identical fix): a NARROWING
/// `width_cast` can `EvalError::Overflow` at runtime for a real wide value, which would silently
/// overstate this leaf's scope past DN-138 §6's stated `u8`/`u16`/`u32` widths. `None` also for
/// `Float`/`Deferred`/`VecOf` (depth-1 scope: a `Vec`-of-`Vec` element is not a supported leaf).
fn leaf_cmp_expr(a: &str, b: &str, ft: &str) -> Option<String> {
    match field_derive_kind(ft) {
        FieldDeriveKind::UserNamed | FieldDeriveKind::BytesLike | FieldDeriveKind::BoolLike => {
            Some(format!("cmp({a}, {b})"))
        }
        FieldDeriveKind::ScalarBinary if is_seeded_scalar_width(ft) => {
            Some(format!("cmp({a}, {b})"))
        }
        FieldDeriveKind::ScalarBinary => {
            let w = scalar_binary_width(ft)?;
            if w > 64 {
                return None; // a NARROWING width_cast can overflow at runtime -- honest gap.
            }
            let w64 = zero_bin_literal(64);
            Some(format!(
                "cmp(width_cast({a}, {w64}), width_cast({b}, {w64}))"
            ))
        }
        FieldDeriveKind::Float | FieldDeriveKind::Deferred | FieldDeriveKind::VecOf => None,
    }
}

/// **DN-138 WU-4 — the `Vec[T]`-recursive `Ord3` auxiliary.** Composes a plain top-level
/// `fn ord_vec_<mangled elem>(a: Vec[ELEM], b: Vec[ELEM]) => Binary{8} = …;` — a lexicographic
/// fold mirroring [`compose`]'s own right-to-left field fold, but over a cons-list: `Nil`/`Nil` is
/// EQ, a shorter list is LT a longer one with an equal-prefix, and a `Cons`/`Cons` pair
/// short-circuits on the element [`leaf_cmp_expr`] before recursing on the tails. A plain fn, not
/// `impl Ord3[Vec[ELEM]] for Vec[ELEM]` — the SAME per-file single-instance coherence-collision
/// concern [`super::show::vec_show_aux`]'s doc explains, applied to `Ord3`. **Disclosed residual**
/// (identical to `vec_show_aux`'s): two different structs in one file needing the same
/// `ord_vec_<mangled>` collide as a duplicate-function refusal, out of this leaf's scope.
fn vec_ord_aux(mangled: &str, elem_ft: &str) -> String {
    let elem_expr =
        leaf_cmp_expr("ha", "hb", elem_ft).expect("eligibility already checked by caller");
    format!(
        "fn ord_vec_{mangled}(a: Vec[{elem_ft}], b: Vec[{elem_ft}]) => Binary{{8}} =\n  match a {{ \
         Nil => match b {{ Nil => {ORD3_EQ}, Cons(_, _) => 0b00000000 }}, Cons(ha, ta) => match b \
         {{ Nil => 0b00000010, Cons(hb, tb) => match {elem_expr} {{ {ORD3_EQ} => \
         ord_vec_{mangled}(ta, tb), other => other }} }} }};"
    )
}

/// The precise, honest reason `ft` is ineligible for `derive(PartialOrd)` composition right now
/// (past the ADR-040 `Float` pre-check, which always fires first).
fn ord_ineligible_reason(ft: &str) -> String {
    match field_derive_kind(ft) {
        FieldDeriveKind::ScalarBinary => format!(
            "a scalar WIDER than the seeded `Ord3` instance's `Binary{{64}}` (`{ft}`, e.g. a \
             `u128`/`i128` field) -- a NARROWING `width_cast` down to 64 bits can overflow at \
             runtime for a real wide value, so this leaf leaves it an honest gap rather than \
             compose a call that would `myc-check` clean but THROW at eval time (post-landing \
             review fix, DN-138 section 6's stated scope is u8/u16/u32 only)"
        ),
        FieldDeriveKind::VecOf => format!(
            "a `Vec` field whose element type `{}` has no `Ord3` route of its own (a \
             `Vec`-of-`Vec` or a `Float`/other-bracketed element -- DN-138 section 6, WU-4's \
             disclosed depth-1 scope)",
            vec_element(ft).unwrap_or(ft)
        ),
        FieldDeriveKind::Deferred => {
            format!("`{ft}`, a `Seq`/tuple or other bracketed shape with no `Ord3` instance yet")
        }
        FieldDeriveKind::Float => {
            unreachable!("Float is refused ahead of this classifier by its own ADR-040 pre-check")
        }
        FieldDeriveKind::UserNamed | FieldDeriveKind::BytesLike | FieldDeriveKind::BoolLike => {
            unreachable!("ord_ineligible_reason is only called for an ineligible kind")
        }
    }
}

/// **Fieldless (unit) struct:** `fn cmp(a: T, b: T) => Binary{8} = <EQ sentinel>;` — trivially
/// always equal, always succeeds. **Struct with fields:** a right-to-left short-circuit fold —
/// the LAST field's `cmp` is the base case; each earlier field wraps it in `match cmp(p_i, q_i) {
/// EQ => <inner>, other => other }`, so the first non-equal field decides the whole comparison
/// (lexicographic order, field 0 dominates). Gated per field via the ADR-040 float check (first)
/// then [`field_derive_kind`] (DN-138 §4.5) — refuses the WHOLE derive (never a partial/fabricated
/// order, G2) the moment any field is ineligible. **DN-138 unblock:**
/// `UserNamed`/`BytesLike`/`BoolLike`/`ScalarBinary`-at-`Binary{64}` fields now compose (the
/// seeded `Ord3` instance resolves `cmp(p, q)` — DN-138 §4.1 Alt A); `Deferred`/a wrong-width
/// `ScalarBinary` stay honest gaps (increment 2, DN-138 §6).
fn compose(ty_name: &str, field_types: &[String]) -> Result<String, GapReason> {
    if field_types.is_empty() {
        return Ok(format!(
            "impl Ord3[{ty_name}] for {ty_name} {{\n  fn cmp(a: {ty_name}, b: {ty_name}) => \
             Binary{{8}} =\n    {ORD3_EQ};\n}};"
        ));
    }
    for (i, ft) in field_types.iter().enumerate() {
        if ft == "Float" {
            return Err(GapReason::new(
                Category::DeriveAttr,
                format!(
                    "struct `{ty_name}` derive(PartialOrd): field {i} has type `Float` — a \
                     derived TOTAL order over a float field is refused (ADR-040 §2.4 NaN \
                     semantics: NaN has no order position under IEEE-754 §5.11's partial order, \
                     so a structural three-way `Ord3.cmp` fold cannot honestly claim a total \
                     order here) — the whole derive is left an honest gap rather than a \
                     silently-wrong order (G2)"
                ),
            ));
        }
        let eligible = if field_derive_kind(ft) == FieldDeriveKind::VecOf {
            vec_element(ft).is_some_and(|elem| leaf_cmp_expr("_a", "_b", elem).is_some())
        } else {
            leaf_cmp_expr("_a", "_b", ft).is_some()
        };
        if !eligible {
            return Err(GapReason::new(
                Category::DeriveAttr,
                format!(
                    "struct `{ty_name}` derive(PartialOrd): field {i} has type `{ft}`, {} — the \
                     whole derive is left an honest gap rather than a partial/fabricated order \
                     (G2)",
                    ord_ineligible_reason(ft)
                ),
            ));
        }
    }
    let mut vec_aux: BTreeMap<String, String> = BTreeMap::new();
    for ft in field_types {
        if field_derive_kind(ft) == FieldDeriveKind::VecOf {
            if let Some(elem) = vec_element(ft) {
                vec_aux
                    .entry(mangle_ty(elem))
                    .or_insert_with(|| elem.to_owned());
            }
        }
    }
    let vars_a: Vec<String> = (0..field_types.len()).map(|i| format!("p{i}")).collect();
    let vars_b: Vec<String> = (0..field_types.len()).map(|i| format!("q{i}")).collect();
    let field_expr = |i: usize| -> String {
        let ft = &field_types[i];
        if field_derive_kind(ft) == FieldDeriveKind::VecOf {
            let elem = vec_element(ft).expect("VecOf implies vec_element(ft).is_some()");
            format!("ord_vec_{}({}, {})", mangle_ty(elem), vars_a[i], vars_b[i])
        } else {
            leaf_cmp_expr(&vars_a[i], &vars_b[i], ft).expect("eligibility already checked above")
        }
    };
    let last = field_types.len() - 1;
    let mut body = field_expr(last);
    for i in (0..last).rev() {
        body = format!(
            "match {expr} {{ {ORD3_EQ} => {inner}, other => other }}",
            expr = field_expr(i),
            inner = body
        );
    }
    let mut out = String::new();
    for (mangled, elem_ft) in &vec_aux {
        out.push_str(&vec_ord_aux(mangled, elem_ft));
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "impl Ord3[{ty_name}] for {ty_name} {{\n  fn cmp(a: {ty_name}, b: {ty_name}) => \
         Binary{{8}} =\n    match a {{ {ty_name}({pa}) => match b {{ {ty_name}({pb}) => {body} \
         }} }};\n}};",
        pa = vars_a.join(", "),
        pb = vars_b.join(", ")
    ));
    Ok(out)
}

/// A **generic** struct refuses `derive(PartialOrd)` — a derived instance for a generic type
/// needs DN-130's generic-trait-instance-impl mechanism, out of this leaf's scope. Mirrors
/// [`super::show`]/[`super::init`]/[`super::eq`]'s identical `is_generic` gate.
fn emit(ctx: &DeriveCtx) -> DeriveOutcome {
    if ctx.is_generic {
        return DeriveOutcome::Gap(GapReason::new(
            Category::DeriveAttr,
            format!(
                "struct `{}` derive(PartialOrd): generic struct — a derived `Ord3` instance for \
                 a generic type needs DN-130's generic-trait-instance-impl mechanism, out of \
                 this leaf's scope (DN-128/M-1086, DN-136 Phase-2 DERIVE-COMPLETION)",
                ctx.ty_name
            ),
        ));
    }
    match compose(ctx.ty_name, ctx.field_types) {
        Ok(myc) => DeriveOutcome::Composed(myc),
        Err(g) => DeriveOutcome::Gap(g),
    }
}

pub const ROW: DeriveHandler = DeriveHandler {
    recognizes,
    emit,
    slug: "DN-128 (Phase-2 DERIVE-COMPLETION) — PartialOrd -> lexicographic Ord3 fold",
    citation: "DN-128 §2 (PartialOrd/Ord -> lexicographic Ord3 fold); DN-122 §13/M-1080 (the \
               landed Ord3 prelude trait); ADR-040 §2.4 (Float/NaN refusal); DN-136 Phase-2 \
               bulk-gap-close worklist B2/L2 (recognizes only PartialOrd — verified overlapping-\
               instance collision when Ord+PartialOrd co-occur, same root cause as eq.rs)",
};
