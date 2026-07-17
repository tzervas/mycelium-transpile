//! DN-136/P1-a — the derive-rule emit-hook axis (Alt B: a static per-axis handler table
//! generalizing the already-landed `prim_map::TABLE` registry, `prim_map.rs:140`).
//!
//! **The two-level guarantee this axis must NOT collapse (DN-136 §3 item 2 / §7, DN-128):**
//! 1. **Per-derived-impl atomicity lives in the *rule* (a row's `emit`).** `derive_show_impl`/
//!    `derive_init_impl` (now [`show::compose`]/[`init::compose`]) refuse the **whole** impl the
//!    moment any field is ineligible — never a partial impl. Moved verbatim; unchanged.
//! 2. **Per-derive independence across the set lives in the *driver*
//!    ([`crate::emit::lower_struct_derives`]), which this axis does NOT touch.** The driver still
//!    owns: the attribute/derive-list walk, routing each derive's [`DeriveOutcome`] to
//!    `impls`/`sub_gaps`/`unrecognized`, and the item-still-emits compose-eligible-sub-gap-the-
//!    rest orchestration. A row can add a NEW derive rule; it can never move that orchestration
//!    out of the driver (a build-blocking review check, DN-136 §8 point 2(e)).
//!
//! A row's `recognizes` matches the derive's **trait-path text** (e.g. `"Debug"`); `emit` gets a
//! [`DeriveCtx`] carrying everything the pre-refactor inline arms closed over (`ty_name`,
//! `field_types`, `is_generic`, and the matched `name` — needed because the `Clone`/`Copy` row
//! interpolates which of the two fired into its satisfied-no-op message, exactly as the
//! pre-refactor `"Clone" | "Copy"` arm did with its shared `name` binding).

use crate::gap::GapReason;

mod clone_copy;
mod eq;
mod hash;
mod init;
mod ord;
mod show;

/// Everything a derive row's `emit` needs — the pre-refactor inline arms' closed-over locals,
/// reified as one struct (DN-136 §2's row shape, adapted to this axis's per-row inputs).
pub struct DeriveCtx<'a> {
    pub ty_name: &'a str,
    pub field_types: &'a [String],
    pub is_generic: bool,
    /// The matched derive-path text (e.g. `"Debug"`, `"Clone"`) — the `Clone`/`Copy` row needs
    /// this to interpolate the fired name into its message, byte-identically to the pre-refactor
    /// `"Clone" | "Copy" => { ... "derive({name}) is a satisfied no-op ..." }` arm.
    pub name: &'a str,
}

/// A derive row's outcome — the three states [`crate::emit::lower_struct_derives`] (the driver)
/// already routed inline, now reified so a row and the driver agree on the shape.
pub enum DeriveOutcome {
    /// The derive composed to this impl text — the driver appends it to `impls`.
    Composed(String),
    /// Not a failure — a satisfied no-op (e.g. `Clone`/`Copy` under value semantics) — the driver
    /// records it as a `Category::DeriveSatisfied` note, no impl emitted.
    Satisfied(GapReason),
    /// The derive could not compose — the driver records it as a sub-gap.
    Gap(GapReason),
}

/// One derive-rule handler row.
pub struct DeriveHandler {
    /// Pure recognizer — does this row own this derive-path text?
    pub recognizes: fn(&str) -> bool,
    /// The lowering — composes an impl, records a satisfied no-op, or gaps.
    pub emit: fn(&DeriveCtx) -> DeriveOutcome,
    /// For `EXPLAIN`/diagnostics (G2).
    #[allow(dead_code)] // read by future EXPLAIN tooling, not yet consumed (DN-136 §2)
    pub slug: &'static str,
    /// The DN/M-id grounding this row (VR-5).
    #[allow(dead_code)] // read by future EXPLAIN tooling, not yet consumed (DN-136 §2)
    pub citation: &'static str,
}

