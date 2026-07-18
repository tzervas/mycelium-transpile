//! DN-125 (M-1081) — the `&mut self`/`&mut T` value-threading lowering. Unit tests over
//! `emit::{map_signature, emit_mutating_block_as_expr}` via the public `transpile_source` driver
//! (per CLAUDE.md "Test layout": data-driven fixtures, complex logic stays out of test bodies).
//!
//! Covers, per the DN-125 §9 Definition of Done and this leaf's brief:
//! - a simple `&mut self` field mutation value-threads and (live-oracle) `myc check`-cleans;
//! - the builder-chain shortcut (`-> &mut Self`) threads without a fabricated tuple;
//! - a top-level `&mut T` PARAMETER (S2) value-threads (whole-value reassignment);
//! - the two DN-125 §6 adversarial narrowings NEVER silently mis-thread: (i) an
//!   interior-`&mut`-return method (`&mut Field`, not the receiver itself) gaps, never a
//!   fabricated value return; (ii) a body shape outside the flat re-assignment sequence (a
//!   conditional mutation, a field read through a non-`self` threaded param) gaps, never a
//!   guessed rebind;
//! - `&self` (shared) still erases to a plain value param, unaffected by this DN (regression
//!   guard on the pre-existing behavior DN-125 §2 says this lowering must not touch);
//! - a `let <threaded-name> = <other-threaded-name>;` ALIASING rebind (the re-review-of-#1527
//!   hole closed by `emit::aliased_threaded_binding`) is REFUSED, never mis-threaded — in both
//!   directions — while the pre-existing, genuinely-safe independent-value shadow shape
//!   (`let y = <plain local>;`) keeps threading correctly even interleaved between two
//!   reassignments.

use crate::transpile::transpile_source;

/// Live-oracle helper shared with `tests::vet`/`tests::emit` (see that module's doc) — every
/// mutating-body case this module claims "emits" is ALSO run through the real `myc-check` binary
/// when available, so "emitted" and "myc check-clean" are never conflated (VR-5: the emitted text
/// assertions below are `Declared`-heuristic; only the live-oracle runs below are the checked
/// claim).
use super::vet::find_myc_check;

fn myc_check_clean(myc: &str, case: &str) {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "mut_thread: live oracle test skipped for `{case}` — no runnable myc-check (set \
             MYC_CHECK_CMD or build `cargo build -p mycelium-check --bin myc-check`)."
        );
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-mut-thread-oracle-{case}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{case}.myc"));
    std::fs::write(&path, myc).expect("write case .myc");

    let checker = crate::vet::MycChecker {
        command: vec![bin.display().to_string()],
        cwd: None,
    };
    let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
    assert_eq!(
        rec.class,
        crate::vet::VetClass::Clean,
        "case `{case}` must check CLEAN with the real myc-check oracle — emitted:\n{myc}\n\
         diagnostic={:?}",
        rec.diagnostic
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A simple `&mut self` compound-assignment mutator (DN-125 §5.1's `incr` illustration, shape-
/// wise) threads: the receiver is taken by value, the body's `self.0 ^= mask;` rebuilds `Counter`
/// positionally via nested-let shadowing, and the signature's return type widens to the
/// receiver's own type (no extra value — the source returned `()`). `^=` remains a stable
/// bitwise path; `+=` on a field projection is covered separately (now `add_u`-clean once the
/// peer field width is recovered — see `mut_self_field_add_assign_emits_add_u_check_clean`).
#[test]
fn mut_self_field_compound_assign_value_threads() {
    let rust =
        "struct Counter(u64); impl Counter { fn toggle(&mut self, mask: u64) { self.0 ^= mask; } }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        report.gaps.is_empty(),
        "`toggle` must be emitted with zero gaps (a `&mut self` method with a supported \
         field-assign body): gaps={:?}\nmyc:\n{myc}",
        report.gaps
    );
    assert!(
        myc.contains(
            "fn toggle(self: Counter, mask: Binary{64}) => Counter = let self = Counter((match \
             self { Counter(p0) => p0 } ) ^ mask) in self;"
        ) || myc.contains(
            "fn toggle(self: Counter, mask: Binary{64}) => Counter = let self = Counter((match \
             self { Counter(p0) => p0 }) ^ mask) in self;"
        ),
        "unexpected emission:\n{myc}"
    );
    myc_check_clean(&myc, "mut_self_field_compound_assign");
}

/// **Pin update (ORACLE-R1 A1 / L1 self-review, 2026-07-16).** DN-125 originally pinned that
/// plain `+`/`+=` on two `Binary{N}` values failed live `myc-check` (`add` T-Op refuse). After
/// the field-type map landed for lit-zero rewrite, a field-projection `self.0 += by` recovers
/// peer width and routes through the existing unsigned `add_u` composed form — so this shape is
/// now **check-clean** (honest side-effect; not a fabricated prim). The free-fn bare-glyph `+`
/// path without known Binary peers may still differ; this pin covers the field-compound shape
/// that mut_thread actually emits.
#[test]
fn mut_self_field_add_assign_emits_add_u_check_clean() {
    let rust =
        "struct Counter(u64); impl Counter { fn incr(&mut self, by: u64) { self.0 += by; } }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        report.gaps.is_empty(),
        "value-threading + field `+=` must emit with zero gaps: gaps={:?}\nmyc:\n{myc}",
        report.gaps
    );
    assert!(
        myc.contains("add_u(") && myc.contains("Counter(p0)"),
        "expected field `+=` to emit `add_u` over the projected payload:\n{myc}"
    );
    myc_check_clean(&myc, "mut_self_field_add_assign");
}

