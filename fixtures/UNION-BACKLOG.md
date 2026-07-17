# Union Surface-Feature Backlog — mycelium-transpile over the 8 core-lib twins

**Guarantee: `Empirical`.** Every number below is measured by actually running
`mycelium-transpile`'s directory/batch mode (M-873 follow-on) over the Rust crates backing
6 of the 8 hand-written core-lib twins in `lib/std/*.myc`; percentages/counts are exact
arithmetic over that run, never estimated. See §Flagged for the 2 twins with no Rust
source to run — `std.option`/`std.result` were authored **directly in Mycelium**
(self-hosted, M-715/M-649), so there is no Rust crate to transpile for them; this is
noted rather than substituting an unrelated crate (VR-5/G2).

Regenerate: `mycelium-transpile <crate>/src <out-dir>` per crate below, then re-run the
aggregation that produced `union-backlog.json` (this file is derived from it).

## Per-crate expressibility

| Twin | Rust crate | Files | Non-test items | Emitted | Gaps | Expressible % |
|---|---|---:|---:|---:|---:|---:|
| `std.cmp` | `mycelium-std-cmp` | 1 | 111 | 14 | 102 | 12.6% |
| `std.iter` | `mycelium-std-iter` | 7 | 55 | 10 | 49 | 18.2% |
| `std.collections` | `mycelium-std-collections` | 6 | 31 | 10 | 30 | 32.3% |
| `std.text` | `mycelium-std-text` | 5 | 65 | 2 | 69 | 3.1% |
| `std.fmt` | `mycelium-std-fmt` | 1 | 32 | 0 | 34 | 0.0% |
| `std.math` | `mycelium-std-math` | 4 | 52 | 7 | 51 | 13.5% |
| **grand union (6 crates)** | — | — | **346** | **43** | **335** | **12.4%** |

## Flagged — no Rust source (self-hosted twins)

- **`std.option`** — no Rust source crate in this repo defines an Option type matching lib/std/option.myc's Option[A] (grep for `enum Option` across every crates/*/src/**/*.rs found zero matches, including mycelium-std-core). lib/std/option.myc's own header says "Self-hosted Option<A>" and traces to M-715 (E13-1, RFC-0031 §5 D4 Tier-0) — it was authored directly in Mycelium, not ported from a Rust prototype. Left out of the transpiled corpus rather than substituting an unrelated crate (VR-5/G2: flagged, not guessed).
- **`std.result`** — no Rust source crate in this repo defines a Result type matching lib/std/result.myc's Result[A, E] (same grep, zero matches). lib/std/result.myc traces to M-649 — issues.yaml titles it "Self-hosting Stage-2: first stdlib module written in Mycelium-lang L1 syntax", i.e. it was the first module authored directly in Mycelium, with no Rust backing crate to transpile. Left out of the transpiled corpus rather than substituting an unrelated crate (VR-5/G2: flagged, not guessed).

## Grand-union ranked gap categories (frequency-ordered)

The demand-grounded surface-feature backlog: what a Rust->Mycelium transpiler needs
next, ranked by how often it actually blocked something across the 6-crate corpus.

| Rank | Category | Count | Share of gaps | Representative reason pattern(s) |
|---:|---|---:|---:|---|
| 1 | Other | 121 | 36.1% | ×34: unsupported Rust type form `_`<br>×20: `_` — `_` only appears inside Dense{N,scalar}/ambient_params in the grammar, never as a bare value type; no confirmed base_type arm<br>×18: `_` declaration — Mycelium's nodule-per-file model has no nested-module construct in this grammar fragment |
| 2 | MacroInvocation | 68 | 20.3% | ×68: item-position macro invocation — no macro system in this grammar |
| 3 | GenericBound | 39 | 11.6% | ×16: impl block has generic parameter(s) — impl_item grammar has no generic-parameter declaration slot<br>×16: generic type path `_` — type-argument mapping not confirmed<br>×7: type parameter `_` carries a bound — type_params/fn generics are bare identifiers only in this grammar fragment |
| 4 | Impl | 37 | 11.0% | ×16: no member of this impl block could be transpiled (N sub-issue(s)): impl method `_` signature: unsupported Rust type form `_`<br>×10: impl target type `_` has no confirmed mapping (signed integer `_` — Binary{N} is documented unsigned-magnitude (lib/std/cmp.myc); mapping a signed type onto it would misrepresent twos-complement semantics, so this is left an explicit gap rather than guessed (VR-N))<br>×2: no member of this impl block could be transpiled (N sub-issue(s)): impl method `_` body: qualified/associated-function call `_` — no established Mycelium surface form for a Rust conversion-op body; emitting the bare last-segment name would fabricate a call (e.g. `_` is not a Mycelium builtin) |
| 5 | Struct | 23 | 6.9% | ×20: struct `_` uses named fields — no record/product-type surface (only a single-ctor positional shape maps to `_`)<br>×1: struct `_` has a field type with no confirmed mapping (`_`/`_` — `_` exists in base_type but is not confirmed equivalent to a UTF-N text type)<br>×1: struct `_` has a field type with no confirmed mapping (`_` has a platform-dependent width; no fixed Binary{N} mapping) |
| 6 | TestItem | 18 | 5.4% | ×18: #[cfg(test)] item — out of scope for this PoC's transpilation surface (excluded from the expressible-fraction denominator, but recorded, never silently skipped) |
| 7 | PayloadVariant | 11 | 3.3% | ×10: enum `_` variant `_` uses named fields — `_` has no named-field/record form<br>×1: enum `_` variant `_` has a field type with no confirmed mapping (`_`/`_` — `_` exists in base_type but is not confirmed equivalent to a UTF-N text type) |
| 8 | DeriveAttr | 8 | 2.4% | ×4: dropped non-doc attribute(s) on enum `_`: # [derive (Debug , Clone , Copy , PartialEq , Eq)]<br>×1: dropped non-doc attribute(s) on enum `_`: # [derive (Debug , Clone , Copy , PartialEq , Eq , Hash)]<br>×1: dropped non-doc attribute(s) on method `_`: # [must_use] |
| 9 | MacroDef | 5 | 1.5% | ×5: `_` definition — no macro system in this grammar |
| 10 | Trait | 5 | 1.5% | ×3: trait `_` method `_` signature has no confirmed mapping (a trait-body `_`/`_` has no concrete referent in this grammar; `_` parameter with no enclosing impl/trait context)<br>×2: trait `_` has supertrait bound(s) — trait_item grammar has no supertrait syntax (`_`) |

