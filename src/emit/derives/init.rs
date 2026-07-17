//! DN-128 (M-1086) `derive(Default)` -> an explicit `impl Init[T] for T` — DN-136/P1-a row. Moved
//! verbatim (no behavior change) from `lower_struct_derives`'s `"Default"` arms + the former
//! free-standing `derive_init_impl` helper.

use super::{
    field_derive_kind, is_seeded_scalar_width, scalar_binary_width, zero_bin_literal, DeriveCtx,
    DeriveHandler, DeriveOutcome, FieldDeriveKind,
};
use crate::gap::{Category, GapReason};

fn recognizes(name: &str) -> bool {
    name == "Default"
}

/// **DN-138 WU-4 — the per-field `Default`/`init` expression.** `UserNamed`/`BytesLike`/`BoolLike`
/// and a `ScalarBinary` AT `Binary{64}` all resolve via the bare, trait-dispatched `init()` call
/// (unchanged, DN-138 increment 1). A NARROWER/wider `ScalarBinary` (WU-4 unblock) does **not**
/// route through the seeded `Binary{64}`-only `Init` instance at all — `width_cast`'s value operand
/// needs a concretely-typed argument, and a bare `init()` has none to offer until AFTER its own
/// "seed from expected" resolution runs (verified: `width_cast(init(), …)` cannot type-check, since
/// `width_cast`'s own arm checks its value operand with `expected: None`, RFC-0019 §4.4's expected-
/// type-propagation never reaches inside it) — so a narrow field's default is instead a **literal
/// zero at the field's own width** ([`zero_bin_literal`]), needing no instance at all (`Exact`, by
/// construction — a zero bit-pattern is trivially representable at any width). **`Vec[T]` fields
/// (WU-4) are default-initialized to `Nil` directly — regardless of the element type** (Rust's own
/// `Vec::default()` is the EMPTY vec; there is no element to recursively default, so this needs no
/// element-kind dependency and no auxiliary fn at all, unlike `Show`/`PartialEq`/`PartialOrd`/`Hash`
/// over the SAME field kind).
fn field_init_expr(ft: &str) -> Option<String> {
    match field_derive_kind(ft) {
        FieldDeriveKind::UserNamed | FieldDeriveKind::BytesLike | FieldDeriveKind::BoolLike => {
            Some("init()".to_owned())
        }
        FieldDeriveKind::ScalarBinary if is_seeded_scalar_width(ft) => Some("init()".to_owned()),
        FieldDeriveKind::ScalarBinary => {
            let w = scalar_binary_width(ft)?;
            Some(zero_bin_literal(w))
        }
        FieldDeriveKind::VecOf => Some("Nil".to_owned()),
        FieldDeriveKind::Float | FieldDeriveKind::Deferred => None,
    }
}

/// **Fieldless (unit) struct:** `fn init() => T = T;` — the bare nullary constructor, always
/// succeeds (live-oracle-proven, `src/tests/emit.rs`). **Struct with fields:**
/// `T(init(), init(), …)`, one bare `init()` per field IN DECLARATION ORDER — no qualified
/// `Type::init()` call is needed (RFC-0019 §4.4's "seed from expected" path), except a narrow
/// `ScalarBinary`/`VecOf` field, which gets its own literal expression ([`field_init_expr`]) — see
/// its doc for why. Gated per field via [`field_init_expr`] (DN-138 §4.5's classification, routed
/// per WU-4's expression table). **DN-138 unblock:** `UserNamed`/`BytesLike`/`BoolLike`/
/// `ScalarBinary` (any width — WU-4)/`VecOf` (any element — WU-4) fields all now compose;
/// `Float`/`Deferred` stay honest gaps.
fn compose(ty_name: &str, field_types: &[String]) -> Result<String, GapReason> {
    if field_types.is_empty() {
        return Ok(format!(
            "impl Init[{ty_name}] for {ty_name} {{\n  fn init() => {ty_name} =\n    {ty_name};\n}};"
        ));
    }
    let mut calls: Vec<String> = Vec::with_capacity(field_types.len());
    for (i, ft) in field_types.iter().enumerate() {
        let Some(expr) = field_init_expr(ft) else {
            return Err(GapReason::new(
                Category::DeriveAttr,
                format!(
                    "struct `{ty_name}` derive(Default): field {i} has type `{ft}`, a primitive \
                     repr (or `Seq`/tuple/other bracketed shape) with no landed `Init` route yet \
                     — the whole derive is left an honest gap rather than a partial/fabricated \
                     init (G2)"
                ),
            ));
        };
        calls.push(expr);
    }
    Ok(format!(
        "impl Init[{ty_name}] for {ty_name} {{\n  fn init() => {ty_name} =\n    {ty_name}({args});\n}};",
        args = calls.join(", ")
    ))
}

/// A **generic** struct refuses `derive(Default)` — a derived impl for a generic type needs
/// DN-130's generic-trait-instance-impl mechanism, out of this leaf's scope. Moved verbatim from
/// `lower_struct_derives`'s `"Default" if is_generic` arm.
fn emit(ctx: &DeriveCtx) -> DeriveOutcome {
    if ctx.is_generic {
        return DeriveOutcome::Gap(GapReason::new(
            Category::DeriveAttr,
            format!(
                "struct `{}` derive(Default): generic struct — a derived impl for a \
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
    slug: "DN-128/M-1086 (Default -> Init)",
    citation: "DN-128 (M-1086); DN-129/M-1091 (prelude Init trait); DN-136 P1-a migration (moved \
               verbatim from lower_struct_derives's Default arms + derive_init_impl)",
};
