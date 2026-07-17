//! Unit tests for `crate::prim_map` (trx2 Lane C Deliverable 2) — both the emitted-text shape
//! (fixture corpus, data-driven per CLAUDE.md "Complex test logic lives in fixtures +
//! parameterization") and, for the `wired: true` rows, a live-oracle proof against the real
//! `myc-check` toolchain (mirrors `src/tests/emit.rs`'s `binop_operand_gated_forms_check_clean`).

use super::vet::find_myc_check;
use crate::gap::Category;
use crate::transpile::transpile_source;

/// WIRED: a receiver known `Float` (via the `f64` parameter, itself mapped by this leaf's
/// `map_type` fix) triggers the real `flt_is_nan`/`flt_is_finite`/`flt_is_infinite` prim call,
/// bridged `Binary{1}` -> `Bool` (Rust's `f64::is_nan`/… always return `bool`).
#[test]
fn wired_float_classification_methods_emit_bridged_prim_calls() {
    let cases = [
        (
            "fn f(x: f64) -> bool { x.is_nan() }",
            "(match flt_is_nan(x) { 0b1 => True, _ => False })",
        ),
        (
            "fn f(x: f64) -> bool { x.is_finite() }",
            "(match flt_is_finite(x) { 0b1 => True, _ => False })",
        ),
        (
            "fn f(x: f64) -> bool { x.is_infinite() }",
            "(match flt_is_infinite(x) { 0b1 => True, _ => False })",
        ),
    ];
    for (rust, needle) in cases {
        let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            report.emitted_items.iter().any(|n| n == "f"),
            "case `{rust}`: expected `f` in emitted_items, got {:?} (gaps={:?})",
            report.emitted_items,
            report.gaps
        );
        assert!(
            myc.contains(needle),
            "case `{rust}`: expected emitted text to contain `{needle}`, got:\n{myc}"
        );
        // The `f64` parameter itself must map to the grammar's real `Float` base_type (this
        // leaf's `map_type` fix) — otherwise the receiver-type gate could never have fired.
        assert!(
            myc.contains("fn f(x: Float)"),
            "case `{rust}`: expected the `f64` param to map to `Float`, got:\n{myc}"
        );
    }
}

/// NOT gated: an `.is_nan()`-named method on a receiver whose type is NOT known to be `Float`
/// (here, an ordinary passed-through named type) must NOT trigger the bridged prim rewrite — the
/// receiver-type gate exists precisely to prevent a coincidentally-same-named method on an
/// unrelated type from being mistranslated (VR-5: never guess the receiver's type). Falls through
/// to the unchanged generic `recv.method(args)` -> `method(recv, args)` desugar.
#[test]
fn is_nan_on_unknown_receiver_type_keeps_generic_desugar() {
    let rust = "fn f(x: Thing) -> bool { x.is_nan() }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` in emitted_items, got {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("is_nan(x)") && !myc.contains("flt_is_nan"),
        "expected the OLD generic bare-call desugar (`is_nan(x)`), not the bridged `flt_is_nan` \
         rewrite, since `Thing` is not a known `Float` receiver — got:\n{myc}"
    );
}

/// PENDING-BACKEND: `.wrapping_add()`/`.wrapping_sub()`/`.wrapping_mul()` on a receiver known to
/// be some concrete `Binary{N}` are recognized (CU-5, RFC-0034 §10/M-791 — the named `wrapping`
/// construct is a decided ruling with no grammar surface or runtime path yet) but ALWAYS refuse —
/// never emitted, per the PENDING-BACKEND contract (VR-5/G2).
#[test]
fn wrapping_methods_on_known_binary_are_pending_backend_gaps() {
    let cases = [
        "fn f(a: u16, b: u16) -> u16 { a.wrapping_add(b) }",
        "fn f(a: u16, b: u16) -> u16 { a.wrapping_sub(b) }",
        "fn f(a: u16, b: u16) -> u16 { a.wrapping_mul(b) }",
    ];
    for rust in cases {
        let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            !report.emitted_items.iter().any(|n| n == "f"),
            "case `{rust}`: expected NO emission for a PENDING-BACKEND row, got emitted_items={:?}",
            report.emitted_items
        );
        assert!(
            !myc.contains("wrapping"),
            "case `{rust}`: a PENDING-BACKEND row must never leak emitted text, got:\n{myc}"
        );
        assert!(
            report
                .gaps
                .iter()
                .any(|g| g.category == Category::Conversion
                    && g.reason.contains("PENDING-BACKEND(CU-5)")),
            "case `{rust}`: expected a Category::Conversion gap citing PENDING-BACKEND(CU-5), got \
             {:?}",
            report
                .gaps
                .iter()
                .map(|g| (g.category.as_str(), g.reason.as_str()))
                .collect::<Vec<_>>()
        );
    }
}

