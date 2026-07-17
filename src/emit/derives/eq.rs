//! DN-128 (M-1086) `derive(PartialEq)`/`derive(Eq)` -> a structural field-wise equality fold —
//! DN-136 Phase-2 (DERIVE-COMPLETION) additive row over the frozen `emit/derives` axis (DN-136
//! P1-a). Composes real, `myc check`-clean equality code over the field-wise `cmp.eq` prim
//! (RFC-0032 D1) DN-128 §2 cites: "PartialEq/Eq = field-wise `cmp.eq` ∧-fold" over "landed
//! `cmp.eq`/`bytes.eq` prims".
//!
//! **Recognizes ONLY `"PartialEq"`, never `"Eq"` — a verified, disclosed, deliberate choice
//! (mitigation #14 verify-first).** `derives::lookup` is consulted independently PER derive-list
//! ENTRY by the driver (`lower_struct_derives`), so a struct written as the extremely common real
//! Rust `#[derive(PartialEq, Eq)]` would, if this row recognized BOTH names, have `emit` invoked
//! TWICE for the SAME struct — composing the identical `fn eq_<T>(...)` text twice, which the real
//! `myc-check` oracle refuses with `"duplicate function"` (empirically confirmed against a scratch
//! probe during this leaf's development; the analogous `impl Ord3[T] for T` shape in
//! [`super::ord`] would instead trip RFC-0019 §4.5's instance-uniqueness "overlapping instance"
//! refusal — same root cause, same fix). Recognizing only `"PartialEq"` sidesteps the collision by
//! construction: Rust's own `Eq: PartialEq` supertrait bound means valid Rust source never derives
//! bare `Eq` without `PartialEq` also present (in the same or a sibling `#[derive(...)]`), so
//! `PartialEq` is the reliable, always-co-occurring signal — a solo `#[derive(Eq)]` (invalid Rust,
//! syntactically representable but never emitted by rustc-accepted source) falls through to the
//! driver's honest `unrecognized` bucket rather than composing twice.
//!
//! **Composes a plain top-level `fn eq_<T>(a: T, b: T) => Binary{1} = ...;`, NOT
//! `impl Eq[T] for T` — a second disclosed, verified deviation from the naive DN-128-worklist
//! sketch.** Unlike [`super::show`]/[`super::init`] (whose `Show`/`Init` targets are landed
//! `PRELUDE_TRAIT_SEEDS` — `crates/mycelium-l1/src/checkty.rs:2282`), there is **no landed `Eq`
//! prelude trait** — only `Fuse`/`Ord3`/`Show`/`Init`/`Fault` are seeded. Composing
//! `impl Eq[T] for T` would therefore need this row to ALSO self-declare `trait Eq[T]{ fn
//! eq(a:T,b:T) => Binary{1}; }` inline in the emitted text (naming the method `eq` was tried first
//! and rejected: it SHADOWS the bare-call `eq` prim for every `eq(...)` call in the whole file,
//! including the ones between primitive-typed inner fields — confirmed empirically: `myc-check`
//! then refuses the inner field comparison with `"no instance Eq for Binary{8}"`; `equal`, the
//! `trait Eq<A> { fn equal(...) }` spelling RFC-0007 §4.4/RFC-0019 §3.1 already use as their
//! illustrative example, avoids the prim-name collision). But self-declaring the trait per-impl
//! ALSO fails the moment a SECOND struct in the same file derives `PartialEq` too (a real, common
//! shape — the landed `derive_composes_end_to_end_over_a_same_file_nested_derived_field` test
//! already exercises multi-struct-per-file derive composition for `Show`/`Init`): `myc-check`
//! refuses the second `trait Eq[T]{...}` with `"duplicate trait declaration"` (confirmed
//! empirically). `lower_struct_derives` calls a row's `emit` once per struct with no cross-call
//! state (a pure `fn(&DeriveCtx) -> DeriveOutcome`), so this row cannot deduplicate a shared
//! trait-decl preamble across multiple derive sites without driver changes — out of this leaf's
//! scope (DN-136 §7's "the driver's per-derive orchestration is NOT touched by a row" invariant).
//! A plain, deterministically-named top-level fn sidesteps all of this: no trait to (re)declare,
//! and the deterministic `eq_<FieldType>` name lets a NESTED eligible field's own derived
//! comparator resolve BY CONSTRUCTION — mirroring [`super::show`]'s `render(field)` compositional
//! call, without needing trait-based dispatch at all.
//!
//! **The ADR-040 Float/NaN refusal** fires FIRST, ahead of the general
//! [`field_derive_kind`] classification (which also excludes `Float`, so this is currently
//! redundant in practice but kept as its own explicit, clearly-worded check per the DN-136 Phase-2 worklist's
//! L1 spec — "not just ineligible-repr fields" — so the emitted [`GapReason`] cites the REAL
//! (NaN/ADR-040) reason for a float field, not the generic no-ambient-instance one).
//!
//! Guarantee: `Empirical` (every emitted shape above is live-oracle-verified against the real
//! `myc-check` toolchain, `src/tests/emit.rs`'s `derive_forms_check_clean_against_real_toolchain`);
//! the field-eligibility heuristic itself stays `Declared` (same VR-5 boundary
//! [`super::show`]/[`super::init`] already carry — a nested field's OWN derive is not verified to
//! have actually succeeded, only that its type NAME has the right shape).

