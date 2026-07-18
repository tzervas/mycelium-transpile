//! DN-135 (M-1092) — the Result/Option combinator-directed match-inline. Unit tests over
//! `emit::visit_method_call`'s `try_inline_result_option_combinator` pass via the public
//! `transpile_source` driver (per CLAUDE.md "Test layout": data-driven fixtures, complex logic
//! stays out of test bodies).
//!
//! Covers, per DN-135 §7 Definition of Done and this leaf's brief:
//! - `.map(|()| E)` and `.map_err(|_| C)` both inline on a confirmed Result receiver;
//! - `.and_then(|x| ..)` inlines (an UNTYPED single-identifier param — DN-118 Phase 1 alone would
//!   have gapped this; DN-135 needs no param type at all, DN-126 §4 mode-invariance);
//! - the Option sibling (`.map`) inlines identically off `Some`/`None`;
//! - the never-silent gaps (VR-5/G2): a non-Result/Option receiver (the iterator-`.map`
//!   false-fire stress test) is UNTOUCHED by this pass; an unresolved (call-expression) receiver
//!   falls through and gaps via the pre-existing DN-118 closure-pattern gate, never a fabricated
//!   `Ok`/`Err`; a multi-parameter closure and a capture-mutating closure both decline to inline
//!   and inherit the identical pre-existing DN-118/DN-109 gap;
//! - a live-oracle `myc check`-clean differential over every inlined form (mirrors
//!   `src/tests/prim_map.rs::wired_methods_check_clean_against_real_toolchain`).
//!
//! **Scope correction against the original DN-135 §3 item 5 (a real-toolchain finding, house rule
//! #4):** a CHAIN (`.map(..).map_err(..)`) does NOT nest — a nested inlined `match` used as an
//! outer match's scrutinee fails `myc check`'s constructor type-parameter inference unless
//! individually ascribed with a type this transpiler cannot generally derive (see
//! `emit::combinator_receiver_kind`'s doc for the full empirical finding). Covered here: the outer
//! combinator of a chain declines and the whole call gaps honestly (never an unsound nested
//! `match`), while an inner combinator with its own independently-resolvable receiver still
//! inlines correctly.

use super::vet::find_myc_check;
use crate::gap::Category;
use crate::transpile::transpile_source;

/// `.map(|()| E)` inlines over a confirmed `Result` receiver — the exact `std-sys-host`
/// `OsEntropy::fill_bytes` residual shape (DN-135 §1), with a resolvable (bare-identifier)
/// receiver so the receiver gate fires.
#[test]
fn map_over_unit_closure_inlines_on_result_receiver() {
    let rust = "fn f(flag: u8, r: Result<u8, u8>) -> Result<u8, u8> { r.map(|()| flag) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` in emitted_items, got {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("match (r) { Ok(_) => Ok(flag), Err(e) => Err(e) }"),
        "expected the inlined match body, got:\n{myc}"
    );
}

/// `.map_err(|_| C)` inlines over a confirmed `Result` receiver — the second half of the
/// `OsEntropy::fill_bytes` residual (DN-135 §1).
#[test]
fn map_err_over_wildcard_closure_inlines_on_result_receiver() {
    let rust =
        "fn f(fallback: u8, r: Result<u8, u8>) -> Result<u8, u8> { r.map_err(|_| fallback) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` in emitted_items, got {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("match (r) { Ok(x) => Ok(x), Err(_) => Err(fallback) }"),
        "expected the inlined match body, got:\n{myc}"
    );
}

/// `.and_then(|x| ..)` inlines with an UNTYPED single-identifier param — DN-118 Phase 1 alone
/// gaps an untyped closure param (`emit.rs`'s `visit_closure` requires `Pat::Type`); DN-135's
/// match-inline needs no param type at all (DN-126 §4 mode-invariance), so this broader win comes
/// for free from the same mechanism.
#[test]
fn and_then_with_untyped_ident_param_inlines() {
    let rust = "fn f(r: Result<u8, u8>) -> Result<u8, u8> { r.and_then(|x| Ok(x)) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` in emitted_items, got {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("match (r) { Ok(x) => Ok(x), Err(e) => Err(e) }"),
        "expected the inlined match body, got:\n{myc}"
    );
}

