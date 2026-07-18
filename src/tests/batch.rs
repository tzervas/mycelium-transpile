//! Unit tests for directory/batch mode (`src/batch.rs`, M-873 follow-on) — no new dev-dependency
//! (e.g. `tempfile`) added for this, per the crate's kickoff-scoped minimal-deps stance (see
//! `Cargo.toml`'s `quote` comment): fixtures are written directly under `std::env::temp_dir()` in
//! a per-test unique subdirectory, cleaned up at the end of each test.

use crate::batch::{
    common_ancestor, discover_rs_files, output_rel_path, summarize, transpile_batch,
};
use crate::gap::Category;
use proptest::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh, empty temp directory scoped to one test (`tag` disambiguates by test name; the
/// counter disambiguates parallel test threads sharing a `tag`/pid).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "mycelium-transpile-batch-test-{tag}-{}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(&path, content).expect("write fixture file");
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `discover_rs_files` recurses `*.rs` but skips any `tests` directory component (both a
/// crate-root `tests/` dir and the in-crate `src/tests/` layout) and any `tests.rs` file (the
/// older single-file test-module shape, e.g. `mycelium-std-fmt/src/tests.rs`).
#[test]
fn discover_skips_tests_dirs_and_files() {
    let tmp = TempDir::new("discover");
    tmp.write("lib.rs", "fn a(x: bool) -> bool { x }");
    tmp.write("helper.rs", "fn b(x: bool) -> bool { x }");
    tmp.write("tests.rs", "fn only_tests() {}");
    tmp.write("tests/integration.rs", "fn only_tests_2() {}");
    tmp.write("nested/mod_a.rs", "fn c(x: bool) -> bool { x }");
    tmp.write("nested/tests/deep.rs", "fn only_tests_3() {}");

    let found = discover_rs_files(tmp.path()).expect("discover succeeds");
    let names: Vec<String> = found
        .iter()
        .map(|p| {
            p.strip_prefix(tmp.path())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();

    assert_eq!(
        names,
        vec![
            "helper.rs".to_string(),
            "lib.rs".to_string(),
            "nested/mod_a.rs".to_string(),
        ],
        "expected exactly the non-test .rs files, sorted; got {names:?}"
    );
}

/// `discover_rs_files` over an empty directory returns an empty (not missing/erroring) list —
/// never-silent for the degenerate case.
#[test]
fn discover_over_empty_dir_returns_empty() {
    let tmp = TempDir::new("discover-empty");
    let found = discover_rs_files(tmp.path()).expect("discover succeeds");
    assert!(found.is_empty(), "expected no files, got {found:?}");
}

/// `transpile_batch` + `summarize` over a small multi-file fixture: per-file summaries roll up
/// exactly into the batch totals (sum of counts, union of gaps), and the per-file never-silent
/// invariant (emitted + gaps >= total items) holds for every file in the batch — the batch-mode
/// analogue of `src/tests/invariant.rs`'s single-file check.
#[test]
fn batch_summary_totals_match_per_file_sums() {
    let tmp = TempDir::new("summary");
    // All-expressible file.
    tmp.write(
        "a.rs",
        "enum Ordering { Less, Equal, Greater }\nfn is_lt(o: bool) -> bool { o }",
    );
    // A file with a mix of emitted + gapped items (a known hard gap: named-field struct).
    tmp.write("b.rs", "struct Foo { x: u8 }\nfn ok(x: bool) -> bool { x }");
    // An all-gapped file (macro_rules! def).
    tmp.write("c.rs", "macro_rules! m { () => {}; }");

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    assert_eq!(files.len(), 3, "expected all 3 fixture files discovered");

    let (results, failures) = transpile_batch(&files);
    assert!(
        failures.is_empty(),
        "expected every fixture file to parse, got failures={failures:?}"
    );
    assert_eq!(results.len(), 3);

    // Per-crate (per-file, here) never-silent invariant: emitted + gaps >= total items.
    for r in &results {
        let covered = r.report.emitted_items.len() + r.report.gaps.len();
        assert!(
            covered >= r.report.total_top_level_items,
            "never-silent invariant violated for {}: {} items but only {covered} \
             emitted+gap record(s)",
            r.path.display(),
            r.report.total_top_level_items
        );
    }

    let (batch_summary, union) = summarize(&results, tmp.path());
    assert_eq!(batch_summary.files.len(), 3);

    let sum_total_items: usize = batch_summary.files.iter().map(|f| f.total_items).sum();
    let sum_non_test: usize = batch_summary.files.iter().map(|f| f.non_test_items).sum();
    let sum_emitted: usize = batch_summary.files.iter().map(|f| f.emitted).sum();
    let sum_gaps: usize = batch_summary.files.iter().map(|f| f.gaps).sum();

    assert_eq!(batch_summary.totals.total_items, sum_total_items);
    assert_eq!(batch_summary.totals.non_test_items, sum_non_test);
    assert_eq!(batch_summary.totals.emitted, sum_emitted);
    assert_eq!(batch_summary.totals.gaps, sum_gaps);
    assert_eq!(
        union.gaps.len(),
        sum_gaps,
        "union.gap.json must carry every gap from every file, none dropped"
    );

    // At least one item landed (a.rs) and at least one gapped (b.rs's struct, c.rs's macro).
    assert!(
        sum_emitted > 0,
        "expected some emitted items across the batch"
    );
    assert!(sum_gaps > 0, "expected some gaps across the batch");

    // Per-category counts in the union must sum to the same total as `totals.category_counts`
    // (they're built from the same per-file counters) and must equal the raw gap count.
    let union_cat_sum: usize = union.category_counts.values().sum();
    assert_eq!(union_cat_sum, sum_gaps);
    let totals_cat_sum: usize = batch_summary.totals.category_counts.values().sum();
    assert_eq!(totals_cat_sum, sum_gaps);

    // Expressible percentage is a real percentage over the non-test denominator.
    assert!(
        (0.0..=100.0).contains(&batch_summary.totals.expressible_pct),
        "expressible_pct out of [0,100]: {}",
        batch_summary.totals.expressible_pct
    );

    // M-1044 / DN-109 §5.2: one pure-`Keep` remap entry per transpiled file, none dropped —
    // never-silent at the nodule-provenance level too, not just the gap level.
    assert_eq!(
        batch_summary.remap.nodules.len(),
        results.len(),
        "expected exactly one remap entry per transpiled file"
    );
    for n in &batch_summary.remap.nodules {
        assert_eq!(n.operation, crate::remap::RemapOperation::Keep);
        assert_eq!(n.safety, crate::remap::RemapSafety::Safe);
        assert!(n.identity_neutral, "a pure Keep is identity-neutral");
        assert!(
            !n.api_surface_changed,
            "a pure Keep must not claim an API-surface change"
        );
        assert_eq!(n.guarantee, "Declared");
        assert_eq!(n.sources.len(), 1, "a Keep has exactly one source file");
    }
    // v0 Mechanical-only: no idiom-choice instrumentation exists yet, so the field is honestly
    // empty rather than fabricated (see `src/remap.rs` module docs).
    assert!(batch_summary.remap.idiom_choices.is_empty());
}

/// A batch over zero files (e.g. a directory that discovers nothing) yields an honest all-zero
/// summary, not a divide-by-zero panic or a fabricated percentage.
#[test]
fn batch_summary_over_zero_files_is_all_zero_not_a_panic() {
    let (batch_summary, union) = summarize(&[], Path::new("crates/x/src"));
    assert!(batch_summary.files.is_empty());
    assert_eq!(batch_summary.totals.total_items, 0);
    assert_eq!(batch_summary.totals.emitted, 0);
    assert_eq!(batch_summary.totals.gaps, 0);
    assert_eq!(batch_summary.totals.expressible_pct, 0.0);
    assert!(union.gaps.is_empty());
    // Honest all-zero: no nodules recorded either (nothing was transpiled).
    assert!(batch_summary.remap.nodules.is_empty());
}

// ── M-1006 Phase-2: path-qualified batch output (`output_rel_path`) ──────────────────────────────

/// A file under the batch root maps to its **relative path** with `.rs` stripped, so a whole-corpus
/// run mirrors the source tree under the out-dir.
#[test]
fn output_rel_path_mirrors_the_tree_under_root() {
    let root = Path::new("crates");
    let got = output_rel_path(Path::new("crates/mycelium-core/src/lib.rs"), root)
        .expect("under root -> Ok");
    assert_eq!(got, PathBuf::from("mycelium-core/src/lib"));
}

/// The whole-corpus collision the fix targets: two crates' `lib.rs` must map to **distinct** outputs
/// (path-qualified), never the same flat `lib` — the property that makes the run non-lossy.
#[test]
fn same_stem_files_in_different_crates_get_distinct_outputs() {
    let root = Path::new("crates");
    let a = output_rel_path(Path::new("crates/mycelium-core/src/lib.rs"), root).unwrap();
    let b = output_rel_path(Path::new("crates/mycelium-std/src/lib.rs"), root).unwrap();
    assert_ne!(
        a, b,
        "two crates' lib.rs must not collide at the same output path"
    );
    assert_eq!(a, PathBuf::from("mycelium-core/src/lib"));
    assert_eq!(b, PathBuf::from("mycelium-std/src/lib"));
}

/// A flat single-crate `src/` root reduces to the bare stem — identical to the pre-Phase-2 flat
/// naming, which is why the committed `gen/myc-drafts/` 17-target manifest sees zero churn.
#[test]
fn flat_single_crate_root_reduces_to_bare_stem() {
    let root = Path::new("crates/mycelium-std-fs/src");
    let got = output_rel_path(Path::new("crates/mycelium-std-fs/src/lib.rs"), root).unwrap();
    assert_eq!(got, PathBuf::from("lib"));
}

/// A file not under the batch root falls back to the bare stem via `Err` (the caller warns — never
/// a silent mis-placement, G2).
#[test]
fn not_under_root_falls_back_to_bare_stem_via_err() {
    let root = Path::new("crates");
    let got = output_rel_path(Path::new("/elsewhere/foo.rs"), root);
    assert_eq!(got, Err(PathBuf::from("foo")));
}

/// Only the final `.rs` is stripped — a `foo.bar.rs` source keeps its `foo.bar` stem (so `append_ext`
/// in the CLI yields `foo.bar.myc`, not `foo.myc`).
#[test]
fn only_the_rs_extension_is_stripped() {
    let root = Path::new("crates/x/src");
    let got = output_rel_path(Path::new("crates/x/src/foo.bar.rs"), root).unwrap();
    assert_eq!(got, PathBuf::from("foo.bar"));
}

// ── `--files` multi-crate-root output-path collision fix (`common_ancestor`) ────────────────────
//
// Regression coverage for the CLI `--files` bug: rooting output naming at only `files[0].parent()`
// left every OTHER file whose crate lives in a sibling directory failing `output_rel_path`'s
// `strip_prefix` and falling back to a bare stem — so three crates' `src/lib.rs` batched together
// all collided on `lib.myc`, silently overwriting one another. `common_ancestor` fixes this by
// rooting at every file's SHARED ancestor instead.

/// The common single-directory case (mutually-referencing sibling files named explicitly, e.g.
/// `--files checkty.rs,elab.rs,eval.rs`) reduces to that shared directory — identical to the
/// pre-fix `files[0].parent()` root, so no existing single-crate `--files` output changes.
#[test]
fn common_ancestor_of_siblings_in_one_dir_is_that_dir() {
    let files = vec![
        PathBuf::from("crates/mycelium-l1/src/checkty.rs"),
        PathBuf::from("crates/mycelium-l1/src/elab.rs"),
        PathBuf::from("crates/mycelium-l1/src/eval.rs"),
    ];
    let root = common_ancestor(&files).expect("a real common ancestor exists for these siblings");
    assert_eq!(root, PathBuf::from("crates/mycelium-l1/src"));
    // And every file's output_rel_path against that root is the bare stem, exactly as before.
    for (f, want) in [
        (&files[0], "checkty"),
        (&files[1], "elab"),
        (&files[2], "eval"),
    ] {
        assert_eq!(output_rel_path(f, &root).unwrap(), PathBuf::from(want));
    }
}

/// **The bug this fixes**: three crates' `src/lib.rs`, batched via `--files`, must NOT collide.
/// `common_ancestor` walks up to the shared `crates/` directory, so `output_rel_path` succeeds
/// (`Ok`, never the bare-stem `Err` fallback) for all three, and each is crate-qualified.
#[test]
fn common_ancestor_of_three_crate_roots_yields_three_distinct_outputs() {
    let files = vec![
        PathBuf::from("crates/mycelium-std-sys-host/src/lib.rs"),
        PathBuf::from("crates/mycelium-std-rand/src/lib.rs"),
        PathBuf::from("crates/mycelium-std-time/src/lib.rs"),
    ];
    let root =
        common_ancestor(&files).expect("a real common ancestor exists across these crate roots");
    assert_eq!(root, PathBuf::from("crates"));

    let outs: Vec<PathBuf> = files
        .iter()
        .map(|f| output_rel_path(f, &root).expect("every file must resolve under the shared root"))
        .collect();
    assert_eq!(
        outs,
        vec![
            PathBuf::from("mycelium-std-sys-host/src/lib"),
            PathBuf::from("mycelium-std-rand/src/lib"),
            PathBuf::from("mycelium-std-time/src/lib"),
        ]
    );
    // The never-silent property the bug violated: all outputs pairwise distinct.
    let unique: std::collections::HashSet<&PathBuf> = outs.iter().collect();
    assert_eq!(
        unique.len(),
        outs.len(),
        "three crates' lib.rs must map to three DISTINCT outputs, never a collision; got {outs:?}"
    );
}

/// **HIGH regression (PR #1545 review) — the `--files` output-collision fix's OWN bug**: a
/// `--files` set with NO common ancestor (mixed absolute + relative paths) must return `None`, not
/// the empty path. Before this fix, `common_ancestor` collapsed this case to `Some(PathBuf::new())`
/// (a bare `PathBuf`, no `Option` at all); because `Path::strip_prefix("")` **always succeeds** and
/// hands the input back unchanged, `output_rel_path` against that empty root then returned the
/// ABSOLUTE member of the set **unchanged and `Ok`** — never the `Err` fallback the doc comment
/// claimed was the backstop. This assertion alone is the direct fix witness: it does not even
/// TYPECHECK against the pre-fix signature (`fn common_ancestor(...) -> PathBuf`, no `Option`), and
/// the pre-fix *value* for this exact input was the empty `PathBuf` — the unsafe case, not `None`.
#[test]
fn common_ancestor_of_mixed_absolute_and_relative_files_is_none() {
    let files = vec![
        PathBuf::from("checkty.rs"),               // relative
        PathBuf::from("/tmp/other-crate/elab.rs"), // absolute — the escape vector
    ];
    let root = common_ancestor(&files);
    assert!(
        root.is_none(),
        "a mixed absolute/relative --files set has no real common ancestor, got {root:?}"
    );
}

/// The full G2 property `common_ancestor`'s `None` return exists to guarantee, exercised end-to-end
/// against `batch.rs`'s public API: a `--files` set with no common ancestor must NEVER produce an
/// output path that escapes the declared `--out-dir`, however the caller routes on
/// `common_ancestor`'s result. This mirrors the CLI's own `write_batch_and_maybe_vet` routing
/// (`root: Option<&Path>` — `Some` takes `output_rel_path`'s `Ok`/`Err` arms, `None` always falls
/// back to the bare stem, never `output_rel_path`'s `Ok` arm) purely against the tested library
/// functions, so it is a real regression guard, not a reimplementation that could drift from the
/// bin's actual logic.
///
/// **Non-vacuous**: against the pre-fix `common_ancestor` (`Some(PathBuf::new())` for this exact
/// input), the `Some` arm below would run `output_rel_path(&"/tmp/other-crate/elab.rs", "")`, which
/// returns `Ok("/tmp/other-crate/elab.rs")` (`strip_prefix("")` is a no-op) — an ABSOLUTE
/// `rel_noext` — and `out_dir.join(<absolute>)` then discards `out_dir` entirely
/// (`Path::join`'s absolute-path-override), so the `starts_with(out_dir)` assertion below would
/// FAIL for that file. With the fix (`None`), the `None` arm's bare-stem fallback is always
/// relative, so the join always stays under `out_dir` and the assertion passes.
#[test]
fn no_common_ancestor_files_set_never_escapes_out_dir() {
    let out_dir = Path::new("/some/declared/out-dir");
    let files = vec![
        PathBuf::from("checkty.rs"),               // relative
        PathBuf::from("/tmp/other-crate/elab.rs"), // absolute — the escape vector
    ];
    let root = common_ancestor(&files);

    for f in &files {
        let rel_noext = match &root {
            Some(root) => output_rel_path(f, root).unwrap_or_else(|fallback| fallback),
            None => PathBuf::from(f.file_stem().and_then(|s| s.to_str()).unwrap_or("output")),
        };
        let written = out_dir.join(&rel_noext);
        assert!(
            written.starts_with(out_dir),
            "output path {written:?} for {f:?} escaped the declared out-dir {out_dir:?} \
             (rel_noext={rel_noext:?}, root={root:?}) — the exact G2 silent-misplacement bug this \
             test guards against"
        );
    }
}

/// End-to-end (through `transpile_batch` + real file writes, not just path arithmetic): batching two
/// real crates that each declare same-named `src/lib.rs` files with DIFFERENT content must write two
/// distinct `.myc` files whose content is NOT cross-contaminated — the full regression the CLI bug
/// would have broken (one write silently overwriting the other, so both ended up with the second
/// crate's content).
#[test]
fn two_crate_roots_transpile_to_distinct_uncontaminated_myc_outputs() {
    let tmp = TempDir::new("two-crate-roots-e2e");
    tmp.write(
        "mycelium-std-rand/src/lib.rs",
        "pub struct RandMarker(u8);\nfn rand_helper(x: bool) -> bool { x }",
    );
    tmp.write(
        "mycelium-std-time/src/lib.rs",
        "pub struct TimeMarker(u16);\nfn time_helper(x: bool) -> bool { x }",
    );

    let files = vec![
        tmp.path().join("mycelium-std-rand/src/lib.rs"),
        tmp.path().join("mycelium-std-time/src/lib.rs"),
    ];
    let root =
        common_ancestor(&files).expect("a real common ancestor exists for these two crate roots");
    assert_eq!(
        root,
        tmp.path().to_path_buf(),
        "the two crates' shared ancestor is the fixture root itself"
    );

    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");
    assert_eq!(results.len(), 2);

    let mut out_paths = std::collections::HashSet::new();
    let mut by_marker: std::collections::HashMap<PathBuf, String> =
        std::collections::HashMap::new();
    for r in &results {
        let rel = output_rel_path(&r.path, &root).expect("under the common-ancestor root -> Ok");
        assert!(
            out_paths.insert(rel.clone()),
            "output path collision at {rel:?} across the batch (the bug this test guards against)"
        );
        by_marker.insert(rel, r.myc.clone());
    }
    assert_eq!(out_paths.len(), 2, "expected two distinct output paths");

    let rand_myc = by_marker
        .get(&PathBuf::from("mycelium-std-rand/src/lib"))
        .expect("rand crate's own output path present");
    let time_myc = by_marker
        .get(&PathBuf::from("mycelium-std-time/src/lib"))
        .expect("time crate's own output path present");

    // Content is NOT cross-contaminated: each crate's own marker is present, the SIBLING's marker
    // is absent, from the correct output.
    assert!(
        rand_myc.contains("RandMarker") && !rand_myc.contains("TimeMarker"),
        "rand crate's output must contain only its own content; got:\n{rand_myc}"
    );
    assert!(
        time_myc.contains("TimeMarker") && !time_myc.contains("RandMarker"),
        "time crate's output must contain only its own content; got:\n{time_myc}"
    );
    assert_ne!(
        rand_myc, time_myc,
        "the two crates' emitted .myc text must differ (they have different source)"
    );
}

// ── Gap-close-2 (DN-34 §8.19/§8.20): the batch-scoped cross-nodule symbol table ─────────────────
//
// `transpile_batch`'s two real sibling files below: `checkty.rs` declares an emittable `Width`
// struct and a deliberately-unemittable `Env` struct (a named-field record whose field type has no
// mapping, so it stays a real `Category::Struct` gap — never in `checkty`'s `emitted_items`).
// `mono.rs` imports both, plus an external `std::` name, exercising: a full resolve (`Width`), an
// in-batch-sibling-but-gapped miss (`Env`), and an out-of-batch miss (`std::collections::BTreeMap`)
// side by side in the same run.

fn checkty_fixture() -> &'static str {
    "pub struct Width(u8);\nstruct Env { x: NotARealMappableType }\nfn helper(x: bool) -> bool { x }"
}

fn mono_fixture() -> &'static str {
    "use std::collections::BTreeMap;\nuse crate::checkty::{Width, Env};\nfn mono_helper(x: bool) -> bool { x }"
}

/// The end-to-end cross-nodule resolution: `mono.rs`'s `use crate::checkty::{Width, Env};`
/// partially resolves (`Width` — `checkty` actually emitted it) and partially gaps (`Env` — a
/// batch sibling, but it gapped that name rather than emitting it), landing as ONE `Outcome::Emitted`
/// item carrying the unresolved leaf as a `sub_gaps` entry (both "emitted" and "honestly flagged" —
/// never neither, G2).
#[test]
fn cross_nodule_use_partially_resolves_against_a_batch_sibling() {
    let tmp = TempDir::new("cross-nodule-partial");
    tmp.write("checkty.rs", checkty_fixture());
    tmp.write("mono.rs", mono_fixture());

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let mono = results
        .iter()
        .find(|r| r.path.file_name().unwrap() == "mono.rs")
        .expect("mono.rs result present");

    // L2-B: a resolved *type* leaf co-includes the sibling type surface (oracle self-containment)
    // with EXPLAIN naming the home path — never a bare `use Width;` short-form collapse.
    assert!(
        mono.myc.contains("type Width") || mono.myc.contains("use checkty.Width;"),
        "expected co-include `type Width` (L2-B) or qualified use in mono.myc, got:\n{}",
        mono.myc
    );
    assert!(
        !mono.myc.lines().any(|l| l.trim() == "use Width;"),
        "must never emit a bare, unqualified `use Width;` — no-bare-name-collapse (VR-5/G2); \
         got:\n{}",
        mono.myc
    );
    assert!(
        mono.report
            .emitted_items
            .iter()
            .any(|n| n.contains("Width")),
        "expected an emitted item naming Width, got {:?}",
        mono.report.emitted_items
    );

    // `Env` and the external `std::` import both still gap — never silently dropped, never
    // guessed — but now with the NEW, more precise reasons (a real symbol table exists, so the
    // old blanket "no cross-nodule symbol table" claim would itself be inaccurate).
    let import_gaps: Vec<&str> = mono
        .report
        .gaps
        .iter()
        .filter(|g| g.category == Category::Import)
        .map(|g| g.reason.as_str())
        .collect();
    assert!(
        import_gaps.iter().any(|r| r.contains("Env")
            && r.contains("checkty")
            && r.contains("gapped it rather than emitting it")),
        "expected an Env-naming, sibling-gapped-it reason among {import_gaps:?}"
    );
    assert!(
        import_gaps
            .iter()
            .any(|r| r.contains("BTreeMap") && r.contains("not a sibling module")),
        "expected a BTreeMap-naming, not-a-batch-sibling reason among {import_gaps:?}"
    );
}

/// The other half of the correctness bar the task names explicitly: a resolved cross-nodule `use`
/// is only the checker-accepted form when the referenced item is itself `pub` in its home nodule
/// (DN-113/M-1060's `resolve_imports` is `pub`-gated) — so `checkty.rs`'s `Width` (referenced by
/// `mono.rs` above) must be emitted `pub`, while `helper` (never referenced by any sibling) stays
/// unmarked, exactly as before this lever landed.
#[test]
fn resolved_cross_nodule_reference_marks_the_sibling_item_pub() {
    let tmp = TempDir::new("pub-propagation");
    tmp.write("checkty.rs", checkty_fixture());
    tmp.write("mono.rs", mono_fixture());

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let checkty = results
        .iter()
        .find(|r| r.path.file_name().unwrap() == "checkty.rs")
        .expect("checkty.rs result present");

    assert!(
        checkty.myc.contains("pub type Width"),
        "Width is referenced by a sibling's resolved `use` — expected a `pub` prefix; got:\n{}",
        checkty.myc
    );
    // `helper` was never imported by any sibling in this batch, so it stays exactly as before —
    // no spurious `pub` on every item (only the genuinely-referenced ones).
    assert!(
        !checkty.myc.contains("pub fn helper"),
        "helper is never cross-nodule-referenced — must not be marked pub; got:\n{}",
        checkty.myc
    );
}

/// DN-133 (M-1094) tier (ii) — the currently-honest boundary of the cross-nodule resolution gate,
/// pinned rather than silently assumed: `bar.rs` imports `Foo` from a batch sibling `foo.rs` (the
/// `use` leaf itself DOES resolve — `Foo` the struct is a real emitted item), and calls its
/// receiver-less constructor qualified (`Foo::new(x)`). This does **not** yet resolve to the
/// mangled `Foo__new` — the batch symbol table indexes each sibling's emitted items by their own
/// TOP-LEVEL name (`GapReport::emitted_items`), and an inherent `impl` block is recorded under its
/// own coarse `"impl Foo"` name, not each individual mangled method it contains (see
/// `emit.rs::cross_nodule_resolve_mangled`'s doc). So this stays a real, FLAGged residual (never a
/// false positive — VR-5/G2), not a silently-assumed close; a follow-up that also indexes each
/// mangled per-method name in the batch table would close it.
#[test]
fn qualified_call_to_cross_nodule_associated_fn_is_a_currently_honest_no_op() {
    let tmp = TempDir::new("dn133-cross-nodule");
    tmp.write(
        "foo.rs",
        "pub struct Foo(u32);\nimpl Foo {\n    pub fn new(x: u32) -> Self { Foo(x) }\n}\n",
    );
    tmp.write(
        "bar.rs",
        "use crate::foo::Foo;\npub fn make(x: u32) -> Foo { Foo::new(x) }\n",
    );

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let bar = results
        .iter()
        .find(|r| r.path.file_name().unwrap() == "bar.rs")
        .expect("bar.rs result present");

    // The `use crate::foo::Foo;` leaf itself DOES resolve — L2-B co-includes the type (or full-path use).
    assert!(
        bar.myc.contains("type Foo")
            || bar.myc.contains("use foo.Foo;")
            || bar.myc.contains(".Foo;"),
        "expected the `Foo` import to resolve (unaffected by this DN-133 tier), got:\n{}",
        bar.myc
    );
    // But `make`'s qualified call to `Foo::new` still gaps today — pinned, not silently assumed.
    assert!(
        !bar.report.emitted_items.iter().any(|n| n == "make"),
        "`make` must still gap (the cross-nodule mangled-method tier is a no-op today, per this \
         test's own doc): emitted={:?}",
        bar.report.emitted_items
    );
    assert!(
        bar.report.gaps.iter().any(|g| g
            .reason
            .contains("did not resolve to a known-emitted associated fn")),
        "expected the DN-133 resolution-gap reason, got {:?}",
        bar.report
            .gaps
            .iter()
            .map(|g| &g.reason)
            .collect::<Vec<_>>()
    );
}

/// A batch with **no** in-batch cross-referencing `use` (every file is import-independent) is
/// byte-identical to the pre-gap-close-2 driver: every `use` still gaps, nothing is ever marked
/// `pub`. Guards against the two-pass driver silently changing behavior for the common case.
#[test]
fn batch_with_no_cross_file_use_is_unaffected() {
    let tmp = TempDir::new("no-cross-file-use");
    tmp.write(
        "a.rs",
        "pub struct Foo(u8);\nfn helper(x: bool) -> bool { x }",
    );
    tmp.write("b.rs", "pub struct Bar(u8);\nuse std::fmt;\n");

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    let (results, _failures) = transpile_batch(&files);

    let a = results
        .iter()
        .find(|r| r.path.file_name().unwrap() == "a.rs")
        .unwrap();
    let b = results
        .iter()
        .find(|r| r.path.file_name().unwrap() == "b.rs")
        .unwrap();
    assert!(
        !a.myc.contains("pub "),
        "no sibling references Foo/helper — nothing should be pub-marked; got:\n{}",
        a.myc
    );
    assert!(
        b.report.gaps.iter().any(|g| g.category == Category::Import),
        "the unresolvable `use std::fmt;` must still gap"
    );
}

/// A rename/self/glob leaf on an in-batch head never resolves (scoped OUT of this increment —
/// deliberately, not a bug): a solitary `use crate::checkty::Width as W;` still gaps the whole
/// item (the only leaf is a `Rename`), never silently emitting the aliased form.
#[test]
fn renamed_glob_and_self_leaves_on_an_in_batch_head_still_gap() {
    let tmp = TempDir::new("scoped-out-leaves");
    tmp.write("checkty.rs", checkty_fixture());
    tmp.write(
        "consumer.rs",
        "use crate::checkty::Width as W;\nuse crate::checkty::*;\nfn f(x: bool) -> bool { x }",
    );

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let consumer = results
        .iter()
        .find(|r| r.path.file_name().unwrap() == "consumer.rs")
        .unwrap();
    let import_gap_count = consumer
        .report
        .gaps
        .iter()
        .filter(|g| g.category == Category::Import)
        .count();
    assert_eq!(
        import_gap_count, 2,
        "both the rename and the glob must gap (scoped out, never guessed); gaps: {:?}",
        consumer.report.gaps
    );
    assert!(
        !consumer.myc.contains("use "),
        "neither leaf resolves, so consumer.myc must carry no `use` line at all; got:\n{}",
        consumer.myc
    );
}

// ── M-1084 (Import net-close): `self::`/`super::` + cross-phylum resolution ────────────────────
//
// These fixtures write under a REAL `<crate>/src/...` layout (unlike the flat fixtures above) so
// `transpile::derive_crate_ident`/`derive_module_segments` see genuine crate/module structure —
// exercising the same derivation path the real corpus (`gen/myc-drafts/regenerate.sh`) uses.

/// `self::`/`super::` resolve relative to the CURRENT file's own module path, within one crate:
/// `foo/mod.rs`'s `pub use self::bar::Thing;` (self:: + a submodule) and `mono/mod.rs`'s
/// `use super::checkty::Width;` (super:: up to the crate root, then back down a DIFFERENT branch)
/// both resolve — the two residual forms gap-close-2's own doc named as scoped out.
#[test]
fn self_and_super_relative_use_resolve_within_one_crate() {
    let tmp = TempDir::new("self-super-relative");
    tmp.write(
        "mycrate/src/checkty.rs",
        "pub struct Width(u8);\nfn helper(x: bool) -> bool { x }",
    );
    tmp.write(
        "mycrate/src/foo/mod.rs",
        "pub use self::bar::Thing;\nfn foo_helper(x: bool) -> bool { x }",
    );
    tmp.write("mycrate/src/foo/bar.rs", "pub struct Thing(u8);");
    tmp.write(
        "mycrate/src/mono/mod.rs",
        "use super::checkty::Width;\nfn mono_helper(x: bool) -> bool { x }",
    );

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    assert_eq!(files.len(), 4, "expected all 4 fixture files discovered");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let foo_mod = results
        .iter()
        .find(|r| r.path.ends_with("foo/mod.rs"))
        .expect("foo/mod.rs result present");
    assert!(
        (foo_mod.myc.contains("type Thing") || foo_mod.myc.contains("use mycrate.foo.bar.Thing;"))
            && !foo_mod.myc.lines().any(|l| l.trim() == "use Thing;"),
        "self::bar::Thing must resolve to L2-B co-include or full-path use (M-1084), never bare; \
         got:\n{}",
        foo_mod.myc
    );
    assert!(
        foo_mod.myc.contains("mycrate.foo.bar") || foo_mod.myc.contains("type Thing"),
        "provenance/home path or co-include must appear; got:\n{}",
        foo_mod.myc
    );

    let mono_mod = results
        .iter()
        .find(|r| r.path.ends_with("mono/mod.rs"))
        .expect("mono/mod.rs result present");
    assert!(
        (mono_mod.myc.contains("type Width") || mono_mod.myc.contains("use mycrate.checkty.Width;"))
            && !mono_mod.myc.lines().any(|l| l.trim() == "use Width;"),
        "super::checkty::Width must resolve to L2-B co-include or full-path use (M-1084), never bare; \
         got:\n{}",
        mono_mod.myc
    );

    // Both resolved references mark their home items `pub` (DN-113/M-1060 pub-gating), keyed by the
    // sibling's own nodule path (the M-1084 fix — never mismatched against a Rust-side module key a
    // consumer happened to look it up through).
    let checkty = results
        .iter()
        .find(|r| r.path.ends_with("checkty.rs"))
        .unwrap();
    assert!(
        checkty.myc.contains("pub type Width"),
        "Width is referenced via super:: — expected pub; got:\n{}",
        checkty.myc
    );
    let bar = results.iter().find(|r| r.path.ends_with("bar.rs")).unwrap();
    assert!(
        bar.myc.contains("pub type Thing"),
        "Thing is referenced via self:: — expected pub; got:\n{}",
        bar.myc
    );
}

/// A `use <phylum>::<mod>::Item;` resolves against a SIBLING PHYLUM's own file when that phylum's
/// files are in the SAME batch (M-1084's general mechanism — no CLI wiring, no `crate-a`/`crate-b`
/// special-casing: any two crates' `src/` trees discovered together form one batch, exactly as a
/// multi-crate `--files` invocation would). The referenced item is marked `pub` in ITS OWN crate's
/// output too (cross-phylum pub-propagation).
#[test]
fn cross_phylum_use_resolves_against_a_sibling_crate_in_the_same_batch() {
    let tmp = TempDir::new("cross-phylum");
    tmp.write(
        "crate-a/src/lib.rs",
        "use crate_b::Foo;\nfn a_helper(x: bool) -> bool { x }",
    );
    tmp.write(
        "crate-b/src/lib.rs",
        "pub struct Foo(u8);\nfn b_helper(x: bool) -> bool { x }",
    );

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    assert_eq!(files.len(), 2, "expected both crates' lib.rs discovered");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let a = results
        .iter()
        .find(|r| r.path.ends_with("crate-a/src/lib.rs"))
        .expect("crate-a lib.rs result present");
    assert!(
        (a.myc.contains("type Foo") || a.myc.contains(".Foo;"))
            && !a.myc.lines().any(|l| l.trim() == "use Foo;"),
        "use crate_b::Foo must resolve cross-phylum (L2-B co-include or qualified use), never bare; \
         got:\n{}",
        a.myc
    );
    assert!(
        a.myc.contains("crate.b") || a.myc.contains("homes: crate.b"),
        "cross-phylum resolve must name sibling home crate.b; got:\n{}",
        a.myc
    );
    assert!(
        a.report.emitted_items.iter().any(|n| n.contains("Foo")),
        "expected an emitted item naming Foo, got {:?}",
        a.report.emitted_items
    );

    let b = results
        .iter()
        .find(|r| r.path.ends_with("crate-b/src/lib.rs"))
        .expect("crate-b lib.rs result present");
    assert!(
        b.myc.contains("pub type Foo"),
        "Foo is referenced cross-phylum by crate-a — expected pub in crate-b's own output; got:\n{}",
        b.myc
    );
    // `b_helper` was never referenced by anything — no spurious pub (only the genuinely-referenced
    // item is marked, cross-phylum or not).
    assert!(
        !b.myc.contains("pub fn b_helper"),
        "b_helper is never cross-phylum-referenced — must not be marked pub; got:\n{}",
        b.myc
    );
}

/// Precedence (M-1084, mirrors real Rust's own shadowing rule — see `symtab.rs` module docs): when
/// a crate has its OWN submodule literally named the same as a sibling PHYLUM's extern-crate
/// identifier, the same-crate interpretation wins — never a wrong cross-phylum resolve.
#[test]
fn same_crate_submodule_shadows_a_same_named_sibling_phylum() {
    let tmp = TempDir::new("shadow-precedence");
    // crate-a has ITS OWN submodule literally named `crate_b` (the same identifier crate-b's own
    // extern-crate name would resolve to).
    tmp.write(
        "crate-a/src/lib.rs",
        "use crate_b::Foo;\nfn a_helper(x: bool) -> bool { x }",
    );
    tmp.write("crate-a/src/crate_b.rs", "pub struct Foo(u16);");
    // A genuinely separate sibling phylum, ALSO named `crate-b`, ALSO exporting a `Foo`.
    tmp.write("crate-b/src/lib.rs", "pub struct Foo(u8);");

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let a = results
        .iter()
        .find(|r| r.path.ends_with("crate-a/src/lib.rs"))
        .unwrap();
    assert!(
        a.myc.contains("crate.a.crate_b")
            || a.myc.lines().any(|l| {
                let t = l.trim();
                t == "use a.crate_b.Foo;" || t.contains("a.crate_b.Foo")
            }),
        "expected the SAME-CRATE submodule interpretation to win (home crate.a.crate_b co-include \
         or full-path use), not the sibling phylum's crate.b; got:\n{}",
        a.myc
    );
    assert!(
        !a.myc.contains("use crate.b.Foo")
            && !a.myc.lines().any(|l| l.contains("use crate.b."))
            && !a.myc.contains("homes: crate.b"),
        "must never resolve against the sibling phylum when a same-crate submodule shadows it; \
         got:\n{}",
        a.myc
    );

    // The GENUINE sibling phylum's own `Foo` must NOT be marked `pub` — it was never actually
    // referenced (the same-crate submodule shadowed it, so crate-b's Foo is unreferenced).
    let b = results
        .iter()
        .find(|r| r.path.ends_with("crate-b/src/lib.rs"))
        .unwrap();
    assert!(
        !b.myc.contains("pub type Foo"),
        "crate-b's Foo was shadowed, never actually referenced — must not be marked pub; got:\n{}",
        b.myc
    );
}

/// **CRITICAL fix (strict-review finding on M-1084/PR #1541), NON-ROOT twin of
/// `same_crate_submodule_shadows_a_same_named_sibling_phylum` above.** Real Rust's bare-`use`
/// same-crate-vs-extern-crate shadowing is ROOT-FILE-ONLY lexical shadowing: a crate-root
/// `mod foo;` is a name only in the crate-root file's own scope, so a NON-root file's bare heads
/// never see it and resolve via the extern prelude (cross-phylum) exclusively. Before this fix,
/// `candidate_lookup_keys` tried the same-crate interpretation FIRST for every file regardless of
/// where the `use` was written, so this exact non-root case silently mis-bound to the unrelated
/// same-crate submodule instead of the genuine sibling phylum.
#[test]
fn cross_phylum_use_from_non_root_file_wins_over_unrelated_same_crate_submodule() {
    let tmp = TempDir::new("non-root-cross-phylum");
    // crate-a's NON-ROOT file `sub.rs` (current_module = ["sub"]) references a bare `crate_b`.
    tmp.write(
        "crate-a/src/sub.rs",
        "use crate_b::Foo;\nfn a_helper(x: bool) -> bool { x }",
    );
    // crate-a ALSO has its own crate-root submodule literally named `crate_b` -- but `sub.rs`
    // itself never lexically sees it (it isn't the crate-root file), so this must NOT shadow the
    // genuine sibling phylum below.
    tmp.write("crate-a/src/crate_b.rs", "pub struct Foo(u16);");
    tmp.write(
        "crate-a/src/lib.rs",
        "fn root_helper(x: bool) -> bool { x }",
    );
    // The GENUINE sibling phylum, also named `crate-b`, exporting a `Foo`.
    tmp.write("crate-b/src/lib.rs", "pub struct Foo(u8);");

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let sub = results
        .iter()
        .find(|r| r.path.ends_with("crate-a/src/sub.rs"))
        .expect("crate-a/src/sub.rs result present");
    assert!(
        sub.myc.contains("homes: crate.b")
            || sub.myc.lines().any(|l| l.trim() == "use crate.b.Foo;"),
        "expected sub.rs's bare `use crate_b::Foo;` to resolve CROSS-PHYLUM to sibling crate.b \
         (co-include home or `use crate.b.Foo;`), never crate-a's same-named submodule; got:\n{}",
        sub.myc
    );
    assert!(
        !sub.myc.contains("crate.a.crate_b"),
        "must NEVER resolve against crate-a's own same-crate `crate_b` submodule (nodule path \
         `crate.a.crate_b`) from a non-root file -- that submodule is not lexically in scope \
         there; got:\n{}",
        sub.myc
    );

    // The genuine sibling phylum's Foo IS referenced -- must be marked pub in its own output.
    let b = results
        .iter()
        .find(|r| r.path.ends_with("crate-b/src/lib.rs"))
        .expect("crate-b lib.rs result present");
    assert!(
        b.myc.contains("pub type Foo"),
        "crate-b's Foo is referenced cross-phylum from sub.rs -- expected pub; got:\n{}",
        b.myc
    );

    // crate-a's OWN `crate_b.rs` submodule's Foo was never actually referenced (sub.rs's bare
    // head never resolved against it) -- must not be marked pub.
    let own_crate_b = results
        .iter()
        .find(|r| r.path.ends_with("crate-a/src/crate_b.rs"))
        .expect("crate-a/src/crate_b.rs result present");
    assert!(
        !own_crate_b.myc.contains("pub type Foo") && !own_crate_b.myc.contains("pub struct Foo"),
        "crate-a's own crate_b.rs Foo was never referenced from a non-root bare head -- must not \
         be marked pub; got:\n{}",
        own_crate_b.myc
    );
}

/// The never-silent refusal path (VR-5/G2): a `super::` with no parent to go up to (the file is
/// already at the crate root) is a genuine structural miss — gapped, never a panic, never a guess.
#[test]
fn super_with_no_parent_at_crate_root_still_gaps_never_panics() {
    let tmp = TempDir::new("super-no-parent");
    tmp.write(
        "mycrate/src/lib.rs",
        "use super::nonexistent::Thing;\nfn ok(x: bool) -> bool { x }",
    );

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let lib = results.first().expect("one result");
    assert!(
        lib.report
            .gaps
            .iter()
            .any(|g| g.category == Category::Import),
        "a crate-root `super::` must gap (no parent to go up to), never panic; gaps: {:?}",
        lib.report.gaps
    );
    assert!(
        !lib.myc.contains("use "),
        "the unresolvable super:: must carry no `use` line at all; got:\n{}",
        lib.myc
    );
}

/// **Regression for the M-1084/#1541 review LOW**: a SYMBOL-TABLE-KEY collision, distinct from the
/// output-PATH collision the `common_ancestor` tests above cover. Two real crates each declare a
/// same-named BARE submodule (`rng.rs`) exporting a same-named item (`Thing`) with DIFFERENT
/// underlying representation. Pre-M-1084, `build_symbol_table` inserted every file under its bare
/// intra-crate module key (`"rng"`) with no crate qualifier, so the SECOND crate's `rng.rs` entry
/// would silently overwrite the FIRST crate's in the batch-wide `HashMap` — after which crate-a's
/// OWN `use crate::rng::Thing;` could resolve against crate-b's `rng.rs` (or vice versa): a real
/// cross-contamination, not merely a lost resolve, because both files legitimately export a `Thing`.
/// `SymbolTable::qualify_key` (crate-qualifying the key whenever a real crate identity is derivable)
/// is the fix under test: each crate's `rng` entry lands under its OWN qualified key
/// (`crate_a.rng`/`crate_b.rng`), so same-crate `use crate::rng::Thing;` resolution in each crate
/// stays within that crate — never resolving against the sibling's same-named submodule.
#[test]
fn same_named_bare_submodule_across_two_crates_does_not_cross_contaminate_symbol_table() {
    let tmp = TempDir::new("same-named-submodule-key-collision");
    // Both crates declare a bare `rng.rs` submodule (identical Rust module path within their own
    // crate) exporting a same-named `Thing` — but genuinely DIFFERENT items (different width).
    tmp.write("crate-a/src/rng.rs", "pub struct Thing(u8);");
    tmp.write(
        "crate-a/src/lib.rs",
        "use crate::rng::Thing;\nfn a_helper(x: bool) -> bool { x }",
    );
    tmp.write("crate-b/src/rng.rs", "pub struct Thing(u16);");
    tmp.write(
        "crate-b/src/lib.rs",
        "use crate::rng::Thing;\nfn b_helper(x: bool) -> bool { x }",
    );

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    assert_eq!(files.len(), 4, "expected all 4 fixture files discovered");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let a = results
        .iter()
        .find(|r| r.path.ends_with("crate-a/src/lib.rs"))
        .expect("crate-a lib.rs result present");
    let b = results
        .iter()
        .find(|r| r.path.ends_with("crate-b/src/lib.rs"))
        .expect("crate-b lib.rs result present");

    // Each crate's own `use crate::rng::Thing;` must resolve to ITS OWN `rng` (never bare, and
    // never the SIBLING crate's `rng`) — the cross-contamination the qualified-key fix prevents.
    // L2-B: co-include EXPLAIN names the home; or full-path use.
    assert!(
        a.myc.contains("crate.a.rng")
            || a.myc.lines().any(|l| {
                let t = l.trim();
                t.contains("crate.a.rng.Thing") || t.contains("a.rng.Thing")
            }),
        "crate-a's use must resolve to crate-a's OWN rng.rs as full nodule path; got:\n{}",
        a.myc
    );
    assert!(
        !a.myc.contains("crate.b.rng"),
        "crate-a's use must NEVER resolve against crate-b's same-named rng.rs (cross-\
         contamination); got:\n{}",
        a.myc
    );
    assert!(
        b.myc.contains("crate.b.rng")
            || b.myc.lines().any(|l| {
                let t = l.trim();
                t.contains("crate.b.rng.Thing") || t.contains("b.rng.Thing")
            }),
        "crate-b's use must resolve to crate-b's OWN rng.rs as full nodule path; got:\n{}",
        b.myc
    );
    assert!(
        !b.myc.contains("crate.a.rng"),
        "crate-b's use must NEVER resolve against crate-a's same-named rng.rs (cross-\
         contamination); got:\n{}",
        b.myc
    );

    // Each crate's OWN rng.rs's Thing is marked pub in ITS OWN output (each was genuinely
    // referenced, by its own crate's lib.rs) — never the sibling's.
    let rng_a = results
        .iter()
        .find(|r| r.path.ends_with("crate-a/src/rng.rs"))
        .expect("crate-a rng.rs result present");
    let rng_b = results
        .iter()
        .find(|r| r.path.ends_with("crate-b/src/rng.rs"))
        .expect("crate-b rng.rs result present");
    assert!(
        rng_a.myc.contains("pub type Thing") || rng_a.myc.contains("pub struct Thing"),
        "crate-a's own rng.rs Thing must be marked pub (referenced by crate-a's own lib.rs); \
         got:\n{}",
        rng_a.myc
    );
    assert!(
        rng_b.myc.contains("pub type Thing") || rng_b.myc.contains("pub struct Thing"),
        "crate-b's own rng.rs Thing must be marked pub (referenced by crate-b's own lib.rs); \
         got:\n{}",
        rng_b.myc
    );
}

// Property (bound): for a corpus of nesting depths 0..=3, a `super::`-headed use that walks up to
// an existing sibling ALWAYS resolves to a qualified (never-bare) reference, and a `super::` chain
// that would walk past the crate root ALWAYS gaps rather than panicking — the never-silent +
// no-bare-name-collapse bounds hold across the whole depth range, not just the hand-picked cases
// above (CLAUDE.md: "every approximate operation ships its bound ... and a property test that
// exercises the bound").
proptest! {
    #[test]
    fn super_relative_resolution_never_bare_collapses_across_nesting_depths(depth in 0usize..=3) {
        let tmp = TempDir::new(&format!("super-depth-prop-{depth}"));
        // A target file directly under `src/` (the crate root sibling every depth's `super::...`
        // chain reaches once it walks all the way up).
        tmp.write("mycrate/src/target.rs", "pub struct Marker(u8);");

        // Build a `depth`-deep module chain `m0/m1/.../m{depth-1}/mod.rs`, whose `mod.rs` uses
        // exactly `depth` leading `super::` segments to reach the crate root, then `target::Marker`.
        let mut rel = String::from("mycrate/src");
        for i in 0..depth {
            rel.push('/');
            rel.push_str(&format!("m{i}"));
        }
        rel.push_str("/mod.rs");
        let supers = "super::".repeat(depth);
        tmp.write(
            &rel,
            &format!("use {supers}target::Marker;\nfn f(x: bool) -> bool {{ x }}"),
        );

        let files = discover_rs_files(tmp.path()).expect("discover succeeds");
        let (results, failures) = transpile_batch(&files);
        prop_assert!(failures.is_empty(), "unexpected failures: {failures:?}");

        let leaf = results
            .iter()
            .find(|r| r.path.ends_with("mod.rs"))
            .expect("mod.rs result present");

        if depth == 0 {
            // `mod.rs` directly under `src/` IS the crate root: a single `super::` has no parent —
            // never-silent refusal, never a panic, never a bare emission.
            prop_assert!(!leaf.myc.lines().any(|l| l.trim() == "use Marker;"));
        } else {
            // `depth` levels deep reached via exactly `depth` `super::` segments lands EXACTLY at
            // the crate root, where `target.rs` lives — must resolve (L2-B co-include or use),
            // never bare.
            prop_assert!(
                leaf.myc.contains("type Marker") || leaf.myc.contains(".Marker;"),
                "depth {depth}: expected co-include or qualified use of Marker; got:\n{}",
                leaf.myc
            );
            prop_assert!(!leaf.myc.lines().any(|l| l.trim() == "use Marker;"));
        }
    }
}

/// M-1084 net-close + L2-B phase-2: under a real `mycelium-*/src` layout a resolved **type**
/// import co-includes with EXPLAIN naming the **full** home nodule path (`std.fs.error`) — never
/// the PR #1635 crate-root-stripped short form (`error.FsErr`).
#[test]
fn m1084_full_nodule_path_use_emit_under_mycelium_crate_layout() {
    let tmp = TempDir::new("m1084-full-path");
    tmp.write(
        "mycelium-std-fs/src/error.rs",
        "pub struct FsErr(u8);\nfn helper(x: bool) -> bool { x }",
    );
    tmp.write(
        "mycelium-std-fs/src/substrate.rs",
        "use crate::error::FsErr;\nfn sub_helper(x: bool) -> bool { x }",
    );

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let sub = results
        .iter()
        .find(|r| r.path.ends_with("substrate.rs"))
        .expect("substrate.rs present");
    // L2-B co-include for types; EXPLAIN retains full home path provenance (M-1084).
    assert!(
        sub.myc.contains("type FsErr") && sub.myc.contains("std.fs.error"),
        "expected co-include of FsErr with full home path `std.fs.error` in EXPLAIN; got:\n{}",
        sub.myc
    );
    assert!(
        !sub.myc.lines().any(|l| l.trim() == "use error.FsErr;"),
        "must never emit crate-root-stripped short form (checker-rejected); got:\n{}",
        sub.myc
    );

    // Live phylum-mode differential when myc-check is built — co-include remains Clean under phylum.
    let Some(bin) = super::vet::find_myc_check() else {
        eprintln!(
            "m1084 full-path live myc-check skipped — set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`"
        );
        return;
    };
    let out = TempDir::new("m1084-full-path-vet");
    for r in &results {
        let name = r.path.file_stem().and_then(|s| s.to_str()).unwrap_or("x");
        fs::write(out.path().join(format!("{name}.myc")), &r.myc).expect("write myc");
    }
    let status = std::process::Command::new(&bin)
        .arg("--phylum")
        .arg(out.path())
        .arg("--json")
        .output()
        .expect("spawn myc-check");
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.status.success() || stdout.contains("\"ok\":true"),
        "phylum check of co-included type imports must Clean; status={:?} out={stdout} err={}",
        status.status,
        String::from_utf8_lossy(&status.stderr)
    );
}

/// ONESHOT L2-B phase-2: batch-resolved **type** imports co-include sibling type surface so
/// **single-file oracle** is self-contained (no longer false-fails with `no such name`).
///
/// Pins: (1) EXPLAIN (L2-B/DN-124) with full home path; (2) `type FsErr` co-include, never short
/// `use error.FsErr`; (3) oracle Clean; (4) phylum Clean. Skips live myc-check when binary absent.
#[test]
fn l2b_resolved_type_co_include_oracle_and_phylum_clean() {
    let tmp = TempDir::new("l2b-oracle-coincluded");
    tmp.write(
        "mycelium-std-fs/src/error.rs",
        "pub struct FsErr(u8);\nfn helper(x: bool) -> bool { x }",
    );
    tmp.write(
        "mycelium-std-fs/src/substrate.rs",
        "use crate::error::FsErr;\nfn sub_helper(x: bool) -> bool { x }",
    );

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let sub = results
        .iter()
        .find(|r| r.path.ends_with("substrate.rs"))
        .expect("substrate.rs present");
    assert!(
        sub.myc.contains("type FsErr") && sub.myc.contains("EXPLAIN (L2-B"),
        "batch must co-include sibling type with L2-B EXPLAIN; got:\n{}",
        sub.myc
    );
    assert!(
        sub.myc.contains("std.fs.error"),
        "EXPLAIN must name full home nodule path (M-1084 provenance); got:\n{}",
        sub.myc
    );
    assert!(
        !sub.myc
            .lines()
            .any(|l| l.trim() == "use error.FsErr;" || l.trim() == "use FsErr;"),
        "must never short-form collapse; got:\n{}",
        sub.myc
    );
    assert!(
        sub.report
            .emitted_items
            .iter()
            .any(|n| n.contains("FsErr") && n.contains("co-include")),
        "expected emitted co-include:…FsErr item, got {:?}",
        sub.report.emitted_items
    );

    let Some(bin) = super::vet::find_myc_check() else {
        eprintln!(
            "l2b co-include live myc-check skipped — set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`"
        );
        return;
    };

    let out = TempDir::new("l2b-oracle-coincluded-vet");
    let mut sub_myc_path = None;
    for r in &results {
        let name = r.path.file_stem().and_then(|s| s.to_str()).unwrap_or("x");
        let path = out.path().join(format!("{name}.myc"));
        fs::write(&path, &r.myc).expect("write myc");
        if r.path.ends_with("substrate.rs") {
            sub_myc_path = Some(path);
        }
    }
    let sub_myc = sub_myc_path.expect("substrate.myc written");

    // Oracle (single-file): co-include makes the consumer self-contained — Clean.
    let oracle = std::process::Command::new(&bin)
        .arg(&sub_myc)
        .output()
        .expect("spawn myc-check oracle");
    let oracle_out = format!(
        "{}{}",
        String::from_utf8_lossy(&oracle.stdout),
        String::from_utf8_lossy(&oracle.stderr)
    );
    assert!(
        oracle.status.success(),
        "oracle must Clean a type co-include (L2-B phase-2); got fail out={oracle_out}"
    );

    // Phylum co-check of the same batch: still Clean (home-qualified dual defs).
    let phylum = std::process::Command::new(&bin)
        .arg("--phylum")
        .arg(out.path())
        .arg("--json")
        .output()
        .expect("spawn myc-check --phylum");
    let phylum_out = String::from_utf8_lossy(&phylum.stdout);
    assert!(
        phylum.status.success() || phylum_out.contains("\"ok\":true"),
        "phylum must Clean co-included type imports; status={:?} out={phylum_out} err={}",
        phylum.status,
        String::from_utf8_lossy(&phylum.stderr)
    );
}

/// G-α Rank-2 / L2-B Import non-type: batch-resolved **free-fn** imports co-include sibling
/// free-fn surface (plus type deps on the fn line) so single-file oracle no longer false-fails
/// with `no such name …read_all` from a phylum-of-one `use`.
///
/// Pins: (1) EXPLAIN (G-α L2-B/DN-124) with full home path; (2) `fn read_all` co-include, never
/// `use std.io.io.read_all`; (3) type co-include of `Source` when referenced; (4) optional live
/// oracle when body is self-contained (this fixture uses a trivial body).
#[test]
fn g_alpha_resolved_free_fn_co_include() {
    let tmp = TempDir::new("g-alpha-fn-coinclude");
    tmp.write(
        "mycelium-std-io/src/io.rs",
        "pub struct Source(u8);\npub fn read_all(src: Source) -> Source { src }\n",
    );
    tmp.write(
        "mycelium-std-io/src/lib.rs",
        "use crate::io::{read_all, Source};\nfn consumer(s: Source) -> Source { read_all(s) }\n",
    );

    let files = discover_rs_files(tmp.path()).expect("discover succeeds");
    let (results, failures) = transpile_batch(&files);
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");

    let lib = results
        .iter()
        .find(|r| r.path.ends_with("lib.rs"))
        .expect("lib.rs present");
    assert!(
        lib.myc.contains("fn read_all") && lib.myc.contains("EXPLAIN (G-α L2-B"),
        "batch must co-include sibling free-fn with G-α EXPLAIN; got:\n{}",
        lib.myc
    );
    assert!(
        lib.myc.contains("std.io.io"),
        "EXPLAIN must name full home nodule path (M-1084 provenance); got:\n{}",
        lib.myc
    );
    assert!(
        !lib.myc.lines().any(|l| {
            let t = l.trim();
            t == "use std.io.io.read_all;" || t.starts_with("use ") && t.contains("read_all")
        }),
        "must not emit full-path use for extractable free-fn; got:\n{}",
        lib.myc
    );
    assert!(
        lib.myc.contains("type Source") || lib.myc.contains("Source"),
        "type dep Source should appear (co-include or local); got:\n{}",
        lib.myc
    );
    assert!(
        lib.report
            .emitted_items
            .iter()
            .any(|n| n.contains("read_all") && n.contains("co-include")),
        "expected emitted co-include:…read_all item, got {:?}",
        lib.report.emitted_items
    );

    let Some(bin) = super::vet::find_myc_check() else {
        eprintln!(
            "g-alpha free-fn co-include live myc-check skipped — set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`"
        );
        return;
    };

    let out = TempDir::new("g-alpha-fn-coinclude-vet");
    let mut lib_myc_path = None;
    for r in &results {
        let name = r.path.file_stem().and_then(|s| s.to_str()).unwrap_or("x");
        let path = out.path().join(format!("{name}.myc"));
        fs::write(&path, &r.myc).expect("write myc");
        if r.path.ends_with("lib.rs") {
            lib_myc_path = Some(path);
        }
    }
    let lib_myc = lib_myc_path.expect("lib.myc written");

    let oracle = std::process::Command::new(&bin)
        .arg(&lib_myc)
        .output()
        .expect("spawn myc-check oracle");
    let oracle_out = format!(
        "{}{}",
        String::from_utf8_lossy(&oracle.stdout),
        String::from_utf8_lossy(&oracle.stderr)
    );
    assert!(
        oracle.status.success(),
        "oracle must Clean a free-fn co-include (G-α Rank-2); got fail out={oracle_out}"
    );

    let phylum = std::process::Command::new(&bin)
        .arg("--phylum")
        .arg(out.path())
        .arg("--json")
        .output()
        .expect("spawn myc-check --phylum");
    let phylum_out = String::from_utf8_lossy(&phylum.stdout);
    assert!(
        phylum.status.success() || phylum_out.contains("\"ok\":true"),
        "phylum must Clean co-included free-fn imports; status={:?} out={phylum_out} err={}",
        phylum.status,
        String::from_utf8_lossy(&phylum.stderr)
    );
}
