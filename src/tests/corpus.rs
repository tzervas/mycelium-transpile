//! The 8-core-lib-twin corpus (M-873 follow-on, DN-34 §8 / kickoff `trx`'s "first-class output"):
//! the never-silent invariant, checked directly against the **real** Rust crates backing 6 of the
//! 8 hand-written twins in `lib/std/*.myc` (`crates/mycelium-transpile/fixtures/UNION-BACKLOG.md`
//! is generated from a batch run over this same corpus — see that file's header for how to
//! regenerate it).
//!
//! **Guarantee: `Empirical`.** This is the batch-mode analogue of
//! `src/tests/invariant.rs`'s fixed-corpus check and `src/tests/diff.rs`'s single-crate
//! real-source check, generalized to every crate the union backlog measures — not `Proven` for
//! the same reason (`syn::Item` is `#[non_exhaustive]`; see `src/tests/invariant.rs`'s doc
//! comment).

use crate::batch::{discover_rs_files, transpile_batch};
use crate::gap::Category;
use std::path::PathBuf;

fn crate_src(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../{name}/src"))
}

/// The 6 Rust crates backing 6 of the 8 core-lib twins (`std.option`/`std.result` are
/// self-hosted directly in Mycelium — M-715/M-649 — with no Rust source to run; see
/// `fixtures/UNION-BACKLOG.md` §Flagged for the grounding).
fn corpus_crates() -> Vec<&'static str> {
    vec![
        "mycelium-std-cmp",
        "mycelium-std-iter",
        "mycelium-std-collections",
        "mycelium-std-text",
        "mycelium-std-fmt",
        "mycelium-std-math",
    ]
}

/// For every crate in the corpus, every file batch-transpiles without a hard parse failure, and
/// the never-silent invariant (`emitted_items.len() + gaps.len() >= total_top_level_items`) holds
/// for every file — the same sum-bound `src/tests/invariant.rs` checks over its fixed corpus,
/// checked here over real, unmodified crate source.
#[test]
fn never_silent_holds_over_the_union_backlog_corpus() {
    for crate_name in corpus_crates() {
        let src = crate_src(crate_name);
        assert!(
            src.is_dir(),
            "expected {crate_name}'s src/ dir at {}",
            src.display()
        );
        let files = discover_rs_files(&src).unwrap_or_else(|e| {
            panic!("failed to discover .rs files under {}: {e}", src.display())
        });
        assert!(
            !files.is_empty(),
            "expected at least one .rs file under {}",
            src.display()
        );

        let (results, failures) = transpile_batch(&files);
        assert!(
            failures.is_empty(),
            "expected every file under {crate_name}/src to parse, got failures={failures:?}"
        );

        for r in &results {
            let covered = r.report.emitted_items.len() + r.report.gaps.len();
            assert!(
                covered >= r.report.total_top_level_items,
                "never-silent invariant violated for {}: {} top-level item(s) but only \
                 {covered} emitted+gap record(s)",
                r.path.display(),
                r.report.total_top_level_items
            );
        }
    }
}

/// A cross-check the union backlog's headline numbers rest on: at least one crate in the corpus
/// has a non-trivial expressible fraction (catches a regression that would silently zero out
/// emission across the whole corpus, e.g. a botched refactor of `dispatch_item`).
#[test]
fn at_least_one_corpus_crate_has_nontrivial_expressible_fraction() {
    let mut any_nontrivial = false;
    for crate_name in corpus_crates() {
        let src = crate_src(crate_name);
        let files = discover_rs_files(&src).expect("discover succeeds");
        let (results, _failures) = transpile_batch(&files);
        let emitted: usize = results.iter().map(|r| r.report.emitted_items.len()).sum();
        let non_test: usize = results.iter().map(|r| r.report.non_test_item_count()).sum();
        if non_test > 0 && emitted as f64 / non_test as f64 > 0.05 {
            any_nontrivial = true;
        }
    }
    assert!(
        any_nontrivial,
        "expected at least one corpus crate with a >5% expressible fraction"
    );
}

/// **DN-138 (increment 1) — the honest DeriveAttr-class delta, measured, not asserted.**
///
/// **Verify-first finding (mitigation #14 / VR-5): `fixtures/UNION-BACKLOG.md`'s checked-in count
/// (8 `DeriveAttr` gaps, 2.4%) is STALE relative to this corpus at `@dev 871b166f`** — re-measuring
/// with `git stash` over JUST this leaf's derive-row changes (i.e. the pre-DN-138 code that was
/// otherwise already at `871b166f`) gives **67**, not 8. That drift predates this leaf entirely
/// (DN-136 Phase-2's `eq`/`ord`/`hash` rows, DN-134's struct-variant work, and other transpiler
/// changes landed after that fixture was last regenerated, each adding its own `DeriveAttr` gaps
/// over this corpus) — flagged here rather than silently used, and NOT fixed by this leaf
/// (regenerating the whole fixture — every category, every crate — is a separate, orchestrator-
/// scoped concern unrelated to DN-138's actual diff). The verified, checked baseline this test
/// diffs against is the **measured** pre-DN-138 count (67), not the stale doc's number.
#[test]
fn derive_attr_gap_count_decreases_over_the_union_backlog_corpus_dn138() {
    /// The MEASURED (not doc-transcribed) pre-DN-138 `DeriveAttr` gap count over this exact
    /// corpus — checked via `git stash` of just this leaf's `emit/derives/*.rs` changes, re-run,
    /// then restored (see this test's own doc for why this diverges from
    /// `fixtures/UNION-BACKLOG.md`'s stale checked-in "8"). `Empirical`, checked once by hand at
    /// authoring time — a real regression here (DeriveAttr count going back UP toward 67) is what
    /// this test exists to catch.
    const PRE_DN138_DERIVE_ATTR_COUNT: usize = 67;

    let mut derive_attr_count = 0usize;
    for crate_name in corpus_crates() {
        let src = crate_src(crate_name);
        let files = discover_rs_files(&src).expect("discover succeeds");
        let (results, _failures) = transpile_batch(&files);
        for r in &results {
            derive_attr_count += r
                .report
                .gaps
                .iter()
                .filter(|g| g.category == Category::DeriveAttr)
                .count();
        }
    }

    assert!(
        derive_attr_count < PRE_DN138_DERIVE_ATTR_COUNT,
        "DN-138 must strictly REDUCE the DeriveAttr gap count over the union-backlog corpus \
         (was {PRE_DN138_DERIVE_ATTR_COUNT} pre-DN-138, checked-in `fixtures/UNION-BACKLOG.md`); \
         got {derive_attr_count} — the scalar/Bytes/Bool unblock should have closed at least one \
         of them, never regressed the count upward"
    );
    eprintln!(
        "DN-138 measured DeriveAttr gap count over the union-backlog corpus: \
         {PRE_DN138_DERIVE_ATTR_COUNT} (pre) -> {derive_attr_count} (post)"
    );
    if std::env::var_os("MYC_DN138_SPOTCHECK").is_some() {
        for crate_name in corpus_crates() {
            let src = crate_src(crate_name);
            let files = discover_rs_files(&src).expect("discover succeeds");
            let (results, _failures) = transpile_batch(&files);
            for r in &results {
                for g in r
                    .report
                    .gaps
                    .iter()
                    .filter(|g| g.category == Category::DeriveAttr)
                {
                    eprintln!("- {}", g.reason);
                }
            }
        }
    }
}
