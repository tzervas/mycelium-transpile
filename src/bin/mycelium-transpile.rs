//! CLI for `mycelium-transpile` (M-873, batch mode added in the follow-on wave; `--vet` added in
//! M-1000; `--files` added in M-1079/DN-124 §3.2): `mycelium-transpile [--vet] <input> <out-dir>`
//! or `mycelium-transpile [--vet] --files <f1,f2,...> <out-dir>`.
//!
//! `<input>` is either:
//! - a single `.rs` file — writes `<out-dir>/<stem>.myc` + `<out-dir>/<stem>.gap.json`, then
//!   prints a one-line summary (unchanged single-file behavior); or
//! - a directory (typically a crate's `src/`, or the whole `crates/` corpus) — recurses every
//!   `*.rs` file (skipping test infrastructure, `src/batch.rs::discover_rs_files`), transpiles each
//!   independently, and writes a `.myc`/`.gap.json` pair for every discovered file at a
//!   **path-qualified** output that mirrors the source tree under `<out-dir>` (a file's path
//!   relative to the batch root becomes its output path — `mycelium-core/src/lib.myc`, not a flat
//!   `lib.myc`), so a whole-corpus run never overwrites two crates' same-stem files (M-1006
//!   Phase-2). For a single-crate `src/` with a flat layout the mirrored path is just the stem, so
//!   the output is identical to the pre-Phase-2 flat naming. Also writes three combined artifacts:
//!   `<out-dir>/summary.json` (per-file + aggregate counts, plus the M-1044/DN-109 §5.2 `remap`
//!   provenance manifest — one `Keep` entry per emitted nodule, v0 Mechanical-only), the
//!   `<out-dir>/REMAP.md` human-rendered projection of that same `remap` field (`src/remap.rs`),
//!   and `<out-dir>/union.gap.json` (every gap from every file, plus aggregate category counts).
//!
//! **`--files <f1,f2,...>` (M-1079/DN-124 §3.2)** batches an **explicit, caller-named file set** —
//! e.g. `crates/mycelium-l1/src/{checkty,elab,eval,mono,fuse}.rs`, mutually-referencing files that
//! do not share one directory exclusively (their real directory holds ~40 *other*, unrelated
//! files) — through the identical `transpile_batch` + `--vet` pipeline as directory mode, **without
//! any directory discovery/staging**: each file's **real repo path** is transpiled and recorded
//! verbatim (never a scratch/staging path — that would be both non-portable and, since a temp dir
//! is randomly named, a determinism hazard the committed `summary.json`/`vet.json` must never
//! carry). This is what lets a *subset* of a directory form its own exact phylum boundary (DN-124
//! §6 Attack 1a's constraint) without capturing everything else alongside it. Mutually exclusive
//! with the positional `<input>`.
//!
//! `--vet` (M-1000) runs the **real** `myc check` oracle over every emitted `.myc`, writes
//! `<out-dir>/vet.json` (per-file + aggregate vet records), and prints the **`checked_fraction`**
//! (myc-check-clean coverage) alongside the emission-only `expressible_fraction`. The oracle is the
//! pre-built `MYC_CHECK_CMD` binary when that env var is set (the sanctioned, build-lock-safe form
//! `scripts/checks/transpile-vet.sh` uses), else the `cargo run -p mycelium-check` fallback
//! (`crate::vet::MycChecker::from_env`). See `src/vet.rs` for the metric's stated denominator. In
//! **directory** mode (bare `<input>` a dir, or `--files`) `--vet` additionally runs **phylum-mode**
//! vetting over the whole written `<out-dir>` and dual-reports `checked_fraction_phylum` (DN-124
//! §3.1/M-A) — single-file mode does not (a lone file names no real phylum boundary).
//!
//! Every emitted artifact is `Declared`/unvalidated (see `src/lib.rs`); the vet verdict is
//! `Empirical` (measured — see `src/vet.rs`). No `clap` dependency — plain `std::env::args`
//! (kickoff-scoped minimal deps).