use std::collections::BTreeMap;

use super::{
    field_derive_kind, mangle_ty, vec_element, DeriveCtx, DeriveHandler, DeriveOutcome,
    FieldDeriveKind,
};
use crate::gap::{Category, GapReason};

fn recognizes(name: &str) -> bool {
    name == "PartialEq"
}

/// **DN-138 WU-4 — the `Vec[T]`-recursive `PartialEq` auxiliary.** Composes a plain top-level
/// `fn eq_vec_<mangled elem>(a: Vec[ELEM], b: Vec[ELEM]) => Binary{1} = …;` that structurally
/// recurses over BOTH cons-lists in lockstep — `Nil`/`Nil` equal, a length mismatch (`Nil` vs
/// `Cons`) unequal, and a `Cons`/`Cons` pair `and`-folds the element comparison ([`field_eq_expr`],
/// recursively reused for the element itself) with the recursive tail comparison. A plain fn, not
/// an `impl`/trait — `PartialEq` already has no landed `Eq` prelude trait to dispatch through (see
/// this file's own module doc), so this mirrors the SAME shape the rest of this row already uses,
/// now applied one level deeper. Named per DISTINCT element shape ([`mangle_ty`]), composed at most
/// once per struct even if several fields share an element type (the caller dedups). **Disclosed
/// residual (mirrors this row's own top-level doc):** two DIFFERENT structs in the SAME file
/// needing the SAME `eq_vec_<mangled>` would collide as a duplicate-function `myc-check` refusal —
/// out of this leaf's scope without cross-struct driver state.
fn vec_eq_aux(mangled: &str, elem_ft: &str) -> String {
    let elem_expr =
        field_eq_expr("ha", "hb", elem_ft).expect("eligibility already checked by caller");
    format!(
        "fn eq_vec_{mangled}(a: Vec[{elem_ft}], b: Vec[{elem_ft}]) => Binary{{1}} =\n  match a {{ \
         Nil => match b {{ Nil => 0b1, Cons(_, _) => 0b0 }}, Cons(ha, ta) => match b {{ Nil => \
         0b0, Cons(hb, tb) => and({elem_expr}, eq_vec_{mangled}(ta, tb)) }} }};"
    )
}

/// The deterministic top-level fn name this row's compose emits/expects for a given type name —
/// shared between a struct's OWN emission and a nested eligible field's compositional call (no
/// cross-call state needed; both derive from `ty_name`/`field_type` alone). Used ONLY for
/// [`FieldDeriveKind::UserNamed`] fields — a primitive field routes to a PRIM instead (DN-138 §3's
/// heterogeneity finding: `eq_Binary{8}` is not even a legal fn name), see [`field_eq_expr`].
fn eq_fn_name(ty_name: &str) -> String {
    format!("eq_{ty_name}")
}