/// The builder-chain shortcut: `-> &mut Self` returning the receiver for chaining threads to a
/// SINGLE `Counter` return (never a `(Counter, Counter)` tuple) — DN-125 §1/§4's "builder
/// methods" classification, not the §6.2 interior-return residual.
#[test]
fn mut_self_builder_chain_return_threads_single_type() {
    let rust = "struct Counter(u64); \
                impl Counter { fn set(&mut self, v: u64) -> &mut Self { self.0 = v; self } }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        report.gaps.is_empty(),
        "`set` must be emitted with zero gaps: gaps={:?}\nmyc:\n{myc}",
        report.gaps
    );
    assert!(
        myc.contains(
            "fn set(self: Counter, v: Binary{64}) => Counter = let self = Counter(v) in self;"
        ),
        "builder-chain return must thread to a single `Counter`, not a tuple:\n{myc}"
    );
    myc_check_clean(&myc, "mut_self_builder_chain");
}

/// S2: a top-level `&mut T` fn PARAMETER (not a receiver) value-threads too — a whole-value
/// re-assignment (`*y = v;`) rebinds `y` directly (no struct layout needed, unlike the `self`
/// field-assign case above).
#[test]
fn mut_t_param_whole_value_deref_assign_value_threads() {
    let rust = "fn set_val(y: &mut u64, v: u64) { *y = v; }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        report.emitted_items.iter().any(|n| n == "set_val"),
        "`set_val` must be emitted: gaps={:?}\nmyc:\n{myc}",
        report.gaps
    );
    assert!(
        myc.contains("fn set_val(y: Binary{64}, v: Binary{64}) => Binary{64} = let y = v in y;"),
        "unexpected emission:\n{myc}"
    );
    myc_check_clean(&myc, "mut_t_param_whole_value_deref_assign");
}

