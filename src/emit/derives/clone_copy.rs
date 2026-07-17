//! DN-128 §6.1 `derive(Clone)`/`derive(Copy)` — a satisfied no-op under Mycelium's value
//! semantics (ADR-003) — DN-136/P1-a row. Moved verbatim (no behavior change) from
//! `lower_struct_derives`'s `"Clone" | "Copy"` arm. Resolves regardless of genericity (value
//! semantics holds for any type, generic or not — unlike the [`super::show`]/[`super::init`]
//! rows, this row does not gate on `ctx.is_generic`, matching the pre-refactor arm exactly).

use super::{DeriveCtx, DeriveHandler, DeriveOutcome};
use crate::gap::{Category, GapReason};

fn recognizes(name: &str) -> bool {
    matches!(name, "Clone" | "Copy")
}

fn emit(ctx: &DeriveCtx) -> DeriveOutcome {
    DeriveOutcome::Satisfied(GapReason::new(
        Category::DeriveSatisfied,
        format!(
            "struct `{}` derive({}) is a satisfied no-op under \
             Mycelium's value semantics (ADR-003 — every value already copies \
             structurally; DN-128 §6.1) — not emitted as an impl, not a gap",
            ctx.ty_name, ctx.name
        ),
    ))
}

pub const ROW: DeriveHandler = DeriveHandler {
    recognizes,
    emit,
    slug: "DN-128 §6.1 (Clone/Copy satisfied no-op)",
    citation: "DN-128 §6.1 (ADR-003 value semantics); DN-136 P1-a migration (moved verbatim from \
               lower_struct_derives's \"Clone\" | \"Copy\" arm)",
};