use mycelium_transpile::batch::{
    common_ancestor, discover_rs_files, output_rel_path, summarize, transpile_batch,
};
use mycelium_transpile::remap::render_remap_md;
use mycelium_transpile::vet::{vet_batch, MycChecker, VetInput, VetReport};
use mycelium_transpile::{transpile_file, GapReport};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    // Parse a minimal flag set: `--vet` and `--files <comma-list>` before the remaining
    // positional(s). Kept hand-rolled (no `clap`) per the crate's minimal-deps stance.
    let mut vet = false;
    let mut files_arg: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--vet" => vet = true,
            "--files" => match args.next() {
                Some(list) => files_arg = Some(list),
                None => {
                    eprintln!("usage: mycelium-transpile [--vet] --files <f1,f2,...> <out-dir>");
                    return ExitCode::FAILURE;
                }
            },
            _ => positional.push(a),
        }
    }

    if let Some(list) = files_arg {
        if positional.len() != 1 {
            eprintln!("usage: mycelium-transpile [--vet] --files <f1,f2,...> <out-dir>");
            return ExitCode::FAILURE;
        }
        let files: Vec<PathBuf> = list.split(',').map(PathBuf::from).collect();
        if files.is_empty() || files.iter().any(|f| f.as_os_str().is_empty()) {
            eprintln!("mycelium-transpile: --files: empty or malformed file list");
            return ExitCode::FAILURE;
        }
        let out_dir = Path::new(&positional[0]);
        if let Err(e) = fs::create_dir_all(out_dir) {
            eprintln!(
                "mycelium-transpile: failed to create {}: {e}",
                out_dir.display()
            );
            return ExitCode::FAILURE;
        }
        return run_explicit_files(&files, out_dir, vet);
    }

    if positional.len() != 2 {
        eprintln!(
            "usage: mycelium-transpile [--vet] <input.rs | input-dir> <out-dir>\n       \
             mycelium-transpile [--vet] --files <f1,f2,...> <out-dir>"
        );
        return ExitCode::FAILURE;
    }
    let input = Path::new(&positional[0]);
    let out_dir = Path::new(&positional[1]);

    if let Err(e) = fs::create_dir_all(out_dir) {
        eprintln!(
            "mycelium-transpile: failed to create {}: {e}",
            out_dir.display()
        );
        return ExitCode::FAILURE;
    }

    if input.is_dir() {
        run_batch(input, out_dir, vet)
    } else {
        run_single_file(input, out_dir, vet)
    }
}

/// Run the vet loop over the written `.myc` files and report `checked_fraction` alongside
/// `expressible_fraction`. Advisory: a vet failure/tool-unavailable is reported (never silent, G2)
/// but does **not** change the process exit code — vetting is a measurement, not a gate.
///
/// `phylum_dir`, when `Some` (batch/directory mode — DN-124 §3.1/§3.2), **additionally** runs
/// `myc check --phylum <dir> --json` over the whole written batch directory and dual-reports
/// `checked_fraction_phylum` alongside the oracle-mode `checked_fraction` (M-A: a transition-cycle
/// basis correction, never presented as lever progress — see `src/vet.rs`'s module docs). `None` in
/// single-file mode: a lone file names no real phylum boundary to check against (DN-124 §6 Attack 1a).
fn run_vet(inputs: &[VetInput], out_dir: &Path, phylum_dir: Option<&Path>) {
    if inputs.is_empty() {
        eprintln!("mycelium-transpile: --vet: no emitted .myc files to vet");
        return;
    }
    // Cargo-fallback runs in the current directory (typically the workspace root); the sanctioned
    // path is a pre-built `MYC_CHECK_CMD` binary, which carries its own absolute program path.
    let checker = MycChecker::from_env(env::current_dir().ok());
    let mut report = vet_batch(&checker, inputs);
    if let Some(dir) = phylum_dir {
        let summary = checker.vet_phylum(dir);
        report = report.with_phylum(dir, inputs, summary);
    }
    let vet_path = out_dir.join("vet.json");
    match serde_json::to_string_pretty(&report) {
        Ok(j) => {
            if let Err(e) = fs::write(&vet_path, j) {
                eprintln!(
                    "mycelium-transpile: failed to write {}: {e}",
                    vet_path.display()
                );
            }
        }
        Err(e) => eprintln!("mycelium-transpile: failed to serialize vet.json: {e}"),
    }
    print_vet_summary(&report, &vet_path);
}

