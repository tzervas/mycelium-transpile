//! The transpile → `myc check` **vet loop** (M-1000).
//!
//! The transpiler's `expressible_fraction` (see [`crate::gap::GapReport`]) measures only that *some*
//! `.myc` text was emitted for an item — it never runs the real toolchain over the emission, so it
//! systematically **over**-counts: an emitted fragment that fails to parse or type-check still counts
//! as "expressible". This module closes that loop. After transpiling, it runs the **real**
//! `myc check` oracle (`crates/mycelium-check`, the same per-file oracle mode
//! `scripts/checks/myc-dogfood.sh` uses) over each emitted `.myc` file, folds each outcome into a
//! structured [`VetRecord`] carrying the exit class + first diagnostic, and reports
//! [`VetReport::checked_fraction`] — myc-check-clean coverage — **alongside** the emission-only
//! `expressible_fraction`. A draft is then `myc-check-clean` or `gap/vet-flagged`, never silently
//! broken (G2).
//!
//! # Guarantee tags (VR-5)
//!
//! - The emitted `.myc` stays **Declared** (a heuristic `syn` → surface-text mapping; see
//!   `src/lib.rs`).
//! - The vet **verdict** is **Empirical**: it is *measured* by running the real `myc check` binary
//!   over the emission. It is never `Proven` (the oracle's own checking depth is name-visibility —
//!   M-365 — not a whole-program proof), and never silently upgraded past that basis. An oracle that
//!   could not be *run at all* (tool absent / spawn failure) is recorded as
//!   [`VetClass::ToolUnavailable`] — a run with no verdict is **never** counted as clean (G2/VR-5).
//!
//! # The `checked_fraction` metric — denominator and numerator, stated honestly
//!
//! `myc check` (oracle mode) is a **per-file** verdict: it parses + type-checks a whole nodule and
//! returns one exit code, not a per-item result. The vet metric bridges that per-file verdict back
//! to the per-item denominator `expressible_fraction` uses, so the two are directly comparable:
//!
//! - **Denominator** = **non-excluded top-level items** (summed over vetted files) — the *same*
//!   denominator as [`crate::gap::GapReport::expressible_fraction`], i.e.
//!   [`crate::gap::GapReport::non_test_item_count`]: every top-level item **minus** the
//!   denominator-excluded ones ([`crate::gap::Category::excluded_from_denominator`] — `#[cfg(test)]`
//!   `TestItem`s **and**, since M-1006 Phase-2, bodyless `mod foo;` `ModuleDecl` file-linkage
//!   declarations, both recorded-never-dropped but not translatable library surface). The
//!   [`VetRecord::non_test_items`] field keeps its name for API stability but carries this
//!   non-*excluded* count. Stated, so the two fractions line up and
//!   `checked_fraction ≤ expressible_fraction` always holds (an item can only be checked-clean if it
//!   was emitted at all).
//! - **Numerator** = the **file-gated** item bridge: a file's emitted items are credited to the
//!   checked numerator **iff the file's *entire* emitted `.myc` is myc-check-clean**; a file that
//!   fails parse/check contributes **0** (we never guess *which* item broke a failing file — VR-5/
//!   G2). So `checked_fraction` is an all-or-nothing-per-file **lower bound** on true per-item
//!   correctness: honestly conservative, never optimistic.
//!
//! A companion **file-level** metric ([`VetReport::clean_file_fraction`]) — clean files over
//! files-with-emissions — is reported too (its denominator stated in situ), for the coarser
//! "how many drafts are wholly clean" view.
//!
//! # Phylum-mode dual-report (DN-124 / M-1079, Unit 2)
//!
//! The oracle above is **phylum-blind**: it checks each `.myc` alone (a phylum-of-one), so a
//! correctly-emitted cross-nodule `use checkty.Width;` is a false-FAIL there (DN-124 §1) even when
//! the transpiler emitted it correctly. [`MycChecker::vet_phylum`] runs the **real** `myc check
//! --phylum <dir> --json` over a **whole batch output directory** — the kernel's cross-nodule
//! resolver, consuming `mycelium-check`'s P-A partial per-nodule verdicts (never fabricated: a
//! nodule is credited only when its own whole import closure checks clean) — and
//! [`VetReport::with_phylum`]/[`VetReport::checked_fraction_phylum`] dual-report the corrected metric
//! **alongside**, never *in place of*, the oracle-mode `checked_fraction` this module already
//! computes, over the identical denominator (so the two are directly comparable).
//!
//! **The basis-change honesty obligation (M-A, DN-124 §4).** `checked_fraction_phylum` is the more
//! faithful basis (it matches the real phylum boundary a build actually checks), but recovering
//! previously-false-failed items is a **basis correction**, not transpiler/lever progress —
//! [`VetReport::delta_basis`] computes and labels that one-time jump explicitly so it is never
//! mistaken for real progress (VR-5). This crate deliberately shells out to the real `myc-check`
//! binary (mirroring the oracle path above) rather than linking `mycelium-check` as a library
//! dependency — the vet verdict stays `Empirical`, measured against the real toolchain.
//!
//! **Known residual (documented, never-silent — YAGNI for now):** if even one file under the batch
//! output directory fails to *parse*, `mycelium-check`'s phylum mode refuses the whole batch with an
//! empty `nodules` list (no partial credit on a parse refusal — see `mycelium-check`'s module docs),
//! so `checked_fraction_phylum` would read `0%` for that whole directory rather than crediting the
//! files that *did* parse. The committed `gen/myc-drafts/` corpus has zero `ParseError`-class records
//! today (every emission at least parses), so this is not a live gap for the current corpus; it is
//! flagged here rather than engineered against speculatively.