/// DN-125 §6.2 adversarial narrowing (HELD): a `&mut self` method returning `&mut <other field
/// type>` — an interior mutable reference INTO self, not the receiver's own value (`get_mut`-
/// shape) — must NOT value-thread. It gaps via the pre-existing, UNCHANGED `&mut T` return-type
/// gap (`map::visit_reference`), never a fabricated value return.
#[test]
fn interior_mut_return_never_value_threads() {
    let rust = "struct Pair(u64, u64); \
                impl Pair { fn peek_mut(&mut self) -> &mut u64 { &mut self.0 } }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        !myc.contains("peek_mut"),
        "an interior-&mut-return method must NEVER be emitted (would fabricate a value return \
         for a reference into self): myc:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("mutable reference")),
        "expected a gap citing the mutable-reference basis (DN-125 §6.2's untouched `&mut T` \
         return-type gap): {:?}",
        report.gaps
    );
}

/// DN-125 §6.1 adversarial narrowing (HELD): a conditional (`if`-guarded, no `else`) mutation of
/// `self` is OUTSIDE the flat re-assignment sequence this lowering accepts — it must gap, never
/// silently mis-thread a value that doesn't actually reflect the (possibly-skipped) mutation.
#[test]
fn conditional_self_mutation_gaps_not_mis_threaded() {
    let rust = "struct Counter(u64); \
                impl Counter { fn maybe_incr(&mut self, by: u64, flag: bool) { \
                if flag { self.0 += by; } } }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        !report.emitted_items.iter().any(|n| n == "maybe_incr"),
        "a conditionally-mutated `&mut self` body must NOT be emitted (outside the flat \
         re-assignment scope DN-125 §6.1 accepts): myc:\n{myc}"
    );
    assert!(
        !report.gaps.is_empty(),
        "the refusal must be a recorded gap, never a silent drop (G2)"
    );
}

/// DN-125 §6.1 adversarial narrowing (HELD): reading a FIELD of a non-`self` threaded `&mut T`
/// parameter (`other.0`) has no supported projection (`visit_field` only resolves `self.<field>`)
/// — the whole method must gap rather than silently reinterpreting `other` as an alias of `self`.
#[test]
fn field_read_through_non_self_threaded_param_gaps() {
    let rust = "struct Counter(u64); \
                impl Counter { fn weird(&mut self, other: &mut Counter) { self.0 = other.0; } }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        !report.emitted_items.iter().any(|n| n == "weird"),
        "a field read through a non-`self` threaded param must NOT be emitted: myc:\n{myc}"
    );
    assert!(
        !report.gaps.is_empty(),
        "the refusal must be a recorded gap, never a silent drop (G2)"
    );
}

/// Regression guard: an ordinary `&self` (shared) method is completely UNAFFECTED by DN-125 —
/// still erased to a plain by-value receiver with no threading, exactly as it landed pre-DN-125
/// (DN-125 §2/§4: this is the ALREADY-landed Native Equivalent, out of this DN's scope).
#[test]
fn shared_self_receiver_still_erases_unthreaded() {
    let rust = "struct Counter(u64); impl Counter { fn get(&self) -> u64 { self.0 } }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        report.gaps.is_empty(),
        "`get` must be emitted: gaps={:?}\nmyc:\n{myc}",
        report.gaps
    );
    assert!(
        myc.contains("fn get(self: Counter) => Binary{64} = (match self { Counter(p0) => p0 });"),
        "a `&self` method must stay a plain (non-tuple, non-threaded) return:\n{myc}"
    );
}

