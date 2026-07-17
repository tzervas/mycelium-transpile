//! Forward-map the **known kernel Π prim/API surface** into Rust `Expr::MethodCall` intrinsic
//! patterns, AHEAD of any backend that isn't fully wired yet (trx2 Lane C Deliverable 2).
//!
//! **VERIFY-FIRST (mitigation #14) — every row here traces to a checked citation, never a
//! doc-derived guess.** Two states, both never-silent (G2):
//!
//! - **`wired: true`** — the kernel prim is landed (confirmed in
//!   `crates/mycelium-l1/src/checkty.rs`'s `prim_kernel_name`/`prim_sig` tables) AND this crate
//!   independently confirmed the *exact emitted surface text* checks clean with the real built
//!   `target/debug/myc` (see each row's citation for the probe). [`emit_expr_inner`]'s
//!   `Expr::MethodCall` arm emits the real call for these.
//! - **`wired: false`** — a **PENDING-BACKEND** row: the mapping is *known* (a decided ADR/RFC/DN
//!   ruling), but the runtime/grammar backend is not landed yet. The emitter NEVER emits text for
//!   these — it always refuses with a structured [`crate::gap::GapReason`] citing the ruling
//!   (VR-5/G2: a forward-declared mapping is documentation + a precise gap, never a fabricated
//!   success).
//!
//! # What is deliberately **excluded** from this table (verify-first findings, not oversights)
//!
//! The kickoff brief's WIRED list also named `bit.mul` (`mul_u`) and `bit.popcount`/`bit.clz`/
//! `bit.ctz` (CU-1/CU-6) — both genuinely kernel-landed
//! (`docs/notes/DN-34-Rust-to-Mycelium-Transpiler-Strategy.md` §8.16: Π 59→66, PRs #1273/#1275/
//! #1291). They are **not** rows here because no Rust `Expr::MethodCall` pattern was found that is
//! *faithful* to their calling convention:
//! - `mul_u(a, b) -> Binary{N}` refuses (a runtime `Overflow`) rather than returning `Option` —
//!   Rust's semantically-closest never-silent method, `.checked_mul(rhs) -> Option<T>`, is a real
//!   corpus idiom (`crates/mycelium-std-math/src/exact.rs:273`), but its VALUE SHAPE does not match
//!   (`Option[Binary{N}]` vs bare `Binary{N}`) — mapping the isolated call node would emit a
//!   *type-mismatched* body wherever the `Option` is actually consumed (the realistic case). Rust's
//!   `.wrapping_mul()` is the wrong direction entirely (silently wraps — the G2 anti-pattern this
//!   whole project exists to avoid).
//! - `popcount`/`clz`/`ctz` are **width-preserving** (`Binary{N} -> Binary{N}`,
//!   `crates/mycelium-l1/src/checkty.rs:7164`), but Rust's `.count_ones()`/`.leading_zeros()`/
//!   `.trailing_zeros()` **always return a fixed `u32`** regardless of receiver width — so any
//!   *real, compiling* Rust source using them has an enclosing `u32`-typed context, which maps to
//!   `Binary{32}` and mismatches a `Binary{N}`-shaped body for every `N != 32`.
//!
//! Under `src/tests/vet.rs`'s **file-gated all-or-nothing** `myc check` classification
//! (`checked_clean_items_is_file_gated_all_or_nothing`), a wrongly-typed `wired: true` emission
//! does not just miss an opportunity — it can cost a whole file's `checked_fraction`. Both CU-1 and
//! CU-6 are already reachable through the sanctioned route instead: Deliverable 1's `Expr::Binary`
//! operand-type-gated rewrite (`&`/`|` -> `and`/`or`; DN-34 §8.16 item 4 names exactly this as
//! *the* transpiler-side path for these units). Forcing a second, unfaithful Call/MethodCall
//! binding here would be the "guessed API" VR-5 forbids — reported as a FLAG, not guessed.
//!
//! # L4 (DN-136 Phase-2, M-1100) — conversion-method mapping, verify-first findings
//!
//! **`clone` ADDED (identity, `wired: true`), narrowed post-hoc by the PR #1552 review
//! (CRITICAL soundness fix).** `Clone::clone`'s sole effect (per Rust's own contract) is producing
//! an owned copy with **no representation change** — in a value-semantic grammar (ADR-003) that is
//! exactly identity, **but only when the receiver's type cannot itself carry a user-written,
//! non-derived `impl Clone`** that does something other than a field-for-field copy (Rust allows
//! exactly that on any local type — the review's reproduced repro: a `struct Ticket{id,gen}` with a
//! hand-written `clone` that bumps `gen`; the original `ReceiverGate::AnyKnown` gate silently fired
//! on it, dropping the bump and reporting a clean success). The frozen [`crate::emit`]
//! call-emission format is always `"{myc_prim}({args})"` (`emit.rs`'s `visit_method_call`,
//! unchanged by this leaf); a row with `myc_prim: ""` therefore emits `"(recv)"` — a
//! **parenthesized-grouping** expression (`primary ::= ... | '(' expr ')'`,
//! `docs/spec/grammar/mycelium.ebnf:406`), confirmed `myc-check`-clean and semantically identical
//! to the bare receiver by direct probe against `target/debug/myc-check` for a `Binary{64}`,
//! `Bool`, and `Bytes` receiver (this leaf's own `src/tests/prim_map.rs` carries the committed
//! regression + a live-oracle witness covering exactly those three receiver types — `u64`/`bool`/
//! `String`). A user struct receiver and a `match`-block receiver expression were manually probed
//! against `target/debug/myc-check` during development (both also emit clean, unchanged, syntax)
//! but that probe is **not** committed as a live-oracle case, and is no longer this row's live
//! behavior anyway — see below. Gated on [`ReceiverGate::AnyBuiltinScalar`] (the receiver's mapped
//! type must be exactly `Bool`/`Bytes`/some concrete `Binary{N}` — i.e. a Rust *primitive* source
//! type, whose `Clone` impl is std's own fixed field-copy and categorically cannot be a user
//! override, per Rust's orphan rule) rather than [`ReceiverGate::AnyKnown`]: a user-named-type
//! receiver (a `struct`/`enum` that resolves but isn't a builtin) now **gaps** — never-silent
//! (G2), never a guessed identity — instead of silently assuming its `Clone` is trivial. See
//! `src/tests/prim_map.rs`'s `clone_on_user_named_type_receiver_never_fires_identity_and_gaps` for
//! the reviewer's exact repro as a regression (confirmed to fail under the pre-fix `AnyKnown`
//! gate, confirmed to pass under `AnyBuiltinScalar`).
//!
//! **`to_owned` ADDED (identity, `wired: true`), the flagged M-1100 residual closed** (this leaf).
//! Same semantic bucket as `clone`
//! (`crate::emit::is_unmappable_conversion_method`'s own doc comment groups both under
//! "ownership/representation identity"): `ToOwned::to_owned`'s value on a receiver whose mapped
//! type is a fixed builtin/primitive scalar is exactly the trait's blanket-`Clone` behavior
//! (`bool`/`u8..u128`/`i8..i128`/`usize`/`isize`/`char` all implement `Clone`, so
//! `to_owned(&self) -> T { self.clone() }` applies) or, for `str`/`String` (both mapping to the
//! builtin scalar `Bytes` — `crate::type_map::TABLE`'s `String`/`str` rows), std's own explicit
//! `impl ToOwned for str { type Owned = String; .. }` — in both cases an owned copy with **no
//! representation change** (ADR-003 value semantics), so the receiver passed through unchanged is
//! exact. Gated identically to `clone` — [`ReceiverGate::AnyBuiltinScalar`] — for the identical
//! reason: Rust's orphan rule forecloses a downstream `impl ToOwned` for any of these foreign
//! primitive types, so identity is sound **only** for this fixed set; a user-named-type receiver
//! still GAPS (never-silent, VR-5/G2), because `ToOwned`'s `Owned` associated type need not even
//! equal `Self` for a user impl (std's own `str -> String`/`[T] -> Vec<T>` show `Owned != Self` is
//! a normal, sanctioned shape), so assuming identity there would be an unchecked guess. The
//! previously-FLAGged blocker — the existing lock in
//! `src/tests/emit.rs::conversion_noop_method_gaps_never_fabricates_unknown_prim` (the "#72
//! co-poison fix") asserting `fn f(s: &str) -> String { s.to_owned() }` gaps the WHOLE function —
//! is resolved by updating that one test's now-stale bare-identifier-receiver case to assert
//! identity instead (its literal-receiver arm, `"a".to_owned()`, and its `.deref()` case are
//! untouched and still gap, since a string literal is not a bare-identifier receiver
//! `crate::emit::expr_env_type` can resolve, and `deref` is not a `TABLE` row).
//!
//! **`to_string` NOT ADDED (gap unchanged).** DN-127/DN-129 landed `impl Show[Binary{64}]`/
//! `Show[Bytes]`/`Show[Bool] for ...` with `render(x) => Bytes`, and `render(recv)` DOES
//! `myc-check`-clean (probed) — but `Show` is a **prelude trait seeded conditionally**: "iff some
//! nodule in the linked env declares an impl of it" (DN-129 §5). The primary `checked_fraction` vet
//! metric (`crate::vet`) runs the oracle in **single-file** mode; an isolated emitted `.myc` file
//! has no guarantee that a `Show` impl for the receiver's concrete type is in scope (no automatic
//! `use std.fmt;` import is emitted anywhere in this pipeline — that is an `emit.rs`-owned,
//! out-of-scope mechanism). Firing `render(recv)` unconditionally would risk an unresolved-trait
//! `myc check` failure the row could not predict — exactly the "never guess a real conversion"
//! instruction this leaf was given. Left gapped, unchanged.
//!
//! **`into` NOT ADDED (gap unchanged).** `Into::into`'s target type is determined by Rust's
//! bidirectional type inference from the *call site's expected type* (an assignment target, a
//! return position, …) — this `syn`-level, per-expression table has no expected-type context at
//! all (only [`crate::emit::TypeEnv`]'s *receiver*-type tracking), so "identity when source/target
//! coincide" is undecidable here without guessing the target. Left gapped, unchanged.
//!
//! CU-3 (float<->int conversion) is also excluded: DN-34 §8.16 records a *directional* ruling
//! ("prims for the total directions") but no confirmed prim **name**, and Rust's natural spelling
//! for a value conversion is the `as` cast (`syn::Expr::Cast`), which is out of this `Call`/
//! `MethodCall` table's scope regardless (`emit.rs`'s dedicated `Expr::Cast` arm handles casts, not
//! this table — see that arm's own doc comment). **Update (DN-51 §2 D3/§6, maintainer-authorized
//! DN-39 post-freeze promotion):** the *int-narrowing* sibling of "conversion"
//! (`Binary{N} as Binary{M}`, `M < N`) is now landed and emits `truncate` — but that fix lives
//! entirely in `emit.rs`'s `Expr::Cast` arm (mirroring how `Binary{N} as Binary{M}` widening
//! already emitted `width_cast` there, never via this table), so it does not add a row here. This
//! CU-3 exclusion is unchanged and still applies exactly to the **float-crossing** cast forms
//! (`Binary{N} as Float`, `Float as Binary{N}`, `Float as Float`), which remain
//! `PENDING-DESIGN(CU-3-fidelity)` — no faithful prim exists for those (ADR-040 §2.4's checked/
//! refusing CU-3 prims don't match Rust `as`'s rounding/saturating float semantics). CU-7
//! (arbitrary-width ternary) is excluded
//! because its natural Rust shape (`BigTernary::add/sub/mul/neg`,
//! `crates/mycelium-core/src/ternary/big_ternary.rs`) uses fully generic method names that collide
//! with an enormous space of unrelated Rust code (`.add()`/`.sub()`/`.mul()`/`.neg()` are common
//! method names on user types) — keying a table row on the bare name alone would misattribute
//! unrelated calls, and the target surface name is itself undecided (DN-34: "needs the
//! growable-`Repr::Ternary` decision"). CU-8 (atomics) and CU-9 (Dense dtype/quant) are excluded
//! per DN-34's own explicit rulings: CU-8 "needs a memory-model RFC ... mint a tracked issue + an
//! RFC stub; do **not** scope a partial stub" (an explicit no-half-measures instruction this row
//! would violate), and CU-9 "rides the E20-1 content-address rehash" (blocked on an unrelated
//! architectural decision, no shape decided at all). All five are reported as FLAGs in the leaf's
//! final report rather than forward-mapped as guesses.