/// NOT gated: `.wrapping_add()` on a receiver NOT known to be a concrete `Binary{N}` (here, an
/// unrelated passed-through type) does not fire the PENDING-BACKEND gap either — same
/// receiver-type-gate discipline as the `is_nan` case above, applied to the `AnyBinaryWidth` gate.
#[test]
fn wrapping_add_on_unknown_receiver_type_keeps_generic_desugar() {
    let rust = "fn f(x: Thing, y: Thing) -> Thing { x.wrapping_add(y) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` in emitted_items (the generic desugar still emits SOME text), got {:?} \
         (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("wrapping_add(x, y)"),
        "expected the OLD generic bare-call desugar (`wrapping_add(x, y)`), not a PENDING-BACKEND \
         gap, since `Thing` is not a known `Binary{{N}}` receiver — got:\n{myc}"
    );
}

/// WIRED (L4, DN-136 Phase-2, M-1100), narrowed by the PR #1552 review CRITICAL fix: `.clone()` on
/// a receiver whose mapped type is a fixed **builtin/primitive scalar**
/// (`ReceiverGate::AnyBuiltinScalar` — `Bool`/`Bytes`/some concrete `Binary{N}`) emits the receiver
/// UNCHANGED via the documented `myc_prim: ""` parenthesized-passthrough convention (`(recv)`,
/// grammar `primary ::= ... | '(' expr ')'`) — never a fabricated bare `clone(recv)` call (the
/// exact fabrication class `is_unmappable_conversion_method` exists to prevent — see
/// `src/tests/emit.rs::conversion_noop_method_gaps_never_fabricates_unknown_prim`, unaffected by
/// this leaf since it only ever exercises `.to_owned()`/`.deref()`, not `.clone()`). A builtin
/// receiver's `Clone` impl is std's own, fixed, field-copy behavior (Rust's orphan rule forbids a
/// downstream `impl Clone for u64`/`bool`/`String`), so identity is sound here — see
/// `clone_on_user_named_type_receiver_never_fires_identity_and_gaps` below for the converse
/// (user-named-type) case, which must NOT fire this row.
#[test]
fn clone_on_known_builtin_receiver_emits_identity_passthrough() {
    let cases = [
        ("fn f(x: u64) -> u64 { x.clone() }", "(x)"),
        ("fn f(x: bool) -> bool { x.clone() }", "(x)"),
        ("fn f(x: String) -> String { x.clone() }", "(x)"),
    ];
    for (rust, needle) in cases {
        let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            report.emitted_items.iter().any(|n| n == "f"),
            "case `{rust}`: expected `f` in emitted_items, got {:?} (gaps={:?})",
            report.emitted_items,
            report.gaps
        );
        assert!(
            myc.contains(needle) && !myc.contains("clone("),
            "case `{rust}`: expected the identity passthrough `{needle}` and NO fabricated \
             `clone(...)` call, got:\n{myc}"
        );
    }
}

