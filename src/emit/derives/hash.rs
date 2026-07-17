//! DN-128 (M-1086) `derive(Hash)` -> a structural fold over the landed `hash.blake3` kernel prim
//! (M-912) — DN-136 Phase-2 (DERIVE-COMPLETION) additive row over the frozen `emit/derives` axis
//! (DN-136 P1-a). DN-128 §2: "Hash = field-wise `hash.blake3` fold" over the "landed
//! `hash.blake3` prim (M-912)".
//!
//! **Composes a plain top-level `fn hash_<T>(a: T) => Bytes = hash_blake3(...);`, NOT
//! `impl Hash[T] for T` — the identical disclosed deviation [`super::eq`] documents in full, for
//! the identical reason.** No `Hash` prelude trait is landed — only `Fuse`/`Ord3`/`Show`/`Init`/
//! `Fault` are seeded (`PRELUDE_TRAIT_SEEDS`, `crates/mycelium-l1/src/checkty.rs:2282`) — so
//! composing an `impl` would need this row to self-declare the trait inline, which fails with a
//! "duplicate trait declaration" `myc-check` refusal the moment a second struct in the same file
//! also derives `Hash` (see [`super::eq`]'s module doc for the full empirical trail; the root
//! cause and the fix are identical here). `recognizes` has no `Eq`/`PartialEq`-style co-occurring-
//! name collision to avoid (Rust has no `PartialHash`), so this row simply recognizes `"Hash"`.
//!
//! **Domain-separated by folding the TYPE NAME itself into the hash input** ahead of the fields
//! (`hash_blake3(bytes_concat("T", bytes_concat(hash_<F0>(p0), hash_<F1>(p1)) ...))`) — mirrors
//! [`super::show`]'s `"T("`-prefixed render discriminator — so two differently-named,
//! identically-shaped types never hash identically (a live-oracle-verified shape, confirmed
//! `myc check`-clean for both the fieldless and the nested-field case during this leaf's
//! development). The field-hash fold reuses [`hash_fn_name`]'s deterministic naming so a nested
//! eligible field's own derived hash fn resolves by construction, exactly like [`super::eq`]'s
//! `eq_<FieldType>` composition.
//!
//! Guarantee: `Empirical` (live-oracle-verified, `src/tests/emit.rs`); the field-eligibility
//! heuristic stays `Declared` (same VR-5 boundary as every other row in this axis).

use std::collections::BTreeMap;

use super::{
    field_derive_kind, mangle_ty, vec_element, DeriveCtx, DeriveHandler, DeriveOutcome,
    FieldDeriveKind,
};
use crate::gap::{Category, GapReason};

fn recognizes(name: &str) -> bool {
    name == "Hash"
}

/// The deterministic top-level fn name this row's compose emits/expects for a given type name —
/// mirrors `eq.rs`'s identical `eq_fn_name` role (no cross-call state needed; both derive from
/// `ty_name`/`field_type` alone). Used ONLY for [`FieldDeriveKind::UserNamed`] fields.
fn hash_fn_name(ty_name: &str) -> String {
    format!("hash_{ty_name}")
}

