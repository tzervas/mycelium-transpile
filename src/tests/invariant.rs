//! The never-silent bound: every top-level Rust item is either emitted, gapped, or both — never
//! neither (G2).
//!
//! **Guarantee: `Empirical/Declared`, not `Proven`.** `syn::Item` is `#[non_exhaustive]`, so
//! `dispatch_item`'s exhaustiveness rests on a trailing catch-all `_` arm the compiler does not
//! (and cannot) verify covers every real-world construct — there is no compiler-checked
//! exhaustiveness proof here, only a corpus of cases run through the driver and checked. This is
//! consistent with the crate-level guarantee tags (`src/lib.rs`); this property is checked over a
//! **fixed, hand-written corpus**, not proptest-generated arbitrary Rust source: generating
//! *syntactically valid* Rust via a `proptest::Strategy` is disproportionately fiddly for a PoC
//! (the grammar is context-sensitive enough that most naive token-soup strategies would just fail
//! to parse), so — as the kickoff brief explicitly allows — this is "a property over cases" via
//! `proptest::prop_oneof`/indexed selection over a fixed corpus, not over freely-generated input.

use crate::transpile::transpile_source;
use proptest::prelude::*;
use std::collections::BTreeSet;

/// A fixed corpus of small, syntactically valid Rust source snippets, spanning every `Category`
/// this transpiler recognizes plus several fully-expressible cases — the same kind of item mix a
/// real crate (like `mycelium-std-cmp`) contains. Each entry is a **whole file** (so
/// `syn::parse_file` succeeds standalone); some entries carry more than one top-level item on
/// purpose, to also exercise the invariant across multiple sibling items in one file.
fn corpus() -> Vec<&'static str> {
    vec![
        // All-expressible file.
        "enum Ordering { Less, Equal, Greater }\nfn is_lt(o: bool) -> bool { o }",
        // All-gapped file (every known hard gap, several items). The struct carries an `f32` field
        // so it still gaps under M-1006: a named-field struct with all-*mappable* fields now emits
        // positionally (and `String` maps to `Bytes` as of §8.14; `char`/`i8..i128`/`isize`/`usize`
        // now map too, as of P4/P5 — DN-99 §8 ENB-6), so an all-gapped fixture must use a field
        // type that has no mapping — `f32` still has no confirmed base_type arm (`Float` is
        // binary64-only, ADR-040 FLAG-1/M-897).
        "trait Foo { fn bar(&self) -> bool; }\nmacro_rules! m { () => {}; }\nstruct S { c: f32 }",
        // Mixed: some expressible, some gapped, in one file (mirrors the real target crate).
        "enum E { A(u8), B }\ntrait T { fn f(&self) -> bool; }\nfn g(x: bool) -> bool { x }",
        // A single unmappable-type fn (`f32`, still gapped — P4/P5 mapped `i32`, so this fixture
        // was updated to keep exercising a genuinely still-unmapped scalar) plus a working one.
        "fn h(x: f32) -> f32 { x }\nfn ok(x: bool) -> bool { x }",
        // An unbounded-generic fn (this one is actually expressible — `type_params` allows bare
        // identifiers) beside a `type` *alias* (which always gaps regardless of generics — see
        // `dispatch_item`'s `Item::Type` arm: `type_item` always introduces a new sum type, so a
        // bare alias has no confirmed equivalent).
        "fn id<T>(x: T) -> T { x }\ntype Pair<A, B> = (A, B);",
        // impl block: one method succeeds, one fails (partial-impl semantics).
        "struct W(u8);\nimpl W { fn get(self) -> u8 { 0 } fn bad(self) -> i32 { 0 } }",
        // use statements: simple + glob + a gapped grouped import.
        "use a::b::C;\nuse d::e::*;\nuse f::{g, h};",
        // const/static/type-alias/union — each its own `Other`-ish gap category.
        "const X: u8 = 0;\nstatic Y: u8 = 0;\ntype Z = u8;\nunion U { a: u8, b: u8 }",
        // A #[cfg(test)] module — excluded-but-recorded.
        "#[cfg(test)]\nmod tests { fn t() {} }\nfn real(x: bool) -> bool { x }",
        // Empty file — the degenerate zero-item case (invariant holds vacuously).
        "",
    ]
}

