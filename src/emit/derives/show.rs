//! DN-128 (M-1086) `derive(Debug)` -> an explicit `impl Show[T] for T` — DN-136/P1-a row. Moved
//! verbatim (no behavior change) from `lower_struct_derives`'s `"Debug"` arms + the former
//! free-standing `derive_show_impl` helper.

use std::collections::BTreeMap;

use super::{
    field_derive_kind, is_seeded_scalar_width, mangle_ty, scalar_binary_width, vec_element,
    zero_bin_literal, DeriveCtx, DeriveHandler, DeriveOutcome, FieldDeriveKind,
};
use crate::gap::{Category, GapReason};

fn recognizes(name: &str) -> bool {
    name == "Debug"
}

/// **DN-138 WU-4 — the LEAF `render` expression for a value of kind `ft`** (a field, or a `Vec`
/// element recursed into by [`vec_show_aux`]): `UserNamed`/`BytesLike`/`BoolLike` and a
/// `ScalarBinary` AT OR NARROWER THAN `Binary{64}` all resolve — a `Binary{64}` field dispatches
/// the seeded `Show` instance directly; a NARROWER width is `width_cast` up to `Binary{64}` first
/// (DN-41; DN-138 §6 increment-2 unblock). **A width WIDER than 64 (`u128`/`i128` map to
/// `Binary{128}`, `map.rs`) is an honest, disclosed GAP, never composed** (post-landing review
/// fix): `width_cast(v, Binary{64})` for a `Binary{128}` value is a NARROWING cast, and
/// `width_cast`'s own checked-narrow contract REFUSES at runtime (`EvalError::Overflow`) for any
/// value `>= 2^64` — composing it unconditionally would let a derived `Debug` (a Rust operation
/// that never panics) `myc-check`-clean but THROW at eval time for a real `u128` value, silently
/// overstating this leaf's scope beyond DN-138 §6's stated `u8`/`u16`/`u32` widths. `None` also for
/// `Float`/`Deferred`/`VecOf` (a `Vec`-of-`Vec` element is NOT a supported leaf — DN-138 WU-4's
/// disclosed depth-1 scope).
fn leaf_show_expr(v: &str, ft: &str) -> Option<String> {
    match field_derive_kind(ft) {
        FieldDeriveKind::UserNamed | FieldDeriveKind::BytesLike | FieldDeriveKind::BoolLike => {
            Some(format!("render({v})"))
        }
        FieldDeriveKind::ScalarBinary if is_seeded_scalar_width(ft) => Some(format!("render({v})")),
        FieldDeriveKind::ScalarBinary => {
            let w = scalar_binary_width(ft)?;
            if w > 64 {
                return None; // a NARROWING width_cast can overflow at runtime -- honest gap.
            }
            Some(format!("render(width_cast({v}, {}))", zero_bin_literal(64)))
        }
        FieldDeriveKind::Float | FieldDeriveKind::Deferred | FieldDeriveKind::VecOf => None,
    }
}