/// **CRITICAL regression (PR #1552 review, reproduced against the compiled transpiler).** A
/// receiver whose mapped type is a user-named type (here: a `struct` with a hand-written, NON-
/// derived `Clone` impl whose body does more than a field-for-field copy) must NEVER fire the
/// `.clone()` identity row — that would silently drop the custom `clone` body's actual effect,
/// reporting a clean success while producing behaviorally wrong output (G2's central anti-pattern:
/// a silent swap). The exact reviewer repro: `struct Ticket{id,gen}` + a custom `clone` that bumps
/// `gen` by one; `fn bump(t: Ticket) -> Ticket { t.clone() }` must GAP (never emit `bump` as bare
/// `(t)`, which would drop the `+1`).
///
/// **This test is non-vacuous by construction:** it asserts the FAILURE mode (no `bump` in
/// `emitted_items`, no `(t)`/no bare `t` passthrough text for `bump`) — under the pre-fix
/// `ReceiverGate::AnyKnown` gate this assertion FAILS (that gate fires on any resolvable receiver,
/// including `Ticket`, silently emitting `bump` as `(t)`); under the fixed
/// `ReceiverGate::AnyBuiltinScalar` gate (this leaf's change) it PASSES, because `Ticket` is not a
/// builtin/primitive mapped type and so the row's gate never matches, and `.clone()` falls through
/// to the pre-existing `is_unmappable_conversion_method` gap exactly as it did before L4 existed.
#[test]
fn clone_on_user_named_type_receiver_never_fires_identity_and_gaps() {
    let rust = "struct Ticket { id: u32, gen: u32 }\n\
                impl Clone for Ticket {\n\
                    fn clone(&self) -> Ticket { Ticket { id: self.id, gen: self.gen + 1 } }\n\
                }\n\
                fn bump(t: Ticket) -> Ticket { t.clone() }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "bump"),
        "CRITICAL: a `.clone()` on a user-named-type receiver (a struct with a custom Clone impl) \
         must NEVER emit `bump` as a clean identity passthrough — that silently bypasses the \
         custom clone body. Got emitted_items={:?}, gaps={:?}, myc=\n{myc}",
        report.emitted_items,
        report.gaps
    );
    // `bump` itself must never appear as an emitted item at all (a gapped function is never
    // partially emitted, G2) — note `myc` legitimately DOES contain the text "clone" here, from
    // the user's own hand-written `impl Clone for Ticket` block (a *different*, faithfully
    // emitted item this fixture also declares) — so the assertion is scoped to `bump`'s own
    // declaration, not a blanket "clone" substring ban.
    assert!(
        !myc.contains("fn bump"),
        "CRITICAL: no `fn bump` declaration of any shape (identity passthrough or otherwise) may \
         ever be emitted, got:\n{myc}"
    );
    assert!(
        report.gaps.iter().any(|g| g
            .reason
            .contains("ownership/identity-conversion no-op method")),
        "expected `bump` to gap via the pre-existing `is_unmappable_conversion_method` catch-all \
         (unchanged by this fix), got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// Direct unit-level pin on the gate itself (belt-and-suspenders alongside the end-to-end
/// transpile test above): `ReceiverGate::AnyBuiltinScalar` matches exactly `Bool`/`Bytes`/any
/// `Binary{N}`, and explicitly does NOT match an arbitrary user-named type's mapped text (a bare
/// passed-through identifier, e.g. `"Ticket"`) even though that text is `Some` (i.e. "known" in the
/// `AnyKnown` sense) — pins the CRITICAL fix's exact boundary independent of the transpile
/// pipeline.
#[test]
fn any_builtin_scalar_gate_excludes_user_named_types() {
    use crate::prim_map::{receiver_gate_matches, ReceiverGate};

    for builtin in ["Bool", "Bytes", "Binary{8}", "Binary{64}", "Binary{128}"] {
        assert!(
            receiver_gate_matches(ReceiverGate::AnyBuiltinScalar, Some(builtin)),
            "expected AnyBuiltinScalar to match builtin mapped type `{builtin}`"
        );
    }
    for user_named in ["Ticket", "Thing", "Ordering", "Float"] {
        assert!(
            !receiver_gate_matches(ReceiverGate::AnyBuiltinScalar, Some(user_named)),
            "expected AnyBuiltinScalar to EXCLUDE non-builtin mapped text `{user_named}` (a \
             user-named type, or — for `Float` — a builtin this gate deliberately does not cover, \
             see the gate's own doc)"
        );
    }
    assert!(
        !receiver_gate_matches(ReceiverGate::AnyBuiltinScalar, None),
        "expected AnyBuiltinScalar to never fire on a wholly-unresolved receiver (VR-5)"
    );
}

/// NOT gated: `.clone()` on a receiver whose type does NOT resolve at all (here, the result of a
/// nested call expression — [`crate::emit::expr_env_type`] only resolves a bare identifier, or a
/// paren/reference wrapper around one) never fires the identity row on an unresolved receiver
/// (VR-5: no guess), same receiver-gate discipline as `is_nan`/`wrapping_add` above, applied to
/// `AnyBuiltinScalar` (an unresolved receiver has no mapped-type text at all, so it fails this
/// gate the same way it would have failed the original `AnyKnown`). Unlike those two (which fall
/// through to the OLD generic bare-call desugar), `clone`
/// falls through to the PRE-EXISTING `is_unmappable_conversion_method` gap instead (`crate::emit`,
/// unchanged by this leaf) — `clone` was already in that gap's method list before this leaf, so an
/// un-gated `.clone()` still refuses cleanly rather than emitting a fabricated bare `clone(...)`
/// call. Confirms the identity row is genuinely additive-only: it can only ever turn a prior gap
/// into a clean emission, never introduce a new fabrication path.
#[test]
fn clone_on_unresolved_receiver_type_still_gaps_never_fabricates() {
    let rust = "fn f(x: Thing) -> Thing { g(x).clone() }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "a `.clone()` on an unresolved receiver must still gap (the pre-existing \
         `is_unmappable_conversion_method` catch-all, unchanged by this leaf), got {:?}",
        report.emitted_items
    );
    assert!(
        !myc.contains("clone("),
        "the fabricated `clone(...)` bare call must NEVER be emitted, got:\n{myc}"
    );
    assert!(
        report.gaps.iter().any(|g| g
            .reason
            .contains("ownership/identity-conversion no-op method")),
        "the conversion gap must name the no-op-conversion class (same as the pre-existing \
         `to_owned`/`deref` gap in `src/tests/emit.rs`), got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// NEVER-SILENT (G2/VR-5) regression guard: `to_string`/`into`/`deref` are DELIBERATELY WITHHELD
/// from `TABLE` (L4, DN-136 Phase-2, M-1100 — see the module doc's L4 section for each one's own
/// verify-first finding), so the pre-existing `is_unmappable_conversion_method` gap in `emit.rs`
/// keeps handling them exactly as before — unchanged. Pins that decision directly against the
/// table (not a guess about emitted text), so a future accidental/silent addition of one of these
/// rows is caught here first. `to_owned` was withheld by the original L4 leaf (a flagged residual)
/// but is added by this follow-on leaf — see the converse check below.
#[test]
fn to_string_into_deref_are_not_in_the_table_deliberately_withheld() {
    for method in ["to_string", "into", "deref"] {
        assert!(
            crate::prim_map::lookup(method).is_none(),
            "`{method}` must NOT be in prim_map::TABLE (deliberately withheld — see the module \
             doc's L4 section for its verify-first finding); the existing \
             `is_unmappable_conversion_method` gap in `emit.rs` must keep handling it unchanged"
        );
    }
    // `clone`/`to_owned` ARE in the table — the converse check, so this test cannot pass by
    // accident (e.g. an empty table).
    for method in ["clone", "to_owned"] {
        assert!(
            crate::prim_map::lookup(method).is_some(),
            "`{method}` must be in prim_map::TABLE (an identity-conversion addition)"
        );
    }
}

/// WIRED (this leaf, M-1100 residual): `.to_owned()` on a receiver whose mapped type is a fixed
/// **builtin/primitive scalar** (`ReceiverGate::AnyBuiltinScalar` — `Bool`/`Bytes`/some concrete
/// `Binary{N}`) emits the receiver UNCHANGED via the documented `myc_prim: ""`
/// parenthesized-passthrough convention (`(recv)`), exactly like `clone` above — never a
/// fabricated bare `to_owned(recv)` call (the exact fabrication class
/// `is_unmappable_conversion_method` exists to prevent — see
/// `src/tests/emit.rs::conversion_noop_method_gaps_never_fabricates_unknown_prim`, updated by this
/// leaf to reflect the new identity behavior for a bare-identifier `&str`/`String` receiver).
#[test]
fn to_owned_on_known_builtin_receiver_emits_identity_passthrough() {
    let cases = [
        ("fn f(x: u64) -> u64 { x.to_owned() }", "(x)"),
        ("fn f(x: bool) -> bool { x.to_owned() }", "(x)"),
        ("fn f(x: String) -> String { x.to_owned() }", "(x)"),
        ("fn f(s: &str) -> String { s.to_owned() }", "(s)"),
    ];
    for (rust, needle) in cases {
        let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            report.emitted_items.iter().any(|n| n == "f"),
            "case `{rust}`: expected `f` in emitted_items, got {:?} (gaps={:?})",
            report.emitted_items,
            report.gaps
        );
        assert!(
            myc.contains(needle) && !myc.contains("to_owned("),
            "case `{rust}`: expected the identity passthrough `{needle}` and NO fabricated \
             `to_owned(...)` call, got:\n{myc}"
        );
    }
}

/// **Soundness regression, mirroring `clone_on_user_named_type_receiver_never_fires_identity_and_gaps`
/// above.** A receiver whose mapped type is a user-named type must NEVER fire the `.to_owned()`
/// identity row — `ToOwned`'s `Owned` associated type need not even equal `Self` for a user impl
/// (std's own `str -> String`/`[T] -> Vec<T>` are exactly this shape), so assuming identity there
/// would silently guess a transformation that may not even preserve the receiver's own type,
/// reporting a clean success while producing behaviorally wrong (or outright type-mismatched)
/// output (G2's central anti-pattern: a silent swap).
#[test]
fn to_owned_on_user_named_type_receiver_never_fires_identity_and_gaps() {
    let rust = "fn snap(t: Ticket) -> Ticket { t.to_owned() }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "snap"),
        "a `.to_owned()` on a user-named-type receiver (`Ticket`, not a builtin scalar) must \
         NEVER emit `snap` as a clean identity passthrough. Got emitted_items={:?}, gaps={:?}, \
         myc=\n{myc}",
        report.emitted_items,
        report.gaps
    );
    assert!(
        !myc.contains("fn snap"),
        "no `fn snap` declaration of any shape (identity passthrough or otherwise) may ever be \
         emitted, got:\n{myc}"
    );
    assert!(
        report.gaps.iter().any(|g| g
            .reason
            .contains("ownership/identity-conversion no-op method")),
        "expected `snap` to gap via the `is_unmappable_conversion_method` catch-all, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// NOT gated: `.to_owned()` on a receiver whose type does NOT resolve at all (a nested call
/// expression) never fires the identity row (VR-5: no guess) — same receiver-gate discipline as
/// `clone_on_unresolved_receiver_type_still_gaps_never_fabricates` above.
#[test]
fn to_owned_on_unresolved_receiver_type_still_gaps_never_fabricates() {
    let rust = "fn f(x: Thing) -> Thing { g(x).to_owned() }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "a `.to_owned()` on an unresolved receiver must still gap (the pre-existing \
         `is_unmappable_conversion_method` catch-all), got {:?}",
        report.emitted_items
    );
    assert!(
        !myc.contains("to_owned("),
        "the fabricated `to_owned(...)` bare call must NEVER be emitted, got:\n{myc}"
    );
    assert!(
        report.gaps.iter().any(|g| g
            .reason
            .contains("ownership/identity-conversion no-op method")),
        "the conversion gap must name the no-op-conversion class, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// Direct unit-level pin (belt-and-suspenders): `ReceiverGate::AnyBuiltinScalar` is the gate both
/// `clone` and `to_owned` rows share — already pinned by `any_builtin_scalar_gate_excludes_user_named_types`
/// above; this test just confirms `lookup("to_owned")` actually uses that gate (not a copy/paste
/// drift onto some other variant).
#[test]
fn to_owned_row_uses_any_builtin_scalar_gate() {
    use crate::prim_map::ReceiverGate;

    let row = crate::prim_map::lookup("to_owned").expect("`to_owned` must be in TABLE");
    assert_eq!(
        row.receiver_gate,
        ReceiverGate::AnyBuiltinScalar,
        "the `to_owned` row must use `AnyBuiltinScalar` (never the broader `AnyKnown`), for the \
         identical soundness reason as `clone`'s row"
    );
    assert!(
        row.wired,
        "`to_owned` must be `wired: true` (an identity emission, not PENDING)"
    );
    assert_eq!(
        row.myc_prim, "",
        "`to_owned` must use the identity-passthrough sentinel"
    );
}

/// **The verify-first proof** (mitigation #14) for the WIRED rows: every bridged
/// `flt_is_nan`/`flt_is_finite`/`flt_is_infinite` emission is run through the REAL `myc-check`
/// oracle, proving the text actually type-checks with zero imports (not just a substring match).
/// Skips gracefully (never fails) when `myc-check` is not built.
#[test]
fn wired_methods_check_clean_against_real_toolchain() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "prim_map: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or \
             build `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text \
             assertions above still cover the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-prim-map-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let rust_snippets = [
        "fn f_is_nan(x: f64) -> bool { x.is_nan() }",
        "fn f_is_finite(x: f64) -> bool { x.is_finite() }",
        "fn f_is_infinite(x: f64) -> bool { x.is_infinite() }",
    ];
    for (i, rust) in rust_snippets.iter().enumerate() {
        let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            !report.emitted_items.is_empty(),
            "case {i} (`{rust}`) failed to emit at all: gaps={:?}",
            report.gaps
        );
        let path = dir.join(format!("case_{i}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");

        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "case {i} (`{rust}`) must check CLEAN with the real myc-check oracle — emitted:\n{myc}\n\
             diagnostic={:?}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// **The verify-first proof** (mitigation #14) for L4's `clone` row: every identity-passthrough
/// `.clone()` emission is run through the REAL `myc-check` oracle over a `Binary{64}`-, `Bool`-,
/// and `Bytes`-typed receiver (`u64`/`bool`/`String` — no in-file declaration needed, so this
/// mirrors `wired_methods_check_clean_against_real_toolchain`'s zero-import shape exactly), proving
/// the emitted conversions check CLEAN, not just a substring match. Skips gracefully (never fails)
/// when `myc-check` is not built.
#[test]
fn clone_identity_checks_clean_against_real_toolchain() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "prim_map: clone live oracle test skipped — no runnable myc-check (set \
             MYC_CHECK_CMD or build `cargo build -p mycelium-check --bin myc-check`). The \
             fixture-corpus text assertions above still cover the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-prim-map-clone-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let rust_snippets = [
        "fn f_binary(x: u64) -> u64 { x.clone() }",
        "fn f_bool(x: bool) -> bool { x.clone() }",
        "fn f_bytes(x: String) -> String { x.clone() }",
    ];
    for (i, rust) in rust_snippets.iter().enumerate() {
        let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            !report.emitted_items.is_empty(),
            "case {i} (`{rust}`) failed to emit at all: gaps={:?}",
            report.gaps
        );
        assert!(
            !myc.contains("clone("),
            "case {i} (`{rust}`) leaked a fabricated `clone(...)` call, got:\n{myc}"
        );
        let path = dir.join(format!("clone_case_{i}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");

        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "case {i} (`{rust}`) must check CLEAN with the real myc-check oracle — emitted:\n{myc}\n\
             diagnostic={:?}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// **The verify-first proof** (mitigation #14) for this leaf's `to_owned` row: every
/// identity-passthrough `.to_owned()` emission is run through the REAL `myc-check` oracle over a
/// `Binary{64}`-, `Bool`-, and `Bytes`-typed receiver (`u64`/`bool`/`String`, plus the `&str`
/// bare-identifier shape from the #72 test), proving the emitted conversions check CLEAN, not
/// just a substring match. Skips gracefully (never fails) when `myc-check` is not built.
#[test]
fn to_owned_identity_checks_clean_against_real_toolchain() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "prim_map: to_owned live oracle test skipped — no runnable myc-check (set \
             MYC_CHECK_CMD or build `cargo build -p mycelium-check --bin myc-check`). The \
             fixture-corpus text assertions above still cover the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-prim-map-to-owned-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let rust_snippets = [
        "fn f_binary(x: u64) -> u64 { x.to_owned() }",
        "fn f_bool(x: bool) -> bool { x.to_owned() }",
        "fn f_bytes(x: String) -> String { x.to_owned() }",
        "fn f_str(s: &str) -> String { s.to_owned() }",
    ];
    for (i, rust) in rust_snippets.iter().enumerate() {
        let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            !report.emitted_items.is_empty(),
            "case {i} (`{rust}`) failed to emit at all: gaps={:?}",
            report.gaps
        );
        assert!(
            !myc.contains("to_owned("),
            "case {i} (`{rust}`) leaked a fabricated `to_owned(...)` call, got:\n{myc}"
        );
        let path = dir.join(format!("to_owned_case_{i}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");

        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "case {i} (`{rust}`) must check CLEAN with the real myc-check oracle — emitted:\n{myc}\n\
             diagnostic={:?}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
