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
/// `combinator_receiver_kind` never resolves a `MethodCall` receiver: the INNER `.map` still
/// inlines on its own (`r` resolves directly), but the OUTER `.map_err`'s receiver (the inner
/// `MethodCall`) does not resolve, so it declines and falls through to the unchanged generic
/// desugar — which then hits the pre-existing DN-118 gap on `.map_err`'s own `|_|` closure
/// (untyped wildcard). Net effect: the whole function gaps honestly rather than emitting an
/// unsound nested `match` — never a half-inlined/incorrect chain.
#[test]
fn map_then_map_err_chain_declines_outer_and_gaps_never_emits_unsound_nesting() {
    let rust = "fn f(flag: u8, fallback: u8, r: Result<u8, u8>) -> Result<u8, u8> { \
                r.map(|()| flag).map_err(|_| fallback) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "expected NO emission (the outer combinator's unresolved receiver sinks the whole fn via \
         the pre-existing DN-118 closure gap), got emitted_items={:?}, myc=\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("match (match"),
        "must NEVER emit a nested `match`-in-`match` chain (real-toolchain-disconfirmed \
         unsound), got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("no explicit type annotation")),
        "expected the pre-existing DN-118 closure-pattern gap on the outer `.map_err`'s `|_|`, \
         got {:?}",
        report
            .gaps
            .iter()
            .map(|g| (g.category.as_str(), g.reason.as_str()))
            .collect::<Vec<_>>()
    );
}

/// An individually-resolvable combinator STILL inlines even when it happens to sit inside a
/// larger chain shape — only the specific OUTER call whose receiver is unresolvable declines. Here
/// `.map(|()| flag)`'s own receiver `r` resolves directly, so it inlines on its own merits
/// (verified via a case where the outer call uses a function-value argument instead of a closure,
/// so nothing downstream of the inner `.map` gaps).
#[test]
fn inner_combinator_of_a_chain_still_inlines_on_its_own_resolvable_receiver() {
    let rust =
        "fn f(flag: u8, r: Result<u8, u8>) -> Result<u8, u8> { r.map(|()| flag).map_err(bump) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` in emitted_items, got {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("map_err(match (r) { Ok(_) => Ok(flag), Err(e) => Err(e) }, bump)"),
        "expected the INNER `.map` inlined, with the OUTER `.map_err` falling through to the \
         unchanged generic free-function call (its receiver — the inner `MethodCall` — is \
         unresolved), got:\n{myc}"
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
/// `.map`-named method on a receiver NOT known to be `Result`/`Option` is left COMPLETELY
/// untouched — the OLD generic desugar (`map(x, lambda(z: Thing) => z)`) still fires, never the
/// combinator match-inline. The receiver gate is the exact no-guess discipline `prim_map`'s
/// `receiver_gate_matches` already uses.
#[test]
fn map_on_non_result_option_receiver_is_untouched() {
    let rust = "fn f(x: Thing) -> Thing { x.map(|z: Thing| z) }";
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
        myc.contains("map(x, lambda(z: Thing) => z)") && !myc.contains("match"),
        "expected the UNCHANGED generic desugar, no `match`-inline, since `Thing` is not a known \
         Result/Option receiver — got:\n{myc}"
    );
}

/// NEVER-SILENT (VR-5/G2) — an UNRESOLVED receiver (a Call expression this transpiler has no
/// return-type resolution for, DN-135 §5 stress #2's bounded-faithfulness point) makes the
/// combinator pass decline; the call falls through to the unchanged generic desugar, which then
/// hits the PRE-EXISTING DN-118 closure-pattern gap for the `|()|` param (never a fabricated
/// `Ok`/`Err`).
#[test]
fn map_over_unresolved_call_receiver_gaps_never_fabricates() {
    let rust = "fn f() -> Result<u8, u8> { make_result().map(|()| 5) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "expected NO emission (the closure-pattern gap should sink the whole fn), got \
         emitted_items={:?}, myc=\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("Ok(") && !myc.contains("Err("),
        "must never fabricate an `Ok`/`Err` construction for an unresolved receiver, got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("no explicit type annotation")),
        "expected the pre-existing DN-118 closure-pattern gap, got {:?}",
        report
            .gaps
            .iter()
            .map(|g| (g.category.as_str(), g.reason.as_str()))
            .collect::<Vec<_>>()
    );
}

