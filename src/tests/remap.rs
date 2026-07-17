//! Unit tests for the remap manifest (`src/remap.rs`, M-1044, DN-109 §5.2): schema round-trip,
//! `build_remap_manifest`'s v0 Keep-only behavior, and `REMAP.md`'s byte-derivability from the
//! JSON manifest — the property this issue's DoD names explicitly ("REMAP.md byte-derivable from
//! remap.json"). Complex/tabular cases live in a small fixture table per the crate's test-layout
//! rule; a test body stays an assert over a case.

use crate::batch::FileResult;
use crate::remap::{
    build_remap_manifest, render_remap_md, IdiomChoice, IdiomClass, NoduleRemap, PhylumRemap,
    RemapManifest, RemapOperation, RemapSafety, RemapSource,
};
use std::path::{Path, PathBuf};

/// A hand-built, non-trivial manifest fixture exercising every field (including a non-empty
/// `idiom_choices`, which `build_remap_manifest` never emits in v0 — this fixture stands in for
/// the "later, once idiom instrumentation lands" shape so the renderer/round-trip logic is
/// exercised over the *full* schema, not just the v0-Keep-only subset).
fn full_fixture() -> RemapManifest {
    RemapManifest {
        phylum: PhylumRemap {
            source_crate: "mycelium-std-cmp".to_string(),
            target_phylum: "std.cmp".to_string(),
        },
        nodules: vec![
            NoduleRemap {
                target_nodule: "std.cmp".to_string(),
                operation: RemapOperation::Keep,
                sources: vec![RemapSource {
                    rust_path: "crates/mycelium-std-cmp/src/lib.rs".to_string(),
                    rust_span: None,
                    moved_items: Vec::new(),
                }],
                rationale: "structure-preserving 1:1".to_string(),
                safety: RemapSafety::Safe,
                api_surface_changed: false,
                identity_neutral: true,
                guarantee: "Declared".to_string(),
            },
            NoduleRemap {
                target_nodule: "std.cmp.ordering".to_string(),
                operation: RemapOperation::Split,
                sources: vec![RemapSource {
                    rust_path: "crates/mycelium-std-cmp/src/lib.rs".to_string(),
                    rust_span: Some("12:1-40:2".to_string()),
                    moved_items: vec!["Ordering".to_string(), "cmp".to_string()],
                }],
                rationale: "example non-Keep entry — hypothetical future restructuring, exercised \
                             here only to prove the renderer/round-trip handle every schema \
                             variant, not just v0's Keep-only output"
                    .to_string(),
                safety: RemapSafety::Review,
                api_surface_changed: true,
                identity_neutral: false,
                guarantee: "Declared".to_string(),
            },
        ],
        idiom_choices: vec![IdiomChoice {
            target_span: "std.cmp:5".to_string(),
            rust_span: "lib.rs:3".to_string(),
            decision: "D4".to_string(),
            class: IdiomClass::Mechanical,
            chose: "erase &T".to_string(),
            alternatives: vec!["keep reference wrapper (no Mycelium reference type)".to_string()],
            reason: "a shared immutable borrow is a no-op under value semantics".to_string(),
        }],
    }
}

// ── Schema round-trip (JSON serialize/deserialize preserves every field) ─────────────────────────

#[test]
fn manifest_json_round_trips_byte_for_byte_in_structure() {
    let manifest = full_fixture();
    let json = serde_json::to_string_pretty(&manifest).expect("serialize");
    let round_tripped: RemapManifest = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        manifest, round_tripped,
        "manifest must survive a JSON round-trip unchanged"
    );
}

#[test]
fn v0_keep_only_manifest_round_trips() {
    let manifest = RemapManifest {
        phylum: PhylumRemap {
            source_crate: "mycelium-core".to_string(),
            target_phylum: "core".to_string(),
        },
        nodules: vec![NoduleRemap {
            target_nodule: "core".to_string(),
            operation: RemapOperation::Keep,
            sources: vec![RemapSource {
                rust_path: "crates/mycelium-core/src/lib.rs".to_string(),
                rust_span: None,
                moved_items: Vec::new(),
            }],
            rationale: "structure-preserving 1:1".to_string(),
            safety: RemapSafety::Safe,
            api_surface_changed: false,
            identity_neutral: true,
            guarantee: "Declared".to_string(),
        }],
        idiom_choices: Vec::new(),
    };
    let json = serde_json::to_string_pretty(&manifest).expect("serialize");
    let round_tripped: RemapManifest = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(manifest, round_tripped);
}

