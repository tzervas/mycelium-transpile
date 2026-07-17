//! Unit tests for `src/gap.rs` — the `Category::is_non_gap_advisory` classification and
//! `GapReport::real_gap_count` headline total it feeds (review LOW on M-1086/#1544: the CLI's
//! one-line "N gap(s)" summary was inflating its total by counting `DeriveSatisfied` — a satisfied,
//! nothing-lost derive — as if it were a real coverage gap).

use crate::gap::{Category, Gap, GapReport};

fn mk_gap(category: Category) -> Gap {
    Gap {
        file: "fixture.rs".to_string(),
        line: 1,
        col: 1,
        category,
        rust_construct: category.as_str().to_string(),
        snippet: "…".to_string(),
        reason: "test fixture".to_string(),
        item_name: Some("Fixture".to_string()),
    }
}

fn mk_report(gaps: Vec<Gap>) -> GapReport {
    GapReport {
        source: "fixture.rs".to_string(),
        emitted_items: Vec::new(),
        gaps,
        total_top_level_items: 0,
    }
}

/// `DeriveSatisfied` is the ONE category currently classified as a non-gap advisory — a satisfied
/// no-op derive names no coverage loss, so it must not inflate a headline gap total.
#[test]
fn derive_satisfied_is_a_non_gap_advisory() {
    assert!(Category::DeriveSatisfied.is_non_gap_advisory());
}

/// `NamedFieldDrop` is DELIBERATELY excluded from the advisory set (see `Category::
/// is_non_gap_advisory`'s doc): it records a REAL, non-recoverable loss (field names dropped from
/// the emitted positional constructor) — surface-similar to `DeriveSatisfied` (both ride an
/// *emitted* item's `sub_gaps`), but semantically the opposite (something WAS lost). Confirms the
/// review's "check NamedFieldDrop's precedent" question resolves to "stays counted".
#[test]
fn named_field_drop_is_not_a_non_gap_advisory() {
    assert!(!Category::NamedFieldDrop.is_non_gap_advisory());
}

/// Every OTHER category (a spot-check sample, not exhaustive) is also not an advisory — the
/// classification is narrow by construction (VR-5: only exclude what genuinely names no loss).
#[test]
fn ordinary_gap_categories_are_not_advisories() {
    for cat in [
        Category::Trait,
        Category::Struct,
        Category::DeriveAttr,
        Category::Import,
        Category::Other,
    ] {
        assert!(
            !cat.is_non_gap_advisory(),
            "{cat:?} must not be classified as a non-gap advisory"
        );
    }
}

/// The headline total: a report with a mix of real gaps and `DeriveSatisfied` advisories reports
/// `real_gap_count()` as ONLY the real gaps — the exact inflation the review LOW named. The full,
/// unfiltered `gaps` list (and therefore `category_counts()`, computed from the same list) is left
/// untouched, so the per-category breakdown still shows the `DeriveSatisfied` row.
#[test]
fn real_gap_count_excludes_derive_satisfied_but_category_counts_keeps_it() {
    let report = mk_report(vec![
        mk_gap(Category::Struct),
        mk_gap(Category::DeriveSatisfied),
        mk_gap(Category::DeriveSatisfied),
        mk_gap(Category::Import),
    ]);

    assert_eq!(report.gaps.len(), 4, "raw gap count is unaffected");
    assert_eq!(
        report.real_gap_count(),
        2,
        "real_gap_count must exclude both DeriveSatisfied advisories, leaving Struct + Import"
    );

    let counts = report.category_counts();
    assert_eq!(
        counts.get("DeriveSatisfied").copied(),
        Some(2),
        "the per-category breakdown must still show DeriveSatisfied — only the headline total \
         excludes it"
    );
    let breakdown_total: usize = counts.values().sum();
    assert_eq!(
        breakdown_total,
        report.gaps.len(),
        "category_counts() must still sum to the RAW gap count, unaffected by the headline fix"
    );
}

/// `NamedFieldDrop` stays counted in `real_gap_count()` (the contrast case to the test above) — it
/// is a genuine fidelity loss, not an advisory.
#[test]
fn real_gap_count_keeps_named_field_drop() {
    let report = mk_report(vec![
        mk_gap(Category::NamedFieldDrop),
        mk_gap(Category::Struct),
    ]);
    assert_eq!(
        report.real_gap_count(),
        2,
        "NamedFieldDrop is a real fidelity-loss gap, not an advisory — must stay counted"
    );
}

/// A report with only advisories has a real gap count of zero, even though `gaps` is non-empty —
/// the degenerate case the "N gap(s)" fix specifically targets (a file with only satisfied derives
/// must not print a misleading nonzero gap headline).
#[test]
fn all_advisory_report_has_zero_real_gaps() {
    let report = mk_report(vec![
        mk_gap(Category::DeriveSatisfied),
        mk_gap(Category::DeriveSatisfied),
    ]);
    assert_eq!(report.gaps.len(), 2);
    assert_eq!(report.real_gap_count(), 0);
}

/// A report with no gaps at all is the trivial zero case — never a panic, never a spurious count.
#[test]
fn empty_gaps_report_has_zero_real_gaps() {
    let report = mk_report(Vec::new());
    assert_eq!(report.real_gap_count(), 0);
}