use crate::gap::GapReport;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The classification of one `myc check` run over one emitted `.myc` file, keyed off the documented
/// `myc-check` oracle-mode exit contract (`crates/mycelium-check/src/bin/myc-check.rs`:
/// `0` ok · `2` parse error · `3` check error · `64` usage · `66` I/O; `5` project-resolution is
/// project-mode-only and never seen here, but any other code is preserved via [`VetClass::Other`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum VetClass {
    /// exit `0` — parsed **and** type-checked clean. The only class that credits the checked numerator.
    Clean,
    /// exit `2` — the emitted `.myc` did not parse (a surface-syntax defect in the emission).
    ParseError,
    /// exit `3` — parsed but failed the L1 type-check (e.g. an unresolved `use`, an impl of an
    /// unknown trait, an unknown prim).
    CheckError,
    /// exit `64` — usage error. This is a bug in *how the vet driver invoked* `myc check`, never a
    /// verdict on the `.myc`; surfaced (never silent) so a driver mistake is not mistaken for a
    /// check failure.
    Usage,
    /// exit `66` — `myc check` could not read the file it was handed (I/O).
    Io,
    /// The oracle could not be **run at all** (spawn failed — binary absent / not built), or exited
    /// via a signal with no exit code. Distinct from every real verdict so an *unavailable* oracle
    /// is never silently read as "clean" (G2/VR-5).
    ToolUnavailable,
    /// Any other/unexpected non-zero exit code — preserved verbatim (forward-compatibility), never
    /// collapsed into `Clean`.
    Other(i32),
}

impl VetClass {
    /// Map a process exit code to a vet class. `None` (killed by signal — no exit code) maps to
    /// [`VetClass::ToolUnavailable`]: a run that did not exit normally yields no verdict we may
    /// count. **Never** maps an unknown code to `Clean` (G2/VR-5).
    pub fn from_exit_code(code: Option<i32>) -> VetClass {
        match code {
            Some(0) => VetClass::Clean,
            Some(2) => VetClass::ParseError,
            Some(3) => VetClass::CheckError,
            Some(64) => VetClass::Usage,
            Some(66) => VetClass::Io,
            Some(other) => VetClass::Other(other),
            None => VetClass::ToolUnavailable,
        }
    }

