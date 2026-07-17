//! Tests for the reserved-word collision guard (`src/reserved.rs`, M-1001), incl. the **drift
//! guard** against `mycelium-l1`'s real lexer.
//!
//! **Guarantee: `Empirical`** for the drift test (it runs the real `mycelium_l1::token::keyword`
//! over every snapshot word); pure/`Declared` for the guard behaviour tests.

use crate::gap::Category;
use crate::reserved::{
    guard_ident, is_reserved, sanitize_nodule_path, valid_ident, RESERVED, RESERVED_SEGMENT_SUFFIX,
};

/// **Drift guard.** Every word in the [`RESERVED`] snapshot must still be rejected as an identifier
/// by the *real* `mycelium-l1` lexer (`mycelium_l1::token::keyword` returns `Some` for a keyword). A
/// snapshot word that drifts to a *non*-reserved word — the direction that would make the guard
/// over-gap and regress a valid emission — fails here. (The under-gap direction — a new keyword `l1`
/// adds that this snapshot misses — is a residual the vet loop catches as a parse error, not a
/// silent bad emission; it is not asserted here because this crate should not have to track every
/// future keyword addition to stay correct — over-gap is the only regressing direction.)
#[test]
fn snapshot_words_are_all_still_reserved_in_the_lexer() {
    for word in RESERVED {
        assert!(
            mycelium_l1::token::keyword(word).is_some(),
            "reserved-word snapshot drift: `{word}` is in crate::reserved::RESERVED but the real \
             mycelium-l1 lexer no longer treats it as a keyword — the snapshot must be re-synced \
             with crates/mycelium-l1/src/token.rs `fn keyword` (this would otherwise over-gap a \
             now-valid identifier)"
        );
    }
}

/// The snapshot is non-empty and free of accidental duplicates (a duplicate is harmless for
/// `contains` but signals a copy error).
#[test]
fn snapshot_is_nonempty_and_deduplicated() {
    assert!(
        !RESERVED.is_empty(),
        "reserved-word snapshot must not be empty"
    );
    let mut seen = std::collections::BTreeSet::new();
    for w in RESERVED {
        assert!(
            seen.insert(*w),
            "duplicate reserved word in snapshot: `{w}`"
        );
    }
}

/// `valid_ident` accepts ordinary identifiers and rewrites reserved words (DN-140).
#[test]
fn guard_rejects_reserved_accepts_ordinary() {
    // Ordinary Rust identifiers that are NOT Mycelium reserved words → accepted.
    for ok in [
        "Ordering",
        "ForageError",
        "reverse",
        "is_lt",
        "NoCandidates",
        "Foo",
        "my_fn",
    ] {
        assert!(!is_reserved(ok), "`{ok}` should not be reserved");
        assert!(
            guard_ident(ok, "test position").is_ok(),
            "`{ok}` should pass the guard call-through"
        );
        assert_eq!(valid_ident(ok).text, ok);
    }
    for bad in [
        "Exact",
        "Proven",
        "Empirical",
        "Declared",
        "F16",
        "Binary",
        "swap",
        "fuse",
    ] {
        assert!(is_reserved(bad), "`{bad}` should be reserved");
        let vi = valid_ident(bad);
        assert_eq!(vi.text, format!("{bad}{RESERVED_SEGMENT_SUFFIX}"));
        assert!(vi.rewrite.is_some());
    }
}

/// **Declaration-site coverage (PR #1207 review HIGH).** A reserved word used as an *unused* fn
/// parameter name never flows through `Expr::Path`'s use-site guard, and its name is emitted
/// verbatim into `param ::= Ident ':' type_ref` — so the guard must fire in `map_signature`
/// itself. Repro from the review: `fn set_default(default: u32)` must not emit the reserved
/// parameter name verbatim — DN-140 renames it to `default_kw`.
#[test]
fn unused_reserved_fn_parameter_is_gapped_not_emitted() {
    let src = "pub fn set_default(default: u32) -> u32 { 42 }\n";
    let (myc, report) = crate::transpile::transpile_source(src, "reserved_param", "test")
        .expect("transpile_source itself succeeds");
    assert!(
        report.emitted_items.iter().any(|n| n == "set_default"),
        "the fn should emit: {myc}"
    );
    assert!(
        myc.contains("default_kw"),
        "reserved parameter must be escaped, got:\n{myc}"
    );
    assert!(
        !myc.contains("default: u32"),
        "verbatim reserved parameter name must not appear, got:\n{myc}"
    );
}

