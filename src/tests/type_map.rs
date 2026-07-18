//! Differential + structural tests for `crate::type_map` (DN-136 §4.2 "P1-c") — the DoD's
//! **byte-identical** requirement: this table must produce IDENTICAL output to the pre-refactor
//! inline `match name.as_str()` in `map.rs`'s `MapTypeVisitor::visit_path`, for every existing
//! mapping (scalars, `Self`, `String`/`str`, and composed through generics/tuples/references/
//! slices). Data-driven per CLAUDE.md "Complex test logic lives in fixtures + parameterization" —
//! the expected surface text below is transcribed directly from the pre-refactor match arms (see
//! the git history of `map.rs` / this table's per-row citations), not re-derived, so a drift in
//! either the table or `visit_path`'s wiring shows up as a mismatch.

use crate::gap::Category;
use crate::map::map_type;
use crate::type_map::{self, TABLE};

// ── DN-137 / M-1102 — live `myc check` differentials for the `()` -> `Unit` row ────────────────

/// Parse a Rust type from text (white-box fixture builder — mirrors `tests/map.rs`'s `ty`).
fn ty(text: &str) -> syn::Type {
    syn::parse_str::<syn::Type>(text)
        .unwrap_or_else(|e| panic!("fixture `{text}` is not a parseable Rust type: {e}"))
}