    /// Whether this class credits the checked numerator (only [`VetClass::Clean`] does).
    pub fn is_clean(self) -> bool {
        matches!(self, VetClass::Clean)
    }

    /// A stable `&'static str` label for per-class counting/serialization. `Other(_)` collapses to
    /// `"Other"` for the count map; the exact code is retained on each [`VetRecord::exit_code`].
    pub fn label(self) -> &'static str {
        match self {
            VetClass::Clean => "Clean",
            VetClass::ParseError => "ParseError",
            VetClass::CheckError => "CheckError",
            VetClass::Usage => "Usage",
            VetClass::Io => "Io",
            VetClass::ToolUnavailable => "ToolUnavailable",
            VetClass::Other(_) => "Other",
        }
    }
}

/// One emitted `.myc` file's `myc check` outcome.
#[derive(Debug, Clone, Serialize)]
pub struct VetRecord {
    /// The emitted `.myc` file that was vetted.
    pub myc_file: String,
    /// The Rust source file it was transpiled from (cross-references back to the gap report).
    pub source_file: String,
    /// The classified outcome.
    pub class: VetClass,
    /// The raw process exit code, when the checker ran to completion (`None` when it could not run
    /// or was signalled).
    pub exit_code: Option<i32>,
    /// The checker's first meaningful diagnostic line — the `parse-error:`/`check-error:` line for a
    /// parse/check failure, else the first non-empty stderr/stdout line. Truncated for report size
    /// (see [`MAX_DIAGNOSTIC_LEN`]), never dropped entirely.
    pub diagnostic: String,
    /// Non-*excluded* top-level items in the source file (every top-level item minus the
    /// denominator-excluded `TestItem`/`ModuleDecl` gaps — see the module docs) — this file's
    /// contribution to the shared denominator. Field name kept for API stability.
    pub non_test_items: usize,
    /// Items for which `.myc` text was emitted — this file's contribution to the expressible
    /// numerator, and (when [`VetClass::Clean`]) the checked numerator.
    pub emitted_items: usize,
}

impl VetRecord {
    /// Items this file contributes to the **checked-clean** numerator: all of its emitted items when
    /// the whole emitted nodule is myc-check-clean, else `0` (the file-gated bridge documented on
    /// the module — `myc check` verdicts a whole file, so a failing file has no per-item credit we
    /// may honestly attribute).
    pub fn checked_clean_items(&self) -> usize {
        if self.class.is_clean() {
            self.emitted_items
        } else {
            0
        }
    }
}

/// Cap on a stored diagnostic line's length (report-size hygiene). A longer line is truncated with a
/// trailing `…` marker — never silently dropped, just bounded.
pub const MAX_DIAGNOSTIC_LEN: usize = 400;

/// One row of the `myc check --phylum --json` `nodules` array (P-A, DN-124 §2.3/§3.1). Parsed
/// generically from the stable one-line JSON contract (no `mycelium-check` library dependency — this
/// crate deliberately shells out to the **real** toolchain binary, mirroring [`MycChecker::vet_file`]'s
/// existing per-file design, so the vet verdict stays `Empirical`).
#[derive(Debug, Clone, Serialize)]
pub struct PhylumNodule {
    /// The nodule's dotted path (`a`, `checkty`).
    pub nodule: String,
    /// The originating source's file label (relative to the checked dir, forward-slashed) — the join
    /// key back to a [`VetInput::myc_path`] (see [`phylum_checked_clean_items`]).
    pub file: String,
    /// `"Clean"` · `"CheckError"` · `"Blocked"` (mirrors `mycelium_check::NoduleClass::label`).
    pub class: String,
    /// The failure site, when `class` is `"CheckError"`.
    pub site: Option<String>,
    /// The closure member this nodule was blocked on, when `class` is `"Blocked"`.
    pub on: Option<String>,
    /// The failure message, when `class` is `"CheckError"`/`"Blocked"`.
    pub message: Option<String>,
}