/// Same exposure for an unused generic type-parameter name (declaration-site guard in
/// `plain_type_params`) — defensive twin of the fn-parameter case.
#[test]
fn reserved_type_parameter_is_gapped_not_emitted() {
    let vi = valid_ident("Binary");
    assert_eq!(vi.text, format!("Binary{RESERVED_SEGMENT_SUFFIX}"));
}

/// **Gap-close-2 Phase-0 regression fix — `sanitize_nodule_path` unit coverage.** M-1042 added
/// intra-crate module-path segments to the derived nodule path; a segment that collides with
/// [`RESERVED`] (`crates/mycelium-l1/src/fuse.rs` -> `l1.fuse`,
/// `crates/mycelium-std-runtime/src/colony.rs` -> `std.runtime.colony`) must never reach
/// `render_nodule` verbatim (repro: `parse-error: expected an identifier, found Fuse`).
///
/// **Revised (PR #1517 review HIGH).** The colliding segment is now **escaped**
/// (`fuse` -> `fuse_kw`), not dropped — dropping collapsed distinct source files (e.g.
/// `l1.fuse`/`l1.nodule`, both losing their sole segment) onto the same nodule path, an
/// undisclosed cross-file collision the per-file vet loop can't see. See
/// `reserved_word_siblings_do_not_collide_after_sanitize` in `src/tests/transpile.rs` for the
/// cross-file collision-freedom proof this escape closes.
#[test]
fn sanitize_nodule_path_escapes_only_colliding_segments() {
    // No collision: unchanged, no gap.
    let (path, gap) = sanitize_nodule_path("std.time");
    assert_eq!(path, "std.time");
    assert!(gap.is_none(), "a non-colliding path must not gap");

    // The exact repro shapes: the colliding segment is escaped in place, not dropped — the
    // crate-prefix segment(s) are preserved unchanged.
    let (path, gap) = sanitize_nodule_path("l1.fuse");
    assert_eq!(
        path,
        format!("l1.fuse{RESERVED_SEGMENT_SUFFIX}"),
        "the reserved `fuse` segment must be escaped in place, not dropped"
    );
    let gap = gap.expect("a collision must produce a gap, never a silent rename");
    assert_eq!(gap.category, Category::ReservedWord);
    assert!(
        gap.reason.contains("fuse") && gap.reason.contains("l1.fuse"),
        "the gap reason names both the original path and the colliding word (never silent): {}",
        gap.reason
    );

    let (path, gap) = sanitize_nodule_path("std.runtime.colony");
    assert_eq!(
        path,
        format!("std.runtime.colony{RESERVED_SEGMENT_SUFFIX}"),
        "the reserved `colony` segment must be escaped, non-colliding segments kept unchanged"
    );
    assert_eq!(gap.expect("must gap").category, Category::ReservedWord);

    // A collision in a NON-trailing segment is also caught (defensive: not special-cased to the
    // last segment only) — and the non-colliding trailing segment is preserved unchanged.
    let (path, gap) = sanitize_nodule_path("fuse.sub");
    assert_eq!(path, format!("fuse{RESERVED_SEGMENT_SUFFIX}.sub"));
    assert!(gap.is_some());

    // Multiple colliding segments in one path are each escaped independently.
    let (path, gap) = sanitize_nodule_path("fuse.colony");
    assert_eq!(
        path,
        format!("fuse{RESERVED_SEGMENT_SUFFIX}.colony{RESERVED_SEGMENT_SUFFIX}")
    );
    assert!(gap.is_some());
}