/// **DN-138 §4.5 — the PRIM-ROUTED half of the heterogeneity finding, `Hash`'s analogue of
/// `eq.rs`'s `field_eq_expr`.** Returns the field's `Bytes`-typed hash-input expression, or `None`
/// for an ineligible kind.
///
/// - `UserNamed` -> `hash_<Type>(p)` (unchanged — the nested-derive compositional call).
/// - `BytesLike` -> `hash_blake3(p)` directly (already `Bytes`-typed, the M-912 prim).
/// - `BoolLike` -> `hash_blake3(match p { True => "True", False => "False" })` — a
///   SELF-CONTAINED inline byte encoding (never a named fn, avoiding the duplicate-fn hazard
///   `eq.rs`'s module doc documents; never a dependency on the `Show` trait being ambiently
///   available in THIS nodule, which — unlike `Show`'s own seeded PRIMITIVE instance — is only
///   conditionally seeded when SOME impl of `Show` is itself declared here, which a `Hash`-only
///   derive does not guarantee; a self-contained inline match sidesteps that cross-trait
///   dependency entirely, a disclosed, deliberate leaf-scoped design choice, VR-5).
/// - `ScalarBinary` (WU-4 unblock, any width) -> `hash_blake3(bin_to_bytes(p))` — the new DN-138
///   WU-4 `bin_to_bytes` prim supplies the previously-missing `Binary{N} -> Bytes` raw-byte
///   conversion, width-generic (no seeded-instance width restriction, unlike `Show`/`Ord3`).
/// - `VecOf` (WU-4) -> `hash_vec_<mangled elem>(p)`, a per-element-type recursive auxiliary
///   ([`vec_hash_aux`]) — only when the element itself has a hash route (depth-1 scope).
/// - `Float`/`Deferred` -> `None` (ineligible, as before).
fn field_hash_expr(p: &str, ft: &str) -> Option<String> {
    match field_derive_kind(ft) {
        FieldDeriveKind::UserNamed => Some(format!("{}({p})", hash_fn_name(ft))),
        FieldDeriveKind::BytesLike => Some(format!("hash_blake3({p})")),
        FieldDeriveKind::BoolLike => Some(format!(
            "hash_blake3(match {p} {{ True => \"True\", False => \"False\" }})"
        )),
        FieldDeriveKind::ScalarBinary => Some(format!("hash_blake3(bin_to_bytes({p}))")),
        FieldDeriveKind::VecOf => {
            let elem = vec_element(ft)?;
            field_hash_expr("_unused", elem)?;
            Some(format!("hash_vec_{}({p})", mangle_ty(elem)))
        }
        FieldDeriveKind::Float | FieldDeriveKind::Deferred => None,
    }
}

/// **DN-138 WU-4 — the `Vec[T]`-recursive `Hash` auxiliary.** Composes a plain top-level
/// `fn hash_vec_<mangled elem>(a: Vec[ELEM]) => Bytes = …;` folding [`hash_blake3`] over the
/// cons-list structure (mirrors [`compose`]'s own type-name-prefixed fold, applied one level
/// deeper): `Nil` hashes the literal `"Nil"`; `Cons(h, t)` hashes the concatenation of the
/// element's own hash-input expression ([`field_hash_expr`], recursively reused) and the
/// recursive tail's hash. A plain fn — same "no landed `Hash` prelude trait to dispatch through"
/// shape this row's own top-level doc already establishes, one level deeper. **Disclosed residual**
/// (identical to [`super::show::vec_show_aux`]'s): two different structs in one file needing the
/// same `hash_vec_<mangled>` collide as a duplicate-function refusal, out of this leaf's scope.
fn vec_hash_aux(mangled: &str, elem_ft: &str) -> String {
    let elem_expr = field_hash_expr("h", elem_ft).expect("eligibility already checked by caller");
    format!(
        "fn hash_vec_{mangled}(a: Vec[{elem_ft}]) => Bytes =\n  match a {{ Nil => \
         hash_blake3(\"Nil\"), Cons(h, t) => hash_blake3(bytes_concat({elem_expr}, \
         hash_vec_{mangled}(t))) }};"
    )
}

/// Left-fold `parts` into a single `bytes_concat(...)` chain — a local copy of
/// [`super::show`]'s private `bytes_concat_chain` helper (not shared across files: each row stays
/// a self-contained, independently-reviewable unit per this axis's row shape — see `mod.rs`'s
/// doc; the ~10-line duplication is a deliberate, disclosed KISS trade-off over refactoring the
/// already-landed, frozen `show.rs`). `parts` is never empty in the caller below.
fn bytes_concat_chain(parts: &[String]) -> String {
    let mut iter = parts.iter();
    let mut acc = iter.next().cloned().unwrap_or_default();
    for p in iter {
        acc = format!("bytes_concat({acc}, {p})");
    }
    acc
}

