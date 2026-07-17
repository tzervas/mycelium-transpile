//! RFC-0041 §4.7/§5 — the guard-hole **census** (W0 safety net; RR-29 guard-hole inventory turned
//! into tracked tests, one per hole this crate owns).
//!
//! Real repros: each test builds genuinely deep Rust source text via `syn::parse_str` (a real,
//! user-triggerable input — the transpiler's whole job is ingesting third-party Rust) and calls the
//! hole's entry point. **W1 closes the hole:** `emit_expr`/`map_type`/`map_pattern` are now guarded
//! by the shared [`mycelium_workstack::RecursionBudget`] (RFC-0041 §4.7) — a pathological input
//! depth refuses with an explicit `Err` (mapped to `Category::RecursionBudget`) instead of an
//! unguarded native-stack overflow.
//!
//! **Why `ensure_sufficient_stack` still wraps every case here, even though the depth budget
//! (default ceiling 4096) is what actually refuses:** `syn::parse_str`/`Parser::parse_str`
//! themselves recurse natively over the same nested-paren/tuple/pattern text with **no budget at
//! all** (they're a third-party dependency, out of this crate's guard surface) — so a depth chosen
//! only to exceed the 4096 ceiling can still overflow the *parser's* native stack before this
//! crate's guarded code ever runs, on the default (small) test-harness thread. Each test therefore
//! runs its whole parse-then-map call inside [`mycelium_workstack::ensure_sufficient_stack`] (the
//! same 256 MiB deep-stack helper the real driver would use around its own `syn::parse_file`), and
//! picks a depth (8,000) empirically well above the 4096 budget ceiling (so *our* guard is what
//! fires) and well below every syn-parser-native-overflow threshold measured for these three shapes
//! under a 256 MiB stack in a debug/test build (`Expr`/`Type`/`Pat` all parse cleanly well past
//! 8,000; the lowest observed native-parser crash threshold, for `Type::Tuple`, was between 10,000
//! and 20,000) — so the *only* thing that can refuse at depth 8,000 is this crate's own
//! `RecursionBudget`, never `syn`'s parser.

use mycelium_transpile::emit::{emit_expr, map_pattern};
use mycelium_transpile::gap::Category;
use mycelium_transpile::map::map_type;
use mycelium_workstack::{ensure_sufficient_stack, RecursionBudget};

/// Depth used by every census case below: comfortably (~2x) past the shared
/// [`RecursionBudget::DEFAULT_DEPTH_LIMIT`] (4096) so this crate's own guard is what refuses, and
/// comfortably below every syn-parser-native stack-overflow threshold measured for these three
/// input shapes under a 256 MiB stack (see module docs) — so the census exercises *this crate's*
/// guard hole, never `syn`'s own (out-of-scope, third-party) recursion.
const CENSUS_DEPTH: usize = 8_000;

/// `n` levels of parenthesized nesting: `(((…(1)…)))`.
fn deep_parens(n: usize) -> String {
    format!("{}1{}", "(".repeat(n), ")".repeat(n))
}

/// `n`-deep right-nested 2-tuple type: `(u8, (u8, (u8, …)))`. `map_type` has no `Type::Paren` arm
/// (a bare parenthesized type falls through its catch-all `_ => Err(..)`, never recursing), so a
/// right-nested `Type::Tuple` (its one recursive arm, `map.rs:127`) is the real repro shape instead.
fn deep_type_tuple(n: usize) -> String {
    format!("{}u8{}", "(u8, ".repeat(n), ")".repeat(n))
}

/// `n` levels of nested tuple-pattern parens: `(((…(x)…)))`.
fn deep_pattern_parens(n: usize) -> String {
    format!("{}x{}", "(".repeat(n), ")".repeat(n))
}

#[test]
fn emit_expr_deep_paren_refuses_cleanly() {
    // Hole: `emit_expr` (crates/mycelium-transpile/src/emit.rs) — recurses through `Expr::Paren`'s
    // inner expression. RFC-0041 §4.7 W1: now guarded by the shared recursion budget.
    let src = deep_parens(CENSUS_DEPTH);
    let budget = RecursionBudget::default();
    let result = ensure_sufficient_stack(&budget, || {
        let expr: syn::Expr = syn::parse_str(&src).expect("deeply-parenthesized Rust still parses");
        // trx2 Lane C Deliverable 1: `emit_expr` now threads a name->type environment (empty here
        // — this census has no fn/method params in scope, and doesn't need any: it only exercises
        // the recursion-budget guard hole, not the operand-type-gated operator emission).
        emit_expr(&expr, None, &std::collections::HashMap::new())
    });
    let err = result
        .expect_err("expected an explicit over-budget GapReason refusal, not success or a SIGABRT");
    assert_eq!(
        err.category,
        Category::RecursionBudget,
        "refusal should be tagged as a recursion-budget gap, not an ordinary unmapped construct"
    );
}

#[test]
fn map_type_deep_tuple_refuses_cleanly() {
    // Hole: `map_type` (crates/mycelium-transpile/src/map.rs) — recurses through `Type::Tuple`
    // elements, the crate's own real repro shape (see `deep_type_tuple` doc comment). RFC-0041
    // §4.7 W1: now guarded by the shared recursion budget.
    let src = deep_type_tuple(CENSUS_DEPTH);
    let budget = RecursionBudget::default();
    let result = ensure_sufficient_stack(&budget, || {
        let ty: syn::Type = syn::parse_str(&src).expect("a right-nested 2-tuple type still parses");
        map_type(&ty, None)
    });
    let err = result
        .expect_err("expected an explicit over-budget GapReason refusal, not success or a SIGABRT");
    assert_eq!(
        err.category,
        Category::RecursionBudget,
        "refusal should be tagged as a recursion-budget gap, not an ordinary unmapped construct"
    );
}

#[test]
fn map_pattern_deep_paren_refuses_cleanly() {
    // Hole: `map_pattern` (crates/mycelium-transpile/src/emit.rs) — recurses through `Pat::Paren`.
    // RFC-0041 §4.7 W1: now guarded by the shared recursion budget.
    let src = deep_pattern_parens(CENSUS_DEPTH);
    let budget = RecursionBudget::default();
    let result = ensure_sufficient_stack(&budget, || {
        // `Pat` has no direct `Parse` impl (it needs disambiguation re: a leading `|`) — go
        // through the `Parser` trait's `Pat::parse_single`, syn 2's documented way to parse a
        // single bare pattern.
        let pat: syn::Pat = syn::parse::Parser::parse_str(syn::Pat::parse_single, &src)
            .expect("deeply-parenthesized Rust pattern still parses");
        map_pattern(&pat, None)
    });
    let err = result
        .expect_err("expected an explicit over-budget GapReason refusal, not success or a SIGABRT");
    assert_eq!(
        err.category,
        Category::RecursionBudget,
        "refusal should be tagged as a recursion-budget gap, not an ordinary unmapped construct"
    );
}