fn print_vet_summary(report: &VetReport, vet_path: &Path) {
    let (clean_files, files_with_emissions) = report.clean_file_fraction();
    // Per-class file breakdown, deterministically ordered (BTreeMap).
    let classes = report
        .class_counts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "mycelium-transpile: --vet over {} file(s) — checked_fraction {:.1}% ({}/{} items \
         myc-check-clean, file-gated) vs expressible_fraction {:.1}% ({}/{} items emitted); \
         {clean_files}/{files_with_emissions} file(s) with emissions fully clean [{classes}] -> {}",
        report.records.len(),
        report.checked_fraction() * 100.0,
        report.total_checked_clean_items,
        report.total_non_test_items,
        report.expressible_fraction() * 100.0,
        report.total_emitted_items,
        report.total_non_test_items,
        vet_path.display(),
    );
    // DN-124 M-A dual-report: printed ONLY when a phylum-mode result was attached, and always
    // labeled a basis correction (never lever progress — VR-5).
    if let Some(phylum) = &report.phylum {
        if !phylum.ran {
            println!(
                "mycelium-transpile: --vet --phylum: myc check --phylum could not be run — \
                 checked_fraction_phylum not reported this run ({})",
                phylum.diagnostic
            );
        } else {
            println!(
                "mycelium-transpile: --vet --phylum: checked_fraction_phylum {:.1}% ({}/{} items, \
                 same denominator) vs checked_fraction (oracle) {:.1}% -- \
                 Δ_basis = {:+.1}pp (a basis CORRECTION -- recovered false-fails, NOT lever \
                 progress, DN-124 §4; phylum ok: {})",
                report.checked_fraction_phylum() * 100.0,
                report.total_checked_clean_items_phylum,
                report.total_non_test_items,
                report.checked_fraction() * 100.0,
                report.delta_basis() * 100.0,
                phylum.ok,
            );
        }
    }
}

