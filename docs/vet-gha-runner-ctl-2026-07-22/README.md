# `--vet` run: `gha-runner-ctl` @ Rust train v0.464.0 (2026-07-22)

A porting-readiness **measurement** of `tzervas/gha-runner-ctl` using
`mycelium-transpile --vet` (the Rust→Mycelium gap profiler — *not* a bulk porter).
Part of the `claude/mycelium-readiness-gaps` review. Full plan:
`mycelium-lang` `docs/planning/PORT-READINESS-2026-07-22.md`.

## How to reproduce

```bash
# with `myc` and `mycelium-transpile` on PATH (built from the components.lock pins)
mycelium-transpile --vet /path/to/gha-runner-ctl/src ./out
```

## Result

| Metric | Value |
|---|---|
| Top-level items (non-test) | 192 |
| `expressible_fraction` (some `.myc` draft emitted) | **32 / 192 = 16.7%** |
| `checked_fraction` — the `--vet` value (unreliable here†) | **not measured** (~~0 / 192~~) |
| `checked_fraction` — measured directly with `myc check` (file-gated) | **4 / 192 ≈ 2.1%** |
| Files with a fully-clean emission (`myc check`) | 1 / 2 — `pool.myc` clean; `lib.myc` fails |

† `--vet` shells out to `myc check` **as a cargo package (`mycelium-check`) in the Mycelium workspace**. This standalone setup has no such workspace, so every file returned `exit 101` (`"Cargo.toml"` / `"mycelium-check not found"`) and `--vet` recorded 0/192 — an **un-run** oracle, not a real "0% type-checks". The honest number above comes from dropping each emitted nodule into a `myc init` project and running `myc check`.

Gap categories (union over `src/`), most-common first:

| Category | Count |
|---|---:|
| `Other` — method call w/ no free-fn referent | 31 |
| `MultiStmtBody` | 38 |
| `Import` (`use`) | 22 |
| `Other` — non-unsigned top-level type | 14 |
| `Other` — unit-returning fn ("no unit value is representable") | 13 |
| `DeriveSatisfied` / `DeriveAttr` | 11 / 9 |
| `Struct` | 11 |
| `MacroInvocation` | 6 |
| `Impl` / `NamedFieldDrop` | 5 / 5 |

Raw artifacts in this directory: `summary.json` (per-file counts + category breakdown),
`vet.json` (checked_fraction detail), `union.gap.json` (every gap with file/line/reason),
`REMAP.md`.

## Reading the numbers honestly

The reliable figure is **`expressible_fraction` = 16.7%** (32/192 items emit some draft).
The `--vet` `checked_fraction` did **not** run here (see † above), so it is reported as
*not measured*, not 0%.

Measured directly, the emitted `pool.myc` draft **`myc check`s clean** — the transpiler's
output for that file is valid `.myc` — while `lib.myc` fails on a real type error (and
`main.myc` is empty), giving a file-gated **~2.1%**. So the automatic transpiler emits
**partial but sometimes-valid** drafts, not "nothing usable"; the ceiling is set by
idiomatic Rust the pure Mycelium value fragment can't express (method calls,
unit-returning/multi-statement bodies, imports, string/struct types — the gap table
above), not by an algorithmic barrier. Hand-porting the *pure* logic works cleanly: see the
checked, runnable `gha-runner-ctl/mycelium-port/`. Treat `mycelium-transpile` as a gap
**instrument**: trust `expressible_fraction` for coverage, and measure `checked_fraction`
inside the Mycelium workspace (where `mycelium-check` resolves) for a real validation number.