impl PhylumNodule {
    /// Whether this row credits the checked-phylum numerator (only `"Clean"` does).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.class == "Clean"
    }
}

/// The whole-batch phylum-mode vet outcome (DN-124 §3.1) — dual-reported alongside the oracle-mode
/// metrics [`VetReport`] already carries, never in place of them (M-A).
#[derive(Debug, Clone, Serialize)]
pub struct PhylumVetSummary {
    /// Whether `myc check --phylum` could be **run at all** (spawn success + a well-formed `--json`
    /// line). `false` ⇒ every file contributes `0` to the phylum numerator — a run with no verdict is
    /// **never** counted clean (G2/VR-5), mirroring [`VetClass::ToolUnavailable`]'s discipline.
    pub ran: bool,
    /// The whole-phylum verdict (unchanged by P-A — see `mycelium-check`'s module docs; **never**
    /// conflated with the per-nodule `nodules` below).
    pub ok: bool,
    /// One row per nodule the phylum check could assemble (empty on a `parse`/`duplicate` refusal —
    /// see the module's "known residual" note).
    pub nodules: Vec<PhylumNodule>,
    /// The whole-phylum error message when `!ok`, or the run failure when `!ran` — never both silent.
    pub diagnostic: String,
}

/// The aggregate vet report for a batch/single-file vet run — the `vet.json` artifact.
#[derive(Debug, Clone, Serialize)]
pub struct VetReport {
    /// One record per vetted `.myc` file (never deduplicated — each is a distinct file).
    pub records: Vec<VetRecord>,
    /// Sum of `non_test_items` across all vetted files — the shared denominator for **both**
    /// `expressible_fraction` and `checked_fraction` (stated denominator: non-*excluded* top-level
    /// items — every top-level item minus the `TestItem`/`ModuleDecl` denominator-excluded gaps).
    pub total_non_test_items: usize,
    /// Sum of `emitted_items` across all vetted files (the expressible numerator).
    pub total_emitted_items: usize,
    /// Sum of `checked_clean_items` across all vetted files (the **oracle-mode** checked numerator —
    /// unchanged by the DN-124 dual-report addition below).
    pub total_checked_clean_items: usize,
    /// Per-class file counts, for the headline "N/M files clean" summary.
    pub class_counts: BTreeMap<&'static str, usize>,
    /// The phylum-mode vet result (DN-124 §3.1/M-A), when attached via [`VetReport::with_phylum`].
    /// `None` for an oracle-only report (backward compatible — [`VetReport::from_records`] alone
    /// never populates this).
    pub phylum: Option<PhylumVetSummary>,
    /// Sum of items credited via the phylum-mode partial verdicts (the **phylum-mode** checked
    /// numerator — DN-124 §3.1's "crediting each Clean nodule's emitted items", generalizing the
    /// oracle path's file-gated bridge from "oracle exit 0" to "nodule Clean"). `0` when `phylum` is
    /// `None`.
    pub total_checked_clean_items_phylum: usize,
}

impl VetReport {
    /// Aggregate a set of per-file [`VetRecord`]s into a report. Pure — no process spawning — so the
    /// metric arithmetic is unit-testable without the toolchain present.
    pub fn from_records(records: Vec<VetRecord>) -> VetReport {
        let mut total_non_test_items = 0usize;
        let mut total_emitted_items = 0usize;
        let mut total_checked_clean_items = 0usize;
        let mut class_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for r in &records {
            total_non_test_items += r.non_test_items;
            total_emitted_items += r.emitted_items;
            total_checked_clean_items += r.checked_clean_items();
            *class_counts.entry(r.class.label()).or_insert(0) += 1;
        }
        VetReport {
            records,
            total_non_test_items,
            total_emitted_items,
            total_checked_clean_items,
            class_counts,
            phylum: None,
            total_checked_clean_items_phylum: 0,
        }
    }