enum Expect {
    Ok(&'static str),
    Gap(Category, &'static str),
}

struct Case {
    rust: &'static str,
    expect: Expect,
    /// Optional `self_ty` context (only `Self` needs one).
    self_ty: Option<&'static str>,
}

/// The byte-identical mapping differential corpus — one case per `TABLE` row (bare use), plus
/// every row composed through a generic application / tuple / reference / slice, exercising the
/// table lookup from every calling shape `visit_path`'s pre-table match reached.
fn cases() -> Vec<Case> {
    use Expect::*;
    vec![
        // ── Every TABLE row, bare (the exact pre-refactor match-arm outcome) ──────────────────
        Case {
            rust: "Self",
            expect: Gap(Category::Other, "no enclosing impl/trait context"),
            self_ty: None,
        },
        Case {
            rust: "Self",
            expect: Ok("Widget"),
            self_ty: Some("Widget"),
        },
        Case {
            rust: "bool",
            expect: Ok("Bool"),
            self_ty: None,
        },
        Case {
            rust: "u8",
            expect: Ok("Binary{8}"),
            self_ty: None,
        },
        Case {
            rust: "u16",
            expect: Ok("Binary{16}"),
            self_ty: None,
        },
        Case {
            rust: "u32",
            expect: Ok("Binary{32}"),
            self_ty: None,
        },
        Case {
            rust: "u64",
            expect: Ok("Binary{64}"),
            self_ty: None,
        },
        Case {
            rust: "u128",
            expect: Ok("Binary{128}"),
            self_ty: None,
        },
        Case {
            rust: "i8",
            expect: Ok("Binary{8}"),
            self_ty: None,
        },
        Case {
            rust: "i16",
            expect: Ok("Binary{16}"),
            self_ty: None,
        },
        Case {
            rust: "i32",
            expect: Ok("Binary{32}"),
            self_ty: None,
        },
        Case {
            rust: "i64",
            expect: Ok("Binary{64}"),
            self_ty: None,
        },
        Case {
            rust: "i128",
            expect: Ok("Binary{128}"),
            self_ty: None,
        },
        Case {
            rust: "usize",
            expect: Ok("Binary{64}"),
            self_ty: None,
        },
        Case {
            rust: "isize",
            expect: Ok("Binary{64}"),
            self_ty: None,
        },
        Case {
            rust: "f64",
            expect: Ok("Float"),
            self_ty: None,
        },
        Case {
            rust: "f32",
            expect: Gap(Category::Other, "IEEE-754 binary64 only at introduction"),
            self_ty: None,
        },
        Case {
            rust: "char",
            expect: Ok("Binary{32}"),
            self_ty: None,
        },
        Case {
            rust: "String",
            expect: Ok("Bytes"),
            self_ty: None,
        },
        Case {
            rust: "str",
            expect: Ok("Bytes"),
            self_ty: None,
        },
        // ── Composed through the structural arms the table lookup now sits ahead of ───────────
        // Generic application: a builtin argument recurses through the public `map_type`, which
        // re-enters `visit_path` -> the table lookup, per nested level.
        Case {
            rust: "Vec<u8>",
            expect: Ok("Vec[Binary{8}]"),
            self_ty: None,
        },
        Case {
            rust: "Option<i32>",
            expect: Ok("Option[Binary{32}]"),
            self_ty: None,
        },
        Case {
            rust: "Result<char, f64>",
            expect: Ok("Result[Binary{32}, Float]"),
            self_ty: None,
        },
        // Tuple: each element recurses through `map_type`.
        Case {
            rust: "(u8, bool)",
            expect: Ok("(Binary{8}, Bool)"),
            self_ty: None,
        },
        // DN-137 Alt D / M-1102: the unit type `()` -> the prelude nullary-ctor `Unit`, reached
        // via `visit_tuple`'s zero-element arm (NOT `visit_path` — `()` is never a `TypePath`),
        // consulting `TABLE`'s synthetic `"()"` row. No longer a gap.
        Case {
            rust: "()",
            expect: Ok("Unit"),
            self_ty: None,
        },
        // Shared reference: the referent recurses through `map_type` (erasure composes with the
        // table lookup).
        Case {
            rust: "&u32",
            expect: Ok("Binary{32}"),
            self_ty: None,
        },
        Case {
            rust: "&str",
            expect: Ok("Bytes"),
            self_ty: None,
        },
        // Slice: a non-`u8` element recurses through `map_type` into the `Vec[T]` convention;
        // preserves the SYNTACTIC-`u8` gating from the #1534 slice work (P1-c requirement) — an
        // `i8` element, though it maps to the SAME `Binary{8}` text as `u8`, must NOT get the
        // `Bytes` shortcut.
        Case {
            rust: "[u8]",
            expect: Ok("Bytes"),
            self_ty: None,
        },
        Case {
            rust: "[i8]",
            expect: Ok("Vec[Binary{8}]"),
            self_ty: None,
        },
        Case {
            rust: "[i32]",
            expect: Ok("Vec[Binary{32}]"),
            self_ty: None,
        },
        // Bare ordinary named type (NOT in the table) still passes through unchanged — the table
        // miss must fall through to the structural passthrough arm, never a silent gap.
        Case {
            rust: "Duration",
            expect: Ok("Duration"),
            self_ty: None,
        },
    ]
}

fn run(case: &Case) {
    let mapped = map_type(&ty(case.rust), case.self_ty);
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
                "case `{}`: mapped surface mismatch (byte-identical requirement)",
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
                "case `{}`: gap reason must be byte-identical to the pre-refactor text (missing \
                 `{reason_substr}`), got: {}",
                case.rust,
                err.reason
            );
        }
    }
}

#[test]
fn byte_identical_mapping_differential_corpus() {
    for case in cases() {
        run(&case);
    }
}

/// Every row's `rust_name` is unique — a duplicate would make `lookup`'s first-match-wins scan
/// silently shadow a later row (never-tested drift the table's own doc claims is pinned here).
#[test]
fn table_has_no_duplicate_rust_names() {
    let mut seen = std::collections::HashSet::new();
    for row in TABLE {
        assert!(
            seen.insert(row.rust_name),
            "duplicate TABLE row for `{}` — first-match-wins would silently shadow the later row",
            row.rust_name
        );
    }
}