/// **DN-138 §4.5 — the PRIM-ROUTED half of the heterogeneity finding.** `PartialEq` never
/// dispatches a primitive field through a trait instance (there is no landed `Eq` prelude trait,
/// and `eq_Binary{8}` is not a legal fn name — DN-138 §3); it composes a direct call to the
/// already-landed prim for that field's kind. Returns the field's `Binary{1}`-typed comparison
/// expression, or `None` for an ineligible kind (`Float` is pre-checked by the caller; `Deferred`
/// returns `None` here).
///
/// - `UserNamed` -> `eq_<Type>(a, b)` (unchanged — the nested-derive compositional call).
/// - `ScalarBinary` (ANY width, not just `Binary{64}`) -> the bare `eq` prim directly: `eq`/`lt`
///   are width-generic over any concrete `Binary{N}` (RFC-0032 D1), so — unlike `Show`/`Init`/
///   `Ord3`'s seeded-instance dispatch — `PartialEq` has NO width restriction at all.
/// - `BytesLike` -> `bytes_eq(a, b)` (the M-912 prim, already `Binary{1}`-typed).
/// - `BoolLike` -> an INLINE `match` (not a call): there is no width-generic prim over `Bool`
///   (only `bool_eq` in `lib/std/cmp.myc`, which is NOT ambiently available and returns `Bool`,
///   not `Binary{1}` — the wrong type for this row's `and`-fold), and generating a named
///   `fn eq_Bool` here risks the exact duplicate-fn hazard this row's own module doc documents the
///   moment a SECOND struct in the same file also derives `PartialEq` over a `Bool` field. An
///   inlined match is self-contained, needs no shared name, and is exactly `Binary{1}`-typed.
/// - `VecOf` (DN-138 WU-4) -> `eq_vec_<mangled elem>(a, b)`, a per-element-type recursive
///   auxiliary ([`vec_eq_aux`]) — only when the element itself has an eq route (depth-1 scope: a
///   `Vec`-of-`Vec` element has none, an honest disclosed gap).
fn field_eq_expr(a: &str, b: &str, ft: &str) -> Option<String> {
    match field_derive_kind(ft) {
        FieldDeriveKind::UserNamed => Some(format!("{}({a}, {b})", eq_fn_name(ft))),
        FieldDeriveKind::ScalarBinary => Some(format!("eq({a}, {b})")),
        FieldDeriveKind::BytesLike => Some(format!("bytes_eq({a}, {b})")),
        FieldDeriveKind::BoolLike => Some(format!(
            "match {a} {{ True => match {b} {{ True => 0b1, False => 0b0 }}, False => match {b} \
             {{ True => 0b0, False => 0b1 }} }}"
        )),
        FieldDeriveKind::VecOf => {
            let elem = vec_element(ft)?;
            // Depth-1 scope: only compose if the ELEMENT itself has its own eq route.
            field_eq_expr("_a", "_b", elem)?;
            Some(format!("eq_vec_{}({a}, {b})", mangle_ty(elem)))
        }
        FieldDeriveKind::Float | FieldDeriveKind::Deferred => None,
    }
}

/// Left-fold `parts` into a single `and(...)` chain — mirrors [`super::show`]'s
/// `bytes_concat_chain` shape, folding with the width-preserving `and` prim (`Binary{1} x
/// Binary{1} -> Binary{1}`, RFC-0032 D2) instead of `bytes_concat`. `parts` is never empty in the
/// caller below (the fieldless case is handled separately, without this helper).
fn and_chain(parts: &[String]) -> String {
    let mut iter = parts.iter();
    let mut acc = iter.next().cloned().unwrap_or_default();
    for p in iter {
        acc = format!("and({acc}, {p})");
    }
    acc
}