    /// Attach a phylum-mode vet result (DN-124 §3.1) — dual-reports [`Self::checked_fraction_phylum`]
    /// alongside the oracle-mode [`Self::checked_fraction`] this report already carries, over the
    /// **same** denominator ([`Self::total_non_test_items`], untouched). Fluent (the crate's M-644
    /// convention); `dir` and `inputs` are exactly what was passed to the phylum check / oracle vet —
    /// used only to compute the file-join credit ([`phylum_checked_clean_items`]).
    #[must_use]
    pub fn with_phylum(
        mut self,
        dir: &Path,
        inputs: &[VetInput],
        summary: PhylumVetSummary,
    ) -> Self {
        self.total_checked_clean_items_phylum = phylum_checked_clean_items(dir, inputs, &summary);
        self.phylum = Some(summary);
        self
    }

    /// **checked_fraction** — myc-check-clean coverage. Numerator: checked-clean items (file-gated,
    /// see the module docs); denominator: non-test top-level items (stated). `0.0` when there are no
    /// non-test items (never a divide-by-zero / fabricated ratio). **Empirical** (measured by the
    /// real toolchain).
    pub fn checked_fraction(&self) -> f64 {
        if self.total_non_test_items == 0 {
            return 0.0;
        }
        self.total_checked_clean_items as f64 / self.total_non_test_items as f64
    }

    /// **checked_fraction_phylum** — the DN-124 P-A/M-A basis-corrected metric: myc-check-clean
    /// coverage measured against the **real phylum** a nodule belongs to (never a phylum-of-one
    /// counterfactual), over the same denominator as [`Self::checked_fraction`]. `0.0` when no
    /// phylum result is attached ([`Self::with_phylum`] was never called), or the denominator is `0`.
    /// **Empirical** — measured by the real `myc check --phylum` toolchain, never a re-derivation.
    pub fn checked_fraction_phylum(&self) -> f64 {
        if self.total_non_test_items == 0 {
            return 0.0;
        }
        self.total_checked_clean_items_phylum as f64 / self.total_non_test_items as f64
    }

    /// **Δ_basis** (DN-124 §4.3 step 2) — the one-time basis-correction delta
    /// (`checked_fraction_phylum − checked_fraction`), to be **labeled a basis correction and
    /// reported separately from any real lever/transpiler gain landing in the same cycle** (VR-5:
    /// never fold a measurement-basis jump into a claimed lever gain — see the module docs). `0.0`
    /// when no phylum result is attached (nothing to correct for).
    pub fn delta_basis(&self) -> f64 {
        if self.phylum.is_none() {
            return 0.0;
        }
        self.checked_fraction_phylum() - self.checked_fraction()
    }

    /// **expressible_fraction** — emission-only coverage, recomputed here over the *same* denominator
    /// as [`Self::checked_fraction`] for a side-by-side comparison (matches
    /// [`crate::gap::GapReport::expressible_fraction`] aggregated across the vetted files).
    /// **Declared** (emission is unvalidated; see `src/lib.rs`).
    pub fn expressible_fraction(&self) -> f64 {
        if self.total_non_test_items == 0 {
            return 0.0;
        }
        self.total_emitted_items as f64 / self.total_non_test_items as f64
    }

    /// Companion **file-level** metric: `(clean_files, files_with_emissions)`. A file "has emissions"
    /// when `emitted_items > 0` (a header-only nodule that trivially checks is not counted as a clean
    /// *draft*). Returned as a raw pair so the caller states the denominator explicitly rather than
    /// hiding a `0/0`.
    pub fn clean_file_fraction(&self) -> (usize, usize) {
        let files_with_emissions = self.records.iter().filter(|r| r.emitted_items > 0).count();
        let clean_files = self
            .records
            .iter()
            .filter(|r| r.emitted_items > 0 && r.class.is_clean())
            .count();
        (clean_files, files_with_emissions)
    }
}