/// **DN-138 WU-4 — the `Vec[T]`-recursive `Show` auxiliary.** Composes a plain top-level
/// `fn show_vec_<mangled elem>(xs: Vec[ELEM]) => Bytes = …;` that structurally recurses over the
/// cons-list, rendering each element via [`leaf_show_expr`] — a `Cons`-chain textual form
/// (`"Cons(e0, Cons(e1, Nil))"`) rather than a bracket-list, a deliberate KISS choice: it names the
/// ACTUAL underlying repr honestly (DN-99's `Vec[A] = Nil | Cons(A, Vec[A])`) with a single
/// recursive fn and no nested-pattern reliance (`Cons(h, Nil)` is never matched — only single-level
/// `Nil`/`Cons(_, _)` patterns, which are the only shape empirically confirmed to parse/check
/// against the real oracle in this leaf's development). **A plain fn, not `impl Show[Vec[ELEM]] for
/// Vec[ELEM]`** — DN-138's own coherence key is per type-HEAD (`type_head(Vec[_]) == "Data:Vec"`
/// regardless of `ELEM`), so a SECOND field of a DIFFERENT element type in the same struct would
/// collide on that ONE global instance slot; a plain, element-mangled-named fn has no coherence key
/// at all, so multiple DIFFERENT `Vec[ELEM]` fields (or the SAME `ELEM` on a second field) compose
/// side-by-side with zero collision risk — mirrors [`super::eq`]/[`super::hash`]'s own identical,
/// already-disclosed "plain fn, not a trait impl" deviation, for the analogous reason.
/// **Disclosed residual (mirrors `eq.rs`'s own identical one):** if TWO DIFFERENT top-level structs
/// in the SAME transpiled file each need `show_vec_<SAME mangled elem>`, both would compose the
/// IDENTICAL fn text — a real `myc-check` duplicate-function refusal this row cannot deduplicate
/// without cross-struct driver state (out of this leaf's scope, DN-136 §7's driver-untouched
/// invariant; `lower_struct_derives` calls a row's `emit` once per struct with no cross-call state).
fn vec_show_aux(mangled: &str, elem_ft: &str) -> String {
    let elem_expr = leaf_show_expr("h", elem_ft).expect("eligibility already checked by caller");
    format!(
        "fn show_vec_{mangled}(xs: Vec[{elem_ft}]) => Bytes =\n  match xs {{ Nil => \"Nil\", Cons(h, t) => \
         bytes_concat(\"Cons(\", bytes_concat({elem_expr}, bytes_concat(\", \", bytes_concat(show_vec_{mangled}(t), \")\")))) }};"
    )
}

/// Left-fold `parts` into a single `bytes_concat(...)` chain — every step stays `Bytes`-typed,
/// matching `bytes_concat`'s 2-ary `Bytes -> Bytes -> Bytes` signature (`lib/std/fmt.myc`'s
/// `to_dec` uses the identical fold shape for its recursive digit accumulation). `parts` is never
/// empty in the caller below (the fieldless case is handled separately, without this helper).
/// Moved verbatim from the former `emit.rs::bytes_concat_chain` (used only here).
fn bytes_concat_chain(parts: &[String]) -> String {
    let mut iter = parts.iter();
    let mut acc = iter.next().cloned().unwrap_or_default();
    for p in iter {
        acc = format!("bytes_concat({acc}, {p})");
    }
    acc
}