## Reading the backlog

- **`MacroInvocation`/`MacroDef`** (73 combined) — no macro system in the grammar; the
  single largest, single-cause bucket (mostly `impl_narrow_int!`/`impl_narrow_f64_to_int!`-
  style item-position macro invocations in `mycelium-std-cmp`). Out of scope for a
  transpiler PoC; would need a real macro system or a macro-expansion pre-pass (`syn`
  alone can't expand `macro_rules!`).
- **`Other`** (121) — a mixed bucket dominated by: unsupported Rust *type* forms (`String`,
  `usize`/`isize`, `char`, closures, references-of-references — everything `map::map_type`
  doesn't confirm a grammar row for), signed integers (no signed `Binary{N}` — ADR-028
  scoped `Binary` as sign-free, so this is a real semantic gap, not a transpiler
  shortcoming), `mod` declarations (no nested-module construct), and grouped `use`
  imports. The single biggest actionable item: a confirmed **`f32`/`f64` scalar surface**
  (`text`/`fmt`/`math` are float-heavy and mostly gap on this) would move the needle more
  than any other single grammar addition.
- **`GenericBound`** (39) — bounded generics (`T: Clone`, lifetimes, const generics,
  `where` clauses, impl-level generic parameters) have no `type_params` slot in this
  grammar fragment. A real generics story is the second-largest lever after floats.
- **`Impl`** (37) — mostly impl blocks that wholesale-gap because every method inside them
  hit one of the above (folded, not double-counted against the finer categories).
- **`Struct`/`PayloadVariant`** (34 combined) — named-field structs/enum variants have no
  record surface; only positional (`Fields::Unnamed`) shapes map today.
- **`TestItem`** (18) — `#[cfg(test)]` items, explicitly out of scope, excluded from the
  expressible-fraction denominator (not a real gap, listed for completeness/G2).
- **`DeriveAttr`** (8) — `#[derive(..)]` is always dropped (sub-gap on an otherwise-
  emitted item); no derive-macro equivalent exists to target.
- **`Trait`** (5) — default trait-method bodies and supertrait bounds have no surface.

## Conversion-op path (DN-41 `width_cast`, M-873 follow-on)

Not a gap category above (it's a positive result folded into `emitted`): the 10
unsigned-integer-chain `Widen<..>` impls in `mycelium-std-cmp` (`u8`->`u16`/`u32`/`u64`/
`u128`, `u16`->`u32`/`u64`/`u128`, `u32`->`u64`/`u128`, `u64`->`u128`) are now emitted
faithfully via the real DN-41 `width_cast` prim instead of gapping — see
`src/emit.rs::try_width_cast_widen_body`. A dedicated `Category::Conversion` gap path
exists for fallible `Narrow::narrow` bodies (`Result<To, NarrowError>`) and is covered by
synthetic unit tests. **Honest note (Empirical, VR-5):** across this 6-crate corpus
`Category::Conversion` has **zero** hits — the only concrete narrow, `impl Narrow<f32> for
f64` (`mycelium-std-cmp/src/lib.rs:675`), gaps *earlier* via the type-mapping path (`f32`/
`f64` have no confirmed `Binary{N}` mapping, so `self_ty_text` fails before the conversion
intercept is reached). So the conversion intercept is real code, not yet exercised by any
real crate here — it would fire once a concrete narrow between mappable `Binary` widths
appears. Reported, not overstated.
