//! L-MAP: unit tests for the `Type::Slice` (`[T]`/`&[T]`) mapping this leaf added to `map.rs`
//! (DN-99 §2 register rows 15/35 — `&[u8]` -> `Bytes`; every other `[T]` -> the `Vec[T]`
//! cons-list convention, matching how `Vec<T>` already maps through the ordinary
//! generic-application arm). `map.rs`'s own `map_type` doc carries the full mapping rationale;
//! this module pins the behaviour with a data-driven corpus.
//!
//! **Guarantee: `Declared`** — same strength as every other row in `map.rs` (grammar-text
//! mapping only, not `myc check`-verified by this leaf; see `map.rs`'s module doc). Data-driven
//! (per CLAUDE.md "Complex test logic lives in fixtures + parameterization"): a test body is
//! `assert over a case`, the cases live in a table.

use crate::gap::Category;
use crate::map::{field_type_user_deps, map_type};

/// Parse a Rust type from text (white-box fixture builder — mirrors `tests/map.rs`'s `ty`).
fn ty(text: &str) -> syn::Type {
    syn::parse_str::<syn::Type>(text)
        .unwrap_or_else(|e| panic!("fixture `{text}` is not a parseable Rust type: {e}"))
}

enum Expect {
    /// `map_type` returns this exact surface text.
    Ok(&'static str),
    /// `map_type` gaps with this category (the whole slice type refused — never a partial
    /// emission), and the reason must contain this substring (naming the real blocker).
    Gap(Category, &'static str),
}

struct Case {
    rust: &'static str,
    expect: Expect,
}

/// The slice-type-mapping corpus. Each row cites the DN-99 rule it pins.
fn cases() -> Vec<Case> {
    use Expect::*;

    vec![
        // ── `[u8]` -> `Bytes` (DN-99 row 15's first mapping) ──────────────────────────────────
        // The bare (unreferenced) slice — syn parses this even though real Rust needs an
        // indirection; this leaf's mapping does not require the reference.
        Case {
            rust: "[u8]",
            expect: Ok("Bytes"),
        },
        // The real-corpus shape: `&[u8]` — the `&` erases (the pre-existing shared-reference arm),
        // then the referent `[u8]` maps to `Bytes`.
        Case {
            rust: "&[u8]",
            expect: Ok("Bytes"),
        },
        // A lifetime on the reference is erased with it, exactly as the ordinary `&'a T` case.
        Case {
            rust: "&'a [u8]",
            expect: Ok("Bytes"),
        },
        // ── Element-gated on the SYNTACTIC element, never the mapped text ─────────────────────
        // `i8` also maps to `Binary{8}` (ADR-028, same width as `u8`), but `[i8]` must NOT get the
        // `Bytes` shortcut — that would silently reinterpret a signed element as an unsigned-octet
        // buffer. It falls to the general `[T]` -> `Vec[T]` rule instead.
        Case {
            rust: "[i8]",
            expect: Ok("Vec[Binary{8}]"),
        },
        Case {
            rust: "&[i8]",
            expect: Ok("Vec[Binary{8}]"),
        },
        // A wider unsigned width is likewise NOT `Bytes` — only the exact syntactic `u8`.
        Case {
            rust: "[u16]",
            expect: Ok("Vec[Binary{16}]"),
        },
        // ── General `[T]` -> `Vec[T]` cons-list (DN-99 row 35) ────────────────────────────────
        // An ordinary named element type passes through unchanged inside `Vec[..]`.
        Case {
            rust: "&[Ordering]",
            expect: Ok("Vec[Ordering]"),
        },
        // A `char` element (P4/P5: `char` -> `Binary{32}`) composes with the slice mapping.
        Case {
            rust: "[char]",
            expect: Ok("Vec[Binary{32}]"),
        },
        // A `String` element maps to `Bytes` first, then the slice wraps it — `Vec<T>` and `[T]`
        // stay surface-uniform per DN-99 row 35 (`&[T]`/`Vec<T>` -> the same `Vec[T]` text): this
        // is the identical surface `Vec<String>` already gets via the generic-application arm.
        Case {
            rust: "&[String]",
            expect: Ok("Vec[Bytes]"),
        },
        // A slice of a mappable generic application composes with the generic arm (recursion
        // re-arms the budget through the public `map_type`, same as every other nested arm).
        Case {
            rust: "&[Option<u32>]",
            expect: Ok("Vec[Option[Binary{32}]]"),
        },
        // ── Never-silent gap: an unmappable element propagates its OWN reason (never a partial
        // `Vec[..]` emission — G2) ────────────────────────────────────────────────────────────
        Case {
            rust: "[f32]",
            expect: Gap(Category::Other, "f32"),
        },
        Case {
            rust: "&[f32]",
            expect: Gap(Category::Other, "f32"),
        },
        // DN-140 (M-1106): reserved element types escape before the `Vec[…]` slice convention.
        Case {
            rust: "[Exact]",
            expect: Ok("Vec[Exact_kw]"),
        },
    ]
}

fn run(case: &Case) {
    let mapped = map_type(&ty(case.rust), None);
    match &case.expect {
        Expect::Ok(surface) => {
            let got = mapped.unwrap_or_else(|e| {
                panic!(
                    "case `{}`: expected Ok(`{surface}`), got gap [{}] {}",
                    case.rust,
                    e.category.as_str(),
                    e.reason
                )
            });
            assert_eq!(
                &got, surface,
                "case `{}`: mapped surface mismatch",
                case.rust
            );
        }
        Expect::Gap(category, reason_substr) => {
            let err = mapped.expect_err(&format!(
                "case `{}`: expected a gap of category {:?}, got Ok",
                case.rust,
                category.as_str()
            ));
            assert_eq!(
                err.category, *category,
                "case `{}`: gap category mismatch — reason was: {}",
                case.rust, err.reason
            );
            assert!(
                err.reason.contains(reason_substr),
                "case `{}`: gap reason must name the real blocker (`{reason_substr}`), got: {}",
                case.rust,
                err.reason
            );
        }
    }
}

#[test]
fn slice_type_mapping_corpus() {
    for case in cases() {
        run(&case);
    }
}

/// Never-silent cascade regression (G2/VR-5): a slice whose element has no mapping must not leak
/// any partial `Vec[..]`/`Bytes` text — `mapped` is a hard `Err`, never a truncated `Ok`.
#[test]
fn unmappable_element_never_leaks_partial_vec_emission() {
    let err = map_type(&ty("[f32]"), None)
        .expect_err("`[f32]` must gap — `f32` has no confirmed mapping");
    assert_eq!(err.category, Category::Other);
    assert!(
        !err.reason.contains("Vec["),
        "must not leak a partial `Vec[..]`: {}",
        err.reason
    );
}

/// Scope boundary (VR-5): `Type::Array` (`[T; N]`, the DN-99 `Seq`-mapping half of rows 15/35) is
/// a DIFFERENT `syn` shape from `Type::Slice` and is untouched by this leaf — it still falls to
/// `map_type`'s generic "unsupported Rust type form" fallback, exactly as before this leaf
/// landed. Pinned so a future reader does not assume `[T; N]` is covered here.
#[test]
fn fixed_size_array_type_is_still_out_of_scope() {
    let err = map_type(&ty("[u8; 4]"), None)
        .expect_err("`[T; N]` (Type::Array) is a distinct shape this leaf did not map");
    assert_eq!(err.category, Category::Other);
    assert!(
        err.reason.contains("unsupported Rust type form"),
        "must still hit the generic fallback, not a slice-shaped mapping: {}",
        err.reason
    );
}

/// The honest std-sys-host boundary this leaf's kickoff called out explicitly: the real Rust
/// construct a buffer-fill signature actually uses is `&mut [u8]` — a MUTABLE-reference slice —
/// which this leaf's new `visit_slice` arm never even reaches: `visit_reference`'s pre-existing
/// `&mut T` branch (ADR-003, no value-semantic correspondence for in-place mutation) gaps it
/// first. So this leaf's slice-TYPE mapping is correct and complete for `[u8]`/`[T]`/`&[T]`, but
/// contributes **zero** movement on a `&mut [u8]` site by construction — that residual is
/// DN-125/M-1081 value-threading's job, not this type-mapping table's (VR-5: build the correct
/// general mapping regardless of its immediate blast radius, never fabricate coverage it doesn't
/// have).
#[test]
fn mutable_slice_reference_still_gaps_via_the_pre_existing_mut_ref_arm_not_this_leafs_slice_arm() {
    let err = map_type(&ty("&mut [u8]"), None)
        .expect_err("`&mut [u8]` must still gap — mutation has no value-semantic correspondence");
    assert_eq!(err.category, Category::Other);
    assert!(
        err.reason.contains("mutable reference") && err.reason.contains("ADR-003"),
        "must be the pre-existing `&mut` gap reason, not a slice-shaped one: {}",
        err.reason
    );
}

/// `field_type_user_deps` (the M-1006 resolvability-fixpoint mirror) must track the same
/// mappable/unmappable boundary `map_type` does for slices (its own doc names the drift risk
/// explicitly): `[u8]` is mappable but contributes no user dep (like every other builtin); a
/// slice of a user type contributes that type as a dep; a slice of an unmappable element is
/// itself unmappable (conservatively withheld — never unsound, per the doc's one-sided-gate note).
#[test]
fn field_type_user_deps_mirrors_the_slice_mapping_boundary() {
    let mut out = Vec::new();
    assert!(
        field_type_user_deps(&ty("&[u8]"), &mut out),
        "`&[u8]` is mappable (to `Bytes`)"
    );
    assert!(
        out.is_empty(),
        "a `Bytes`-mapped slice contributes no user dep"
    );

    let mut out = Vec::new();
    assert!(
        field_type_user_deps(&ty("&[Ordering]"), &mut out),
        "`&[Ordering]` is mappable (to `Vec[Ordering]`)"
    );
    assert_eq!(out, vec!["Ordering".to_string()]);

    let mut out = Vec::new();
    assert!(
        !field_type_user_deps(&ty("[f32]"), &mut out),
        "`[f32]` has no `map_type` mapping ⇒ unmappable field"
    );
}