// ── REMAP.md byte-derivability from the JSON manifest (the M-1044 DoD property) ──────────────────

/// The core DoD property: `render_remap_md` reads only what's in the (de)serialized manifest, so
/// rendering directly and rendering a manifest that has round-tripped through JSON produce
/// byte-identical text. If the renderer ever grew a dependency on data outside the `RemapManifest`
/// struct (e.g. some ambient state), this test would catch the divergence — never-silent (G2).
#[test]
fn remap_md_is_byte_derivable_from_json() {
    let manifest = full_fixture();
    let direct = render_remap_md(&manifest);

    let json = serde_json::to_string_pretty(&manifest).expect("serialize to remap.json shape");
    let from_json: RemapManifest = serde_json::from_str(&json).expect("deserialize remap.json");
    let via_json = render_remap_md(&from_json);

    assert_eq!(
        direct, via_json,
        "REMAP.md must be byte-derivable from remap.json — rendering the JSON-round-tripped \
         manifest must produce identical output to rendering the manifest directly"
    );
}

/// The same property over the v0 Keep-only shape `build_remap_manifest` actually produces (as
/// opposed to the hand-built full-schema fixture above).
#[test]
fn remap_md_is_byte_derivable_from_json_for_a_real_v0_batch_manifest() {
    let results = vec![fixture_file_result("crates/mycelium-fake/src/lib.rs")];
    let manifest = build_remap_manifest(&results, Path::new("crates/mycelium-fake/src"));

    let direct = render_remap_md(&manifest);
    let json = serde_json::to_string_pretty(&manifest).expect("serialize");
    let from_json: RemapManifest = serde_json::from_str(&json).expect("deserialize");
    let via_json = render_remap_md(&from_json);

    assert_eq!(direct, via_json);
}

/// Rendering is deterministic: the same manifest rendered twice yields identical bytes (no
/// timestamps, no non-manifest state, no iteration-order nondeterminism).
#[test]
fn remap_md_rendering_is_deterministic() {
    let manifest = full_fixture();
    assert_eq!(render_remap_md(&manifest), render_remap_md(&manifest));
}

/// A literal `|` or newline in free-text schema fields (`rationale`/`reason`/`chose`/
/// `alternatives`/spans/paths — all plain `String`s per DN-109 §5.2, unconstrained by v0's own
/// fixed-constant usage) must not corrupt the rendered Markdown table: the `|` is escaped so it
/// doesn't terminate the cell early, and a newline is collapsed to a space so the row stays one
/// physical line. Every table row present, and every row still has exactly 7 unescaped `|`
/// column separators (a corrupted table would show more, from an unescaped cell value).
#[test]
fn remap_md_escapes_pipes_and_newlines_in_free_text_cells() {
    let mut manifest = full_fixture();
    manifest.nodules[0].rationale = "contains | a pipe\nand a newline".to_string();
    manifest.idiom_choices[0].reason = "also | pipes\nand newlines".to_string();

    let rendered = render_remap_md(&manifest);
    assert!(
        rendered.contains("contains \\| a pipe and a newline"),
        "expected the pipe escaped and the newline collapsed to a space in the nodule table; \
         got:\n{rendered}"
    );
    assert!(
        rendered.contains("also \\| pipes and newlines"),
        "expected the pipe escaped and the newline collapsed to a space in the idiom-choices \
         table; got:\n{rendered}"
    );

    // Every data row in both tables still has exactly 7 column-separating `|` (8 pipe characters
    // total per row: the 6 internal separators plus the leading/trailing border pipes) once the
    // escaped `\|` occurrences are excluded — i.e. the corrupted-cell text did not add extra
    // (unescaped) column breaks.
    for line in rendered.lines() {
        if !line.starts_with('|') {
            continue;
        }
        let unescaped_pipes = line.replace("\\|", "").matches('|').count();
        assert_eq!(
            unescaped_pipes, 8,
            "row has the wrong column count (table corrupted by an unescaped `|`): {line:?}"
        );
    }
}

// ── `build_remap_manifest` — the v0 Mechanical-only Keep-only behavior ────────────────────────────

/// A synthetic [`FileResult`] for a source path, without running the real transpiler (this module
/// tests `remap.rs`'s own logic, not `transpile_file`'s — `src/tests/batch.rs` already covers the
/// real transpile-then-summarize path end-to-end).
fn fixture_file_result(rust_path: &str) -> FileResult {
    use crate::gap::GapReport;
    FileResult {
        path: PathBuf::from(rust_path),
        myc: String::new(),
        report: GapReport {
            source: rust_path.to_string(),
            emitted_items: Vec::new(),
            gaps: Vec::new(),
            total_top_level_items: 0,
        },
    }
}