/// NEVER-SILENT (VR-5/G2) — a multi-parameter closure argument (DN-135 §3 item 3's "multi-param /
/// value-unsafe closure" fallthrough) declines to inline and inherits the pre-existing DN-118
/// multi-param gap unchanged.
#[test]
fn map_with_multi_param_closure_declines_and_gaps() {
    let rust = "fn f(r: Result<u8, u8>) -> Result<u8, u8> { r.map(|a: u8, b: u8| a) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "expected NO emission (multi-param closure gaps), got emitted_items={:?}, myc=\n{myc}",
        report.emitted_items
    );
    assert!(
        report.gaps.iter().any(|g| g.category == Category::Closure
            && g.reason.contains("no auto-emittable Mechanical form")),
        "expected the pre-existing DN-118 multi-param gap, got {:?}",
        report
            .gaps
            .iter()
            .map(|g| (g.category.as_str(), g.reason.as_str()))
            .collect::<Vec<_>>()
    );
}

/// NEVER-SILENT (VR-5/G2) — a closure that mutates a captured outer binding in place (DN-135 §5
/// stress #4's DN-109 D5/D7 safety gate, applied BEFORE inlining) declines to inline and
/// inherits the pre-existing capture-mutation gap unchanged.
#[test]
fn map_with_capture_mutating_closure_declines_and_gaps() {
    let rust = "fn f(mut acc: u8, r: Result<u8, u8>) -> Result<u8, u8> { \
                r.map(|x: u8| { acc += x; acc }) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "expected NO emission (capture-mutating closure gaps), got emitted_items={:?}, \
         myc=\n{myc}",
        report.emitted_items
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.category == Category::Closure
                && g.reason.contains("cannot be proven value-safe")),
        "expected the pre-existing DN-109 capture-mutation gap, got {:?}",
        report
            .gaps
            .iter()
            .map(|g| (g.category.as_str(), g.reason.as_str()))
            .collect::<Vec<_>>()
    );
}

/// A function-VALUE argument (no body to inline — Alt B's residual role, DN-135 §3 item 3) keeps
/// the unchanged `m(recv, f)` free-function call; the combinator pass does not touch it.
#[test]
fn map_with_function_value_argument_keeps_generic_call() {
    let rust = "fn f(r: Result<u8, u8>) -> Result<u8, u8> { r.map(bump) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` in emitted_items, got {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("map(r, bump)") && !myc.contains("match"),
        "expected the UNCHANGED generic free-function call, no `match`-inline, got:\n{myc}"
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
        // A self-contained nodule needs `Result`/`Option` declared locally so `myc check` can
        // resolve `Ok`/`Err`/`Some`/`None` with no cross-nodule import (a standalone `.myc` file
        // has no `lib/std` search path here) — the SAME `type Result[A, E] = Ok(A) | Err(E);` /
        // `type Option[A] = Some(A) | None;` shapes `lib/std/result.myc:10`/`lib/std/option.myc:9`
        // declare, inserted right after the rendered nodule header.
        let full = myc.replacen(
            &format!("nodule {NODULE_PATH};\n\n"),
            &format!(
                "nodule {NODULE_PATH};\n\ntype Result[A, E] = Ok(A) | Err(E);\ntype Option[A] = \
                 Some(A) | None;\n\n"
            ),
            1,
        );
        assert_ne!(
            full, myc,
            "case {i}: expected the nodule-header insertion point to be found, got:\n{myc}"
        );
        let path = dir.join(format!("case_{i}.myc"));
        std::fs::write(&path, &full).expect("write case .myc");

        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "case {i} (`{rust}`) must check CLEAN with the real myc-check oracle — emitted:\n{full}\n\
             diagnostic={:?}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