/// **Fieldless (unit) struct:** `fn hash_T(a: T) => Bytes = hash_blake3("T");` — the type-name
/// string literal alone is the hash input, always succeeds (live-oracle-proven,
/// `src/tests/emit.rs`). **Struct with fields:** `hash_blake3(bytes_concat("T", ...))` folding
/// each field's hash-input expression (routed per [`field_hash_expr`] — DN-138 §4.5), gated per
/// field — refuses the WHOLE derive (never a partial/fabricated hash, G2) the moment any field is
/// ineligible. **DN-138 unblock:** `UserNamed`/`BytesLike`/`BoolLike`/`ScalarBinary` (any width,
/// via the new `bin_to_bytes` prim — WU-4)/`VecOf` (any eligible element — WU-4) fields all now
/// compose; only `Float`/`Deferred` still gap.
fn compose(ty_name: &str, field_types: &[String]) -> Result<String, GapReason> {
    let fname = hash_fn_name(ty_name);
    if field_types.is_empty() {
        return Ok(format!(
            "fn {fname}(a: {ty_name}) => Bytes =\n    hash_blake3(\"{ty_name}\");"
        ));
    }
    for (i, ft) in field_types.iter().enumerate() {
        if field_hash_expr("p", ft).is_none() {
            let why = if field_derive_kind(ft) == FieldDeriveKind::VecOf {
                format!(
                    "a `Vec` field whose element type `{}` has no hash route of its own (a \
                     `Vec`-of-`Vec` or a `Float`/other-bracketed element -- DN-138 section 6, \
                     WU-4's disclosed depth-1 scope)",
                    vec_element(ft).unwrap_or(ft)
                )
            } else {
                "a primitive repr (or `Seq`/tuple/other bracketed shape) with no derived (or \
                 hand-written) structural-hash route yet"
                    .to_owned()
            };
            return Err(GapReason::new(
                Category::DeriveAttr,
                format!(
                    "struct `{ty_name}` derive(Hash): field {i} has type `{ft}`, {why} — the \
                     whole derive is left an honest gap rather than a partial/fabricated hash (G2)"
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
    let vars: Vec<String> = (0..field_types.len()).map(|i| format!("p{i}")).collect();
    let mut parts = vec![format!("\"{ty_name}\"")];
    for (i, ft) in field_types.iter().enumerate() {
        parts.push(field_hash_expr(&vars[i], ft).expect("eligibility already checked above"));
    }
    let inner = bytes_concat_chain(&parts);
    let mut out = String::new();
    for (mangled, elem_ft) in &vec_aux {
        out.push_str(&vec_hash_aux(mangled, elem_ft));
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "fn {fname}(a: {ty_name}) => Bytes =\n    match a {{ {ty_name}({pats}) => \
         hash_blake3({inner}) }};",
        pats = vars.join(", ")
    ));
    Ok(out)
}

/// A **generic** struct refuses `derive(Hash)` — a derived fn for a generic type needs DN-130's
/// generic-instance mechanism, out of this leaf's scope. Mirrors every other row's identical
/// `is_generic` gate.
fn emit(ctx: &DeriveCtx) -> DeriveOutcome {
    if ctx.is_generic {
        return DeriveOutcome::Gap(GapReason::new(
            Category::DeriveAttr,
            format!(
                "struct `{}` derive(Hash): generic struct — a derived hash fn for a generic \
                 type needs DN-130's generic-instance mechanism, out of this leaf's scope \
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
    slug: "DN-128 (Phase-2 DERIVE-COMPLETION) — Hash -> structural hash.blake3 fold",
    citation: "DN-128 §2 (Hash -> field-wise hash.blake3 fold); M-912 (the landed hash.blake3 \
               kernel prim); DN-136 Phase-2 bulk-gap-close worklist B3/L3 (disclosed deviation: a \
               plain fn, not `impl Hash[T] for T` — no landed Hash prelude trait; same root cause \
               eq.rs documents)",
};