/// The table. **DN-136 Phase-2 (DERIVE-COMPLETION) update (append-only, additive):** `PartialEq`/
/// `PartialOrd`/`Hash` now have rows ([`eq`]/[`ord`]/[`hash`]); bare `Eq`/`Ord` are DELIBERATELY
/// still NOT recognized (each new row's own module doc explains why: recognizing both names in a
/// derive-list would invoke that row's `emit` twice for one struct, composing a duplicate
/// fn/impl the real toolchain refuses — `PartialEq`/`PartialOrd` are the reliable co-occurring
/// signal Rust's own `Eq: PartialEq`/`Ord: Eq + PartialOrd` supertrait bounds guarantee). A solo
/// `#[derive(Eq)]`/`#[derive(Ord)]` (invalid Rust on its own, syntactically representable but
/// never rustc-accepted) still falls through to the driver's `unrecognized` tracking, exactly as
/// every other never-built name does — this is unchanged from the pre-Phase-2 catch-all behavior.
/// A future derive leaf adds one file here + one append-only row — never touches
/// `lower_struct_derives` (DN-136's stated objective).
pub const TABLE: &[DeriveHandler] = &[
    show::ROW,
    init::ROW,
    clone_copy::ROW,
    eq::ROW,
    ord::ROW,
    hash::ROW,
];

/// First-match-wins linear scan over [`TABLE`] (same shape as [`crate::prim_map::lookup`]).
/// `None` for any derive name not in the DN-128 standard-derive set this leaf builds — the
/// driver's `unrecognized` bucket covers it, unchanged.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static DeriveHandler> {
    TABLE.iter().find(|row| (row.recognizes)(name))
}

/// One struct field's DN-138 §4.5 derive-composition classification — shared by all five
/// field-gating rows ([`show`]/[`init`]/[`ord`]/[`eq`]/[`hash`]; [`clone_copy`] does not gate).
/// **Replaces the former boolean `field_derive_eligible`** (DN-136 P1-a): a classification, not a
/// `bool`, because DN-138 §3's heterogeneity finding means the SAME primitive kind composes
/// differently depending on the row — `Show`/`Init`/`Ord3` dispatch through a resolvable TRAIT
/// INSTANCE (`crates/mycelium-l1/src/checkty.rs`'s `PRELUDE_INSTANCE_SEEDS`), while `PartialEq`/
/// `Hash` route directly to an already-landed PRIM (`eq`/`bytes_eq`/`hash.blake3`) — a bare `bool`
/// cannot express that distinction; each row's own `compose` routes per kind (see each row's doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldDeriveKind {
    /// A leading-uppercase, non-bracketed, non-primitive-repr name (a user-declared type; the
    /// pre-DN-138 boolean gate's sole `true` case). Composes exactly as before this DN — every row
    /// routes it through its own pre-existing user-type call shape (`render`/`init`/`cmp` trait
    /// dispatch; `eq_<Type>`/`hash_<Type>` deterministic nested-derive fn names).
    UserNamed,
    /// `Binary{N}` for some concrete width `N` (any width matches this KIND). DN-138 increment 1
    /// seeds a trait instance (`Show`/`Init`/`Ord3`) at exactly ONE concrete width, `Binary{64}`
    /// (§2 fact 1 — the width-erased coherence key admits at most one instance per head): a row
    /// that dispatches through that SEEDED INSTANCE must additionally gate on
    /// [`is_seeded_scalar_width`] before composing (a narrower/wider width is an honest, disclosed
    /// gap — increment 2, DN-138 §6). A row that routes to a PRIM instead (`eq` — `PartialEq`) has
    /// no such restriction: `eq`/`lt` are width-generic over any concrete `Binary{N}` (RFC-0032
    /// D1), so `PartialEq` composes over EVERY width, not just 64 (`Hash` still defers every width
    /// — no `Binary{N} -> Bytes` raw-byte prim exists yet, DN-138 §6).
    ScalarBinary,
    /// `Bytes` (mapped from a Rust `String`/`str`/`[u8]` field).
    BytesLike,
    /// `Bool`.
    BoolLike,
    /// `Float` — ineligible for every row (ADR-040 §2.3/§2.4): no `Show`/`Init`/`Ord3` instance is
    /// ever seeded for it, and a derived TOTAL `Eq`/`Ord` over a float field is refused (NaN has no
    /// order position, NaN != NaN) — `eq.rs`/`ord.rs` special-case this ahead of the classifier so
    /// their gap message cites the real (NaN/ADR-040) reason, not the generic no-route one.
    Float,
    /// `Vec[T]` (DN-138 WU-4, §6 increment 2) — the `Vec` cons-list, ONE bracket-level deep
    /// (`vec_element` peels exactly one `Vec[...]` layer). Each row classifies the peeled-off
    /// element type SEPARATELY (a second `field_derive_kind` call on the inner text) and decides
    /// its OWN eligibility for that element kind — this variant only marks the outer SHAPE as
    /// "a `Vec`", not that it composes. **Depth-1 only, by deliberate, disclosed scope:** a nested
    /// `Vec[Vec[T]]` field's element reclassifies as `VecOf` too, and none of this leaf's rows
    /// treat `VecOf` as an eligible ELEMENT kind (only `UserNamed`/`ScalarBinary`/`BytesLike`/
    /// `BoolLike` are), so a doubly-nested `Vec` is an honest, disclosed gap rather than an
    /// unbounded/silently-mis-composed recursion (DN-138 §6 "gaps at a sensible depth" — the
    /// sensible depth chosen here is 1, matching the corpus's own observed shapes, e.g.
    /// `CtorInfo.fields: Vec<Ty>`; YAGNI against speculative deeper nesting no corpus struct needs).
    VecOf,
    /// `Seq`/tuples, or any other bracketed shape this leaf does not resolve (including a `Vec[T]`
    /// whose element itself failed its OWN eligibility check — a row reports THAT case with its
    /// own specific reason, not this generic catch-all) — deferred to increment 2 (WU-4, DN-138
    /// §6) or later. Also the fallback for a non-uppercase-leading, non-primitive name the
    /// pre-DN-138 boolean gate's implicit "else ineligible" branch covered (never silently
    /// reclassified as `UserNamed`).
    Deferred,
}