use crate::gap::Category;

/// The gate on a `wired: true` row's **receiver** — required so a coincidentally-same-named method
/// on an unrelated Rust type never triggers a wrong emission (VR-5: never fire on an unconfirmed
/// operand type). Checked against the receiver's [`crate::emit::TypeEnv`] entry (only a bare
/// identifier already known in scope can ever match — never a guess).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverGate {
    /// The receiver's mapped type text must equal exactly this string (e.g. `"Float"`).
    Exact(&'static str),
    /// The receiver's mapped type text must be some concrete `Binary{N}` (any width).
    AnyBinaryWidth,
    /// The receiver's type must resolve at all (any concrete mapped type) — no further
    /// constraint. **CAUTION (post-hoc CRITICAL fix, PR #1552 review):** this gate fires on ANY
    /// resolvable receiver, including a **user-named type** — sound only for a row whose identity
    /// claim holds regardless of what code the type's author wrote (there is none such among this
    /// leaf's rows any more; see [`AnyBuiltinScalar`](ReceiverGate::AnyBuiltinScalar) for the
    /// gate `clone` actually needs). Kept in the enum for any future row that is genuinely
    /// receiver-type-independent; not used by any row in [`TABLE`] as of this fix.
    AnyKnown,
    /// The receiver's mapped type text must be one of the **fixed builtin/primitive** mapped
    /// types — `"Bool"`, `"Bytes"`, or some concrete `Binary{N}` — i.e. exactly the types
    /// `crate::type_map::TABLE` produces for a Rust *primitive* source type (`bool`, `u8..u128`/
    /// `i8..i128`/`usize`/`isize`/`char`, `String`/`str`). Added by the PR #1552 review fix
    /// (CRITICAL, `.clone()`-identity soundness hole): unlike [`AnyKnown`](ReceiverGate::AnyKnown),
    /// this gate **excludes any user-named type** — a bare passed-through identifier (a `struct`/
    /// `enum` name that fell through `type_map::lookup` to `map.rs`'s ordinary-named-type
    /// passthrough arm) never matches, because Rust's orphan rule lets a user crate write its own
    /// `impl Clone` for such a type (and often does, non-trivially — see `clone`'s row doc). A
    /// receiver mapped from an actual Rust primitive can **never** carry a user `impl Clone`
    /// (orphan rule: `bool`/`u8`../`String`/… are foreign types whose `Clone` impl is std's own,
    /// fixed, field-copy behavior) — so identity is sound for exactly this set, never a guess.
    AnyBuiltinScalar,
}