/// Append a `.ext` suffix to a path **without** replacing any existing extension — unlike
/// `Path::with_extension`, which would eat a trailing dotted segment (`foo.bar` +`myc` →`foo.myc`).
/// So `<base>` → `<base>.myc` / `<base>.gap.json` faithfully, even for a `foo.bar` stem.
fn append_ext(base: &Path, ext: &str) -> PathBuf {
    let mut s = base.as_os_str().to_os_string();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// Write `<out_dir>/<rel_noext>.myc` + `<out_dir>/<rel_noext>.gap.json` for one already-transpiled
/// file, creating any parent directories. `rel_noext` is the output path **without** extension,
/// relative to `out_dir`: a bare stem in single-file mode (`lib`), or the source's path **mirrored
/// under the batch root** in directory mode (`mycelium-core/src/lib`) — the latter is what makes a
/// whole-corpus run non-lossy (two crates' `lib.rs` land at distinct, path-qualified outputs instead
/// of one overwriting the other; M-1006 Phase-2). Shared by both modes so they never drift. Returns
/// the written `.myc` path (for the vet loop).
fn write_pair(
    out_dir: &Path,
    rel_noext: &Path,
    myc_text: &str,
    report: &GapReport,
) -> Result<PathBuf, String> {
    let base = out_dir.join(rel_noext);
    if let Some(parent) = base.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    let myc_path = append_ext(&base, "myc");
    let gap_path = append_ext(&base, "gap.json");
    fs::write(&myc_path, myc_text)
        .map_err(|e| format!("failed to write {}: {e}", myc_path.display()))?;
    let gap_json = serde_json::to_string_pretty(report)
        .map_err(|e| format!("failed to serialize gap report for {}: {e}", base.display()))?;
    fs::write(&gap_path, gap_json)
        .map_err(|e| format!("failed to write {}: {e}", gap_path.display()))?;
    Ok(myc_path)
}

fn run_single_file(input: &Path, out_dir: &Path, vet: bool) -> ExitCode {
    let (myc_text, report) = match transpile_file(input) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("mycelium-transpile: {e}");
            return ExitCode::FAILURE;
        }
    };

    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");

    let myc_path = match write_pair(out_dir, Path::new(stem), &myc_text, &report) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("mycelium-transpile: {e}");
            return ExitCode::FAILURE;
        }
    };

    let emitted = report.emitted_items.len();
    // The headline count excludes non-gap advisories (e.g. `DeriveSatisfied` — "you already have
    // it", not a coverage loss; see `Category::is_non_gap_advisory`'s doc for why `NamedFieldDrop`
    // is deliberately NOT excluded here) — a review LOW on M-1086/#1544: the raw `gaps.len()` was
    // inflating this total by counting satisfied-no-op derives as if they were gaps.
    let gapped = report.real_gap_count();
    let non_test = report.non_test_item_count();
    println!(
        "mycelium-transpile: {} top-level item(s) ({} non-test) — {} emitted, {} gap(s) \
         recorded, {:.1}% expressible -> {}/{stem}.myc, {}/{stem}.gap.json",
        report.total_top_level_items,
        non_test,
        emitted,
        gapped,
        report.expressible_fraction() * 100.0,
        out_dir.display(),
        out_dir.display(),
    );

    if vet {
        let inputs = vec![VetInput::from_report(myc_path, &report)];
        // Single-file mode names no real phylum boundary (a lone file's dir may hold unrelated
        // artifacts) — phylum-mode vetting is directory-mode only (DN-124 §3.2/§6 Attack 1a).
        run_vet(&inputs, out_dir, None);
    }
    ExitCode::SUCCESS
}

fn run_batch(input_dir: &Path, out_dir: &Path, vet: bool) -> ExitCode {
    let files = match discover_rs_files(input_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "mycelium-transpile: failed to walk {}: {e}",
                input_dir.display()
            );
            return ExitCode::FAILURE;
        }
    };
    if files.is_empty() {
        eprintln!(
            "mycelium-transpile: no .rs files found under {} (after skipping test \
             infrastructure)",
            input_dir.display()
        );
        return ExitCode::FAILURE;
    }
    write_batch_and_maybe_vet(&files, Some(input_dir), out_dir, vet)
}

/// **M-1079/DN-124 §3.2**: batch an **explicit, caller-named** file set — no directory discovery,
/// no staging. Each file's real path (exactly as given) is transpiled and recorded, so the
/// committed `summary.json`/`vet.json` provenance is the actual repo source location, never a
/// scratch/staging path (which would be both non-portable and non-deterministic across reruns —
/// see the module docs). The output-naming `root` is the **common ancestor** of every named file's
/// own parent directory (`batch::common_ancestor`, bug-fixed from an earlier `files[0].parent()`-only
/// root — see that function's doc for the collision it fixes): a single sibling group (the common
/// `--files` case, DN-124 §6 Attack 1a's boundary constraint remains the caller's responsibility)
/// degenerates to that shared directory exactly as before, while a set spanning MULTIPLE crate roots
/// (e.g. batching several crates' `src/lib.rs` in one run) now walks up to their shared ancestor so
/// every file's output is crate-qualified instead of colliding on a bare stem. When there is
/// genuinely no common ancestor (`common_ancestor` returns `None` — a mixed absolute/relative
/// `--files` set), `write_batch_and_maybe_vet` routes every file through its bare-stem fallback with
/// a warning rather than mis-writing outside `out_dir` (see that function's `root: Option<&Path>`).
fn run_explicit_files(files: &[PathBuf], out_dir: &Path, vet: bool) -> ExitCode {
    let root = common_ancestor(files);
    write_batch_and_maybe_vet(files, root.as_deref(), out_dir, vet)
}