/// `.or_else(|_| ..)` (Result only) inlines — `lib/std/result.myc:45`'s `{ Ok(x) => Ok(x),
/// Err(<p>) => <body> }` arm template.
#[test]
fn or_else_over_wildcard_closure_inlines_on_result_receiver() {
    let rust = "fn f(alt: u8, r: Result<u8, u8>) -> Result<u8, u8> { r.or_else(|_| Ok(alt)) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` in emitted_items, got {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("match (r) { Ok(x) => Ok(x), Err(_) => Ok(alt) }"),
        "expected the inlined match body, got:\n{myc}"
    );
}

/// `.fold(on_ok, on_err)` (Result, BOTH arguments closures) inlines — `lib/std/result.myc:33`'s
/// two-arm eliminator template.
#[test]
fn fold_with_two_closures_inlines_on_result_receiver() {
    let rust = "fn f(dflt: u8, r: Result<u8, u8>) -> u8 { r.fold(|x: u8| x, |_| dflt) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` in emitted_items, got {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("match (r) { Ok(x) => x, Err(_) => dflt }"),
        "expected the inlined match body, got:\n{myc}"
    );
}

/// `.fold(on_some, on_none)` on Option: `on_some` is a closure, `on_none` is a plain VALUE
/// (`lib/std/option.myc:44`) — emitted directly via `emit_expr`, never through the closure path.
#[test]
fn fold_option_with_closure_and_value_inlines() {
    let rust = "fn f(o: Option<u8>, dflt: u8) -> u8 { o.fold(|x: u8| x, dflt) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` in emitted_items, got {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("match (o) { Some(x) => x, None => dflt }"),
        "expected the inlined match body, got:\n{myc}"
    );
}

/// NEVER-SILENT (VR-5/G2) — a `.map(..).map_err(..)` CHAIN over a resolvable base receiver: a
/// REAL-TOOLCHAIN finding (house rule #4) disconfirmed DN-135 §3 item 5's original "chains nest"
/// design (it was `Declared`/unverified) — a nested inlined `match` used as an outer match's
/// scrutinee does NOT `myc check`-clean without a type ascription this transpiler cannot generally
/// derive (see `combinator_receiver_kind`'s doc for the full empirical finding). So
/// `combinator_receiver_kind` never resolves a `MethodCall` receiver: the OUTER `.map_err`'s
/// receiver (the inner `MethodCall`) does not resolve, the combinator pass declines, and **G-β
/// Rank A** then refuses the bare free-fn desugar (`map_err(...)` is not a proven-emitted
/// referent) — the whole function gaps honestly rather than emitting an unsound nested `match`
/// **or** a fabricated free-fn around a partial inline (G2/VR-5).
#[test]
fn map_then_map_err_chain_declines_outer_and_gaps_never_emits_unsound_nesting() {
    let rust = "fn f(flag: u8, fallback: u8, r: Result<u8, u8>) -> Result<u8, u8> { \
                r.map(|()| flag).map_err(|_| fallback) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "expected NO emission (outer combinator declines + G-β Rank A refuses bare map_err), got \
         emitted_items={:?}, myc=\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("match (match") && !myc.contains("map_err("),
        "must NEVER emit a nested `match`-in-`match` chain or a fabricated bare `map_err(`, got:\n\
         {myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("no proven-emitted free-fn referent")
                || g.reason.contains("no explicit type annotation")),
        "expected G-β Rank A (or legacy DN-118) gap on the outer chain, got {:?}",
        report
            .gaps
            .iter()
            .map(|g| (g.category.as_str(), g.reason.as_str()))
            .collect::<Vec<_>>()
    );
}