/// One forward-mapped Rust `Expr::MethodCall` pattern (see module docs).
#[derive(Debug, Clone, Copy)]
pub struct PrimMapping {
    /// The Rust method name this row matches (`syn::ExprMethodCall::method`).
    pub rust_method: &'static str,
    /// The Mycelium prim/surface call name — a bare, no-import-needed identifier when `wired`; a
    /// forward-declared (not-yet-real) name otherwise (still cited, never fabricated wholesale —
    /// see each row's `citation`). **Identity-conversion convention (L4, DN-136 Phase-2,
    /// M-1100):** the empty string `""` is a deliberate, documented sentinel — the emitter's fixed
    /// `"{myc_prim}({args})"` format then renders `"(recv)"`, a parenthesized-grouping expression
    /// (grammar `primary`), which is the receiver **unchanged** (confirmed `myc-check`-clean by
    /// direct probe; see the module-doc L4 section). Used only by `wired: true` rows whose whole
    /// semantic content is "identity" — never a fabricated bare identifier.
    pub myc_prim: &'static str,
    /// Kernel backend landed? `true` -> emit the real call; `false` -> always refuse
    /// (PENDING-BACKEND, never emitted).
    pub wired: bool,
    /// The operand-type gate gating whether this row applies at a given call site.
    pub receiver_gate: ReceiverGate,
    /// When `wired` and the prim's own return is `Binary{1}` where Rust's method returns `bool`
    /// (e.g. `flt_is_nan`), bridge it via the Deliverable-1-proven
    /// `(match <call> { 0b1 => True, _ => False })` composition rather than a bare call (a bare
    /// `Binary{1}` value fails `myc check` against a `Bool`-typed context — confirmed empirically,
    /// same class of gap as `eq`/`lt`'s `Binary{1}` result in `emit.rs`'s `Expr::Binary` docs).
    pub bridge_binary1_to_bool: bool,
    /// The gap category to file a PENDING-BACKEND refusal under (irrelevant for `wired: true`
    /// rows, which never gap).
    pub pending_category: Category,
    /// The `<M-id or slug>` used in the `PENDING-BACKEND(<slug>)` annotation / gap reason, and the
    /// human citation trail backing this row's mapping decision.
    pub slug: &'static str,
    pub citation: &'static str,
}

