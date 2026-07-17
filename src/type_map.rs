//! The **named-type mapping table** — `map.rs`'s `MapTypeVisitor::visit_path` fixed-name arms
//! (`bool`, `u8`..`u128`, `i8`..`i128`, `usize`/`isize`, `f64`/`f32`, `char`, `String`/`str`,
//! `Self`), generalized into a `&[TypeMapRow]` (DN-136 §4.2 "P1-c" — the `prim_map::TABLE`
//! pattern applied to `map.rs`'s type-name -> Mycelium-type dispatch).
//!
//! # Why this table exists (DN-136 §4.2)
//!
//! `crate::visit::TypeVisitor`/`walk_type` already centralize the *`syn::Type`-variant* dispatch
//! (`Path`/`Tuple`/`Reference`/`Slice`); the residual collision seam was the **type-name -> surface
//! text** mapping *inside* `MapTypeVisitor::visit_path` — a growing inline `match name.as_str()`
//! body every new type-vocabulary addition (signed ints, `usize`/`isize`, `char` — the DN-99 §8
//! ENB-6 rows this table's own rows still cite) had to edit. This table makes a new named-type
//! mapping an **additive row in its own place**, never a shared-body edit — mirroring the landed
//! `prim_map::TABLE` (`prim_map.rs:140`), the pattern DN-136 recommends generalizing.
//!
//! **Scope, deliberately narrow (KISS/YAGNI, DN-136 §4.2 "light" table):** only the **fixed-name,
//! zero-or-special-cased** builtin mappings move here. The two *structural* arms that follow them
//! in `visit_path` — the bare ordinary-named-type passthrough (any non-builtin, no generic args)
//! and the generic-application arm (`Head[arg, ...]`, which recurses through [`crate::map::map_type`]
//! and needs the full `syn::PathArguments::AngleBracketed` shape, not a fixed name) — are NOT
//! per-name rows and stay in `map.rs`'s visitor; a table row here would have to smuggle that
//! recursive machinery through a `fn` pointer for no collision-surface benefit (there is exactly
//! one such arm each, not a growing per-type list).
//!
//! **The one exception: the `"()"` row (DN-137/M-1102).** `()` is a `syn::Type::Tuple` with zero
//! elements, not a `TypePath` — it can never reach `visit_path`'s name-keyed lookup at all, so
//! `"()"` is a **synthetic** lookup key (not a real Rust identifier `visit_path` could ever pass
//! in), consulted from `MapTypeVisitor::visit_tuple`'s zero-element arm instead. It still belongs
//! in this table (not hand-inlined in `visit_tuple`) for the same reason every other fixed-name
//! mapping does: one citation-carrying, `EXPLAIN`-able row, not a second ad hoc mapping site.
//!
//! **Guarantee: `Declared`**, identical strength to every row in `map.rs` (this is a pure
//! relocation of that module's existing rows, not a new claim — see `map.rs`'s module doc for the
//! full per-row grammar/ADR grounding this table's `citation` fields summarize).
//!
//! **Never-silent (G2):** a name with no row here is not a mapping failure — `map.rs`'s
//! `visit_path` falls through to its passthrough/generic-application arms exactly as before this
//! table existed, and only its own final `fallback`/gap arms ever produce a `GapReason` for a name
//! this table doesn't recognize. This table can only ever produce `Ok`/`Err` for a name it DOES
//! recognize — it never silently drops a lookup.

use crate::gap::{Category, GapReason};

/// One named-type mapping row. `map` is a plain `fn` pointer (no captured state beyond the
/// `self_ty` parameter every row receives) — matches [`crate::prim_map::PrimMapping`]'s data-row
/// shape (a `Fn`-coercible closure with no captures IS a `fn` pointer in Rust, so each row below
/// reads as a closure literal without any trait-object/dyn-dispatch ceremony).
#[derive(Clone, Copy)]
pub struct TypeMapRow {
    /// The bare Rust type name this row matches (`syn::TypePath`'s last segment ident, exactly —
    /// same recognition granularity `visit_path`'s pre-table `match name.as_str()` used).
    pub rust_name: &'static str,
    /// The mapping. Takes the enclosing `Self`-substitution context (`None` outside any
    /// impl/trait) and returns the mapped surface text, or an explicit gap — never a silent
    /// fallback (G2).
    pub map: fn(self_ty: Option<&str>) -> Result<String, GapReason>,
    /// For `EXPLAIN`/diagnostics (mirrors `prim_map::PrimMapping::slug`).
    pub slug: &'static str,
    /// The grounding citation for this row's mapping decision (VR-5) — see `map.rs`'s module doc
    /// for the full prose; this is the short form.
    pub citation: &'static str,
}