/// Build a [`VetRecord`] from one completed `myc check` run's parts. **Pure** — testable without
/// spawning a process. Chooses the most informative diagnostic line for the class (the oracle prints
/// `parse-error:`/`check-error:` to stdout; other failures land on stderr).
#[allow(clippy::too_many_arguments)]
pub fn classify_run(
    myc_file: String,
    source_file: String,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    non_test_items: usize,
    emitted_items: usize,
) -> VetRecord {
    let class = VetClass::from_exit_code(exit_code);
    let diagnostic = extract_diagnostic(class, stdout, stderr);
    VetRecord {
        myc_file,
        source_file,
        class,
        exit_code,
        diagnostic,
        non_test_items,
        emitted_items,
    }
}

/// First non-empty, trimmed line of `s` (or `""`).
fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Pick the class-appropriate diagnostic line, bounded to [`MAX_DIAGNOSTIC_LEN`] (never fully
/// dropped — a truncation appends `…`).
fn extract_diagnostic(class: VetClass, stdout: &str, stderr: &str) -> String {
    let raw = match class {
        VetClass::Clean => String::new(),
        // Oracle mode prints the `parse-error:`/`check-error:` line to stdout; fall back to stderr.
        VetClass::ParseError | VetClass::CheckError => {
            let d = first_line(stdout);
            if d.is_empty() {
                first_line(stderr)
            } else {
                d
            }
        }
        // I/O, usage, tool-unavailable, and unexpected codes: prefer stderr, else stdout.
        _ => {
            let d = first_line(stderr);
            if d.is_empty() {
                first_line(stdout)
            } else {
                d
            }
        }
    };
    truncate_diagnostic(&raw)
}

fn truncate_diagnostic(s: &str) -> String {
    if s.chars().count() <= MAX_DIAGNOSTIC_LEN {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_DIAGNOSTIC_LEN).collect();
    out.push('…');
    out
}

/// How to invoke the `myc check` oracle: a command **prefix** (program + any leading args) to which
/// the target `.myc` path is appended as the final argument. Two sanctioned forms:
///
/// - a **pre-built binary** — `["<repo>/target/debug/myc-check"]` — what
///   `scripts/checks/transpile-vet.sh` passes (built once up front, so no nested-`cargo` build-lock
///   contention with an outer `cargo run` of the transpiler); or
/// - the **cargo fallback** — `["cargo","run","-q","-p","mycelium-check","--bin","myc-check","--"]`
///   — the default when nothing is configured (mirrors `scripts/checks/myc-dogfood.sh`); requires
///   `cwd` = the workspace root.
#[derive(Debug, Clone)]
pub struct MycChecker {
    /// Command prefix; the `.myc` path is appended per file. Must be non-empty.
    pub command: Vec<String>,
    /// Working directory for the checker (the cargo fallback needs the workspace root). `None`
    /// inherits the current directory.
    pub cwd: Option<PathBuf>,
}

impl MycChecker {
    /// The checker configured from the environment. `MYC_CHECK_CMD`, when set, is whitespace-split
    /// into the command prefix (typically a pre-built binary path — the sanctioned, lock-safe form).
    /// Otherwise the cargo fallback is used, with `cwd` = `workspace_root`.
    pub fn from_env(workspace_root: Option<PathBuf>) -> MycChecker {
        match std::env::var("MYC_CHECK_CMD") {
            Ok(cmd) if !cmd.trim().is_empty() => MycChecker {
                command: cmd.split_whitespace().map(str::to_string).collect(),
                // An explicit override carries its own (absolute) program path; do not force a cwd.
                cwd: None,
            },
            _ => MycChecker {
                command: [
                    "cargo",
                    "run",
                    "-q",
                    "-p",
                    "mycelium-check",
                    "--bin",
                    "myc-check",
                    "--",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect(),
                cwd: workspace_root,
            },
        }
    }

    /// Run `myc check` on one `.myc` file and classify the outcome. A spawn failure (tool absent) is
    /// returned as a [`VetClass::ToolUnavailable`] record — never an error that aborts the whole run
    /// (the run reports *which* files could not be vetted; never-silent, never a hard stop).
    pub fn vet_file(
        &self,
        myc_file: &Path,
        source_file: &str,
        non_test_items: usize,
        emitted_items: usize,
    ) -> VetRecord {
        if self.command.is_empty() {
            return classify_run(
                myc_file.display().to_string(),
                source_file.to_string(),
                None,
                "",
                "vet driver misconfigured: empty myc-check command",
                non_test_items,
                emitted_items,
            );
        }
        let mut cmd = Command::new(&self.command[0]);
        cmd.args(&self.command[1..]);
        cmd.arg(myc_file);
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        match cmd.output() {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                classify_run(
                    myc_file.display().to_string(),
                    source_file.to_string(),
                    out.status.code(),
                    &stdout,
                    &stderr,
                    non_test_items,
                    emitted_items,
                )
            }
            Err(e) => VetRecord {
                myc_file: myc_file.display().to_string(),
                source_file: source_file.to_string(),
                class: VetClass::ToolUnavailable,
                exit_code: None,
                diagnostic: truncate_diagnostic(&format!(
                    "could not run `{}`: {e}",
                    self.command.join(" ")
                )),
                non_test_items,
                emitted_items,
            },
        }
    }

    /// Run `myc check --phylum <dir> --json` (DN-124 §2.3/§3.1, P-A partial verdicts) and parse the
    /// stable one-line JSON contract into a [`PhylumVetSummary`]. A spawn failure or a
    /// malformed/missing JSON line is `ran: false` — **never** fabricates a clean result (G2/VR-5),
    /// mirroring [`Self::vet_file`]'s [`VetClass::ToolUnavailable`] discipline.
    pub fn vet_phylum(&self, dir: &Path) -> PhylumVetSummary {
        if self.command.is_empty() {
            return PhylumVetSummary {
                ran: false,
                ok: false,
                nodules: Vec::new(),
                diagnostic: "vet driver misconfigured: empty myc-check command".to_owned(),
            };
        }
        let mut cmd = Command::new(&self.command[0]);
        cmd.args(&self.command[1..]);
        cmd.arg("--phylum");
        cmd.arg(dir);
        cmd.arg("--json");
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        let out = match cmd.output() {
            Ok(out) => out,
            Err(e) => {
                return PhylumVetSummary {
                    ran: false,
                    ok: false,
                    nodules: Vec::new(),
                    diagnostic: truncate_diagnostic(&format!(
                        "could not run `{}` --phylum: {e}",
                        self.command.join(" ")
                    )),
                };
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        // The `--json` contract is ONE line, one JSON object — take the first non-empty stdout line.
        let line = stdout.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let diag = if stderr.trim().is_empty() {
                    format!("malformed `--phylum --json` output: {e}")
                } else {
                    first_line(&stderr)
                };
                return PhylumVetSummary {
                    ran: false,
                    ok: false,
                    nodules: Vec::new(),
                    diagnostic: truncate_diagnostic(&diag),
                };
            }
        };
        parse_phylum_json(&value)
    }
}

/// Parse a decoded `--phylum --json` [`serde_json::Value`] into a [`PhylumVetSummary`]. Defensive by
/// construction (VR-5/G2): a missing/wrongly-typed required field on a `nodules` row drops just that
/// row (never fabricates a verdict for it — it simply is not credited, the conservative direction).
fn parse_phylum_json(value: &serde_json::Value) -> PhylumVetSummary {
    let ok = value
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let diagnostic = value
        .get("error")
        .filter(|e| !e.is_null())
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map(str::to_owned)
        .unwrap_or_default();
    let nodules = value
        .get("nodules")
        .and_then(serde_json::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let nodule = row.get("nodule")?.as_str()?.to_owned();
                    let file = row.get("file")?.as_str()?.to_owned();
                    let class = row.get("class")?.as_str()?.to_owned();
                    let site = row.get("site").and_then(|s| s.as_str()).map(str::to_owned);
                    let on = row.get("on").and_then(|s| s.as_str()).map(str::to_owned);
                    let message = row
                        .get("message")
                        .and_then(|s| s.as_str())
                        .map(str::to_owned);
                    Some(PhylumNodule {
                        nodule,
                        file,
                        class,
                        site,
                        on,
                        message,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    PhylumVetSummary {
        ran: true,
        ok,
        nodules,
        diagnostic: truncate_diagnostic(&diagnostic),
    }
}

/// Relative, forward-slash path of `file` under `dir` — mirrors exactly the relative-path convention
/// `mycelium-check`'s `collect_myc`/`check_phylum_dir` use to label each `NoduleVerdict::file`
/// (`file.strip_prefix(dir)…to_string_lossy()…replace('\\','/')`), so a [`PhylumNodule::file`] can be
/// joined back to a [`VetInput::myc_path`] by plain string equality.
fn relative_slash_path(dir: &Path, file: &Path) -> String {
    file.strip_prefix(dir)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Credit each input's items via the phylum-mode partial verdicts (DN-124 §3.1): a file is credited
/// its `emitted_items` **iff** `summary.ran` **and** its corresponding nodule verdict is `Clean` — the
/// same file-gated bridge [`VetRecord::checked_clean_items`] uses for oracle mode, generalized from
/// "oracle exit 0" to "nodule Clean". Returns the phylum numerator over the same denominator
/// [`VetReport::total_non_test_items`] already reports, so `checked_fraction_phylum` and
/// `checked_fraction`/`expressible_fraction` are directly comparable.
#[must_use]
pub fn phylum_checked_clean_items(
    dir: &Path,
    inputs: &[VetInput],
    summary: &PhylumVetSummary,
) -> usize {
    if !summary.ran {
        return 0; // a run with no verdict is never counted clean (G2/VR-5).
    }
    let clean_files: std::collections::BTreeSet<&str> = summary
        .nodules
        .iter()
        .filter(|n| n.is_clean())
        .map(|n| n.file.as_str())
        .collect();
    inputs
        .iter()
        .filter(|i| clean_files.contains(relative_slash_path(dir, &i.myc_path).as_str()))
        .map(|i| i.emitted_items)
        .sum()
}

/// One file's inputs to the vet loop: the emitted `.myc` to check, its originating source label, and
/// the per-file counts the metric bridges (from that file's [`GapReport`]).
#[derive(Debug, Clone)]
pub struct VetInput {
    pub myc_path: PathBuf,
    pub source_file: String,
    pub non_test_items: usize,
    pub emitted_items: usize,
}

impl VetInput {
    /// Construct from a written `.myc` path plus the file's [`GapReport`].
    pub fn from_report(myc_path: PathBuf, report: &GapReport) -> VetInput {
        VetInput {
            myc_path,
            source_file: report.source.clone(),
            non_test_items: report.non_test_item_count(),
            emitted_items: report.emitted_items.len(),
        }
    }
}

/// Vet a batch of emitted `.myc` files with `checker`, returning the aggregate [`VetReport`]. The
/// per-file `myc check` runs are independent; a tool-unavailable file is recorded, not fatal.
pub fn vet_batch(checker: &MycChecker, inputs: &[VetInput]) -> VetReport {
    let records = inputs
        .iter()
        .map(|i| {
            checker.vet_file(
                &i.myc_path,
                &i.source_file,
                i.non_test_items,
                i.emitted_items,
            )
        })
        .collect();
    VetReport::from_records(records)
}