/// **Escape-suffix soundness (PR #1517 review HIGH).** The escaped form of every reserved word
/// must never itself be reserved — otherwise escaping would re-trigger the very collision it
/// exists to close (an escaped segment feeding back into `guard_ident`/`is_reserved` as a false
/// positive, or worse, a *second* silent collision). Exhaustive over the whole snapshot, not just
/// the `fuse`/`colony` repro shapes.
#[test]
fn escaped_reserved_words_are_never_themselves_reserved() {
    for word in RESERVED {
        let escaped = format!("{word}{RESERVED_SEGMENT_SUFFIX}");
        assert!(
            !is_reserved(&escaped),
            "escaping `{word}` produced `{escaped}`, which is ITSELF a reserved word — the \
             escape suffix `{RESERVED_SEGMENT_SUFFIX}` must never turn a keyword into another \
             keyword"
        );
    }
}

/// **Escape-suffix injectivity (PR #1517 review HIGH).** Two *different* reserved words must
/// never escape to the same string — otherwise the escape itself could reintroduce the
/// cross-file collision it exists to close. (Trivially true for a fixed-suffix scheme, but pinned
/// here as an explicit, checked property rather than an unchecked assumption — VR-5.)
#[test]
fn escaped_reserved_words_are_pairwise_distinct() {
    let mut seen = std::collections::BTreeSet::new();
    for word in RESERVED {
        let escaped = format!("{word}{RESERVED_SEGMENT_SUFFIX}");
        assert!(
            seen.insert(escaped.clone()),
            "two distinct reserved words escaped to the same string `{escaped}` — the escape \
             suffix no longer guarantees injectivity"
        );
    }
}

/// **Live-oracle regression proof** for the fuse.rs/colony.rs repros: a nodule path whose only
/// segment collides with a reserved word must yield a `ReservedWord`-category [`Gap`][crate::gap::Gap]
/// (never a hard parse error) — and the resulting `.myc` header must itself `myc check`-clean
/// parse. Skips gracefully (never fails) when `myc-check` is not built.
#[test]
fn reserved_nodule_path_segment_gaps_never_hard_parse_fails() {
    let (myc, report) =
        crate::transpile::transpile_source("pub fn f() -> u32 { 42 }\n", "fixture.rs", "l1.fuse")
            .expect("transpile_source itself succeeds — the header collision is a recorded gap");
    let gap = report
        .gaps
        .iter()
        .find(|g| g.category == Category::ReservedWord && g.reason.contains("nodule path"))
        .unwrap_or_else(|| {
            panic!("expected a ReservedWord gap naming the nodule-path collision, got: {report:?}")
        });
    assert!(
        gap.reason.contains("fuse"),
        "the gap must name the colliding segment: {}",
        gap.reason
    );
    assert!(
        myc.contains(&format!("nodule l1.fuse{RESERVED_SEGMENT_SUFFIX};")),
        "the sanitized header must escape the colliding segment in place, got:\n{myc}"
    );
    assert!(
        !myc.contains("nodule l1.fuse;"),
        "the colliding segment must never reach the emitted header verbatim, got:\n{myc}"
    );

    let Some(bin) = super::vet::find_myc_check() else {
        eprintln!(
            "reserved: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or \
             build `cargo build -p mycelium-check --bin myc-check`)."
        );
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-reserved-nodule-path-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("case.myc");
    std::fs::write(&path, &myc).expect("write case .myc");
    let checker = crate::vet::MycChecker {
        command: vec![bin.display().to_string()],
        cwd: None,
    };
    let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
    assert_ne!(
        rec.class,
        crate::vet::VetClass::ParseError,
        "the sanitized nodule header must never hard-parse-fail (the G2 \"zero hard parse \
         failures\" invariant this fix restores) — diagnostic={:?}",
        rec.diagnostic
    );
    let _ = std::fs::remove_dir_all(&dir);
}
