# mycelium-transpile

<!-- FLEET-BADGES:BEGIN -->
[![CI](https://github.com/tzervas/mycelium-transpile/actions/workflows/fleet-ci.yml/badge.svg?branch=main)](https://github.com/tzervas/mycelium-transpile/actions/workflows/fleet-ci.yml?query=branch%3Amain)
[![Security](https://github.com/tzervas/mycelium-transpile/actions/workflows/fleet-security.yml/badge.svg?branch=main)](https://github.com/tzervas/mycelium-transpile/actions/workflows/fleet-security.yml?query=branch%3Amain)
[![Runner](https://img.shields.io/badge/runs--on-self--hosted%20podman-informational)](https://github.com/tzervas/gha-runner-ctl)
<!-- FLEET-BADGES:END -->


Component extracted from monorepo [`tzervas/mycelium`](https://github.com/tzervas/mycelium)
at archive tip `aad96b7a425710db5e91094d4fc2ca21a129e41a` (`archive/main-pre-component-transpile-2026-07-17`).

| Field | Value |
|---|---|
| **Program** | PROGRAM-SELFHOST-DECOMPOSE-2026-07-17 Phase D |
| **Source paths** | crates/mycelium-transpile |
| **License** | MIT |
| **Honesty** | Extract is mechanical copy from archive; not DN-88 production-ready dogfood; guarantee tags stay Declared/Empirical until differential upgrades |

## Build

MSRV 1.96.1. Path deps on sibling components may still point at monorepo-relative paths — wire git deps in a follow-up (FLAG).

```bash
cargo test
```