/// The table. Order is insertion order; [`lookup`] does a linear scan (small, fixed table — no
/// need for a map).
///
/// **WIRED rows (CU-2, ADR-040 §2.5 / `checkty.rs:7325-7327` / DN-34 §8.16 "CU-2 ... landed
/// #1274"):** `flt_is_nan`/`flt_is_finite`/`flt_is_infinite` are confirmed bare-call prims (no
/// import) whose Rust intrinsics (`f64::is_nan`/`is_finite`/`is_infinite`) are real, attested
/// corpus usage (`crates/mycelium-std-math/src/approx.rs`, `exact.rs`). Verified `myc check`-clean
/// with the receiver typed `Float` and the `Binary{1}`->`Bool` bridge applied (probed against
/// `target/debug/myc` — `fn f(x: Float) => Bool = (match flt_is_nan(x) { 0b1 => True, _ => False
/// });` checks clean; see this crate's `src/tests/prim_map.rs` for the committed regression).
/// Requires `crate::map::map_type`'s companion fix (this leaf) mapping Rust `f64` -> the grammar's
/// real nullary `Float` base_type (`docs/spec/grammar/mycelium.ebnf:251`, ADR-040 FLAG-1/M-897) —
/// without that fix these rows are reachable in the table but never actually applicable (no `Float`
/// receiver can ever appear in `env`).
///
/// **PENDING-BACKEND rows (CU-5, RFC-0034 §10 / M-791 / DN-34 §8.16 item 2):** the named
/// `wrapping` construct is a **decided** ruling ("implement the M-791 named construct, no new
/// `wrapping_*` prims... wire the construct to modular evaluation over `bin.add`/`sub`/`mul`"), but
/// has **no grammar surface at all yet** (confirmed: `wrapping` does not appear anywhere in
/// `docs/spec/grammar/mycelium.ebnf`) and no wired runtime evaluation path — per
/// `crates/mycelium-core/src/wrapping.rs`'s module doc, the op-layer wiring (arithmetic/swap
/// operations that actually honor the `WrappingOpt` marker) is a downstream task, and "the op
/// layer is wired once arithmetic/swap operations exist". Gated on the receiver being a known
/// `Binary{N}` (any width) so an unrelated user type's `.wrapping_add()`-named method never
/// produces a misleading citation.
pub const TABLE: &[PrimMapping] = &[
    PrimMapping {
        rust_method: "is_nan",
        myc_prim: "flt_is_nan",
        wired: true,
        receiver_gate: ReceiverGate::Exact("Float"),
        bridge_binary1_to_bool: true,
        pending_category: Category::Other,
        slug: "CU-2",
        citation: "ADR-040 §2.5; checkty.rs:7325 (\"flt_is_nan\" => \"flt.is_nan\"); DN-34 §8.16 \
                   (landed #1274); f64::is_nan real corpus usage \
                   crates/mycelium-std-math/src/approx.rs",
    },
    PrimMapping {
        rust_method: "is_finite",
        myc_prim: "flt_is_finite",
        wired: true,
        receiver_gate: ReceiverGate::Exact("Float"),
        bridge_binary1_to_bool: true,
        pending_category: Category::Other,
        slug: "CU-2",
        citation: "ADR-040 §2.5; checkty.rs:7326 (\"flt_is_finite\" => \"flt.is_finite\"); DN-34 \
                   §8.16 (landed #1274); f64::is_finite real corpus usage \
                   crates/mycelium-std-math/src/approx.rs",
    },
    PrimMapping {
        rust_method: "is_infinite",
        myc_prim: "flt_is_infinite",
        wired: true,
        receiver_gate: ReceiverGate::Exact("Float"),
        bridge_binary1_to_bool: true,
        pending_category: Category::Other,
        slug: "CU-2",
        citation: "ADR-040 §2.5; checkty.rs:7327 (\"flt_is_infinite\" => \"flt.is_infinite\"); \
                   DN-34 §8.16 (landed #1274); f64::is_infinite real corpus usage \
                   crates/mycelium-std-math/src/approx.rs",
    },
    PrimMapping {
        rust_method: "wrapping_add",
        myc_prim: "wrapping(bin.add) [name TBD]",
        wired: false,
        receiver_gate: ReceiverGate::AnyBinaryWidth,
        bridge_binary1_to_bool: false,
        pending_category: Category::Conversion,
        slug: "CU-5",
        citation: "RFC-0034 §10; M-791; DN-34 §8.16 item 2 (\"implement the M-791 named \
                   construct, no new wrapping_* prims ... wire the construct to modular \
                   evaluation over bin.add/sub/mul\"); no `wrapping` token in \
                   docs/spec/grammar/mycelium.ebnf (grammar surface unwired) and no wired \
                   runtime evaluation path per crates/mycelium-core/src/wrapping.rs's module \
                   doc (op-layer wiring is a downstream task; \"the op layer is wired once \
                   arithmetic/swap operations exist\")",
    },
    PrimMapping {
        rust_method: "wrapping_sub",
        myc_prim: "wrapping(bin.sub) [name TBD]",
        wired: false,
        receiver_gate: ReceiverGate::AnyBinaryWidth,
        bridge_binary1_to_bool: false,
        pending_category: Category::Conversion,
        slug: "CU-5",
        citation: "RFC-0034 §10; M-791; DN-34 §8.16 item 2; see wrapping_add's citation \
                   (identical basis)",
    },
    PrimMapping {
        rust_method: "wrapping_mul",
        myc_prim: "wrapping(bin.mul) [name TBD]",
        wired: false,
        receiver_gate: ReceiverGate::AnyBinaryWidth,
        bridge_binary1_to_bool: false,
        pending_category: Category::Conversion,
        slug: "CU-5",
        citation: "RFC-0034 §10; M-791; DN-34 §8.16 item 2; see wrapping_add's citation \
                   (identical basis)",
    },
    // L4 (DN-136 Phase-2, M-1100) — conversion-method mapping. `myc_prim: ""` is the documented
    // identity-emission sentinel (see this row's own field doc + the module-doc L4 section):
    // `Clone::clone`'s sole effect is an owned copy with no representation change (value
    // semantics, ADR-003), so the receiver passed through unchanged (via a parenthesized-grouping
    // `(recv)`) is exact, never a guess. `to_owned` (below) is the same identity class, added by a
    // follow-on leaf closing the M-1100 residual FLAG. `to_string`/`into` are deliberately NOT rows
    // here — see the module-doc L4 section for each one's own verify-first finding.
    PrimMapping {
        rust_method: "clone",
        myc_prim: "",
        wired: true,
        // CRITICAL fix (PR #1552 review, post-hoc): was `ReceiverGate::AnyKnown`, which fires on
        // ANY resolvable receiver INCLUDING a user-named type with a hand-written, non-derived
        // `impl Clone` (repro: `struct Ticket{id,gen}` + a custom `clone` that bumps `gen` --
        // `AnyKnown` silently emitted `bump`'s `t.clone()` as bare `(t)`, dropping the `+1`, as a
        // clean success). `Clone::clone`'s "no representation change" claim is only true when the
        // type CANNOT carry a user override -- i.e. a fixed builtin/primitive mapped type, never a
        // user-named one (Rust's orphan rule is exactly what makes a foreign primitive's `Clone`
        // impl non-overridable; a local struct/enum has no such guarantee). Narrowed to
        // `AnyBuiltinScalar`; a user-named-type receiver now GAPS (never-silent, VR-5/G2) instead
        // of silently assuming identity. See `src/tests/prim_map.rs`'s
        // `clone_on_user_named_type_receiver_never_fires_identity_and_gaps` regression (the
        // reviewer's exact repro, confirmed to FAIL under the pre-fix `AnyKnown` gate).
        receiver_gate: ReceiverGate::AnyBuiltinScalar,
        bridge_binary1_to_bool: false,
        pending_category: Category::Other,
        slug: "M-1100",
        citation: "this leaf's L4 lane (DN-136 Phase-2 worklist, M-1100) narrowed by the PR #1552 \
                   review fix; ADR-003 (value semantics — no reference/ownership distinction); \
                   Clone::clone's own contract (\"returns a duplicate of the value\", no \
                   representation change) — sound here only because `AnyBuiltinScalar` restricts \
                   the receiver to a fixed builtin/primitive mapped type (`Bool`/`Bytes`/some \
                   `Binary{N}`), which Rust's orphan rule guarantees can never carry a \
                   user-written `impl Clone` (a user-named-type receiver GAPS instead — see the \
                   gate's own doc); grammar primary ::= ... | '(' expr ')' \
                   (docs/spec/grammar/mycelium.ebnf:406); confirmed myc-check-clean by direct \
                   probe against target/debug/myc-check for a Binary{64}/Bool/Bytes receiver (see \
                   src/tests/prim_map.rs's committed regression + live-oracle witness — the \
                   witness covers exactly u64/bool/String, matching this gate's scope; a prior \
                   claim of user-struct/match-block live-oracle coverage was inaccurate and is \
                   corrected here, PR #1552 review MEDIUM finding)",
    },
    // Follow-on leaf to L4 (M-1100 residual, flagged by the `clone` leaf/PR #1552): closes the
    // `to_owned` gap the same `AnyBuiltinScalar`-gated identity way as `clone` above (see this
    // row's own citation + the module-doc L4 section for the full soundness argument).
    PrimMapping {
        rust_method: "to_owned",
        myc_prim: "",
        wired: true,
        // Same soundness basis + same CRITICAL-class caution as `clone`'s gate: a user-named-type
        // receiver's `ToOwned` impl is not foreclosed by the orphan rule (only a *foreign* type's
        // impl is), and its `Owned` associated type need not equal `Self` at all (std's own
        // `str -> String`/`[T] -> Vec<T>` are exactly this shape) — so `AnyBuiltinScalar`, never
        // `AnyKnown`, is required here from the start (no post-hoc narrowing needed, unlike
        // `clone`, because this row is added after that CRITICAL finding). See
        // `src/tests/prim_map.rs`'s `to_owned_on_user_named_type_receiver_never_fires_identity_and_gaps`
        // for the direct regression.
        receiver_gate: ReceiverGate::AnyBuiltinScalar,
        bridge_binary1_to_bool: false,
        pending_category: Category::Other,
        slug: "M-1100",
        citation: "M-1100 residual closed (flagged by the `clone` leaf/PR #1552 review); ADR-003 \
                   (value semantics — no reference/ownership distinction); ToOwned::to_owned's \
                   contract for a `Clone` receiver (`bool`/`u8..u128`/`i8..i128`/`usize`/`isize`/\
                   `char` — the blanket `impl<T: Clone> ToOwned for T`) and for `str`/`String` \
                   (std's own explicit `impl ToOwned for str { type Owned = String; .. }`, both \
                   mapping to the builtin scalar `Bytes` per `crate::type_map::TABLE`'s \
                   `String`/`str` rows) — both an owned copy with no representation change; sound \
                   here only because `AnyBuiltinScalar` restricts the receiver to a fixed \
                   builtin/primitive mapped type (`Bool`/`Bytes`/some `Binary{N}`), which Rust's \
                   orphan rule guarantees can never carry a user-written `impl ToOwned` (a \
                   user-named-type receiver GAPS instead — see the gate's own doc); grammar \
                   primary ::= ... | '(' expr ')' (docs/spec/grammar/mycelium.ebnf:406); confirmed \
                   myc-check-clean by direct probe against target/debug/myc-check for a \
                   Binary{64}/Bool/Bytes receiver (see src/tests/prim_map.rs's committed \
                   regression + live-oracle witness)",
    },
];

/// Look up `rust_method` in [`TABLE`] (first match; the table has no duplicate `rust_method`
/// entries by construction). `None` for any method name this table doesn't cover — the caller's
/// existing (unchanged) generic method-call desugar applies.
#[must_use]
pub fn lookup(rust_method: &str) -> Option<&'static PrimMapping> {
    TABLE.iter().find(|row| row.rust_method == rust_method)
}

/// Whether `receiver_ty` (a resolved [`crate::emit::TypeEnv`] entry, when the receiver is a known
/// bare identifier — see [`crate::emit::expr_env_type`]) satisfies `gate`.
#[must_use]
pub fn receiver_gate_matches(gate: ReceiverGate, receiver_ty: Option<&str>) -> bool {
    match (gate, receiver_ty) {
        (ReceiverGate::Exact(want), Some(got)) => want == got,
        (ReceiverGate::AnyBinaryWidth, Some(got)) => crate::emit::binary_width(got).is_some(),
        (ReceiverGate::AnyKnown, Some(_)) => true,
        (ReceiverGate::AnyBuiltinScalar, Some(got)) => {
            got == "Bool" || got == "Bytes" || crate::emit::binary_width(got).is_some()
        }
        (_, None) => false,
    }
}
