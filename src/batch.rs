//! Directory/batch mode (M-873 follow-on): discover every `*.rs` file under a crate's `src/`,
//! transpile each independently, and summarize the results — the per-file CLI-mode logic pulled
//! out into a reusable, testable module so the CLI (`src/bin/mycelium-transpile.rs`) stays a thin
//! I/O shell.
//!
//! **Guarantee: `Declared`** (same basis as `emit`/`transpile` — see `src/lib.rs`); the
//! aggregation here (sums, percentages, category merges) is exact arithmetic over already-Declared
//! per-file [`crate::gap::GapReport`]s, so it inherits their tag rather than degrading it further.

use crate::gap::{Gap, GapReport};
use crate::remap::{build_remap_manifest, RemapManifest};
use crate::symtab::{self, SymbolTable};
use crate::transpile::{
    derive_crate_ident, derive_module_segments, derive_nodule_path, transpile_file,
    transpile_file_with_ctx,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Recursively discover every `*.rs` file under `root`, skipping test infrastructure: any
/// directory component named `tests` (covers both a crate-root `tests/` integration-test dir and
/// the in-crate `src/tests/` unit-test layout — CLAUDE.md "Test layout") and any file whose stem
/// is exactly `tests` (the older single-file `src/tests.rs` shape, e.g.
/// `mycelium-std-fmt/src/tests.rs`). Both are out of this PoC's transpilation scope (the same
/// scope `emit::is_cfg_test`/`Category::TestItem` already exclude at the item level for
/// `#[cfg(test)] mod`); skipping the *files* here avoids parsing pure test bodies as if they were
/// library surface. Returns files in a deterministic (sorted) order.
pub fn discover_rs_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if path.file_name().and_then(|n| n.to_str()) == Some("tests") {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file()
                && path.extension().and_then(|e| e.to_str()) == Some("rs")
                && path.file_stem().and_then(|s| s.to_str()) != Some("tests")
            {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The **path-qualified** output stem (extension-stripped, relative to the run's `out-dir`) for a
/// discovered file in a directory/batch run (M-1006 Phase-2): the file's path **relative to the
/// batch `root`**, with the `.rs` extension stripped, so a whole-corpus run mirrors the source tree
/// and two crates' `lib.rs` land at distinct outputs (`mycelium-core/src/lib` vs
/// `mycelium-std/src/lib`) instead of one overwriting the other. Distinct source files have distinct
/// relative paths, so the mapping is injective by construction.
///
/// Returns `Ok(rel_noext)` when `file` is under `root` (the normal case), or `Err(fallback)` — the
/// bare file stem — when it is not (the caller reports the fallback, never silently mis-placing the
/// output; G2). The `.with_extension("")` strips only the final `.rs`, so a `foo.bar.rs` source
/// keeps its `foo.bar` stem.
pub fn output_rel_path(file: &Path, root: &Path) -> Result<PathBuf, PathBuf> {
    match file.strip_prefix(root) {
        Ok(rel) => Ok(rel.with_extension("")),
        Err(_) => Err(PathBuf::from(
            file.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("output"),
        )),
    }
}

/// The output-naming `root` for an **explicit** `--files` set (M-1079/DN-124 §3.2 CLI mode,
/// bug-fixed here): the **common ancestor directory** of every file's own parent directory, not just
/// the first file's parent. Bug this replaces: rooting at `files[0].parent()` alone made every OTHER
/// file whose crate lives in a sibling directory fail `output_rel_path`'s `strip_prefix` and fall
/// back to a bare stem — so batching e.g. three different crates' `src/lib.rs` (`mycelium-std-sys-host`,
/// `mycelium-std-rand`, `mycelium-std-time`) had all three collide on `lib.myc`, each silently
/// overwriting the last write. Walking up to the shared ancestor instead makes `output_rel_path`
/// succeed (`Ok`, not the fallback `Err`) for every file in the set, and its relative path is then
/// automatically **crate-qualified** whenever the files span more than one crate root
/// (`mycelium-std-rand/src/lib` vs `mycelium-std-time/src/lib`) — while degenerating to the identical
/// pre-fix bare-stem naming when every file already shares one directory (the common mutually-
/// referencing-siblings case, e.g. `--files checkty.rs,elab.rs,eval.rs`), so no existing single-crate
/// `--files` output changes.
///
/// **`None` — genuinely no common ancestor (bug-fixed here, was `Some(PathBuf::new())`).** Returned
/// for an empty file set, or when the set mixes **absolute and relative** paths (their `Path`
/// component sequences are fundamentally incompatible — a `RootDir` component never matches a bare
/// `Normal` one — so no directory is a real prefix of every file). The previous behavior collapsed
/// this case to the empty path `""`; because `Path::strip_prefix("")` **always succeeds** and hands
/// the input back unchanged, `output_rel_path` would then return the ABSOLUTE members of the set
/// **unchanged and `Ok`**, never routing through its `Err` fallback — so the caller's per-file
/// warning path was never actually reached for exactly the case it was documented as backstopping
/// (a stale claim, corrected here). Downstream, `out_dir.join(<that unchanged absolute path>)`
/// silently **discards `out_dir`** (`Path::join`'s absolute-path-override), writing the `.myc`/
/// `.gap.json` pair outside the declared `--out-dir` with no warning at all — a G2 silent
/// misplacement. Returning `None` instead makes the caller detect the no-common-ancestor case
/// explicitly and route every file through the bare-stem fallback (with a warning), so this can
/// never happen (see `write_batch_and_maybe_vet`'s `root: Option<&Path>` handling).
///
/// A same-absoluteness file set that shares no path *components* (e.g. relative
/// `mycelium-std-rand/src/lib.rs` vs `mycelium-std-time/src/lib.rs`, which diverge at their very
/// first component) is **not** the `None` case: it degenerates to `Some(PathBuf::new())` — the
/// intentional "root is the invocation's own working directory" case that is exactly what makes the
/// multi-crate-root fix above work (`output_rel_path` against `""` keeps each file's full relative,
/// crate-qualified path unchanged and `Ok`, never mis-writing since a *relative* path staying
/// relative never triggers `Path::join`'s absolute-override). The unsafe case is specifically an
/// absolute member of the set landing on an unchanged-and-`Ok` empty-root strip — which requires a
/// *mixed* set to reach, since an all-absolute set's shared root (`/` on Unix) is never empty.
pub fn common_ancestor(files: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = files.iter();
    let first = iter.next()?;

    let first_is_absolute = first.is_absolute();
    if files.iter().any(|f| f.is_absolute() != first_is_absolute) {
        // Mixed absolute/relative — see the doc above for why this specific case is the unsafe one.
        return None;
    }

    let mut common: Vec<std::ffi::OsString> = first
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .components()
        .map(|c| c.as_os_str().to_os_string())
        .collect();
    for f in iter {
        if common.is_empty() {
            break;
        }
        let parent_components: Vec<std::ffi::OsString> = f
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .components()
            .map(|c| c.as_os_str().to_os_string())
            .collect();
        let shared = common
            .iter()
            .zip(parent_components.iter())
            .take_while(|(a, b)| a == b)
            .count();
        common.truncate(shared);
    }

    if common.is_empty() && first_is_absolute {
        // Defensive/unreachable on Unix (a shared absolute root always keeps at least the leading
        // `RootDir` component) — but if every file is absolute and they still share NOTHING (e.g.
        // different filesystem roots/drives on another platform), that is also genuinely "no common
        // ancestor", and returning `Some(PathBuf::new())` here would reproduce the exact bug this
        // `Option` return exists to prevent.
        return None;
    }

    Some(common.into_iter().collect())
}

/// One file's contribution to a [`BatchSummary`].
#[derive(Debug, Clone, Serialize)]
pub struct FileSummary {
    pub file: String,
    pub total_items: usize,
    pub non_test_items: usize,
    pub emitted: usize,
    pub gaps: usize,
    pub expressible_pct: f64,
    pub category_counts: BTreeMap<&'static str, usize>,
}

impl FileSummary {
    fn from_report(file: String, report: &GapReport) -> Self {
        FileSummary {
            file,
            total_items: report.total_top_level_items,
            non_test_items: report.non_test_item_count(),
            emitted: report.emitted_items.len(),
            gaps: report.gaps.len(),
            expressible_pct: report.expressible_fraction() * 100.0,
            category_counts: report.category_counts(),
        }
    }
}

/// The batch-wide aggregate — same shape as [`FileSummary`] minus the per-file `file` name, so a
/// consumer can treat `totals` as "one more row" without a meaningless synthetic filename.
#[derive(Debug, Clone, Serialize)]
pub struct Totals {
    pub total_items: usize,
    pub non_test_items: usize,
    pub emitted: usize,
    pub gaps: usize,
    pub expressible_pct: f64,
    pub category_counts: BTreeMap<&'static str, usize>,
}

/// The combined `summary.json` artifact for a batch/directory transpile run.
///
/// **M-1044 / DN-109 §5.2 (§7-c ratified "extend, don't mint a new artifact"):** carries the
/// [`RemapManifest`] provenance ledger as an additive `remap` field alongside the pre-existing
/// per-file/aggregate counts — the same summary artifact, not a second sidecar. See
/// [`crate::remap`] for the schema and its `Declared`-and-stays-`Declared` guarantee posture.
#[derive(Debug, Clone, Serialize)]
pub struct BatchSummary {
    pub files: Vec<FileSummary>,
    pub totals: Totals,
    pub remap: RemapManifest,
}

/// The combined `union.gap.json` artifact: every [`Gap`] from every file in the batch, plus the
/// aggregate per-category counts — never deduplicated or dropped (G2: a gap recorded once per
/// file it occurs in, since each occurrence is a distinct construct at a distinct file/line).
#[derive(Debug, Clone, Serialize)]
pub struct UnionGapReport {
    pub gaps: Vec<Gap>,
    pub category_counts: BTreeMap<&'static str, usize>,
}

/// One file's parse/transpile outcome, kept alongside its report so the CLI can still write the
/// per-file `.myc`/`.gap.json` artifacts after batch summarization.
pub struct FileResult {
    pub path: PathBuf,
    pub myc: String,
    pub report: GapReport,
}

/// Transpile every file in `files` (already-discovered `.rs` paths), collecting a
/// [`FileResult`] per file that parses. A file that fails to parse/read (a hard `syn` failure,
/// distinct from a per-item gap) is **not** silently skipped — its path/error is returned
/// separately so the caller can report it (never-silent, G2).
///
/// **Gap-close-2 (DN-34 §8.19/§8.20) — the batch-scoped cross-nodule `Import` resolution, extended
/// by M-1084 to `self::`/`super::` + cross-phylum resolution (`symtab.rs` module docs).** This is
/// a **three-pass** driver, all internal (the public signature/contract is unchanged from before
/// this lever landed):
/// 1. **Baseline pass** — transpile every file exactly as [`transpile_file`] always has (no
///    cross-nodule resolution), which is also the source of each file's actually-**emitted**
///    item-name set — the only names a sibling's `use` may ever resolve to (a name that merely
///    exists in the Rust source but itself gapped is never a valid target; VR-5/G2).
/// 2. **Symbol-table build + a light `use`-only scan** — [`build_symbol_table`] indexes every
///    file's emitted names under its derived (same-crate-bare, or crate-qualified when a real crate
///    identity is derivable — M-1084) key; [`scan_pub_needed`] then walks every file's `use` items
///    (a cheap re-parse, no full re-dispatch) to find which sibling names at least one OTHER file in
///    the batch actually resolves against — the `pub`-propagation input the final pass needs
///    (DN-113/M-1060's `resolve_imports` only accepts a `pub` cross-nodule name; emitting a resolved
///    `use` against a non-`pub` sibling item would be the exact "plausible but wrong" emission
///    VR-5/G2 forbid). Accumulated by the target sibling's own **nodule path** (stable and
///    lookup-perspective-independent — the same physical sibling may be reached via more than one
///    lookup key, e.g. both a same-crate and a cross-phylum reference in different consumer files).
/// 3. **Final pass** — re-transpile every file via [`transpile_file_with_ctx`] with the symbol
///    table and this file's own pub-needed set (looked up by ITS OWN derived nodule path) installed,
///    so `use crate::<mod>::Item`/`self::`/`super::`/a cross-phylum `use <phylum>::<mod>::Item`
///    resolves against a genuine in-batch sibling and the referenced item is emitted `pub`.
///
/// A single-file batch (or one whose files carry no in-batch cross-referencing `use`) degenerates
/// to byte-identical output vs. the pre-lever driver (no sibling ⇒ nothing resolves). A multi-crate
/// batch (M-1084: e.g. a `--files` invocation spanning more than one crate's `src/`) is the vehicle
/// for cross-phylum resolution — every file's own crate identity is derived from ITS OWN real repo
/// path (`transpile::derive_crate_ident`), so files from different crates never collide on a bare
/// (unqualified) key.
pub fn transpile_batch(files: &[PathBuf]) -> (Vec<FileResult>, Vec<(PathBuf, String)>) {
    let mut pass1: Vec<(PathBuf, GapReport)> = Vec::with_capacity(files.len());
    let mut failures = Vec::new();
    for path in files {
        match transpile_file(path) {
            Ok((_myc, report)) => pass1.push((path.clone(), report)),
            Err(e) => failures.push((path.clone(), e)),
        }
    }
    if pass1.is_empty() {
        return (Vec::new(), failures);
    }

    let symtab = build_symbol_table(&pass1);
    let pub_needed = scan_pub_needed(&pass1, &symtab);

    let mut results = Vec::with_capacity(pass1.len());
    for (path, _baseline_report) in &pass1 {
        // Keyed by this file's own derived nodule path (stable, lookup-perspective-independent —
        // see the driver doc above), NOT the Rust-side module key a consumer used to reach it.
        let nodule_path = derive_nodule_path(path);
        let needed = pub_needed.get(&nodule_path).cloned().unwrap_or_default();
        match transpile_file_with_ctx(path, &symtab, &needed) {
            Ok((myc, report)) => results.push(FileResult {
                path: path.clone(),
                myc,
                report,
            }),
            // The baseline pass already confirmed this file reads + parses; a failure here would
            // mean the file changed on disk mid-run — never silently dropped either way (G2).
            Err(e) => failures.push((path.clone(), e)),
        }
    }
    (results, failures)
}

/// Build the batch-wide cross-nodule [`SymbolTable`] from every file's baseline-pass
/// [`GapReport`] (see [`transpile_batch`] step 2). Each file is inserted under exactly ONE key: its
/// own crate-qualified key (`SymbolTable::qualify_key`) when a real crate identity is derivable from
/// its path (`transpile::derive_crate_ident` — every genuine repo path under a crate's `src/`), else
/// the bare intra-crate module key unchanged from pre-M-1084 behavior (a `src`-ancestor-less path,
/// e.g. this crate's own temp-dir test fixtures — never spuriously qualified). Because every file in
/// a real single-crate batch shares the identical crate identity, this is byte-identical to the
/// pre-M-1084 table for that (today's only real) case; a multi-crate batch instead gets one
/// non-colliding key per file (M-1084's cross-phylum extension — see `symtab.rs` module docs).
fn build_symbol_table(pass1: &[(PathBuf, GapReport)]) -> SymbolTable {
    let mut table = SymbolTable::new();
    for (path, report) in pass1 {
        let module_key = SymbolTable::module_key(&derive_module_segments(path));
        let nodule_path = derive_nodule_path(path);
        let emitted: HashSet<String> = report.emitted_items.iter().cloned().collect();
        let key = match derive_crate_ident(path) {
            Some(crate_ident) => SymbolTable::qualify_key(&crate_ident, &module_key),
            None => module_key,
        };
        table.insert(key, nodule_path, emitted);
    }
    table
}

/// Light `use`-only scan (see [`transpile_batch`] step 2): for every file in the batch, walk its
/// `Item::Use`s, resolve each candidate leaf against `symtab` via
/// [`SymbolTable::candidate_lookup_keys`] (the SAME precedence-ordered policy
/// `transpile::dispatch_use` consults — DRY, one resolution policy not two divergent copies), and
/// accumulate — keyed by the **target** sibling's own **nodule path** (stable regardless of which
/// key a particular consumer resolved through) — every item name at least one file in the batch
/// actually resolves a `use` against. A file that fails to re-read/re-parse here (should not happen;
/// the baseline pass already succeeded) is simply skipped for this scan — never a hard failure,
/// since a missed pub-propagation opportunity degrades to the pre-lever "stays gapped" behavior, not
/// to an incorrect emission (VR-5: conservative on failure, never a guess).
fn scan_pub_needed(
    pass1: &[(PathBuf, GapReport)],
    symtab: &SymbolTable,
) -> BTreeMap<String, HashSet<String>> {
    let mut needed: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for (path, _report) in pass1 {
        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(parsed) = syn::parse_file(&source) else {
            continue;
        };
        let current_module = derive_module_segments(path);
        let current_crate = derive_crate_ident(path);
        for item in &parsed.items {
            let syn::Item::Use(u) = item else {
                continue;
            };
            let Some(candidates) = symtab::use_candidates(&u.tree, &current_module) else {
                continue;
            };
            for c in &candidates {
                let symtab::CandidateKind::Name(name) = &c.kind else {
                    continue;
                };
                for key in
                    SymbolTable::candidate_lookup_keys(current_crate.as_deref(), &current_module, c)
                {
                    if let Some(nodule_path) = symtab.resolve(&key, name) {
                        needed
                            .entry(nodule_path.to_string())
                            .or_default()
                            .insert(name.clone());
                        // Matches `dispatch_use`'s own precedence: the first key that hits wins,
                        // never both (a leaf resolves against exactly one sibling).
                        break;
                    }
                }
            }
        }
    }
    needed
}

/// Build the [`BatchSummary`] + [`UnionGapReport`] artifacts from a batch's [`FileResult`]s.
///
/// `root` is the batch's discovery root (the same path passed to [`discover_rs_files`]/
/// [`output_rel_path`]) — threaded through so [`BatchSummary::remap`]'s `phylum` header (M-1044)
/// can derive the source-crate/target-phylum names the same way the rest of the batch pipeline
/// derives paths, without re-deriving it from scratch per file.
pub fn summarize(results: &[FileResult], root: &Path) -> (BatchSummary, UnionGapReport) {
    let mut files = Vec::with_capacity(results.len());
    let mut all_gaps: Vec<Gap> = Vec::new();

    let mut total_items = 0usize;
    let mut non_test_items = 0usize;
    let mut emitted = 0usize;
    let mut gaps = 0usize;
    let mut category_counts: BTreeMap<&'static str, usize> = BTreeMap::new();

    for r in results {
        let label = r.path.display().to_string();
        files.push(FileSummary::from_report(label, &r.report));

        total_items += r.report.total_top_level_items;
        non_test_items += r.report.non_test_item_count();
        emitted += r.report.emitted_items.len();
        gaps += r.report.gaps.len();
        for (cat, count) in r.report.category_counts() {
            *category_counts.entry(cat).or_insert(0) += count;
        }
        all_gaps.extend(r.report.gaps.iter().cloned());
    }

    let expressible_pct = if non_test_items == 0 {
        0.0
    } else {
        emitted as f64 / non_test_items as f64 * 100.0
    };

    let totals = Totals {
        total_items,
        non_test_items,
        emitted,
        gaps,
        expressible_pct,
        category_counts: category_counts.clone(),
    };

    let remap = build_remap_manifest(results, root);

    (
        BatchSummary {
            files,
            totals,
            remap,
        },
        UnionGapReport {
            gaps: all_gaps,
            category_counts,
        },
    )
}