/// A chain whose outer combinator declines must **not** fabricate a free-fn wrapper around a
/// partial inner inline (pre-G-β this emitted `map_err(match …, bump)` — a check-failing bare
/// `map_err`). G-β Rank A gaps the whole free fn. The INNER `.map` alone is still covered by
/// standalone inlining tests (`map_over_unit_closure_inlines_on_result_receiver`, etc.).
#[test]
fn chain_outer_map_err_function_value_gaps_never_fabricates_free_fn() {
    let rust =
        "fn f(flag: u8, r: Result<u8, u8>) -> Result<u8, u8> { r.map(|()| flag).map_err(bump) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "expected NO emission (outer `.map_err(bump)` is not a proven free-fn referent), got \
         emitted_items={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("map_err("),
        "must never emit bare `map_err(` (G-β Rank A poison stop), got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("no proven-emitted free-fn referent")),
        "expected G-β Rank A gap, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// The Option sibling: `.map(|()| E)` inlines identically off a confirmed `Option` receiver
/// (`Some`/`None` in place of `Ok`/`Err` — DN-135 §3 item 4 "Some/None variants for Option").
#[test]
fn map_over_unit_closure_inlines_on_option_receiver() {
    let rust = "fn f(flag: u8, o: Option<u8>) -> Option<u8> { o.map(|()| flag) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` in emitted_items, got {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("match (o) { Some(_) => Some(flag), None => None }"),
        "expected the inlined match body, got:\n{myc}"
    );
}

/// NEVER-SILENT (VR-5/G2) — the iterator `.map` false-fire stress test (DN-135 §5 stress #1): a
/// `.map`-named method on a receiver NOT known to be `Result`/`Option` must NOT fire the
/// combinator match-inline. Pre-G-β the OLD generic desugar still emitted bare `map(...)`
/// (file-poison); **G-β Rank A** gaps instead (no proven-emitted free-fn referent). The
/// receiver gate is the exact no-guess discipline `prim_map`'s `receiver_gate_matches` already
/// uses.
#[test]
fn map_on_non_result_option_receiver_never_inlines_and_never_fabricates() {
    let rust = "fn f(x: Thing) -> Thing { x.map(|z: Thing| z) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` to gap (no proven `map` free-fn referent on non-Result/Option), got \
         emitted_items={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("map(") && !myc.contains("match"),
        "must neither combinator-inline nor fabricate bare `map(`, got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("no proven-emitted free-fn referent")),
        "expected G-β Rank A gap, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// NEVER-SILENT (VR-5/G2) — an UNRESOLVED receiver (a Call expression this transpiler has no
/// return-type resolution for, DN-135 §5 stress #2's bounded-faithfulness point) makes the
/// combinator pass decline; **G-β Rank A** then refuses the bare free-fn desugar (never a
/// fabricated `Ok`/`Err` **or** bare `map(...)`).
#[test]
fn map_over_unresolved_call_receiver_gaps_never_fabricates() {
    let rust = "fn f() -> Result<u8, u8> { make_result().map(|()| 5) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "expected NO emission (unresolved receiver + Rank A / closure residual), got \
         emitted_items={:?}, myc=\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("Ok(") && !myc.contains("Err(") && !myc.contains("map("),
        "must never fabricate `Ok`/`Err` or bare `map(` for an unresolved receiver, got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("no proven-emitted free-fn referent")
                || g.reason.contains("no explicit type annotation")),
        "expected G-β Rank A (or legacy DN-118) gap, got {:?}",
        report
            .gaps
            .iter()
            .map(|g| (g.category.as_str(), g.reason.as_str()))
            .collect::<Vec<_>>()
    );
}

/// NEVER-SILENT (VR-5/G2) — a multi-parameter closure argument (DN-135 §3 item 3's "multi-param /
/// value-unsafe closure" fallthrough) declines combinator inline; **G-β Rank A** then refuses
/// bare `map(...)` free-fn desugar (the pre-G-β path re-derived DN-118 via the generic desugar's
/// arg emit — both end in a gap, Rank A never fabricates the free name first).
#[test]
fn map_with_multi_param_closure_declines_and_gaps() {
    let rust = "fn f(r: Result<u8, u8>) -> Result<u8, u8> { r.map(|a: u8, b: u8| a) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "expected NO emission (multi-param closure / Rank A), got emitted_items={:?}, myc=\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("map("),
        "must never emit bare `map(` for a declined combinator, got:\n{myc}"
    );
    assert!(
        report.gaps.iter().any(|g| {
            (g.category == Category::Closure
                && g.reason.contains("no auto-emittable Mechanical form"))
                || g.reason.contains("no proven-emitted free-fn referent")
        }),
        "expected DN-118 multi-param and/or G-β Rank A gap, got {:?}",
        report
            .gaps
            .iter()
            .map(|g| (g.category.as_str(), g.reason.as_str()))
            .collect::<Vec<_>>()
    );
}