/// Classify one struct field's mapped Mycelium type for derive composition (DN-138 §4.5) — shared
/// by all five field-gating rows. See [`FieldDeriveKind`]'s own doc for why this replaces the
/// former `field_derive_eligible(&str) -> bool` (DN-136 P1-a).
#[must_use]
pub(super) fn field_derive_kind(mapped_ty: &str) -> FieldDeriveKind {
    if mapped_ty == "Float" {
        return FieldDeriveKind::Float;
    }
    if mapped_ty == "Bool" {
        return FieldDeriveKind::BoolLike;
    }
    if mapped_ty == "Bytes" {
        return FieldDeriveKind::BytesLike;
    }
    if mapped_ty.starts_with("Binary{") && mapped_ty.ends_with('}') {
        return FieldDeriveKind::ScalarBinary;
    }
    // DN-138 WU-4: checked BEFORE the generic bracket-catch-all below (`Vec[...]` also contains
    // `[`) so a `Vec[T]` field gets its own dedicated kind rather than falling into the generic
    // `Deferred` bucket.
    if vec_element(mapped_ty).is_some() {
        return FieldDeriveKind::VecOf;
    }
    if mapped_ty.contains(['{', '(', '[']) {
        return FieldDeriveKind::Deferred;
    }
    if mapped_ty
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
    {
        return FieldDeriveKind::UserNamed;
    }
    FieldDeriveKind::Deferred
}

