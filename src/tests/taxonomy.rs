//! M-1006 Phase-2 (kickoff `trx2`, E33-1) — the whole-corpus taxonomy refinement: bodyless
//! `mod foo;` file-linkage declarations and crate/file-level inner attributes (`#![…]`) get their
//! own honest [`Category`] instead of the opaque `Other`, and the bodyless `mod foo;` is excluded
//! from the expressible-fraction denominator exactly as a `#[cfg(test)]` item is (recorded, never
//! dropped — G2/VR-5). These are data-driven asserts over small fixtures, not bespoke logic.

use crate::gap::Category;
use crate::transpile::transpile_source;

/// A bodyless `mod foo;` is file-linkage, not translatable library surface: it is recorded as a
/// [`Category::ModuleDecl`] gap (never silently dropped) **and** excluded from the denominator.
#[test]
fn external_mod_decl_is_module_decl_and_denominator_excluded() {
    // Two items: one emittable fn + one `mod foo;`. The denominator must count only the fn.
    let rust = "fn a() -> bool { true }\nmod foo;";
    let (_myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));

    assert_eq!(report.total_top_level_items, 2, "two top-level items");
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.category == Category::ModuleDecl),
        "expected a ModuleDecl gap for `mod foo;`, got {:?}",
        report.category_counts()
    );
    // Denominator excludes the module-decl → 1, not 2 (the identical treatment a test item gets).
    assert_eq!(
        report.non_test_item_count(),
        1,
        "the bodyless `mod foo;` must be excluded from the expressible-fraction denominator"
    );
    assert_eq!(report.denominator_excluded_count(), 1);
}

/// `pub mod foo;` is the same file-linkage case (visibility does not change the disposition).
#[test]
fn pub_external_mod_decl_is_module_decl() {
    let rust = "pub mod backend;";
    let (_myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(report
        .gaps
        .iter()
        .any(|g| g.category == Category::ModuleDecl));
    assert_eq!(
        report.non_test_item_count(),
        0,
        "a file that is only a `pub mod foo;` has zero translatable-surface items"
    );
}

/// An **inline** `mod foo { … }` is *not* file-linkage — its body is real dropped content, so it
/// stays a counted `Other` coverage gap and is **not** excluded from the denominator (VR-5: only
/// genuine non-surface is excluded; a dropped body is a real gap).
#[test]
fn inline_mod_stays_counted_other_gap() {
    let rust = "mod inner { fn helper() -> bool { true } }";
    let (_myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.category == Category::Other && g.reason.contains("inline `mod")),
        "inline mod must be a counted Other gap, got {:?}",
        report.category_counts()
    );
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| g.category == Category::ModuleDecl),
        "inline mod must NOT be reclassified as file-linkage ModuleDecl"
    );
    assert_eq!(
        report.non_test_item_count(),
        1,
        "inline mod stays in the denominator (its dropped body is a real coverage gap)"
    );
}

/// A crate/file-level inner attribute (`#![…]`) is not a `syn::Item` — it is recorded as a
/// [`Category::InnerAttr`] gap (its own honest label, not `Other`), and it does **not** change the
/// denominator (it was never in `total_top_level_items`).
#[test]
fn inner_attribute_is_inner_attr_not_other_and_leaves_denominator_untouched() {
    let rust = "#![forbid(unsafe_code)]\nfn a() -> bool { true }";
    let (_myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.category == Category::InnerAttr),
        "expected an InnerAttr gap for `#![forbid(unsafe_code)]`, got {:?}",
        report.category_counts()
    );
    assert!(
        !report.gaps.iter().any(|g| g.category == Category::Other),
        "inner attribute must not land in the opaque Other bucket"
    );
    // The inner attr was never an item, so it is not denominator-excluded — the single `fn` is the
    // whole denominator.
    assert_eq!(report.total_top_level_items, 1);
    assert_eq!(report.non_test_item_count(), 1);
    assert_eq!(report.denominator_excluded_count(), 0);
}

/// The `excluded_from_denominator` predicate is exactly {`TestItem`, `ModuleDecl`} — a real gap
/// (`Import`, `Other`, `Impl`, …) is never excluded (VR-5: excluding a real gap would flatter the
/// coverage number).
#[test]
fn only_test_and_module_decl_are_denominator_excluded() {
    assert!(Category::TestItem.excluded_from_denominator());
    assert!(Category::ModuleDecl.excluded_from_denominator());
    for c in [
        Category::InnerAttr,
        Category::Import,
        Category::Other,
        Category::Impl,
        Category::Struct,
        Category::DeriveAttr,
    ] {
        assert!(
            !c.excluded_from_denominator(),
            "{} must stay in the denominator",
            c.as_str()
        );
    }
}