/// `lookup` finds every row by its own `rust_name` (round-trip), and returns `None` for a name
/// with no row — the never-silent contract the table's doc names (a miss falls through to
/// `map.rs`'s structural arms, it never fabricates a mapping).
#[test]
fn lookup_round_trips_every_row_and_misses_cleanly() {
    for row in TABLE {
        let found = type_map::lookup(row.rust_name)
            .unwrap_or_else(|| panic!("`lookup(\"{}\")` must find its own row", row.rust_name));
        assert_eq!(found.rust_name, row.rust_name);
    }
    assert!(
        type_map::lookup("Duration").is_none(),
        "an ordinary user type name must not spuriously match a table row"
    );
    assert!(
        type_map::lookup("NotARealType").is_none(),
        "an unrecognized name must miss cleanly (None), never panic or fabricate a row"
    );
}

/// Direct-table-vs-`map_type` differential: for every `Ok`-mapping row, calling the row's `map` fn
/// directly must equal what `map_type` produces when parsing that exact bare type name — pins the
/// wiring in `visit_path` (a table hit must return EXACTLY `(row.map)(self.self_ty)`, nothing
/// transformed in between).
#[test]
fn every_table_row_matches_map_type_on_its_own_bare_name() {
    for row in TABLE {
        if row.rust_name == "Self" {
            // `Self` needs an enclosing self_ty; covered by the corpus above (both branches).
            continue;
        }
        let direct = (row.map)(None);
        let via_visitor = map_type(&ty(row.rust_name), None);
        match (direct, via_visitor) {
            (Ok(a), Ok(b)) => assert_eq!(
                a, b,
                "row `{}`: direct table call and map_type disagree",
                row.rust_name
            ),
            (Err(a), Err(b)) => assert_eq!(
                a.category, b.category,
                "row `{}`: direct table call and map_type gap-category disagree",
                row.rust_name
            ),
            (a, b) => panic!(
                "row `{}`: direct table call ({}) and map_type ({}) disagree on Ok/Err",
                row.rust_name,
                if a.is_ok() { "Ok" } else { "Err" },
                if b.is_ok() { "Ok" } else { "Err" }
            ),
        }
    }
}

// ── DN-137 Alt D / M-1102 — live `myc check` differentials for `()` -> `Unit` ──────────────────
//
// **Scope note (mitigation #11-style file ownership):** these differentials exercise exactly
// what this leaf owns — the TYPE-position mapping (`type_map::TABLE`'s `"()"` row, reached via
// `map.rs`'s `visit_tuple`) plus the `mycelium-l1` prelude `Unit` it targets. The Rust
// EXPRESSION-side unit VALUE (`emit.rs`'s `MapExprVisitor::visit_tuple`, "unit value `()` has no
// Mycelium literal") is a *separate*, still-open residual in a file this leaf does not touch
// (FLAGged in the leaf report, not silently left implicit) — so the second differential below
// deliberately picks a body shape (`Err(1)`) that never constructs a Rust `()` expression, to
// stay a genuine, unmodified `transpile_source` round-trip rather than a hand-patched one.

use super::vet::find_myc_check;
use crate::transpile::transpile_source;