/// DN-138 WU-4 — peel exactly ONE `Vec[...]` bracket layer off `mapped_ty` (the
/// `crate::map::map_type` `Vec[{elem}]` convention, DN-99 row 35), returning the inner element's
/// own mapped-type text, or `None` if `mapped_ty` is not (syntactically) a `Vec[...]` shape.
/// `format!("Vec[{elem}]")` is the ONLY producer of this shape (`crate::map`), so a literal
/// `"Vec["` prefix + trailing `']'` suffix is exact — never a false match against some OTHER
/// bracketed shape (a tuple/`Seq` mapped-type text never starts with the literal bytes `"Vec["`).
#[must_use]
pub(super) fn vec_element(mapped_ty: &str) -> Option<&str> {
    mapped_ty
        .strip_prefix("Vec[")
        .and_then(|s| s.strip_suffix(']'))
}

/// DN-138 WU-4 — sanitize an arbitrary mapped-type string (e.g. `"Binary{64}"`, `"Bytes"`,
/// `"CheckError"`) into a valid Mycelium bare-identifier segment, for building deterministic,
/// per-element-type auxiliary fn names (`show_vec_<mangled>`, `eq_vec_<mangled>`, …) that never
/// collide across DIFFERENT element shapes within the SAME struct's compose call (every
/// non-alphanumeric byte — `{`, `}`, `[`, `]` — becomes `_`).
#[must_use]
pub(super) fn mangle_ty(ft: &str) -> String {
    ft.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// `true` iff `ft` is the ONE concrete `Binary{N}` width DN-138 increment 1 seeds a trait instance
/// at directly (`Binary{64}` — DN-138 §2 fact 1's width-erased coherence key: at most one
/// `Show`/`Init`/`Ord3` instance may exist per head `"Binary"`, and the real corpus's `u64` fields
/// hit it exactly). **DN-138 WU-4 (increment 2) update:** a narrower/wider `ScalarBinary` width is
/// no longer a bare refusal for [`show`]/[`ord`] — [`scalar_binary_width`] extracts `N`, and those
/// two rows wrap the call in a `width_cast` up to `Binary{64}` (see each row's `compose` doc); a
/// bare `render`/`cmp` call is only correct AT `Binary{64}` directly, which is exactly what this
/// predicate still answers (used to decide whether the width_cast wrapper is needed at all, never
/// a silent width-mismatch `myc check` failure at the emitted call site —
/// `crate::checkty::Checker::require_instance`'s own `info.for_ty == concrete` guard would refuse a
/// bare mismatched call, so this gate keeps that decision at EMIT time).
#[must_use]
pub(super) fn is_seeded_scalar_width(ft: &str) -> bool {
    ft == "Binary{64}"
}

/// DN-138 WU-4 — extract `N` from a `ScalarBinary`-kind mapped-type text `"Binary{N}"` (the ONLY
/// producer of this text is `crate::map`'s `Binary{{{n}}}` format, so the parse is exact — never a
/// silent default on a malformed width; `None` only if `ft` is not actually `ScalarBinary`-shaped,
/// which every call site here already gates on via [`field_derive_kind`]).
#[must_use]
pub(super) fn scalar_binary_width(ft: &str) -> Option<u32> {
    ft.strip_prefix("Binary{")?.strip_suffix('}')?.parse().ok()
}

/// DN-138 WU-4 — a `Binary{width}` all-zero literal, grouped in nibbles (`0b0000_0000`-style),
/// for the `width_cast` witness operand (only its *type* is read — the value is ignored, DN-41)
/// and for a narrow `ScalarBinary` field's `Init` route (a literal zero at the field's OWN width,
/// never dispatched through the seeded `Binary{64}`-only `Init` instance — see [`init`]'s
/// `compose` doc). Mirrors `crate::emit::zero_bin_literal`'s identical shape (a disclosed,
/// deliberate small duplication — this axis's rows stay self-contained per-file units, the same
/// KISS trade-off [`hash`]'s module doc already makes for its own `bytes_concat_chain` copy).
#[must_use]
pub(super) fn zero_bin_literal(width: u32) -> String {
    let mut s = String::with_capacity(2 + width as usize + width as usize / 4);
    s.push_str("0b");
    for i in 0..width {
        if i > 0 && i % 4 == 0 {
            s.push('_');
        }
        s.push('0');
    }
    s
}
