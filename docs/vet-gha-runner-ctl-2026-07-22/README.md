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
| `checked_fraction` (emitted `.myc` that `myc check` accepts, file-gated) | **0 / 192 = 0.0%** |
| Files with any fully-clean emission | 0 / 2 |

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

## Reading the 0.0% honestly

`checked_fraction 0.0%` does **not** mean "0% portable." It means the *automatic*
transpiler emits nothing that type-checks unmodified, because idiomatic Rust here is
imperative, method-call-heavy, unit-returning, and string/struct-typed — none of which
the pure Mycelium value fragment expresses. Hand-porting the *pure* logic works: see the
checked, runnable `gha-runner-ctl/mycelium-port/`. The transpiler is a gap **instrument**,
and the gap it measures is the host-effect + imperative-surface distance captured in the
plan, not an algorithmic-complexity barrier.