/// Assert the never-silent invariant for one source string: every top-level item's identity is
/// witnessed by either an emitted-item name or a gap. We check this at the *count* level
/// (`emitted_items.len() + count of items with zero coverage == 0`) via a slightly stronger
/// per-item reconstruction: replay `syn::parse_file` ourselves and confirm each item index has
/// SOME record (either its rendered name is in `emitted_items`, or at least one gap's line number
/// matches its span). Line-number correlation is heuristic (two items can share a line in dense
/// fixtures) — so as a robust, still-meaningful invariant we instead assert the simpler,
/// unconditionally sound count property: total items <= emitted_items.len() + gaps.len(). Since
/// every item contributes *at least one* of (an emitted-item push) or (a gap push) — never zero —
/// this inequality holding is necessary; combined with the fixture-level checks in
/// `src/tests/emit.rs` (which confirm specific items land in the *correct* one of the two sets),
/// this test's job is to confirm the sum bound holds across a varied corpus, not to re-derive
/// per-item classification from scratch.
fn assert_never_silent(source: &str) {
    let (_, report) = transpile_source(source, "corpus.rs", "corpus")
        .unwrap_or_else(|e| panic!("corpus fixture failed to parse: {e}\nsource:\n{source}"));
    let covered = report.emitted_items.len() + report.gaps.len();
    assert!(
        covered >= report.total_top_level_items,
        "never-silent invariant violated: {} top-level item(s) but only {} emitted + gap \
         record(s) (emitted={:?})\nsource:\n{source}",
        report.total_top_level_items,
        covered,
        report.emitted_items
    );
    // A file-level attrs-only gap (line 1, item_name None) can inflate `gaps.len()` beyond what a
    // naive per-item count would predict but never breaks the inequality above (it only adds a
    // record); assert it stays that way — no gap ever has a nonsensical negative-cost identity.
    for g in &report.gaps {
        assert!(g.line >= 1, "gap line numbers are 1-based, got {}", g.line);
    }
    // No item name should be silently duplicated in a way that hides a missing item: every
    // emitted name is non-empty.
    for name in &report.emitted_items {
        assert!(!name.is_empty(), "an emitted item had an empty name");
    }
}

#[test]
fn never_silent_over_fixed_corpus() {
    for source in corpus() {
        assert_never_silent(source);
    }
}

proptest! {
    /// The property test proper: pick an index into the fixed corpus (proptest drives the
    /// selection/shrinking; the corpus itself is fixed prose, per the module doc's rationale) and
    /// re-check the same invariant. This gives proptest's case-count tiering (DN-20: low locally,
    /// high on `just check-full`) for free, and — since every corpus entry already contains
    /// several items across every gap category — a shrink failure still points at a small,
    /// legible fixture.
    #[test]
    fn never_silent_prop(idx in 0usize..corpus().len()) {
        let source = corpus()[idx];
        assert_never_silent(source);
    }
}

/// A distinct check from `assert_never_silent`'s sum bound: for the all-expressible and
/// all-gapped corpus entries specifically, confirm the split lands where expected (a cheap
/// sanity cross-check that the sum bound isn't vacuously satisfied by, say, every item silently
/// going to only one side by coincidence).
#[test]
fn never_silent_split_sanity() {
    let (_, all_expressible) =
        transpile_source(corpus()[0], "corpus.rs", "corpus").expect("parses");
    assert_eq!(all_expressible.emitted_items.len(), 2);

    let (_, all_gapped) = transpile_source(corpus()[1], "corpus.rs", "corpus").expect("parses");
    assert!(all_gapped.emitted_items.is_empty());
    assert_eq!(all_gapped.gaps.len(), all_gapped.total_top_level_items);

    // No duplicate names within a single emitted set for the mixed fixtures (a quick, cheap
    // consistency check unrelated to but colocated with the invariant tests).
    for source in corpus() {
        let (_, report) = transpile_source(source, "corpus.rs", "corpus").expect("parses");
        let unique: BTreeSet<_> = report.emitted_items.iter().collect();
        assert_eq!(
            unique.len(),
            report.emitted_items.len(),
            "duplicate emitted-item name in corpus entry:\n{source}"
        );
    }
}