/// **Fieldless (unit) struct:** `fn eq_T(a: T, b: T) => Binary{1} = 0b1;` — always equal, always
/// succeeds (live-oracle-proven, `src/tests/emit.rs`). **Struct with fields:** an `and`-fold of
/// each field's comparison expression (routed per [`field_eq_expr`] — DN-138 §4.5), gated per
/// field via the ADR-040 float check (first) then [`field_derive_kind`] — refuses the WHOLE
/// derive (never a partial/fabricated equality, G2) the moment any field is ineligible. **DN-138
/// unblock:** `UserNamed`/`ScalarBinary` (any width)/`BytesLike`/`BoolLike` fields now compose
/// (routed to `eq_<Type>`/`eq`/`bytes_eq`/an inline match respectively — never a seeded instance,
/// per DN-138 §3's heterogeneity finding); only `Deferred` (`Vec`/tuple, increment 2) still gaps.
fn compose(ty_name: &str, field_types: &[String]) -> Result<String, GapReason> {
    let fname = eq_fn_name(ty_name);
    if field_types.is_empty() {
        return Ok(format!(
            "fn {fname}(a: {ty_name}, b: {ty_name}) => Binary{{1}} =\n    0b1;"
        ));
    }
    for (i, ft) in field_types.iter().enumerate() {
        if ft == "Float" {
            return Err(GapReason::new(
                Category::DeriveAttr,
                format!(
                    "struct `{ty_name}` derive(PartialEq): field {i} has type `Float` — a \
                     derived TOTAL equality over a float field is refused (ADR-040 §2.4 NaN \
                     semantics: NaN != NaN under IEEE-754, so a structural `and`-fold cannot \
                     honestly claim total equality here — matching Rust's own `derive(Eq)` \
                     refusal for `f64`) — the whole derive is left an honest gap rather than a \
                     silently-wrong equality (G2)"
                ),
            ));
        }
        if field_eq_expr("p", "q", ft).is_none() {
            let why = if field_derive_kind(ft) == FieldDeriveKind::VecOf {
                format!(
                    "a `Vec` field whose element type `{}` has no equality route of its own (a \
                     `Vec`-of-`Vec` or a `Float`/other-bracketed element -- DN-138 section 6, \
                     WU-4's disclosed depth-1 scope)",
                    vec_element(ft).unwrap_or(ft)
                )
            } else {
                "a `Seq`/tuple or other bracketed shape with no derived (or hand-written) \
                 structural-equality route yet"
                    .to_owned()
            };
            return Err(GapReason::new(
                Category::DeriveAttr,
                format!(
                    "struct `{ty_name}` derive(PartialEq): field {i} has type `{ft}`, {why} — \
                     the whole derive is left an honest gap rather than a partial/fabricated \
                     equality (G2)"
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
    let parts: Vec<String> = field_types
        .iter()
        .enumerate()
        .map(|(i, ft)| {
            field_eq_expr(&vars_a[i], &vars_b[i], ft).expect("eligibility already checked above")
        })
        .collect();
    let body = and_chain(&parts);
    let mut out = String::new();
    for (mangled, elem_ft) in &vec_aux {
        out.push_str(&vec_eq_aux(mangled, elem_ft));
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "fn {fname}(a: {ty_name}, b: {ty_name}) => Binary{{1}} =\n    match a {{ {ty_name}({pa}) \
         => match b {{ {ty_name}({pb}) => {body} }} }};",
        pa = vars_a.join(", "),
        pb = vars_b.join(", ")
    ));
    Ok(out)
}

/// A **generic** struct refuses `derive(PartialEq)` — a derived fn for a generic type needs
/// DN-130's generic-instance mechanism, out of this leaf's scope. Mirrors
/// [`super::show`]/[`super::init`]'s identical `is_generic` gate.
fn emit(ctx: &DeriveCtx) -> DeriveOutcome {
    if ctx.is_generic {
        return DeriveOutcome::Gap(GapReason::new(
            Category::DeriveAttr,
            format!(
                "struct `{}` derive(PartialEq): generic struct — a derived equality fn for a \
                 generic type needs DN-130's generic-instance mechanism, out of this leaf's scope \
                 (DN-128/M-1086, DN-136 Phase-2 DERIVE-COMPLETION)",
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
    slug: "DN-128 (Phase-2 DERIVE-COMPLETION) — PartialEq -> structural `and`-fold over cmp.eq",
    citation: "DN-128 §2 (PartialEq/Eq -> field-wise cmp.eq fold); ADR-040 §2.4 (Float/NaN \
               refusal); RFC-0007 §4.4 / RFC-0019 §3.1 (`equal` as the collision-free method-name \
               precedent); DN-136 Phase-2 bulk-gap-close worklist B1/L1 (disclosed deviation: a \
               plain fn, not `impl Eq[T] for T` — no landed Eq prelude trait; verified \
               duplicate-trait/duplicate-fn collision when Eq+PartialEq or two structs co-occur)",
};