/// The exact `map_type`-produced text for `()` (`"Unit"`) checks CLEAN in every grammatical
/// position `Unit` can occupy: a bare `=> type_ref` return, and as the sole value of that type.
/// Hand-assembled `.myc` (not routed through `transpile_source`, which cannot yet emit the
/// unit-VALUE expression — see the module-level scope note): this differential's job is to prove
/// the LANGUAGE side (prelude `Unit` + the mapped text) is real and clean, independent of the
/// still-open expression-emission residual. Skips gracefully (never fails) when `myc-check` is
/// not built.
#[test]
fn unit_return_position_checks_clean_against_real_toolchain() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "type_map: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or \
             build `cargo build -p mycelium-check --bin myc-check`). The mapping-differential \
             corpus above still covers the `()` -> `Unit` text."
        );
        return;
    };
    let checker = crate::vet::MycChecker {
        command: vec![bin.display().to_string()],
        cwd: None,
    };

    let mapped = map_type(&ty("()"), None).expect("`()` must map (DN-137/M-1102)");
    assert_eq!(
        mapped, "Unit",
        "the mapped text feeding this differential must stay in sync"
    );

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-type-map-unit-live-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    // A `fn f()` analogue of Rust's `fn f() -> () { () }`: an explicit `=> Unit` return, whose
    // sole value is the ordinary constructor-application expression `Unit` (DN-137's dissolved
    // OQ-1 — no new literal). Exactly the mapped text `map_type` produces for `()`, asserted above.
    let path = dir.join("unit_fn.myc");
    std::fs::write(&path, "// nodule: p\nnodule p;\n\nfn f() => Unit = Unit;\n")
        .expect("write unit_fn.myc");
    let rec = checker.vet_file(&path, "unit_fn.rs", 1, 1);
    assert_eq!(
        rec.class,
        crate::vet::VetClass::Clean,
        "a bare `() -> Unit`-returning fn must check CLEAN with the prelude Unit mapping; \
         diagnostic={:?}",
        rec.diagnostic
    );
    assert_eq!(rec.checked_clean_items(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A genuine, **unmodified** `transpile_source` round-trip for the realistic `Result<(), E>`
/// idiom (the shape driving most of the corpus's 26+ `()`-in-"Other" instances per
/// `docs/planning/DN-136-phase2-bulk-gap-close-worklist.md` §3 item D1) — a real Rust fn whose
/// return type nests `()` as a generic argument, with a body (`Err(1)`) that never constructs the
/// Rust unit EXPRESSION itself (staying clear of the separate, unowned expr-emission residual —
/// see the module-level scope note). Proves the TYPE-position mapping end-to-end through the
/// actual transpiler entry point, not just `map_type` in isolation.
#[test]
fn result_unit_return_type_transpiles_and_checks_clean_against_real_toolchain() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "type_map: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or \
             build `cargo build -p mycelium-check --bin myc-check`). The mapping-differential \
             corpus above still covers the `()` -> `Unit` text."
        );
        return;
    };
    let checker = crate::vet::MycChecker {
        command: vec![bin.display().to_string()],
        cwd: None,
    };

    const NODULE_PATH: &str = "oracle";
    // `x` (a parameter reference), not a bare integer literal — `Err(1)` would additionally trip
    // the SEPARATE, unowned "bare integer literal has no representation family" refusal
    // (`emit.rs`'s `Lit::Int` arm emits a plain decimal digit string myc-check legitimately
    // rejects; unrelated to `()`/`Unit` and out of this leaf's scope). Isolating the differential
    // to exactly the `()` -> `Unit` mapping this leaf owns.
    let rust = "fn f(x: u8) -> Result<(), u8> { Err(x) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", NODULE_PATH)
        .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
    assert!(
        !report.emitted_items.is_empty(),
        "expected a clean emission (no gap), got: gaps={:?}\nmyc={myc}",
        report.gaps
    );
    assert!(
        myc.contains("Result[Unit, Binary{8}]"),
        "expected the mapped signature to carry `Result[Unit, Binary{{8}}]`, got:\n{myc}"
    );

    // G-α Rank-1 ambient co-emit supplies `type Result[A, E] = Ok(A) | Err(E);` when the body
    // mentions `Result[…]` — no manual inject (would double-define under myc-check).
    assert!(
        myc.contains("type Result[A, E] = Ok(A) | Err(E);"),
        "expected ambient Result co-emit for Result[Unit, …] signature, got:\n{myc}"
    );
    assert!(
        report.emitted_items.iter().any(|n| n == "co-emit:Result"),
        "expected co-emit:Result in emitted_items; got {:?}",
        report.emitted_items
    );

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-type-map-result-unit-live-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("result_unit.myc");
    std::fs::write(&path, &myc).expect("write result_unit.myc");
    let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
    assert_eq!(
        rec.class,
        crate::vet::VetClass::Clean,
        "a real `-> Result<(), u8>` transpile must check CLEAN with the Unit mapping; \
         diagnostic={:?}\nmyc:\n{myc}",
        rec.diagnostic
    );

    let _ = std::fs::remove_dir_all(&dir);
}
