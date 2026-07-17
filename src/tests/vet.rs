//! Unit tests for the transpile → `myc check` vet loop (`src/vet.rs`, M-1000).
//!
//! **Guarantee: `Empirical`** for the live oracle test (it measures the real toolchain);
//! `Declared`/pure for the classification + aggregation tests (they exercise deterministic logic
//! over hand-built inputs, no process spawn — so they run fast and never depend on the toolchain
//! being present). Complex setup stays in fixtures/tables per the house test-layout rule; each test
//! body is `assert over a case`.

use crate::gap::{Category, Gap, GapReport};
use crate::vet::{
    classify_run, phylum_checked_clean_items, vet_batch, MycChecker, PhylumNodule,
    PhylumVetSummary, VetClass, VetInput, VetRecord, VetReport, MAX_DIAGNOSTIC_LEN,
};
use std::path::{Path, PathBuf};

// ─────────────────────────────────────────────────────────────────────────────────────────────
// VetClass::from_exit_code — the exit-contract mapping (data-driven).
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Every documented `myc-check` oracle exit code maps to exactly its class; `None` (signal, no code)
/// and every unknown code map to a non-`Clean` class — an unknown outcome is **never** read as
/// clean (G2/VR-5).
#[test]
fn exit_code_maps_to_class() {
    let cases: &[(Option<i32>, VetClass)] = &[
        (Some(0), VetClass::Clean),
        (Some(2), VetClass::ParseError),
        (Some(3), VetClass::CheckError),
        (Some(64), VetClass::Usage),
        (Some(66), VetClass::Io),
        (Some(5), VetClass::Other(5)),
        (Some(101), VetClass::Other(101)),
        (None, VetClass::ToolUnavailable),
    ];
    for (code, expect) in cases {
        assert_eq!(
            VetClass::from_exit_code(*code),
            *expect,
            "exit code {code:?} misclassified"
        );
        // Only exit 0 is ever clean.
        assert_eq!(
            VetClass::from_exit_code(*code).is_clean(),
            *code == Some(0),
            "is_clean wrong for {code:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// classify_run — diagnostic extraction chooses the informative stream per class.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A clean run carries no diagnostic; a parse/check failure lifts the oracle's stdout
/// `parse-error:`/`check-error:` line; an I/O/unavailable outcome prefers stderr. Never picks an
/// empty line over a non-empty one.
#[test]
fn classify_run_picks_the_right_diagnostic_line() {
    // Clean: no diagnostic.
    let clean = classify_run("f.myc".into(), "f.rs".into(), Some(0), "ok\n", "", 3, 3);
    assert_eq!(clean.class, VetClass::Clean);
    assert!(clean.diagnostic.is_empty(), "clean run has no diagnostic");

    // Check error: the `check-error:` line is on stdout (oracle contract).
    let check = classify_run(
        "f.myc".into(),
        "f.rs".into(),
        Some(3),
        "\ncheck-error: `impl` for unknown trait `Widen`\n",
        "myc-check: 1 finding\n",
        5,
        2,
    );
    assert_eq!(check.class, VetClass::CheckError);
    assert_eq!(
        check.diagnostic,
        "check-error: `impl` for unknown trait `Widen`"
    );

    // Parse error: stdout too.
    let parse = classify_run(
        "f.myc".into(),
        "f.rs".into(),
        Some(2),
        "parse-error: expected a pattern, found Strength(Exact)\n",
        "",
        4,
        1,
    );
    assert_eq!(parse.class, VetClass::ParseError);
    assert!(parse.diagnostic.starts_with("parse-error:"));

    // I/O: prefers stderr.
    let io = classify_run(
        "f.myc".into(),
        "f.rs".into(),
        Some(66),
        "",
        "io-error: nope\n",
        1,
        0,
    );
    assert_eq!(io.class, VetClass::Io);
    assert_eq!(io.diagnostic, "io-error: nope");
}

/// A diagnostic longer than the cap is truncated with a marker, never fully dropped.
#[test]
fn long_diagnostic_is_truncated_not_dropped() {
    let long = format!("check-error: {}", "x".repeat(MAX_DIAGNOSTIC_LEN * 2));
    let rec = classify_run("f.myc".into(), "f.rs".into(), Some(3), &long, "", 1, 1);
    assert!(!rec.diagnostic.is_empty(), "diagnostic not dropped");
    assert!(
        rec.diagnostic.chars().count() <= MAX_DIAGNOSTIC_LEN + 1,
        "diagnostic bounded to the cap (+ the `…` marker)"
    );
    assert!(rec.diagnostic.ends_with('…'), "truncation marker present");
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// VetRecord::checked_clean_items — the file-gated bridge.
// ─────────────────────────────────────────────────────────────────────────────────────────────

fn record(class: VetClass, non_test: usize, emitted: usize) -> VetRecord {
    VetRecord {
        myc_file: "x.myc".into(),
        source_file: "x.rs".into(),
        class,
        exit_code: None,
        diagnostic: String::new(),
        non_test_items: non_test,
        emitted_items: emitted,
    }
}

/// A file's emitted items credit the checked numerator iff the whole file is clean; a failing file
/// contributes 0 (all-or-nothing per file — never a guessed partial attribution).
#[test]
fn checked_clean_items_is_file_gated_all_or_nothing() {
    assert_eq!(record(VetClass::Clean, 10, 4).checked_clean_items(), 4);
    assert_eq!(record(VetClass::CheckError, 10, 4).checked_clean_items(), 0);
    assert_eq!(record(VetClass::ParseError, 10, 4).checked_clean_items(), 0);
    assert_eq!(
        record(VetClass::ToolUnavailable, 10, 4).checked_clean_items(),
        0,
        "a tool-unavailable run credits nothing (never counted as clean)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// VetReport aggregation + the two fractions (denominator = non-test items, stated).
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Aggregation sums per-file counts, the shared denominator is total non-test items, and
/// `checked_fraction ≤ expressible_fraction` always holds (an item can only be checked-clean if it
/// was emitted). A failing file zeroes its checked credit but still contributes to the denominator.
#[test]
fn vet_report_fractions_over_stated_denominator() {
    // File A: 10 items, 4 emitted, clean → 4 checked-clean.
    // File B: 10 items, 5 emitted, check-error → 0 checked-clean (poisoned).
    // Denominator = 20 non-test items. Emitted = 9. Checked-clean = 4.
    let report = VetReport::from_records(vec![
        record(VetClass::Clean, 10, 4),
        record(VetClass::CheckError, 10, 5),
    ]);
    assert_eq!(report.total_non_test_items, 20);
    assert_eq!(report.total_emitted_items, 9);
    assert_eq!(report.total_checked_clean_items, 4);
    assert!((report.expressible_fraction() - 9.0 / 20.0).abs() < 1e-9);
    assert!((report.checked_fraction() - 4.0 / 20.0).abs() < 1e-9);
    assert!(
        report.checked_fraction() <= report.expressible_fraction(),
        "checked_fraction must never exceed expressible_fraction"
    );

    // Per-class file counts and the clean-file companion metric.
    assert_eq!(report.class_counts.get("Clean"), Some(&1));
    assert_eq!(report.class_counts.get("CheckError"), Some(&1));
    let (clean_files, files_with_emissions) = report.clean_file_fraction();
    assert_eq!((clean_files, files_with_emissions), (1, 2));
}

/// An empty report (zero files/items) yields honest all-zero fractions, never a divide-by-zero
/// panic or a fabricated ratio.
#[test]
fn vet_report_over_zero_items_is_all_zero_not_a_panic() {
    let report = VetReport::from_records(vec![]);
    assert_eq!(report.total_non_test_items, 0);
    assert_eq!(report.checked_fraction(), 0.0);
    assert_eq!(report.expressible_fraction(), 0.0);
    assert_eq!(report.clean_file_fraction(), (0, 0));

    // A file with items but zero emissions: 0/N, and it is not counted as a clean *draft*.
    let report2 = VetReport::from_records(vec![record(VetClass::Clean, 5, 0)]);
    assert_eq!(report2.checked_fraction(), 0.0);
    assert_eq!(report2.expressible_fraction(), 0.0);
    assert_eq!(
        report2.clean_file_fraction(),
        (0, 0),
        "a header-only (zero-emission) clean nodule is not a clean draft"
    );
}

/// `VetInput::from_report` reads the per-file counts straight off a `GapReport`, so the vet
/// denominator matches the report's own `non_test_item_count`.
#[test]
fn vet_input_reads_counts_from_gap_report() {
    let report = GapReport {
        source: "s.rs".into(),
        emitted_items: vec!["A".into(), "B".into()],
        gaps: vec![Gap {
            file: "s.rs".into(),
            line: 1,
            col: 1,
            category: Category::TestItem,
            rust_construct: Category::TestItem.as_str().into(),
            snippet: String::new(),
            reason: String::new(),
            item_name: None,
        }],
        total_top_level_items: 3, // 3 total, 1 test → 2 non-test.
    };
    let input = VetInput::from_report(PathBuf::from("s.myc"), &report);
    assert_eq!(input.non_test_items, 2);
    assert_eq!(input.emitted_items, 2);
    assert_eq!(input.source_file, "s.rs");
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// A tool-unavailable checker is recorded, never fatal (never-silent, never a hard stop).
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Vetting with a checker that cannot be spawned yields a `ToolUnavailable` record (not a panic /
/// not a silent skip), and that record credits nothing to `checked_fraction`.
#[test]
fn unavailable_checker_records_tool_unavailable() {
    let checker = MycChecker {
        command: vec!["/nonexistent/definitely-not-a-real-binary-xyz".into()],
        cwd: None,
    };
    let inputs = vec![VetInput {
        myc_path: PathBuf::from("/tmp/does-not-need-to-exist.myc"),
        source_file: "x.rs".into(),
        non_test_items: 3,
        emitted_items: 2,
    }];
    let report = vet_batch(&checker, &inputs);
    assert_eq!(report.records.len(), 1);
    assert_eq!(report.records[0].class, VetClass::ToolUnavailable);
    assert_eq!(report.total_checked_clean_items, 0);
    assert_eq!(report.checked_fraction(), 0.0);
    assert!(
        report.records[0].diagnostic.contains("could not run"),
        "the unavailable-tool diagnostic names the failure, never silent"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// DN-124 / M-1079 Unit 2 — the phylum-mode dual-report (`phylum_checked_clean_items`,
// `VetReport::with_phylum`/`checked_fraction_phylum`/`delta_basis`). Pure logic, no process spawn.
// ─────────────────────────────────────────────────────────────────────────────────────────────

fn nodule(file: &str, class: &str) -> PhylumNodule {
    PhylumNodule {
        nodule: file.trim_end_matches(".myc").replace('/', "."),
        file: file.to_owned(),
        class: class.to_owned(),
        site: None,
        on: None,
        message: None,
    }
}

fn vet_input(myc_path: &str, non_test_items: usize, emitted_items: usize) -> VetInput {
    VetInput {
        myc_path: PathBuf::from(myc_path),
        source_file: format!("{myc_path}.rs"),
        non_test_items,
        emitted_items,
    }
}

/// Only a `Clean`-classed nodule credits its file's `emitted_items`; `CheckError`/`Blocked` credit
/// nothing (the same file-gated bridge oracle mode uses, generalized to nodule-Clean).
#[test]
fn phylum_checked_clean_items_credits_only_clean_nodules() {
    let dir = Path::new("/out");
    let inputs = vec![
        vet_input("/out/a.myc", 10, 4),
        vet_input("/out/b.myc", 10, 5),
        vet_input("/out/c.myc", 10, 3),
    ];
    let summary = PhylumVetSummary {
        ran: true,
        ok: false,
        nodules: vec![
            nodule("a.myc", "Clean"),
            nodule("b.myc", "CheckError"),
            nodule("c.myc", "Blocked"),
        ],
        diagnostic: "check error at `b.<use>`".to_owned(),
    };
    let credited = phylum_checked_clean_items(dir, &inputs, &summary);
    assert_eq!(credited, 4, "only a.myc (Clean) is credited");
}

/// A run that could not be executed (`ran: false`) credits **nothing** — never a fabricated clean
/// result when there is no real verdict to back it (G2/VR-5).
#[test]
fn phylum_checked_clean_items_never_credits_an_unran_summary() {
    let dir = Path::new("/out");
    let inputs = vec![vet_input("/out/a.myc", 10, 4)];
    let summary = PhylumVetSummary {
        ran: false,
        ok: false,
        // Even a (hypothetically) fabricated Clean row must not be trusted when `ran` is false —
        // the guard is on `ran`, never on the presence of rows.
        nodules: vec![nodule("a.myc", "Clean")],
        diagnostic: "could not run myc-check --phylum: tool not found".to_owned(),
    };
    assert_eq!(phylum_checked_clean_items(dir, &inputs, &summary), 0);
}

/// The file-join is exact-relative-path (mirrors `mycelium-check`'s `collect_myc` convention): a
/// nested batch output (`<out_dir>/sub/dir/foo.myc`) still joins correctly against its nodule row's
/// `file` (`sub/dir/foo.myc`, forward-slashed relative to `dir`).
#[test]
fn phylum_checked_clean_items_joins_nested_batch_paths() {
    let dir = Path::new("/out");
    let inputs = vec![vet_input("/out/sub/dir/foo.myc", 6, 6)];
    let summary = PhylumVetSummary {
        ran: true,
        ok: true,
        nodules: vec![nodule("sub/dir/foo.myc", "Clean")],
        diagnostic: String::new(),
    };
    assert_eq!(phylum_checked_clean_items(dir, &inputs, &summary), 6);
}

/// `VetReport::with_phylum` dual-reports `checked_fraction_phylum` alongside the oracle-mode
/// `checked_fraction`, over the SAME denominator — and `delta_basis` is exactly their difference,
/// simulating the DN-124 lever: a file that was oracle-`CheckError` (an unresolved cross-nodule
/// `use`) becomes phylum-`Clean` once its whole import closure is visible.
#[test]
fn with_phylum_dual_reports_and_delta_basis_is_the_recovered_delta() {
    // Oracle mode: a.myc clean (4 items), b.myc CheckError (5 items) -> checked_fraction = 4/20.
    let report = VetReport::from_records(vec![
        record_at("a.myc", VetClass::Clean, 10, 4),
        record_at("b.myc", VetClass::CheckError, 10, 5),
    ]);
    assert!((report.checked_fraction() - 4.0 / 20.0).abs() < 1e-9);
    assert!(
        report.phylum.is_none(),
        "an oracle-only report attaches no phylum result"
    );
    assert_eq!(
        report.delta_basis(),
        0.0,
        "nothing to correct for without a phylum result"
    );

    let dir = Path::new("/out");
    let inputs = vec![
        vet_input("/out/a.myc", 10, 4),
        vet_input("/out/b.myc", 10, 5),
    ];
    let summary = PhylumVetSummary {
        ran: true,
        ok: true,
        nodules: vec![nodule("a.myc", "Clean"), nodule("b.myc", "Clean")],
        diagnostic: String::new(),
    };
    let dual = report.with_phylum(dir, &inputs, summary);
    // Phylum mode recovers b.myc too: (4+5)/20 = 9/20.
    assert_eq!(dual.total_checked_clean_items_phylum, 9);
    assert!((dual.checked_fraction_phylum() - 9.0 / 20.0).abs() < 1e-9);
    // checked_fraction (oracle) is UNCHANGED by attaching phylum.
    assert!((dual.checked_fraction() - 4.0 / 20.0).abs() < 1e-9);
    // Δ_basis is exactly phylum - oracle, over the SAME denominator (never a different basis).
    let expected_delta = 9.0 / 20.0 - 4.0 / 20.0;
    assert!((dual.delta_basis() - expected_delta).abs() < 1e-9);
    assert!(
        dual.delta_basis() > 0.0,
        "phylum mode must recover, never regress, here"
    );
}

/// Attaching a phylum result whose tool did not run (`ran: false`) reports `checked_fraction_phylum
/// == 0.0` honestly (never inherits/guesses the oracle numerator) — and `delta_basis` is negative in
/// that degenerate case, which is exactly why the CLI only prints the phylum line when `ran`.
#[test]
fn with_phylum_unran_never_fabricates_a_recovered_fraction() {
    let report = VetReport::from_records(vec![record_at("a.myc", VetClass::Clean, 10, 4)]);
    let dir = Path::new("/out");
    let inputs = vec![vet_input("/out/a.myc", 10, 4)];
    let summary = PhylumVetSummary {
        ran: false,
        ok: false,
        nodules: vec![],
        diagnostic: "could not run".to_owned(),
    };
    let dual = report.with_phylum(dir, &inputs, summary);
    assert_eq!(dual.total_checked_clean_items_phylum, 0);
    assert_eq!(dual.checked_fraction_phylum(), 0.0);
}

/// A record helper carrying an explicit `myc_file` label (the plain [`record`] helper above always
/// uses `"x.myc"`, which collides across cases needing distinct file identities for the join tests).
fn record_at(myc_file: &str, class: VetClass, non_test: usize, emitted: usize) -> VetRecord {
    VetRecord {
        myc_file: myc_file.to_owned(),
        source_file: format!("{myc_file}.rs"),
        class,
        exit_code: None,
        diagnostic: String::new(),
        non_test_items: non_test,
        emitted_items: emitted,
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Live end-to-end witness against the REAL `myc check` — skip-gracefully when it isn't built.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Locate a runnable `myc-check`: `MYC_CHECK_CMD` (first whitespace token), else the workspace
/// `target/debug/myc-check`. Returns `None` (→ graceful skip) when neither is present — this test
/// must not *build* the checker (it isn't a dep of this crate), only exercise it if already built.
///
/// `pub(in crate::tests)` (not private/`pub`): the `binop_operand_gated_forms_check_clean` live
/// oracle in `src/tests/emit.rs` and the forward-map oracle tests in `src/tests/prim_map.rs` reuse
/// this exact helper (DRY, CLAUDE.md house rule 5) instead of each keeping a drifting copy — scoped
/// to `crate::tests` since it is test-only infrastructure, never part of the crate's real API.
pub(in crate::tests) fn find_myc_check() -> Option<PathBuf> {
    if let Ok(cmd) = std::env::var("MYC_CHECK_CMD") {
        if let Some(first) = cmd.split_whitespace().next() {
            let p = PathBuf::from(first);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let built = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/myc-check");
    if built.exists() {
        Some(built)
    } else {
        None
    }
}

/// End-to-end: a hand-written, known-clean `.myc` classifies `Clean`; a known-broken one (an
/// unresolved `use`) classifies `CheckError`. Skips (never fails) when `myc-check` is not built.
#[test]
fn live_myc_check_classifies_clean_and_broken() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "vet: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). Pure vet tests still cover the logic."
        );
        return;
    };
    let checker = MycChecker {
        command: vec![bin.display().to_string()],
        cwd: None,
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-vet-live-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    // Known-clean: a nullary sum type + a total projection (both confirmed to check by the profile).
    let clean = dir.join("clean.myc");
    std::fs::write(
        &clean,
        "// nodule: p\nnodule p;\n\ntype Ordering = Lt | Eq | Gt;\n",
    )
    .expect("write clean.myc");
    let clean_rec = checker.vet_file(&clean, "clean.rs", 1, 1);
    assert_eq!(
        clean_rec.class,
        VetClass::Clean,
        "known-clean .myc must classify Clean; diagnostic={:?}",
        clean_rec.diagnostic
    );
    assert_eq!(clean_rec.checked_clean_items(), 1);

    // Known-broken: an unresolved external `use` (the dominant real-toolchain check poison).
    let broken = dir.join("broken.myc");
    std::fs::write(
        &broken,
        "// nodule: p\nnodule p;\n\nuse mycelium_core.GuaranteeStrength;\ntype X = A | B;\n",
    )
    .expect("write broken.myc");
    let broken_rec = checker.vet_file(&broken, "broken.rs", 1, 1);
    assert_eq!(
        broken_rec.class,
        VetClass::CheckError,
        "an unresolved `use` must classify CheckError; diagnostic={:?}",
        broken_rec.diagnostic
    );
    assert_eq!(
        broken_rec.checked_clean_items(),
        0,
        "a check-failing file credits nothing"
    );
    assert!(
        !broken_rec.diagnostic.is_empty(),
        "the failure diagnostic is captured, never silent"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **The DN-124 lever, end-to-end against the real toolchain.** Two nodules in ONE directory: `a`
/// exports a `pub fn`; `b` imports it (`use a.*;`). Under **oracle** (per-file) mode `b` is
/// `CheckError` (a phylum-of-one cannot resolve `a.*`) — under **phylum** mode (this whole dir as one
/// phylum) `b` resolves and is credited `Clean`. Asserts `checked_fraction_phylum >
/// checked_fraction` (strict recovery, never a regression) and that `a` (which has no `use` at all)
/// stays `Clean` under both bases (DN-124's own guarantee: an oracle-clean file is never worsened by
/// switching to phylum mode). Skips gracefully when `myc-check` is not built.
#[test]
fn live_phylum_mode_recovers_a_cross_nodule_use_oracle_mode_false_fails() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "vet: live phylum test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`)."
        );
        return;
    };
    let checker = MycChecker {
        command: vec![bin.display().to_string()],
        cwd: None,
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-vet-phylum-live-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let a_path = dir.join("a.myc");
    std::fs::write(
        &a_path,
        "nodule a;\npub fn helper(x: Binary{8}) => Binary{8} = not(x);\n",
    )
    .expect("write a.myc");
    let b_path = dir.join("b.myc");
    std::fs::write(
        &b_path,
        "nodule b;\nuse a.*;\nfn g(x: Binary{8}) => Binary{8} = helper(x);\n",
    )
    .expect("write b.myc");

    // Oracle mode (per-file, phylum-blind): a is Clean (no use), b is CheckError (unresolved a.*).
    let a_oracle = checker.vet_file(&a_path, "a.rs", 1, 1);
    let b_oracle = checker.vet_file(&b_path, "b.rs", 1, 1);
    assert_eq!(a_oracle.class, VetClass::Clean, "{:?}", a_oracle.diagnostic);
    assert_eq!(
        b_oracle.class,
        VetClass::CheckError,
        "b must false-FAIL under oracle mode — this IS the DN-124 problem statement: {:?}",
        b_oracle.diagnostic
    );

    let oracle_report = VetReport::from_records(vec![a_oracle, b_oracle]);
    assert_eq!(
        oracle_report.total_checked_clean_items, 1,
        "only a credited"
    );

    // Phylum mode over the whole dir: both a AND b are credited.
    let phylum = checker.vet_phylum(&dir);
    assert!(
        phylum.ran,
        "the real myc-check must be runnable here (it was just used for oracle mode): {:?}",
        phylum.diagnostic
    );
    assert!(
        phylum.ok,
        "the phylum should check clean end-to-end: {:?}",
        phylum.diagnostic
    );
    let by_nodule: std::collections::BTreeMap<&str, &PhylumNodule> = phylum
        .nodules
        .iter()
        .map(|n| (n.nodule.as_str(), n))
        .collect();
    assert!(by_nodule["a"].is_clean(), "{:?}", by_nodule["a"]);
    assert!(
        by_nodule["b"].is_clean(),
        "b must be credited Clean under phylum mode -- the DN-124 fix: {:?}",
        by_nodule["b"]
    );

    let inputs = vec![
        VetInput {
            myc_path: a_path.clone(),
            source_file: "a.rs".to_owned(),
            non_test_items: 1,
            emitted_items: 1,
        },
        VetInput {
            myc_path: b_path.clone(),
            source_file: "b.rs".to_owned(),
            non_test_items: 1,
            emitted_items: 1,
        },
    ];
    let dual = oracle_report.with_phylum(&dir, &inputs, phylum);
    assert_eq!(
        dual.total_checked_clean_items_phylum, 2,
        "both a and b credited under phylum mode"
    );
    assert!(
        dual.checked_fraction_phylum() > dual.checked_fraction(),
        "phylum mode must strictly recover here: phylum={} oracle={}",
        dual.checked_fraction_phylum(),
        dual.checked_fraction()
    );
    assert!(
        (dual.delta_basis() - (dual.checked_fraction_phylum() - dual.checked_fraction())).abs()
            < 1e-9
    );

    let _ = std::fs::remove_dir_all(&dir);
}