/// **CRITICAL regression (strict review of PR #1527).** A plain `let y = ..;` in the body whose
/// pattern name SHADOWS the threaded `&mut T` parameter `y` is a genuinely new, ordinarily-scoped
/// local (Rust lexical shadowing) with NO effect on the referent — the correct threaded return is
/// still the value from the earlier `*y = v;` reassignment, never the unrelated shadow's value.
/// Pre-fix this silently emitted `let y = v in let y = w in y`, which evaluates to `w` — the
/// wrong, corrupted value — and still `myc check`-cleaned. This pins the fix non-vacuously: it
/// would fail against the pre-fix emission (whose tail is the bare, shadowable `y`).
#[test]
fn let_binding_shadowing_threaded_param_name_does_not_corrupt_threaded_return() {
    let rust = "fn set_val(y: &mut u64, v: u64, w: u64) { *y = v; let y = w; }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        report.emitted_items.iter().any(|n| n == "set_val"),
        "`set_val` must be emitted (synthetic-name routing, not a refusal, for this shape): \
         gaps={:?}\nmyc:\n{myc}",
        report.gaps
    );
    // The fold's TAIL reference must be the synthetic carrier, not the bare (shadowable) `y` —
    // i.e. the emitted body must NOT end `.. in y;` (the pre-fix, corruption-prone shape).
    assert!(
        !myc.contains("in y;"),
        "the threaded return must not resolve through the plain (shadow-vulnerable) name `y` — \
         unexpected emission:\n{myc}"
    );
    assert!(
        myc.contains(
            "fn set_val(y: Binary{64}, v: Binary{64}, w: Binary{64}) => Binary{64} = \
             let __myc_thread_y = y in let y = v in let __myc_thread_y = y in let y = w in \
             __myc_thread_y;"
        ),
        "unexpected emission (expected the `v`-preserving synthetic-carrier form):\n{myc}"
    );
    myc_check_clean(&myc, "let_shadows_threaded_param_name");
}

/// **CRITICAL regression (strict review of PR #1527), `want_extra` tuple shape.** Same shadow
/// hazard as above, but with a genuine extra return value read AFTER the shadow — the extra value
/// correctly reads the shadow's value (`y = 999`, ordinary Rust lexical scoping: `y + 1` after
/// `let y = 999` is `1000`), while the THREADED component of the tuple must still be `v`, never
/// `999`. Pre-fix this emitted `(999, 1000)`; the fix must emit `(v, 1000)`.
#[test]
fn let_binding_shadowing_threaded_param_name_preserves_threaded_value_in_extra_tuple() {
    let rust = "fn set_val(y: &mut u64, v: u64) -> u64 { *y = v; let y = 999u64; y + 1 }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        report.emitted_items.iter().any(|n| n == "set_val"),
        "`set_val` must be emitted: gaps={:?}\nmyc:\n{myc}",
        report.gaps
    );
    assert!(
        !myc.contains("(999, "),
        "the threaded tuple component must never be the unrelated shadow's value `999` — \
         unexpected emission:\n{myc}"
    );
    assert!(
        myc.contains(
            "fn set_val(y: Binary{64}, v: Binary{64}) => (Binary{64}, Binary{64}) = \
             let __myc_thread_y = y in let y = v in let __myc_thread_y = y in let y = 999 in \
             (__myc_thread_y, y + 1);"
        ),
        "unexpected emission (expected the `(v, 1000)`-preserving synthetic-carrier form):\n{myc}"
    );
}

/// **Non-regression companion:** with NO `let` shadowing the threaded name, emission is
/// completely unaffected by the CRITICAL fix — no synthetic carrier, byte-identical to the
/// pre-fix nested-`let`-on-`y` chain (the fix is opt-in per-body, never adds unneeded verbosity).
#[test]
fn no_shadow_means_no_synthetic_carrier_emitted() {
    let rust = "fn incr(y: &mut u64) { *y += 1; }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        report.gaps.is_empty(),
        "`incr` must be emitted with zero gaps: gaps={:?}\nmyc:\n{myc}",
        report.gaps
    );
    assert!(
        !myc.contains("__myc_thread_"),
        "a body with no shadow of the threaded name must never introduce the synthetic carrier: \
         {myc}"
    );
}