/// NEVER-SILENT (VR-5/G2) — a closure that mutates a captured outer binding in place (DN-135 §5
/// stress #4's DN-109 D5/D7 safety gate, applied BEFORE inlining) declines combinator inline;
/// **G-β Rank A** then refuses bare `map(...)` free-fn desugar.
#[test]
fn map_with_capture_mutating_closure_declines_and_gaps() {
    let rust = "fn f(mut acc: u8, r: Result<u8, u8>) -> Result<u8, u8> { \
                r.map(|x: u8| { acc += x; acc }) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "expected NO emission (capture-mutating closure / Rank A), got emitted_items={:?}, \
         myc=\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("map("),
        "must never emit bare `map(` for a declined combinator, got:\n{myc}"
    );
    assert!(
        report.gaps.iter().any(|g| {
            (g.category == Category::Closure && g.reason.contains("cannot be proven value-safe"))
                || g.reason.contains("no proven-emitted free-fn referent")
        }),
        "expected DN-109 capture-mutation and/or G-β Rank A gap, got {:?}",
        report
            .gaps
            .iter()
            .map(|g| (g.category.as_str(), g.reason.as_str()))
            .collect::<Vec<_>>()
    );
}

/// A function-VALUE argument (no body to inline — Alt B's residual role, DN-135 §3 item 3): the
/// combinator pass does not touch it. Pre-G-β the generic desugar emitted bare `map(r, bump)`
/// (file-poison — `map` is not a proven free-fn). **G-β Rank A** gaps instead (G2/VR-5).
#[test]
fn map_with_function_value_argument_gaps_never_fabricates_free_fn() {
    let rust = "fn f(r: Result<u8, u8>) -> Result<u8, u8> { r.map(bump) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` to gap (function-value `.map` is not a proven free-fn referent), got \
         emitted_items={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("map(") && !myc.contains("match"),
        "must neither combinator-inline nor fabricate bare `map(`, got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("no proven-emitted free-fn referent")),
        "expected G-β Rank A gap, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// **The verify-first proof** (mitigation #14): every inlined form above is run through the REAL
/// `myc-check` oracle, proving the emitted text actually type-checks with zero imports (not just
/// a substring match). Skips gracefully (never fails) when `myc-check` is not built.
#[test]
fn inlined_combinator_forms_check_clean_against_real_toolchain() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "combinator: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or \
             build `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text \
             assertions above still cover the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-combinator-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    const NODULE_PATH: &str = "oracle";

    // NOTE: no chained-combinator case here (`.map(..).map_err(..)`) — the real-toolchain finding
    // documented on `combinator_receiver_kind` disconfirmed that shape (a nested inlined `match`
    // as an outer match's scrutinee does not `myc check`-clean without a type ascription this
    // transpiler cannot generally derive); chain-receiver resolution is deliberately unbuilt, so
    // there is no chain-shaped inline to differential-witness here.
    let rust_snippets = [
        "fn f_map(flag: u8, r: Result<u8, u8>) -> Result<u8, u8> { r.map(|()| flag) }",
        "fn f_map_err(fallback: u8, r: Result<u8, u8>) -> Result<u8, u8> { \
         r.map_err(|_| fallback) }",
        "fn f_and_then(r: Result<u8, u8>) -> Result<u8, u8> { r.and_then(|x| Ok(x)) }",
        "fn f_or_else(alt: u8, r: Result<u8, u8>) -> Result<u8, u8> { r.or_else(|_| Ok(alt)) }",
        "fn f_fold(dflt: u8, r: Result<u8, u8>) -> u8 { r.fold(|x: u8| x, |_| dflt) }",
        "fn f_option_map(flag: u8, o: Option<u8>) -> Option<u8> { o.map(|()| flag) }",
        "fn f_option_fold(dflt: u8, o: Option<u8>) -> u8 { o.fold(|x: u8| x, dflt) }",
    ];
    for (i, rust) in rust_snippets.iter().enumerate() {
        let (myc, report) = transpile_source(rust, "fixture.rs", NODULE_PATH)
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            !report.emitted_items.is_empty(),
            "case {i} (`{rust}`) failed to emit at all: gaps={:?}",
            report.gaps
        );
        // G-α Rank-1 ambient co-emit: Result/Option type shapes matching lib/std/result.myc /
        // option.myc are co-emitted when signatures mention them — no manual inject (would
        // double-define). Combinators stay emitted via the M-1092 rewrite path, not ambient.
        assert!(
            myc.contains("type Result[A, E] = Ok(A) | Err(E);")
                || myc.contains("type Option[A] = Some(A) | None;"),
            "case {i}: expected ambient Result and/or Option co-emit, got:\n{myc}"
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