#[test]
fn build_remap_manifest_emits_one_keep_entry_per_file() {
    let results = vec![
        fixture_file_result("crates/mycelium-std-cmp/src/lib.rs"),
        fixture_file_result("crates/mycelium-std-cmp/src/foo/mod.rs"),
    ];
    let manifest = build_remap_manifest(&results, Path::new("crates/mycelium-std-cmp/src"));

    assert_eq!(manifest.nodules.len(), 2);
    for n in &manifest.nodules {
        assert_eq!(n.operation, RemapOperation::Keep);
        assert_eq!(n.safety, RemapSafety::Safe);
        assert!(n.identity_neutral);
        assert!(!n.api_surface_changed);
        assert_eq!(n.guarantee, "Declared");
    }
    // Distinct source files get distinct target nodules (mirrors M-1042's nodule-path
    // qualification — no collision).
    assert_ne!(
        manifest.nodules[0].target_nodule,
        manifest.nodules[1].target_nodule
    );
}

/// v0 never fabricates an idiom choice — the field is present (never-silent structurally) but
/// honestly empty until per-item idiom instrumentation lands (DN-109 §7-e Mechanical-only
/// boundary).
#[test]
fn build_remap_manifest_v0_idiom_choices_is_empty_not_fabricated() {
    let results = vec![fixture_file_result("crates/mycelium-core/src/lib.rs")];
    let manifest = build_remap_manifest(&results, Path::new("crates/mycelium-core/src"));
    assert!(manifest.idiom_choices.is_empty());
}

/// A batch over zero files yields an honest empty manifest, not a panic.
#[test]
fn build_remap_manifest_over_zero_files_is_empty_not_a_panic() {
    let manifest = build_remap_manifest(&[], Path::new("crates/mycelium-core/src"));
    assert!(manifest.nodules.is_empty());
    assert!(manifest.idiom_choices.is_empty());
}

// ── `phylum` derivation — data-driven fixture table (per the crate's test-layout rule) ────────────

struct PhylumCase {
    root: &'static str,
    expect_source_crate: &'static str,
    expect_target_phylum: &'static str,
}

const PHYLUM_CASES: &[PhylumCase] = &[
    // The common single-crate invocation: root is `<crate>/src`.
    PhylumCase {
        root: "crates/mycelium-std-cmp/src",
        expect_source_crate: "mycelium-std-cmp",
        expect_target_phylum: "std.cmp",
    },
    PhylumCase {
        root: "crates/mycelium-core/src",
        expect_source_crate: "mycelium-core",
        expect_target_phylum: "core",
    },
    // A root that is itself the crate directory (no trailing `src`) falls back to its own name.
    PhylumCase {
        root: "crates/mycelium-std-fs",
        expect_source_crate: "mycelium-std-fs",
        expect_target_phylum: "std.fs",
    },
    // A whole-corpus run rooted at `crates/` — not a single crate, so the raw dir name surfaces
    // (never fabricated as if it were a real crate name).
    PhylumCase {
        root: "crates",
        expect_source_crate: "crates",
        expect_target_phylum: "crates",
    },
];

#[test]
fn phylum_derivation_matches_the_fixture_table() {
    for case in PHYLUM_CASES {
        let manifest = build_remap_manifest(&[], Path::new(case.root));
        assert_eq!(
            manifest.phylum.source_crate, case.expect_source_crate,
            "root={}",
            case.root
        );
        assert_eq!(
            manifest.phylum.target_phylum, case.expect_target_phylum,
            "root={}",
            case.root
        );
    }
}

// ── Guarantee-tag posture (VR-5: no upgrade from the manifest's existence alone) ──────────────────

/// Every v0-built `NoduleRemap` is tagged `Declared`, never a stronger tag — the manifest records
/// proposals, it does not certify them (DN-109 §8 DoD).
#[test]
fn every_v0_nodule_remap_is_tagged_declared() {
    let results = vec![
        fixture_file_result("crates/mycelium-a/src/lib.rs"),
        fixture_file_result("crates/mycelium-a/src/nested/mod.rs"),
    ];
    let manifest = build_remap_manifest(&results, Path::new("crates/mycelium-a/src"));
    assert!(manifest.nodules.iter().all(|n| n.guarantee == "Declared"));
}