/// **CRITICAL regression (re-review of PR #1527 after merge — the aliasing-rebind hole this
/// leaf closes).** A `let y = other;` where `other` is ITSELF another threaded `&mut` binding is
/// not the harmless independent-value shadow the CRITICAL fix above assumes — it MOVES `other`'s
/// live reference into the name `y`, so the subsequent `*y = c;` actually mutates `other`'s
/// referent, not the original `y`'s. Pre-fix, this emitter's purely name-based
/// `try_threaded_assign` matching had no way to notice the rebind and kept folding `*y = c;`
/// against the ORIGINAL `y` binding regardless — silently producing `y_final = c, other_final =
/// other` (both wrong; the correct pair is `y_final = a, other_final = c`) while still `myc
/// check`-cleaning. This pins the closure: the body must be REFUSED (a recorded gap), never
/// emitted with a mis-threaded value.
#[test]
fn let_binding_aliasing_another_threaded_param_refuses_rather_than_mis_thread() {
    let rust = "fn f(y: &mut u64, other: &mut u64, a: u64, c: u64) { *y = a; let y = other; \
                *y = c; }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "an aliasing `let <threaded-name> = <other-threaded-name>;` rebind must NEVER be \
         emitted (it would silently mis-thread the subsequent reassignment onto the wrong \
         referent): myc:\n{myc}"
    );
    assert!(
        !myc.contains("fn f("),
        "no partial/incorrect emission for `f` is acceptable: myc:\n{myc}"
    );
    assert!(
        !report.gaps.is_empty(),
        "the refusal must be a recorded gap, never a silent drop (G2)"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("alias another threaded binding")),
        "expected the aliasing-rebind gap reason specifically, got: {:?}",
        report.gaps
    );
}

/// **Non-regression companion (symmetric direction):** the same aliasing hazard, rebinding the
/// OTHER threaded name (`other`) to alias `y` — confirms the refusal isn't accidentally
/// direction-dependent (only checking the first-declared threaded param, say).
#[test]
fn let_binding_aliasing_another_threaded_param_refuses_symmetric_direction() {
    let rust = "fn g(y: &mut u64, other: &mut u64, a: u64, c: u64) { *other = a; let other = y; \
                *other = c; }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        !report.emitted_items.iter().any(|n| n == "g"),
        "aliasing `other` to `y` must also be refused, not just the reverse direction: \
         myc:\n{myc}"
    );
    assert!(
        !report.gaps.is_empty(),
        "the refusal must be a recorded gap, never a silent drop (G2)"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("alias another threaded binding")),
        "expected the aliasing-rebind gap reason specifically, got: {:?}",
        report.gaps
    );
}

/// **Non-regression companion (the safe case the aliasing-rebind refusal must NOT catch):**
/// interleaved reassign -> shadow-with-INDEPENDENT-value -> reassign. `b` here is a plain `u64`
/// value param (not a `&mut` threaded binding), so `let y = b;` is the pre-existing, already-safe
/// shadow shape (no live aliasing introduced) — the subsequent `*y = c;` must still fold onto the
/// ORIGINAL threaded `y`, correctly returning `c` (never refused, never `b`-corrupted).
#[test]
fn interleaved_reassign_shadow_independent_value_reassign_still_returns_last_reassign() {
    let rust = "fn set_val(y: &mut u64, a: u64, b: u64, c: u64) { *y = a; let y = b; *y = c; }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "mut_thread").expect("parse/transpile");
    assert!(
        report.emitted_items.iter().any(|n| n == "set_val"),
        "an independent-value shadow interleaved between two threaded reassignments must still \
         emit (not be refused): gaps={:?}\nmyc:\n{myc}",
        report.gaps
    );
    assert!(
        myc.contains(
            "fn set_val(y: Binary{64}, a: Binary{64}, b: Binary{64}, c: Binary{64}) => \
             Binary{64} = let __myc_thread_y = y in let y = a in let __myc_thread_y = y in \
             let y = b in let y = c in let __myc_thread_y = y in __myc_thread_y;"
        ),
        "unexpected emission (expected the `c`-preserving synthetic-carrier form, unaffected \
         by the unrelated `b` shadow in between):\n{myc}"
    );
    myc_check_clean(&myc, "interleaved_reassign_shadow_independent_reassign");
}