/// **Fieldless (unit) struct:** `fn render(x: T) => Bytes = "T";` — always succeeds, no field
/// dependency (live-oracle-proven, `src/tests/emit.rs`). **Struct with fields:** a left-to-right
/// `bytes_concat` fold of `"T(", render(f0), ", ", render(f1), …, ")"`, gated per field via
/// [`field_derive_kind`] (DN-138 §4.5) — refuses the WHOLE derive (never a partial/fabricated
/// render, G2) the moment any field is ineligible, citing that field's index + mapped type + the
/// real reason. **DN-138 unblock:** `UserNamed`/`BytesLike`/`BoolLike`/`ScalarBinary` (any width,
/// via `width_cast` for narrower/wider — WU-4) fields now compose (the seeded `Show` instance
/// resolves `render(field)` — DN-138 §4.1 Alt A); **`Vec[T]` fields now compose too** (WU-4),
/// routed through a per-element-type auxiliary `show_vec_<mangled>` fn ([`vec_show_aux`]) rather
/// than trait dispatch (`Vec`'s coherence head admits only one instance per file — see
/// [`vec_show_aux`]'s doc). `Float`/`Deferred`/a `Vec`-of-ineligible-element stay honest gaps
/// (DN-138 §6).
fn compose(ty_name: &str, field_types: &[String]) -> Result<String, GapReason> {
    if field_types.is_empty() {
        return Ok(format!(
            "impl Show[{ty_name}] for {ty_name} {{\n  fn render(x: {ty_name}) => Bytes =\n    \"{ty_name}\";\n}};"
        ));
    }
    // (mangled elem name -> elem's own mapped-type text) -- a distinct aux fn is composed EXACTLY
    // once per element shape actually needed by THIS struct, even if several fields share it.
    let mut vec_aux: BTreeMap<String, String> = BTreeMap::new();
    let mut exprs: Vec<String> = Vec::with_capacity(field_types.len());
    for (i, ft) in field_types.iter().enumerate() {
        let v = format!("p{i}");
        let expr = if field_derive_kind(ft) == FieldDeriveKind::VecOf {
            let elem = vec_element(ft).expect("VecOf implies vec_element(ft).is_some()");
            leaf_show_expr("_unused", elem).map(|_| {
                vec_aux
                    .entry(mangle_ty(elem))
                    .or_insert_with(|| elem.to_owned());
                format!("show_vec_{}({v})", mangle_ty(elem))
            })
        } else {
            leaf_show_expr(&v, ft)
        };
        let Some(expr) = expr else {
            let why = if field_derive_kind(ft) == FieldDeriveKind::VecOf {
                format!(
                    "a `Vec` field whose element type `{}` has no `Show` route of its own (a \
                     `Vec`-of-`Vec` or a `Float`/other-bracketed element -- DN-138 section 6, \
                     WU-4's disclosed depth-1 scope)",
                    vec_element(ft).unwrap_or(ft)
                )
            } else if field_derive_kind(ft) == FieldDeriveKind::ScalarBinary {
                format!(
                    "a scalar WIDER than the seeded `Show` instance's `Binary{{64}}` (`{ft}`, e.g. \
                     a `u128`/`i128` field) -- a NARROWING `width_cast` down to 64 bits can \
                     overflow at runtime for a real wide value, so this leaf leaves it an honest \
                     gap rather than compose a call that would `myc-check` clean but THROW at \
                     eval time (post-landing review fix, DN-138 section 6's stated scope is \
                     u8/u16/u32 only)"
                )
            } else {
                "a primitive repr (or `Seq`/tuple/other bracketed shape) with no ambient `Show` \
                 instance in this file (`lib/std/fmt.myc`'s primitive impls live in a separate, \
                 unimported nodule)"
                    .to_owned()
            };
            return Err(GapReason::new(
                Category::DeriveAttr,
                format!(
                    "struct `{ty_name}` derive(Debug): field {i} has type `{ft}`, {why} — the \
                     whole derive is left an honest gap rather than a partial/fabricated render \
                     (G2)"
                ),
            ));
        };
        exprs.push(expr);
    }
    let mut parts = vec![format!("\"{ty_name}(\"")];
    for (i, expr) in exprs.iter().enumerate() {
        if i > 0 {
            parts.push("\", \"".to_string());
        }
        parts.push(expr.clone());
    }
    parts.push("\")\"".to_string());
    let body = bytes_concat_chain(&parts);
    let vars: Vec<String> = (0..field_types.len()).map(|i| format!("p{i}")).collect();
    let mut out = String::new();
    for (mangled, elem_ft) in &vec_aux {
        out.push_str(&vec_show_aux(mangled, elem_ft));
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "impl Show[{ty_name}] for {ty_name} {{\n  fn render(x: {ty_name}) => Bytes =\n    match x {{ {ty_name}({pats}) => {body} }};\n}};",
        pats = vars.join(", ")
    ));
    Ok(out)
}

/// A **generic** struct refuses `derive(Debug)` — a derived impl for a generic type needs
/// DN-130's generic-trait-instance-impl mechanism, out of this leaf's scope. Moved verbatim from
/// `lower_struct_derives`'s `"Debug" if is_generic` arm.
fn emit(ctx: &DeriveCtx) -> DeriveOutcome {
    if ctx.is_generic {
        return DeriveOutcome::Gap(GapReason::new(
            Category::DeriveAttr,
            format!(
                "struct `{}` derive(Debug): generic struct — a derived impl for a \
                 generic type needs DN-130's generic-trait-instance-impl mechanism, out of \
                 this leaf's scope (DN-128/M-1086)",
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
    slug: "DN-128/M-1086 (Debug -> Show)",
    citation: "DN-128 (M-1086); DN-127/M-1090 (prelude Show trait); DN-136 P1-a migration (moved \
               verbatim from lower_struct_derives's Debug arms + derive_show_impl)",
};