/// The table. Order is insertion order; [`lookup`] does a linear scan (small, fixed table — same
/// shape/rationale as [`crate::prim_map::TABLE`]).
///
/// **Mechanical relocation, not a behavior change (DN-136 §4.2 / DoD):** every row's `map` body is
/// the *unmodified* content of its former `visit_path` match arm (only a bare `self.self_ty` field
/// reference became the `self_ty` parameter this table's `fn` signature takes). No mapped surface
/// text and no `GapReason` message changed — verified byte-identical by
/// `src/tests/type_map.rs`'s differential corpus (drives every row through both `map.rs`'s public
/// `map_type` entry point AND this table's own `lookup`, asserting the same result either way).
pub const TABLE: &[TypeMapRow] = &[
    TypeMapRow {
        rust_name: "Self",
        map: |self_ty| {
            self_ty.map(str::to_string).ok_or_else(|| {
                GapReason::new(
                    Category::Other,
                    "`Self` type with no enclosing impl/trait context",
                )
            })
        },
        slug: "self-ty",
        citation: "the enclosing impl/trait's Self substitution (map.rs's map_type_inner caller)",
    },
    TypeMapRow {
        rust_name: "bool",
        map: |_| Ok("Bool".to_string()),
        slug: "bool",
        citation: "base_type's Ident type_args? arm; lib/std/cmp.myc bare `Bool` usage",
    },
    TypeMapRow {
        rust_name: "u8",
        map: |_| Ok("Binary{8}".to_string()),
        slug: "u8",
        citation: "base_type ::= 'Binary' '{' Int '}'",
    },
    TypeMapRow {
        rust_name: "u16",
        map: |_| Ok("Binary{16}".to_string()),
        slug: "u16",
        citation: "base_type ::= 'Binary' '{' Int '}'",
    },
    TypeMapRow {
        rust_name: "u32",
        map: |_| Ok("Binary{32}".to_string()),
        slug: "u32",
        citation: "base_type ::= 'Binary' '{' Int '}'",
    },
    TypeMapRow {
        rust_name: "u64",
        map: |_| Ok("Binary{64}".to_string()),
        slug: "u64",
        citation: "base_type ::= 'Binary' '{' Int '}'",
    },
    TypeMapRow {
        rust_name: "u128",
        map: |_| Ok("Binary{128}".to_string()),
        slug: "u128",
        citation: "base_type ::= 'Binary' '{' Int '}'",
    },
    // P4/P5 (DN-99 §8 ENB-6 / M-1029 / ADR-028 — see `map.rs`'s `map_type` doc for the full
    // verify-first correction): `Binary{N}` is sign-free (ADR-028 Accepted); a signed integer maps
    // to the SAME width `Binary{N}` as its unsigned counterpart. Signedness lives entirely in
    // which op the transpiler emits (`crate::emit`'s signed-operand gate), never in this mapped
    // type text.
    TypeMapRow {
        rust_name: "i8",
        map: |_| Ok("Binary{8}".to_string()),
        slug: "i8",
        citation: "ADR-028 (Binary is sign-free); DN-99 §8 ENB-6; checkty.rs:8005-8040",
    },
    TypeMapRow {
        rust_name: "i16",
        map: |_| Ok("Binary{16}".to_string()),
        slug: "i16",
        citation: "ADR-028 (Binary is sign-free); DN-99 §8 ENB-6; checkty.rs:8005-8040",
    },
    TypeMapRow {
        rust_name: "i32",
        map: |_| Ok("Binary{32}".to_string()),
        slug: "i32",
        citation: "ADR-028 (Binary is sign-free); DN-99 §8 ENB-6; checkty.rs:8005-8040",
    },
    TypeMapRow {
        rust_name: "i64",
        map: |_| Ok("Binary{64}".to_string()),
        slug: "i64",
        citation: "ADR-028 (Binary is sign-free); DN-99 §8 ENB-6; checkty.rs:8005-8040",
    },
    TypeMapRow {
        rust_name: "i128",
        map: |_| Ok("Binary{128}".to_string()),
        slug: "i128",
        citation: "ADR-028 (Binary is sign-free); DN-99 §8 ENB-6; checkty.rs:8005-8040",
    },
    // P4/P5 (DN-99 §8 ENB-6 row #22 — see `map.rs`'s `map_type` doc): a canonicalized, FLAGged
    // platform width — `Binary{64}` — for both `usize` and `isize` (`isize`'s signedness is
    // tracked separately by `crate::emit`, exactly like the bare `i*` types above).
    TypeMapRow {
        rust_name: "usize",
        map: |_| Ok("Binary{64}".to_string()),
        slug: "usize",
        citation: "DN-99 §8 ENB-6 row #22 (\"usize/uN -> domain Binary{N} + FLAG\")",
    },
    TypeMapRow {
        rust_name: "isize",
        map: |_| Ok("Binary{64}".to_string()),
        slug: "isize",
        citation: "DN-99 §8 ENB-6 row #22; signedness tracked separately by crate::emit",
    },
    // trx2 Lane C Deliverable 2 (verify-first correction, mitigation #14) — see `map.rs`'s
    // `map_type` doc for the full grammar/empirical-check basis.
    TypeMapRow {
        rust_name: "f64",
        map: |_| Ok("Float".to_string()),
        slug: "f64",
        citation: "docs/spec/grammar/mycelium.ebnf:251 (nullary Float base_type, IEEE-754 \
                   binary64); ADR-040 FLAG-1/M-897",
    },
    TypeMapRow {
        rust_name: "f32",
        map: |_| {
            Err(GapReason::new(
                Category::Other,
                "`f32` has no confirmed Mycelium representation — `Float` \
                 (docs/spec/grammar/mycelium.ebnf:251) is IEEE-754 binary64 only at \
                 introduction (ADR-040 FLAG-1/M-897); a width extension is a future, \
                 separately-decided append, never silently assumed (VR-5)",
            ))
        },
        slug: "f32",
        citation:
            "docs/spec/grammar/mycelium.ebnf:251; ADR-040 FLAG-1/M-897 (Float is binary64-only)",
    },
    // P4/P5 (DN-99 §8 ENB-6 row #45 — see `map.rs`'s `map_type` doc): the Unicode-scalar-value
    // codepoint idiom, `Binary{32}` — consistent with row #25's char-*literal* codepoint
    // convention. Unsigned (never tracked signed).
    TypeMapRow {
        rust_name: "char",
        map: |_| Ok("Binary{32}".to_string()),
        slug: "char",
        citation:
            "DN-99 §8 ENB-6 row #45 (codepoint idiom, consistent with row #25's char-literal \
                   convention)",
    },
    // RFC-0033 §3.2 (grounded via tero, DN-34 §8.14): `Bytes` is the language's *dedicated,
    // never-silent UTF-8* text repr — see `map.rs`'s `map_type` doc for the full basis.
    TypeMapRow {
        rust_name: "String",
        map: |_| Ok("Bytes".to_string()),
        slug: "String",
        citation: "RFC-0033 §3.2; grammar base_type line 250; checkty.rs:6669; DN-34 §8.14",
    },
    TypeMapRow {
        rust_name: "str",
        map: |_| Ok("Bytes".to_string()),
        slug: "str",
        citation: "RFC-0033 §3.2; grammar base_type line 250; checkty.rs:6669; DN-34 §8.14",
    },
    // DN-137 Alt D (M-1102): the unit type `()` -> the prelude nullary-constructor `Unit`
    // (`type Unit = Unit;`, the arity-0 M-826 tuple/product-family member — hand-seeded in
    // `mycelium-l1`'s `checkty::unit_prelude`, no `mycelium-core`/grammar edit). Not a `TypePath`
    // (this row's key, `"()"`, is a synthetic lookup name, not a real Rust identifier) — reached
    // from `MapTypeVisitor::visit_tuple`'s zero-element arm (`map.rs`), the one call site this
    // shape can come from, mirroring how every `visit_path` row is reached from its one call site.
    // **Guarantee: `Exact`** (VR-5) — a single-inhabitant type has nothing to approximate.
    TypeMapRow {
        rust_name: "()",
        map: |_| Ok("Unit".to_string()),
        slug: "unit",
        citation: "DN-137 Alt D / M-1102: prelude `type Unit = Unit;` (M-826 arity-0 product-\
                   family member); mycelium.ebnf:151/156 (type_item/constructor, optional \
                   field-parens); checkty.rs::unit_prelude",
    },
];

/// Look up `rust_name` in [`TABLE`] (first match; the table has no duplicate `rust_name` entries
/// by construction — pinned by `src/tests/type_map.rs`). `None` for any name this table doesn't
/// cover — the caller (`map.rs`'s `visit_path`) falls through to its passthrough/generic-
/// application arms exactly as before this table existed (never a silent drop, G2).
#[must_use]
pub fn lookup(rust_name: &str) -> Option<&'static TypeMapRow> {
    TABLE.iter().find(|row| row.rust_name == rust_name)
}