/// Shared batch-write + optional-vet tail for both directory-discovered ([`run_batch`]) and
/// explicit ([`run_explicit_files`]) file sets: write every emitted `.myc`/`.gap.json` pair
/// (**path-qualified** by mirroring each file's path relative to `root`, M-1006 Phase-2), the three
/// combined artifacts (`summary.json`/`REMAP.md`/`union.gap.json`), and — when `--vet` — the
/// oracle-mode vet loop **plus** phylum-mode dual-reporting over `out_dir` as one real phylum
/// (DN-124 §3.1/M-A; both directory mode and `--files` name a real phylum boundary, so both dual-
/// report identically).
///
/// `root: None` means there is no safe root to mirror the source tree under (directory mode always
/// passes `Some` — the discovered directory itself; only `--files` can hit `None`, via
/// `batch::common_ancestor`'s no-common-ancestor case). Every file then falls back to its bare stem
/// individually, with a warning — never `output_rel_path`'s `Ok` arm, which would require a `root`
/// to strip against; this is what keeps the no-common-ancestor case from ever silently writing
/// outside `out_dir` (the bug `common_ancestor`'s `Option` return closes; see its doc).
fn write_batch_and_maybe_vet(
    files: &[PathBuf],
    root: Option<&Path>,
    out_dir: &Path,
    vet: bool,
) -> ExitCode {
    let (results, failures) = transpile_batch(files);
    // A hard parse/read failure is never silently dropped from the run (G2) — it is reported and
    // fails the process, distinct from a per-item gap (which the summary/union artifacts do
    // capture).
    for (path, err) in &failures {
        eprintln!("mycelium-transpile: {}: {err}", path.display());
    }

    // Per-file artifacts, **path-qualified** by mirroring the source tree under `out_dir` (M-1006
    // Phase-2): each file's path relative to `root` becomes its output path, so two crates'
    // `lib.rs` land at distinct outputs (`mycelium-core/src/lib.myc` vs `mycelium-std/src/lib.myc`)
    // instead of one silently overwriting the other — the whole-corpus-completeness fix that lets an
    // automated multi-crate wave keep every emission. Distinct source files have distinct relative
    // paths, so a collision is impossible by construction; a defensive guard still flags the
    // impossible case (never silent, G2) rather than trusting the invariant blindly.
    let mut written: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    // One vet input per written `.myc` (only used when `--vet`), keyed by the actual output path so
    // the order is deterministic and the file vetted is exactly the file written.
    let mut vet_inputs: std::collections::BTreeMap<PathBuf, VetInput> =
        std::collections::BTreeMap::new();
    for r in &results {
        // Path relative to `root`, `.rs` extension stripped (pure logic in `batch.rs` so it is unit
        // -tested there). Fall back to the bare stem if the path is somehow not under `root`, or if
        // there is no `root` at all (never-silent — warned, not silently mis-placed; see
        // `write_batch_and_maybe_vet`'s doc for why `root: None` must NEVER take the `Ok` arm below).
        let rel_noext = match root {
            Some(root) => match output_rel_path(&r.path, root) {
                Ok(rel) => rel,
                Err(fallback) => {
                    eprintln!(
                        "mycelium-transpile: WARNING {} is not under the batch root {} — falling \
                         back to a bare-stem output name",
                        r.path.display(),
                        root.display()
                    );
                    fallback
                }
            },
            None => {
                let fallback = PathBuf::from(
                    r.path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("output"),
                );
                eprintln!(
                    "mycelium-transpile: WARNING {} — no common ancestor for this --files set \
                     (mixed absolute/relative paths, or divergent roots) — falling back to a \
                     bare-stem output name",
                    r.path.display()
                );
                fallback
            }
        };
        let myc_path = match write_pair(out_dir, &rel_noext, &r.myc, &r.report) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("mycelium-transpile: {e}");
                return ExitCode::FAILURE;
            }
        };
        if !written.insert(myc_path.clone()) {
            eprintln!(
                "mycelium-transpile: WARNING output path collision at {} — a prior file already \
                 wrote here (should be impossible with path-qualified naming)",
                myc_path.display()
            );
        }
        if vet {
            vet_inputs.insert(myc_path.clone(), VetInput::from_report(myc_path, &r.report));
        }
    }

    // `summarize`/`build_remap_manifest`'s `derive_phylum` already degrades gracefully (falls back
    // to `"unknown"`) for a root with no usable file name (`src/remap.rs::derive_phylum` doc) — so
    // the `None` (no-common-ancestor) case passes an empty placeholder rather than needing its own
    // signature change through `summarize`/`build_remap_manifest`.
    let (batch_summary, union) = summarize(&results, root.unwrap_or_else(|| Path::new("")));

    let summary_path = out_dir.join("summary.json");
    match serde_json::to_string_pretty(&batch_summary) {
        Ok(j) => {
            if let Err(e) = fs::write(&summary_path, j) {
                eprintln!(
                    "mycelium-transpile: failed to write {}: {e}",
                    summary_path.display()
                );
                return ExitCode::FAILURE;
            }
        }
        Err(e) => {
            eprintln!("mycelium-transpile: failed to serialize summary.json: {e}");
            return ExitCode::FAILURE;
        }
    }

    // M-1044 / DN-109 §5.2: REMAP.md is a pure projection of `batch_summary.remap` (the JSON is
    // the source of truth, this is a rendered view) — see `src/remap.rs`.
    let remap_md_path = out_dir.join("REMAP.md");
    if let Err(e) = fs::write(&remap_md_path, render_remap_md(&batch_summary.remap)) {
        eprintln!(
            "mycelium-transpile: failed to write {}: {e}",
            remap_md_path.display()
        );
        return ExitCode::FAILURE;
    }

    let union_path = out_dir.join("union.gap.json");
    match serde_json::to_string_pretty(&union) {
        Ok(j) => {
            if let Err(e) = fs::write(&union_path, j) {
                eprintln!(
                    "mycelium-transpile: failed to write {}: {e}",
                    union_path.display()
                );
                return ExitCode::FAILURE;
            }
        }
        Err(e) => {
            eprintln!("mycelium-transpile: failed to serialize union.gap.json: {e}");
            return ExitCode::FAILURE;
        }
    }

    // The headline count excludes non-gap advisories, same as the single-file print above (a
    // review LOW on M-1086/#1544) — computed straight from `union.gaps` (the batch-wide raw list)
    // rather than `batch_summary.totals.gaps`, so `Totals`/`summary.json`'s own `gaps` field (a
    // committed artifact other tooling may already parse) is left untouched; only this printed
    // headline changes.
    let real_gap_total = union
        .gaps
        .iter()
        .filter(|g| !g.category.is_non_gap_advisory())
        .count();
    println!(
        "mycelium-transpile: batch over {} file(s) ({} failed to parse) — {} top-level item(s) \
         ({} non-test), {} emitted, {} gap(s), {:.1}% expressible, {} nodule(s) recorded in the \
         remap manifest -> {}, {}, {}",
        results.len(),
        failures.len(),
        batch_summary.totals.total_items,
        batch_summary.totals.non_test_items,
        batch_summary.totals.emitted,
        real_gap_total,
        batch_summary.totals.expressible_pct,
        batch_summary.remap.nodules.len(),
        summary_path.display(),
        remap_md_path.display(),
        union_path.display(),
    );

    if vet {
        let inputs: Vec<VetInput> = vet_inputs.into_values().collect();
        // Batch/directory mode (incl. an explicit `--files` set): everything just written under
        // `out_dir` forms the ONE real phylum boundary for this run (DN-124 §3.2) — dual-report
        // phylum-mode alongside oracle-mode.
        run_vet(&inputs, out_dir, Some(out_dir));
    }

    if failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
