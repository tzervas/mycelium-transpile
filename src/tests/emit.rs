//! Unit tests for the `.myc` emitter, over a small fixture corpus (data-driven — per CLAUDE.md
//! "Complex test logic lives in fixtures + parameterization, not in test bodies").

use crate::emit::{emit_expr, TypeEnv};
use crate::gap::Category;
use crate::transpile::transpile_source;

/// The expected outcome for one fixture.
enum Expect {
    /// The item is emitted, and the `.myc` text contains this substring.
    Emitted {
        item: &'static str,
        contains: &'static str,
    },
    /// The item is not emitted at all, and at least one gap of this category is recorded.
    Gapped { category: Category },
    /// The item is emitted (containing the substring) AND at least one sub-gap of the given
    /// category is also recorded for it (e.g. a dropped `#[derive(..)]`).
    EmittedAndGapped {
        item: &'static str,
        contains: &'static str,
        sub_gap_category: Category,
    },
}

struct Case {
    name: &'static str,
    rust: &'static str,
    expect: Expect,
}

/// The fixture corpus. Each row cites the grammar production it exercises.
fn cases() -> Vec<Case> {
    vec![
        // `type_item`: C-like enum -> a sum type (grammar §type_item/constructor).
        Case {
            name: "c_like_enum",
            rust: "enum Ordering { Less, Equal, Greater }",
            expect: Expect::Emitted {
                item: "Ordering",
                contains: "type Ordering = Less | Equal | Greater;",
            },
        },
        // `fn_item`: a single-expression body (grammar §fn_item).
        Case {
            name: "simple_fn",
            rust: "fn is_lt(o: bool) -> bool { o }",
            expect: Expect::Emitted {
                item: "is_lt",
                contains: "fn is_lt(o: Bool) => Bool = o;",
            },
        },
        // `match_expr` over bool literal patterns (grammar §match_expr/pattern).
        Case {
            name: "match_expr",
            rust: "fn pick(o: bool) -> bool { match o { true => false, false => true } }",
            expect: Expect::Emitted {
                item: "pick",
                contains: "match o { True => False, False => True }",
            },
        },
        // A `let`-chain + tail expr desugars to nested `let ... in ...` (still a single
        // `fn_item` body expression).
        Case {
            name: "let_chain_body",
            rust: "fn double(x: bool) -> bool { let y = x; y }",
            expect: Expect::Emitted {
                item: "double",
                contains: "let y = x in y",
            },
        },
        // Tuple-variant enum: positional fields map via `constructor`'s optional field list.
        Case {
            name: "tuple_variant_enum",
            rust: "enum Foo { A(u8), B }",
            expect: Expect::Emitted {
                item: "Foo",
                contains: "type Foo = A(Binary{8}) | B;",
            },
        },
        // A tuple struct maps to a single-constructor `type_item`.
        Case {
            name: "tuple_struct",
            rust: "struct Bf16Bits(u16);",
            expect: Expect::Emitted {
                item: "Bf16Bits",
                contains: "type Bf16Bits = Bf16Bits(Binary{16});",
            },
        },
        // KNOWN HARD GAP: `trait` — every realistic trait in the target crate gaps (default
        // bodies, supertraits, or an unresolvable `Self`); this fixture exercises the
        // unresolvable-`self` path specifically (no default body, no supertrait).
        Case {
            name: "trait_self_unresolvable",
            rust: "trait Foo { fn bar(&self) -> bool; }",
            expect: Expect::Gapped {
                category: Category::Trait,
            },
        },
        // KNOWN HARD GAP: `macro_rules!` definitions — no macro system in the grammar.
        Case {
            name: "macro_rules_gap",
            rust: "macro_rules! foo { () => {}; }",
            expect: Expect::Gapped {
                category: Category::MacroDef,
            },
        },
        // Item-position macro invocations are a distinct category from macro *definitions*.
        Case {
            name: "macro_invocation_gap",
            rust: "some_macro!(a, b, c);",
            expect: Expect::Gapped {
                category: Category::MacroInvocation,
            },
        },
        // M-1006 (E33-1): a named-field ("record") struct whose fields all resolve in-file now emits
        // POSITIONALLY (field names dropped + recorded via a `NamedFieldDrop` sub-gap) — the
        // grammar-grounded mapping the `lib/std/*.myc` hand-ports use (`type GuaranteeRow = Row(..)`).
        Case {
            name: "struct_named_fields_emits_positionally",
            rust: "struct Foo { x: u8, y: bool }",
            expect: Expect::EmittedAndGapped {
                item: "Foo",
                contains: "type Foo = Foo(Binary{8}, Bool)",
                sub_gap_category: Category::NamedFieldDrop,
            },
        },
        // M-1006 §8.14: a named-field struct with a `String` field now EMITS — `String` maps to
        // `Bytes` (RFC-0033 §3.2), so the record is fully mappable and emits positionally.
        Case {
            name: "struct_named_field_string_maps_to_bytes",
            rust: "struct WithText { s: String, n: u32 }",
            expect: Expect::EmittedAndGapped {
                item: "WithText",
                contains: "type WithText = WithText(Bytes, Binary{32})",
                sub_gap_category: Category::NamedFieldDrop,
            },
        },
        // M-1006: a named-field struct with an UNMAPPABLE field type (`f32`) still gaps — the
        // field's own precise repr reason wins (mapped *before* the resolvability gate), so the gap
        // profile keeps "unmappable field" distinct from "out-of-file reference". (P4/P5, DN-99 §8
        // ENB-6: `char` itself now maps to `Binary{32}`, so this fixture moved to `f32` — still
        // genuinely unmapped, `Float` being binary64-only — to keep exercising the still-gapped
        // case.)
        Case {
            name: "struct_named_field_unmappable_type_still_gaps",
            rust: "struct Bad { c: f32 }",
            expect: Expect::Gapped {
                category: Category::Struct,
            },
        },
        // M-1006 resolvability gate: a named-field struct whose fields all MAP but reference a type
        // not declared in this file (`Elsewhere`) is gated — emitting it would introduce an
        // unresolved reference that poisons the file's `myc check`. Left an honest `Struct` gap.
        Case {
            name: "struct_named_field_out_of_file_ref_is_gated",
            rust: "struct Ref { h: Elsewhere }",
            expect: Expect::Gapped {
                category: Category::Struct,
            },
        },
        // M-1006 greatest-fixpoint: mutually-recursive named-field structs (`A` <-> `B`) resolve as a
        // group and emit — a *least* fixpoint would wrongly gate both (each waits on the other). Both
        // are declared in-file and reference only each other + builtins, so the cycle is resolvable.
        Case {
            name: "mutually_recursive_named_structs_resolve",
            rust: "struct A { b: B, x: u8 }\nstruct B { a: A }",
            expect: Expect::EmittedAndGapped {
                item: "A",
                contains: "type A = A(B, Binary{8})",
                sub_gap_category: Category::NamedFieldDrop,
            },
        },
        // M-1006 Lever 1: a `self.<field>` projection in an impl body desugars to a `match` on the
        // struct's single (positional) constructor — the faithful equivalent (no projection surface in
        // the grammar). `Perm` is resolvable (its ctor emits), so the projection is gated ON.
        Case {
            name: "field_projection_desugars_to_match",
            rust: "struct Perm { mode: u8 }\nimpl Perm { fn get(self) -> u8 { self.mode } }",
            expect: Expect::Emitted {
                item: "impl Perm",
                contains: "match self { Perm(p0) => p0 }",
            },
        },
        // M-1006 Lever 1: struct-literal construction `Foo { mode: a }` -> the positional ctor call
        // `Foo(a)` (fields ordered by declaration). `Self { .. }` resolves the same way in impl context.
        Case {
            name: "struct_literal_construction_emits_positional_ctor",
            rust: "struct Foo { mode: u8 }\nfn mk(a: u8) -> Foo { Foo { mode: a } }",
            expect: Expect::Emitted {
                item: "mk",
                contains: "Foo(a)",
            },
        },
        // M-1006 Lever 1 gate: a field access on a NON-`self` base gaps — the transpiler tracks no
        // local types, so it cannot resolve the projection to a constructor position (never a guess).
        // (No struct is declared here, so the sole item is the gapping `peek`.)
        Case {
            name: "field_access_on_non_self_base_gaps",
            rust: "fn peek(f: u8) -> u8 { f.mode }",
            expect: Expect::Gapped {
                category: Category::Other,
            },
        },
        // M-873 follow-on (DN-41): a numeric-widening `impl Widen<..> for ..` whose body is a
        // qualified associated-function call (`u16::from(self)`, the real shape of Rust's
        // widening bodies in `mycelium-std-cmp`) must never be emitted with the *fabricated*
        // `from(self)` text (`from` is not a Mycelium builtin — no grammar production; only prose
        // mentions in `docs/spec/grammar/mycelium.ebnf`). Once both `Self`/target map to
        // `Binary{N}`/`Binary{M}` (unsigned widening), it is now instead emitted **faithfully**
        // via the real DN-41 `width_cast` prim — a strict improvement over the earlier "gap the
        // whole impl" behavior this case originally pinned (see
        // `widen_impls_never_fabricate_from_in_real_crate` in `src/tests/diff.rs` for the
        // real-crate-scale version of this guard).
        Case {
            name: "widen_binary_emits_width_cast",
            rust: "impl Widen<u16> for u8 { fn widen(self) -> u16 { u16::from(self) } }",
            expect: Expect::Emitted {
                item: "widen_free Binary{8} -> Binary{16}",
                contains: "width_cast(self, 0b0000_0000_0000_0000)",
            },
        },
        // Widen over a non-`Binary` `Self` (e.g. `bool`) has no `width_cast` witness path (`Self`
        // doesn't map to `Binary{N}` at all) — the qualified `u32::from(self)` call stays an
        // honest gap, unchanged from the pre-DN-41 behavior.
        Case {
            name: "widen_bool_from_call_still_gapped_not_fabricated",
            rust: "impl Widen<u32> for bool { fn widen(self) -> u32 { u32::from(self) } }",
            expect: Expect::Gapped {
                category: Category::Impl,
            },
        },
        // DN-41 §2: `Narrow::narrow` is fallible (`Result<To, NarrowError>`) — no `= expr
        // fn_item` body can express a Result-returning refuse, so it stays an explicit,
        // DN-41-cited gap rather than a forced/fabricated emission.
        Case {
            name: "narrow_gapped_cites_dn41",
            rust: "impl Narrow<u8> for u16 { fn narrow(self) -> Result<u8, NarrowError> { u8::try_from(self) } }",
            expect: Expect::Gapped {
                category: Category::Impl,
            },
        },
        // KNOWN HARD GAP: multi-statement fn body (an interior statement that is neither a
        // simple `let` nor the trailing expression).
        Case {
            name: "multi_stmt_body_gap",
            rust: "fn foo(x: bool) -> bool { let y = x; println!(\"{}\", 1); y }",
            expect: Expect::Gapped {
                category: Category::MultiStmtBody,
            },
        },
        // A string literal maps to a `StrLit` (grammar line 414/430; M-910/M-911) — reachable in
        // an emittable body as a call argument (its type is inferred, not named). The Rust `\n`
        // decodes to a raw newline which is re-escaped back to `\n` in the emitted StrLit.
        Case {
            name: "string_literal_arg_emits_strlit",
            rust: "fn f(x: u8) -> u8 { g(x, \"hi\\n\") }",
            expect: Expect::Emitted {
                item: "f",
                contains: "g(x, \"hi\\n\")",
            },
        },
        // A float literal maps to a `FloatLit` (grammar line 414/443; ADR-040/M-897) when its
        // digit string is a well-formed, finite FloatLit — reachable as a call argument.
        Case {
            name: "float_literal_arg_emits_floatlit",
            rust: "fn f(x: u8) -> u8 { g(x, 1.5) }",
            expect: Expect::Emitted {
                item: "f",
                contains: "g(x, 1.5)",
            },
        },
        // An exponent-form float likewise maps (`syn` normalizes `E`→`e`, drops the `+`).
        Case {
            name: "float_exponent_arg_emits_floatlit",
            rust: "fn f(x: u8) -> u8 { g(x, 2.5E+3) }",
            expect: Expect::Emitted {
                item: "f",
                contains: "g(x, 2.5e3)",
            },
        },
        // An explicit-element array maps to a `ListLit` (grammar line 415; RFC-0032 D3) —
        // reachable as a call argument.
        Case {
            name: "array_literal_arg_emits_listlit",
            rust: "fn f(x: u8) -> u8 { g(x, [x, x]) }",
            expect: Expect::Emitted {
                item: "f",
                contains: "g(x, [x, x])",
            },
        },
        // KNOWN HARD GAP: a string literal carrying a control char with no Mycelium escape
        // (`\x07` bell) — StrLit has no `\xNN` form, so it is never-silently gapped, never emitted
        // as a raw byte (G2/VR-5).
        Case {
            name: "string_control_char_gapped",
            rust: "fn f(x: u8) -> u8 { g(x, \"\\x07\") }",
            expect: Expect::Gapped {
                category: Category::Other,
            },
        },
        // KNOWN HARD GAP: a Rust-only float shape (trailing-dot `2.` → digit string "2.", empty
        // fraction) has no faithful Mycelium FloatLit spelling — gapped rather than reshaped (VR-5).
        Case {
            name: "float_trailing_dot_gapped",
            rust: "fn f(x: u8) -> u8 { g(x, 2.) }",
            expect: Expect::Gapped {
                category: Category::Other,
            },
        },
        // KNOWN HARD GAP: a well-shaped float whose value is not finite binary64 (`1e999` → +inf)
        // — a literal is a conversion boundary, so out-of-range is a never-silent refuse, never a
        // silent ±inf (ADR-040 §2.4).
        Case {
            name: "float_non_finite_gapped",
            rust: "fn f(x: u8) -> u8 { g(x, 1e999) }",
            expect: Expect::Gapped {
                category: Category::Other,
            },
        },
        // KNOWN HARD GAP: an array-repeat `[x; N]` — `ListLit` has no repeat form.
        Case {
            name: "array_repeat_gapped",
            rust: "fn f(x: u8) -> u8 { g(x, [x; 4]) }",
            expect: Expect::Gapped {
                category: Category::Other,
            },
        },
        // A bounded generic type parameter has no bare-identifier `type_params` mapping (`fn`
        // generics are unaffected by DN-131 — that note lifts the refusal on the impl slot
        // only, `plain_type_params` still refuses any bound here).
        Case {
            name: "generic_bound_gap",
            rust: "fn foo<T: Clone>(x: T) -> T { x }",
            expect: Expect::Gapped {
                category: Category::GenericBound,
            },
        },
        // DN-131 (Accepted; M-1088/M-1101) — a plain non-generic inherent impl is the
        // overwhelmingly common case; the regression guard that this leaf's restructuring of
        // `emit_impl` leaves its output byte-identical when there are no impl-level generic
        // parameters at all (`impl_type_params` is empty, so `impl{}` + `" "` == the pre-leaf
        // `"impl "` text exactly).
        Case {
            name: "impl_no_generics_unchanged",
            rust: "impl Bx { fn dup(self) -> Bx { self } }",
            expect: Expect::Emitted {
                item: "impl Bx",
                contains: "impl Bx {\n",
            },
        },
        // DN-131 — an UNBOUNDED impl-level type parameter (`impl<T> Bx<T>`, DN-103's own slot)
        // now emits: `bounded_impl_type_params` returns the bare identifier `"T"` when a
        // parameter carries no inline bound, exactly the DN-103 backward-compatible identity
        // case DN-131 §3 names ("an unbounded `impl[T] Foo[T]` yields `bounds: []`").
        Case {
            name: "impl_unbounded_generic_emits",
            rust: "impl<T> Bx<T> { fn dup(self) -> Bx<T> { self } }",
            expect: Expect::Emitted {
                item: "impl[T] Bx[T]",
                contains: "impl[T] Bx[T] {",
            },
        },
        // DN-131 — a BOUNDED impl-level type parameter now emits the bound verbatim into the
        // impl slot's own `[T: Bound]` text (the leaf's headline capability).
        Case {
            name: "impl_bounded_generic_emits",
            rust: "impl<T: Clone> Bx<T> { fn dup(self) -> Bx<T> { self } }",
            expect: Expect::Emitted {
                item: "impl[T: Clone] Bx[T]",
                contains: "impl[T: Clone] Bx[T] {",
            },
        },
        // DN-131 §3's multi-bound surface (`bound ::= Ident type_args? ('+' Ident
        // type_args?)*`, reusing the landed `parse_bound` grammar) — two plain trait-name
        // bounds joined by `+`.
        Case {
            name: "impl_multi_bound_generic_emits",
            rust: "impl<T: Clone + Copy> Bx<T> { fn dup(self) -> Bx<T> { self } }",
            expect: Expect::Emitted {
                item: "impl[T: Clone + Copy] Bx[T]",
                contains: "impl[T: Clone + Copy] Bx[T] {",
            },
        },
        // Scope boundary (never-silent, G2): a TRAIT-INSTANCE impl with a non-empty generics
        // list is DN-130's territory (parametric trait-instance heads + coherence), not
        // authorized by DN-131 — still an explicit gap, unchanged from before this leaf.
        Case {
            name: "impl_generic_trait_instance_still_gapped",
            rust: "impl<T: Clone> Foo<T> for Bx<T> { fn f(a: T) -> T { a } }",
            expect: Expect::Gapped {
                category: Category::GenericBound,
            },
        },
        // Scope boundary: a lifetime parameter on the impl slot has no grammar surface — gaps
        // exactly as `plain_type_params` gaps one on a `fn`/`struct`/`enum`/`trait` decl head.
        Case {
            name: "impl_lifetime_param_still_gapped",
            rust: "impl<'a> Bx<'a> { fn dup(self) -> Bx<'a> { self } }",
            expect: Expect::Gapped {
                category: Category::GenericBound,
            },
        },
        // Scope boundary: a const-generic impl-level parameter has no confirmed width-const
        // correspondence — gaps.
        Case {
            name: "impl_const_generic_param_still_gapped",
            rust: "impl<const N: usize> Bx<N> { fn dup(self) -> Bx<N> { self } }",
            expect: Expect::Gapped {
                category: Category::GenericBound,
            },
        },
        // Scope boundary: a bound carrying type arguments (`Into<u8>`) is outside the DN-131 v1
        // plain-trait-name surface this leaf builds — gapped rather than guessed (VR-5).
        Case {
            name: "impl_bound_with_type_args_still_gapped",
            rust: "impl<T: Into<u8>> Bx<T> { fn dup(self) -> Bx<T> { self } }",
            expect: Expect::Gapped {
                category: Category::GenericBound,
            },
        },
        // Scope boundary: an impl `where` clause still has no Mycelium equivalent (DN-131 §3:
        // inline bounds only, no `where` in v1) — unchanged from before this leaf, now reported
        // under its own precise `WhereClause` category rather than the blanket `GenericBound`
        // the old unconditional-refusal gate produced for every generic impl (a strictly more
        // precise, not a weaker, diagnosis).
        Case {
            name: "impl_where_clause_still_gapped",
            rust: "impl<T> Bx<T> where T: Clone { fn dup(self) -> Bx<T> { self } }",
            expect: Expect::Gapped {
                category: Category::WhereClause,
            },
        },
        // M-1006 (E33-1): a named-field enum variant whose fields resolve now emits POSITIONALLY
        // (`A { x: u8 }` -> `A(Binary{8})`), names dropped + recorded via a `NamedFieldDrop` sub-gap.
        Case {
            name: "payload_variant_named_fields_emits_positionally",
            rust: "enum Foo { A { x: u8 }, B }",
            expect: Expect::EmittedAndGapped {
                item: "Foo",
                contains: "type Foo = A(Binary{8}) | B",
                sub_gap_category: Category::NamedFieldDrop,
            },
        },
        // M-1006 §8.14: a named-field variant with a `String` field now EMITS — `String` maps to
        // `Bytes` (RFC-0033 §3.2), names dropped + recorded via a `NamedFieldDrop` sub-gap.
        Case {
            name: "payload_variant_string_field_maps_to_bytes",
            rust: "enum Msg { Text { s: String }, Empty }",
            expect: Expect::EmittedAndGapped {
                item: "Msg",
                contains: "type Msg = Text(Bytes) | Empty",
                sub_gap_category: Category::NamedFieldDrop,
            },
        },
        // M-1006: a named-field variant with an UNMAPPABLE field type (`char`) still gaps — the
        // variant's own precise reason wins (mapped before the resolvability gate). (P4/P5,
        // DN-99 §8 ENB-6: `char` itself now maps to `Binary{32}`, so this fixture moved to `f32`
        // — still genuinely unmapped, `Float` being binary64-only — to keep exercising the
        // still-gapped case.)
        Case {
            name: "payload_variant_unmappable_field_still_gaps",
            rust: "enum Bad { A { c: f32 } }",
            expect: Expect::Gapped {
                category: Category::PayloadVariant,
            },
        },
        // ONESHOT C2 — enum `#[derive(Debug, Clone)]` lowers Debug→Show + Clone satisfied no-op
        // (no longer a bulk DeriveAttr drop). The type still emits; Clone is DeriveSatisfied.
        Case {
            name: "derive_enum_debug_clone_composes_show",
            rust: "#[derive(Debug, Clone)]\nenum Foo { A, B }",
            expect: Expect::Emitted {
                item: "Foo",
                contains: "impl Show[Foo] for Foo {\n  fn render(x: Foo) => Bytes =\n    match x { A => \"A\", B => \"B\" };\n};",
            },
        },
        // Hash on an enum is still unrecognized (product-only for this leaf) — DeriveAttr sub-gap.
        Case {
            name: "derive_enum_hash_still_gaps",
            rust: "#[derive(Hash)]\nenum Foo { A, B }",
            expect: Expect::EmittedAndGapped {
                item: "Foo",
                contains: "type Foo = A | B;",
                sub_gap_category: Category::DeriveAttr,
            },
        },
        // DN-128 (M-1086) — `derive(Debug)` on a FIELDLESS struct composes a trivial `impl
        // Show[T] for T` (no field-walk dependency): the primary "make sure this case emits clean"
        // sub-case the leaf's kickoff names (the std-sys-host `OsEntropy`/`OsClock` canary shape).
        Case {
            name: "derive_debug_unit_struct_composes_show_impl",
            rust: "#[derive(Debug)]\nstruct OsEntropy;",
            expect: Expect::Emitted {
                item: "OsEntropy",
                contains: "impl Show[OsEntropy] for OsEntropy {\n  fn render(x: OsEntropy) => Bytes =\n    \"OsEntropy\";\n};",
            },
        },
        // DN-128 (M-1086) — `derive(Default)` on a FIELDLESS struct composes the bare nullary
        // `impl Init[T] for T` (the constructor with no field args).
        Case {
            name: "derive_default_unit_struct_composes_init_impl",
            rust: "#[derive(Default)]\nstruct OsEntropy;",
            expect: Expect::Emitted {
                item: "OsEntropy",
                contains: "impl Init[OsEntropy] for OsEntropy {\n  fn init() => OsEntropy =\n    OsEntropy;\n};",
            },
        },
        // DN-128 (M-1086) — `derive(Debug)` on a struct with a PRIMITIVE field with no `Show`
        // route stays an honest gap. **DN-138 WU-4 update:** a narrow `u8`/`Binary{8}` field is NO
        // LONGER this fixture (WU-4's `width_cast` unblock composes it — see
        // `derive_forms_check_clean_against_real_toolchain`'s `Narrow` case); `f64`/`Float` is the
        // one primitive repr that stays a genuine, disclosed gap for every row (ADR-040 §2.3/§2.4 —
        // no `Show[Float]` is ever seeded). The struct's own `type` declaration still emits (only
        // the derive impl is dropped).
        Case {
            name: "derive_debug_primitive_field_gaps_never_fabricates",
            rust: "#[derive(Debug)]\nstruct Pair(f64, bool);",
            expect: Expect::EmittedAndGapped {
                item: "Pair",
                contains: "type Pair = Pair(Float, Bool);",
                sub_gap_category: Category::DeriveAttr,
            },
        },
        // DN-128 (M-1086) — `derive(Default)` on a struct with a PRIMITIVE field stays an honest
        // gap for the identical reason. **DN-138 WU-4 update:** see the `Debug` case above — `f64`
        // replaces the now-unblocked `u8` fixture.
        Case {
            name: "derive_default_primitive_field_gaps_never_fabricates",
            rust: "#[derive(Default)]\nstruct Pair(f64, bool);",
            expect: Expect::EmittedAndGapped {
                item: "Pair",
                contains: "type Pair = Pair(Float, Bool);",
                sub_gap_category: Category::DeriveAttr,
            },
        },
        // DN-128 §6.1 (M-1086) — `derive(Clone)`/`derive(Copy)` are a SATISFIED no-op under
        // Mycelium's value semantics (ADR-003): recorded via the dedicated `DeriveSatisfied`
        // category (never `DeriveAttr` — this is not a gap), and no `impl Clone`/`impl Copy` text
        // is ever emitted (Mycelium has no such trait to implement).
        Case {
            name: "derive_clone_copy_satisfied_no_op",
            rust: "#[derive(Clone, Copy)]\nstruct Flag(bool);",
            expect: Expect::EmittedAndGapped {
                item: "Flag",
                contains: "type Flag = Flag(Bool);",
                sub_gap_category: Category::DeriveSatisfied,
            },
        },
        // DN-128 (M-1086) — a derive name outside this leaf's standard set (`Serialize`, or
        // `Eq`/`Ord`/`Hash`/`PartialEq`/`PartialOrd` — DN-128 §2's OTHER rows, a separate unbuilt
        // increment) stays an honest `DeriveAttr` gap, exactly like any other unrecognized derive.
        Case {
            name: "derive_unrecognized_name_gaps",
            rust: "#[derive(Serialize)]\nstruct Flag(bool);",
            expect: Expect::EmittedAndGapped {
                item: "Flag",
                contains: "type Flag = Flag(Bool);",
                sub_gap_category: Category::DeriveAttr,
            },
        },
        // DN-128 (M-1086) — a GENERIC struct's `derive(Debug)` gaps (a derived impl for a generic
        // type needs DN-130's generic-trait-instance-impl mechanism, out of this leaf's scope); the
        // struct's own (generic) `type` declaration still emits unaffected.
        Case {
            name: "derive_debug_generic_struct_gaps",
            rust: "#[derive(Debug)]\nstruct Wrap<T>(T);",
            expect: Expect::EmittedAndGapped {
                item: "Wrap",
                contains: "type Wrap[T] = Wrap(T);",
                sub_gap_category: Category::DeriveAttr,
            },
        },
        // M-1001: a `use` import is FLAGGED, not emitted — the transpiler has no cross-nodule symbol
        // table so it cannot confirm the path resolves (the vet loop confirms such imports fail
        // `myc check` name-resolution), and an emitted `use` poisons the whole draft's check.
        Case {
            name: "simple_use_gapped",
            rust: "use a::b::C;",
            expect: Expect::Gapped {
                category: Category::Import,
            },
        },
        // Grouped `use` is likewise an Import gap.
        Case {
            name: "grouped_use_gap",
            rust: "use a::{b, c};",
            expect: Expect::Gapped {
                category: Category::Import,
            },
        },
        // DN-140 (M-1106): reserved type/variant names rewrite to `*_kw` (G2 comment + emitted text).
        Case {
            name: "reserved_type_name",
            rust: "enum Float { A, B }",
            expect: Expect::Emitted {
                item: "Float_kw",
                contains: "type Float_kw = A | B",
            },
        },
        Case {
            name: "reserved_variant",
            rust: "enum GuaranteeStrength { Exact, Loose }",
            expect: Expect::Emitted {
                item: "GuaranteeStrength",
                contains: "Exact_kw",
            },
        },
        // Shared-reference erasure (this leaf, ADR-003): a fn whose params are `&T` shared references
        // now maps — the references are erased so the signature becomes value params, exactly as the
        // hand-port renders it. This is the item-level effect that unblocks emission (the real-corpus
        // shape: `fn digest_eq(a: &ContentHash, b: &ContentHash) -> bool`).
        Case {
            name: "shared_ref_params_emit",
            rust: "fn digest_eq(a: &Ordering, b: &Ordering) -> bool { a == b }",
            expect: Expect::Emitted {
                item: "digest_eq",
                contains: "fn digest_eq(a: Ordering, b: Ordering) => Bool = a == b;",
            },
        },
        // DN-125 (M-1081): a fn taking a top-level `&mut T` parameter now value-threads (Alt A,
        // Rank 1) instead of hard-gapping — `x` erases to a by-value `Ordering` param, and the
        // return type widens to carry `x` back out alongside the genuine `bool` return value
        // (this fixture's body never actually reassigns `x`, so the threaded slot is just `x`
        // itself, unchanged — a vacuously-correct rebind, not a special case; `map_signature`'s
        // `FnArg::Typed` `&mut T` arm does not require the body to mutate). Was
        // `mut_ref_param_gapped` pre-DN-125; kept the same fixture Rust source so the two
        // behaviors (gap -> emit) are directly comparable in history.
        Case {
            name: "mut_ref_param_value_threads",
            rust: "fn bump(x: &mut Ordering) -> bool { true }",
            expect: Expect::Emitted {
                item: "bump",
                contains: "fn bump(x: Ordering) => (Ordering, Bool) = (x, True);",
            },
        },
        // M-1006 §8.14: a fn taking `&str` now emits — the reference erases to `str`, which maps to
        // `Bytes` (RFC-0033 §3.2). The real-corpus shape `fn message(&self) -> &str` (a String/`str`
        // accessor) is the class this unblocks.
        Case {
            name: "shared_ref_to_str_emits_bytes",
            rust: "fn tag(msg: &str) -> bool { true }",
            expect: Expect::Emitted {
                item: "tag",
                contains: "fn tag(msg: Bytes) => Bool = True;",
            },
        },
        // NEVER-SILENT CASCADE: a fn taking `&f32` still gaps — the reference erases but the referent
        // `f32` has no confirmed base_type arm, so the honest deeper blocker surfaces (Other), never
        // a fabricated emission. (P4/P5, DN-99 §8 ENB-6: `char` itself now maps to `Binary{32}`, so
        // this fixture moved to `f32` — still genuinely unmapped — to keep exercising the cascade.)
        Case {
            name: "shared_ref_to_unmappable_referent_still_gapped",
            rust: "fn is_err(c: &f32) -> bool { true }",
            expect: Expect::Gapped {
                category: Category::Other,
            },
        },
        // ── trx2 Lane C Deliverable 1: operand-type-gated operator emission ─────────────────────
        // (verify-first, mitigation #14 — every surface name below was confirmed against the real
        // built `target/debug/myc`/`target/debug/myc-check` toolchain; see this module's
        // `binop_operand_gated_forms_check_clean` live-oracle test for the `myc check`-clean
        // proof, and `emit.rs`'s `Expr::Binary` arm doc for the full citation trail.)
        //
        // Both operands are known `Binary{16}` params (from `MappedSig::params` via `sig_type_env`)
        // -> `&`/`|` rewrite to the bare-call prim forms `and`/`or` (the glyph desugar target
        // `band`/`bor` is NOT a prim — `myc check`-confirmed to fail with no import).
        Case {
            name: "bitand_known_binary_emits_and_call",
            rust: "fn f(a: u16, b: u16) -> u16 { a & b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "and(a, b)",
            },
        },
        Case {
            name: "bitor_known_binary_emits_or_call",
            rust: "fn f(a: u16, b: u16) -> u16 { a | b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "or(a, b)",
            },
        },
        // `^` is already the correct prim name after the parser's glyph desugar (`Tok::Caret` ->
        // word `"xor"`, which IS a bare-call prim) — left as the unchanged glyph; no rewrite.
        Case {
            name: "bitxor_known_binary_stays_glyph",
            rust: "fn f(a: u16, b: u16) -> u16 { a ^ b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "a ^ b",
            },
        },
        // `!=`/`>` desugar to `ne`/`gt`, which are non-`pub` `lib/std/cmp.myc` functions, not
        // prims — a bare `ne(a,b)`/`gt(a,b)` call fails identically to the glyph (both parse to the
        // same `Expr::App`). The verified fix composes them from the `eq`/`lt` prims directly
        // (exactly `cmp.myc`'s own `ne{N}`/`gt{N}` derivation), which DOES check clean with no
        // import.
        Case {
            name: "ne_known_binary_composes_from_eq",
            rust: "fn f(a: u16, b: u16) -> bool { a != b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "(match eq(a, b) { 0b1 => False, _ => True })",
            },
        },
        Case {
            name: "gt_known_binary_composes_from_eq_and_lt",
            rust: "fn f(a: u16, b: u16) -> bool { a > b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "(match eq(a, b) { 0b1 => False, _ => match lt(a, b) { 0b1 => False, \
                            _ => True } })",
            },
        },
        // `==`/`<` are RFC-0032 D1's ratified glyphs — unchanged by this deliverable even though
        // both operands here are known `Binary{16}` (the operand-gate only fires for the
        // `& | != >` arms).
        Case {
            name: "eq_lt_known_binary_stay_glyphs",
            rust: "fn f(a: u16, b: u16) -> bool { a == b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "a == b",
            },
        },
        // Non-`Binary{N}` operand (a `bool` param, mapped to `Bool` — never a `Binary{N}` text per
        // `map_type`) keeps the CURRENT (pre-deliverable) emission unchanged: still the bare glyph,
        // not a call. Proves the gate is genuinely operand-typed, not unconditional.
        Case {
            name: "bitand_non_binary_operand_keeps_glyph",
            rust: "fn f(a: bool, b: bool) -> bool { a & b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "a & b",
            },
        },
        Case {
            name: "gt_non_binary_operand_keeps_glyph",
            rust: "fn f(a: bool, b: bool) -> bool { a > b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "a > b",
            },
        },
        // One operand unknown (a call result, not a bare in-scope identifier) — the gate requires
        // BOTH operands resolved, so this also keeps the glyph (never a half-composed emission).
        Case {
            name: "ne_one_operand_unresolved_keeps_glyph",
            rust: "fn f(a: u16, b: u16) -> bool { a != g(b) }",
            expect: Expect::Emitted {
                item: "f",
                contains: "a != g(b)",
            },
        },
        // ── P4/P5 (DN-99 §8 ENB-6 / M-1029 / ADR-028): signed-operand-gated op emission ─────────
        // (verify-first, mitigation #14 — see this module's `signed_numeric_idiom_check_clean`
        // live-oracle test for the `myc check`-clean proof over the real toolchain, and
        // `emit.rs`'s `Expr::Binary`/`Expr::Unary` arm docs for the full citation trail.) Both
        // operands are known source-signed `i32` params -> the `_s`-suffixed op family.
        Case {
            name: "signed_add_emits_add_s",
            rust: "fn f(a: i32, b: i32) -> i32 { a + b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "add_s(a, b)",
            },
        },
        Case {
            name: "signed_sub_emits_sub_s",
            rust: "fn f(a: i32, b: i32) -> i32 { a - b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "sub_s(a, b)",
            },
        },
        Case {
            name: "signed_mul_emits_mul_s",
            rust: "fn f(a: i32, b: i32) -> i32 { a * b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "mul_s(a, b)",
            },
        },
        Case {
            name: "signed_neg_emits_neg_s",
            rust: "fn f(a: i32) -> i32 { -a }",
            expect: Expect::Emitted {
                item: "f",
                contains: "neg_s(a)",
            },
        },
        Case {
            name: "signed_lt_composes_bridged_lt_s",
            rust: "fn f(a: i32, b: i32) -> bool { a < b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "(match lt_s(a, b) { 0b1 => True, _ => False })",
            },
        },
        Case {
            name: "signed_gt_composes_from_eq_and_lt_s",
            rust: "fn f(a: i32, b: i32) -> bool { a > b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "(match eq(a, b) { 0b1 => False, _ => match lt_s(a, b) { 0b1 => False, \
                            _ => True } })",
            },
        },
        // Post-#1645 residual (ORACLE-R1 L2-A1 / std-time lit-zero): a signed *param* compared
        // to bare decimal `0` rewrites the lit to equal-width BinLit and uses signed `lt_s`
        // (not unsigned `lt`) so high-bit payloads order correctly (ADR-028). Bare `0` is a
        // file-level Q6 poison under myc-check.
        Case {
            name: "signed_param_lt_zero_emits_binlit_lt_s",
            rust: "fn f(a: i32) -> bool { a < 0 }",
            expect: Expect::Emitted {
                item: "f",
                contains: "(match lt_s(a, 0b0000_0000_0000_0000_0000_0000_0000_0000) { 0b1 => True, \
                            _ => False })",
            },
        },
        Case {
            name: "signed_param_eq_zero_emits_binlit_eq",
            rust: "fn f(a: i128) -> bool { a == 0 }",
            expect: Expect::Emitted {
                item: "f",
                contains: "(match eq(a, 0b0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_\
                            0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_\
                            0000_0000_0000_0000_0000_0000_0000) { 0b1 => True, _ => False })",
            },
        },
        // Duration::is_negative / is_zero shape: signed field access on an in-file product.
        // Field-type map (not name-only layout) recovers Binary{128}!s so lit-zero rewrites
        // and signed order fire. Mutant: bare `0` or unsigned `lt` without `_s`.
        Case {
            name: "signed_field_is_negative_emits_binlit_lt_s",
            rust: "struct Duration { nanos: i128 } impl Duration { pub const fn is_negative(self) \
                   -> bool { self.nanos < 0 } }",
            expect: Expect::Emitted {
                item: "impl Duration",
                contains: "lt_s((match self { Duration(p0) => p0 }), 0b0000_0000_0000_0000_0000_\
                            0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_\
                            0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000)",
            },
        },
        Case {
            name: "signed_field_is_zero_emits_binlit_eq",
            rust: "struct Duration { nanos: i128 } impl Duration { pub const fn is_zero(self) -> \
                   bool { self.nanos == 0 } }",
            expect: Expect::Emitted {
                item: "impl Duration",
                contains: "eq((match self { Duration(p0) => p0 }), 0b0000_0000_0000_0000_0000_\
                            0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_\
                            0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000)",
            },
        },
        // Unsigned field compare-to-zero: BinLit rewrite, but unsigned `lt` (not `lt_s`).
        Case {
            name: "unsigned_field_lt_zero_emits_binlit_lt",
            rust: "struct Tick { n: u64 } impl Tick { fn before_epoch(self) -> bool { self.n < 0 } }",
            expect: Expect::Emitted {
                item: "impl Tick",
                contains: "(match lt((match self { Tick(p0) => p0 }), 0b0000_0000_0000_0000_0000_\
                            0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000) { 0b1 => True, \
                            _ => False })",
            },
        },
        // D3 arithmetic-operator-emission residual (this leaf): the UNSIGNED counterpart to the
        // `_s`-suffixed arms above. Prior to this leaf the unsigned `Add`/`Sub`/`Mul` operand-gate
        // fell through to the plain glyph (pinned by this same case's now-superseded
        // `unsigned_add_keeps_glyph_unchanged_by_this_leaf` name/comment), which did NOT
        // `myc check`-clean for a `Binary{N}` operand pair — `add` is the *ternary*-only
        // `prim_family` member (checkty.rs:9975), so `a + b` on two `Binary{N}` values failed with
        // `` `add` does not accept argument types [Binary(..), Binary(..)] `` (T-Op; RFC-0007
        // §4.4). Confirmed the exact repro `fn add2(a: u64, b: u64) -> u64 { a + b }` before this
        // fix. Now composes to the already-registered `add_u`/`sub_u`/`mul_u` prims (width-
        // preserving `Binary{N}` arithmetic, RFC-0032 D2/M-748 + RFC-0033 §4.1.2 CU-1) — proven
        // `myc check`-clean with no import (`binop_operand_gated_forms_check_clean` below).
        Case {
            name: "unsigned_add_emits_add_u",
            rust: "fn f(a: u32, b: u32) -> u32 { a + b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "add_u(a, b)",
            },
        },
        Case {
            name: "unsigned_sub_emits_sub_u",
            rust: "fn f(a: u32, b: u32) -> u32 { a - b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "sub_u(a, b)",
            },
        },
        Case {
            name: "unsigned_mul_emits_mul_u",
            rust: "fn f(a: u32, b: u32) -> u32 { a * b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "mul_u(a, b)",
            },
        },
        // Non-`Binary{N}` operand keeps the plain glyph (the gate is genuinely operand-typed, not
        // unconditional) — the twin of `bitand_non_binary_operand_keeps_glyph` for `+`.
        Case {
            name: "add_non_binary_operand_keeps_glyph",
            rust: "fn f(a: bool, b: bool) -> bool { a + b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "a + b",
            },
        },
        // A `let`-aliased local of a known `Binary{N}` param is itself recognized as known (the
        // `Stmt::Local` env-extension case (a): "RHS is a bare param already in the env").
        Case {
            name: "let_alias_of_known_binary_extends_env",
            rust: "fn f(a: u16, b: u16) -> bool { let c = a; c > b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "match eq(c, b) { 0b1 => False, _ => match lt(c, b) { 0b1 => False, \
                            _ => True } }",
            },
        },
        // An impl method's `self` parameter is threaded into the env too (via `sig_type_env`
        // already covering the `Receiver` arm's `("self", ty)` entry from `map_signature`) — a
        // `Binary{N}`-mapped `Self` type (here `u16` -> `Binary{16}`) participates in the same
        // operand gate. Uses a non-`Widen` trait name so `try_width_cast_widen_body`'s DN-41
        // special-case (which bypasses this body-emission path entirely) never intercepts it.
        Case {
            name: "impl_method_self_known_binary_participates_in_gate",
            rust: "impl u16 { fn m(self, b: u16) -> u16 { self & b } }",
            expect: Expect::Emitted {
                item: "impl Binary{16}",
                contains: "and(self, b)",
            },
        },
        // trx2 A1 (DN-34 §8.18): an `as` cast that WIDENS one unsigned `Binary` to a wider one
        // (`u16 as u32`, `Binary{16}` -> `Binary{32}`, `M >= N`) emits the faithful DN-41
        // `width_cast` — end-to-end through a fn body whose param type seeds the operand's env
        // entry. `width_cast` zero-extends (unsigned), matching Rust's unsigned widening exactly.
        // (The float-crossing / unknown-operand fidelity cases are pinned at the reason-string
        // level in `expr_cast_fidelity` below, which this table's `Expect` cannot express — it
        // asserts category, not the FLAG reason.)
        Case {
            name: "cast_widen_binary_emits_width_cast",
            rust: "fn f(x: u16) -> u32 { x as u32 }",
            expect: Expect::Emitted {
                item: "f",
                contains: "width_cast(x, 0b0000_0000_0000_0000_0000_0000_0000_0000)",
            },
        },
        // DN-51 §2 D3/§6 (maintainer-authorized DN-39 post-freeze promotion): an `as` cast that
        // NARROWS one unsigned `Binary` to a smaller one (`u32 as u16`, `Binary{32}` -> `Binary{16}`,
        // `M < N`) now emits the faithful DN-51 `truncate` — end-to-end through a fn body whose
        // param type seeds the operand's env entry. `truncate` unconditionally keeps the low `M`
        // bits, matching Rust's wrapping narrow exactly (where `width_cast`'s checked narrow would
        // refuse — see `expr_cast_fidelity`'s `narrow_u32_as_u16_emits_truncate` for the direct
        // gap-reason-level pin of the prior FLAGged state, now an emission).
        Case {
            name: "cast_narrow_binary_emits_truncate",
            rust: "fn f(x: u32) -> u16 { x as u16 }",
            expect: Expect::Emitted {
                item: "f",
                contains: "truncate(x, 0b0000_0000_0000_0000)",
            },
        },
        // ── ONESHOT C3: mask lit / !=0 / Bool not (std-fs metadata residual) ─────────────────────
        // When the *other* operand is a known Binary{N} (env / field), a decimal/octal/hex mask
        // lit rewrites to an equal-width BinLit and rides `and`/`or`/`eq`-composed `!=` —
        // never a bare decimal (Q6) and never the unknown prims `band`/`ne`. Bool `!`/`!=`
        // compose total match forms (`lib/std/core.myc` bool_not / inverted bool_eq); Binary `!`
        // keeps the glyph (`bit.not` via parse desugar — already clean).
        Case {
            name: "bitand_known_binary_with_suffixed_literal_emits_and_binlit",
            rust: "fn f(a: u16) -> u16 { a & 5u16 }",
            expect: Expect::Emitted {
                item: "f",
                contains: "and(a, 0b0000_0000_0000_0101)",
            },
        },
        Case {
            name: "bitand_known_binary_with_unsuffixed_literal_emits_and_binlit",
            rust: "fn f(a: u16) -> u16 { a & 5 }",
            expect: Expect::Emitted {
                item: "f",
                contains: "and(a, 0b0000_0000_0000_0101)",
            },
        },
        Case {
            name: "ne_known_binary_vs_zero_composes_from_eq",
            rust: "fn f(a: u32) -> bool { a != 0 }",
            expect: Expect::Emitted {
                item: "f",
                contains: "(match eq(a, 0b0000_0000_0000_0000_0000_0000_0000_0000) { 0b1 => False, \
                           _ => True })",
            },
        },
        Case {
            name: "bitand_ne_zero_mask_composes_clean",
            rust: "fn f(a: u32) -> bool { a & 0o400 != 0 }",
            expect: Expect::Emitted {
                item: "f",
                contains: "(match eq(and(a, 0b0000_0000_0000_0000_0000_0001_0000_0000), \
                           0b0000_0000_0000_0000_0000_0000_0000_0000) { 0b1 => False, _ => True })",
            },
        },
        Case {
            name: "bool_not_composes_match_invert",
            rust: "fn f(b: bool) -> bool { !b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "match (b) { True => False, False => True }",
            },
        },
        Case {
            name: "bool_ne_composes_match",
            rust: "fn f(a: bool, b: bool) -> bool { a != b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "match (a) { True => match (b) { True => False, False => True }, False => (b) }",
            },
        },
        Case {
            name: "binary_not_keeps_glyph",
            rust: "fn f(a: u16) -> u16 { !a }",
            expect: Expect::Emitted {
                item: "f",
                contains: "!a",
            },
        },
        // ONESHOT C3: user-enum `==` routes through co-emitted `eq_<T>` (not kernel `eq`).
        Case {
            name: "enum_eq_uses_co_emitted_eq_fn",
            rust: "#[derive(PartialEq)] enum K { A, B } impl K { fn is_a(self) -> bool { self == K::A } }",
            expect: Expect::Emitted {
                item: "impl K",
                contains: "(match eq_K(self, A) { 0b1 => True, _ => False })",
            },
        },
        // `(e)`/`&e` ARE structurally transparent to the operand-type gate (this module's own
        // `Expr::Paren`/`Expr::Reference` emission arms treat them identically to `e` itself).
        Case {
            name: "bitand_known_binary_through_paren_emits_and_call",
            rust: "fn f(a: u16, b: u16) -> u16 { (a) & b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "and((a), b)",
            },
        },
        Case {
            name: "bitand_known_binary_through_reference_emits_and_call",
            rust: "fn f(a: u16, b: u16) -> u16 { &a & b }",
            expect: Expect::Emitted {
                item: "f",
                contains: "and(a, b)",
            },
        },
        // ── DN-99 #72 string-literal pattern — ENABLER LANDED (M-1035/ENB-12), now EMITS ─────────
        // A string-literal match arm `"yes" => …` is grammatically valid Mycelium surface. It was
        // gapped while the L1 checker rejected a `match` on a `Bytes` scrutinee; M-1035/ENB-12
        // landed that enabler (`check_match` admits `Ty::Bytes` with a required wildcard/default
        // arm for the open domain). So the faithful surface now EMITS and `myc check`-cleans —
        // verified against the real oracle. `&str` → `Bytes`, `true`/`false` → `True`/`False`, so
        // this lowers to `match s { "yes" => True, _ => False }` (the first enabler-driven trx win).
        // See `string_literal_pattern_emits_with_l1_enabler`.
        Case {
            name: "string_literal_pattern_emits_with_l1_enabler",
            rust: "fn classify(s: &str) -> bool { match s { \"yes\" => true, _ => false } }",
            expect: Expect::Emitted {
                item: "classify",
                contains: "match s { \"yes\" => True, _ => False }",
            },
        },
        // ── DN-118 Phase 1 — the closure-EMIT pass (`lambda_expr`) ────────────────────────────────
        // A move/`Copy`-capture closure (every free var read-only, no mutation signal): Mechanical
        // per the DN-109 D5/D7 ratchet, auto-emitted as `lambda(params) => body`, captures left as
        // ordinary in-scope references (mono's whole-program defunctionalization, RFC-0024 §4A,
        // resolves the capture set — this transpiler never synthesizes an env record). Verified
        // `myc check`-clean end-to-end (this exact shape) in DN-118 Phase 0's verify-first probe —
        // the `apply$Fn` synthetic-`Env` gap the facility hit is a *different*, unrelated mechanism
        // (`elaborate_lower_rule`'s ad-hoc single-function `Env`, `lower`-rule RHS elaboration
        // only), not a general `myc check`/whole-program limitation.
        Case {
            name: "closure_move_copy_capture_emits_lambda",
            rust: "fn make_masker(n: u16) -> u16 { let f = |x: u16| x & n; f(n) }",
            expect: Expect::Emitted {
                item: "make_masker",
                contains: "let f = lambda(x: Binary{16}) => and(x, n) in f(n)",
            },
        },
        // An untyped closure parameter has no `lambda_expr`'s `Ident ':' type_ref` correspondence
        // — this transpiler has no type-inference pass to recover an omitted type (VR-5: absence,
        // never a guess).
        Case {
            name: "closure_untyped_param_gapped",
            rust: "fn f(n: u16) -> u16 { let g = |x| x; g(n) }",
            expect: Expect::Gapped {
                category: Category::Closure,
            },
        },
        // VERIFY-FIRST FINDING (mitigation #14): a multi-parameter closure PARSES to a `lambda`
        // declaration but the L1 checker curries it (RFC-0024 §4A.8/M-822), so an ordinary
        // multi-arg call site (`f(a, b)`, this transpiler's existing `Expr::Call` emission)
        // fails `myc check` — confirmed empirically against the real oracle, NOT emitted as a
        // plausible-but-failing form (G2/VR-5); deferred as a separate, larger call-site-aware
        // unit of work.
        Case {
            name: "closure_multi_param_gapped",
            rust: "fn combine(a: u16, b: u16) -> u16 { let f = |x: u16, y: u16| and(x, y); \
                   f(a, b) }",
            expect: Expect::Gapped {
                category: Category::Closure,
            },
        },
        // A zero-parameter closure has no v0 `lambda` form (the grammar note on `lambda_expr`).
        Case {
            name: "closure_zero_param_gapped",
            rust: "fn f(n: u16) -> u16 { let g = || n; g() }",
            expect: Expect::Gapped {
                category: Category::Closure,
            },
        },
        // The DN-109 D7 safety gate: a captured binding mutated via compound assignment
        // (`total += x`, the syntactic shape of an `FnMut`-style accumulator capture) is FLAGGED,
        // never auto-emitted — `syn` carries no borrowck facts, so this cannot be proven
        // value-safe (mono would otherwise silently snapshot `total`'s value at closure
        // construction, diverging from the Rust closure's per-call-mutated semantics).
        Case {
            name: "closure_fnmut_compound_assign_capture_gapped",
            rust: "fn f(n: u16) -> u16 { let mut total = 0; let mut g = |x: u16| total += x; \
                   g(n); total }",
            expect: Expect::Gapped {
                category: Category::Closure,
            },
        },
        // The same D7 gate for an explicit `&mut` on a captured binding.
        Case {
            name: "closure_fnmut_explicit_mut_ref_capture_gapped",
            rust: "fn f(n: u16) -> u16 { let mut total = 0; let g = |x: u16| { let r = &mut \
                   total; x }; g(n) }",
            expect: Expect::Gapped {
                category: Category::Closure,
            },
        },
        // The same D7 gate for a captured binding used as a method-call RECEIVER — `syn` cannot
        // decide `&self` vs `&mut self` from syntax alone, so this is conservatively flagged too
        // (never auto-emitted on the hope the method happens to be read-only).
        Case {
            name: "closure_captured_method_receiver_gapped",
            rust: "fn f(n: u16) -> u16 { let v = n; let g = |x: u16| v.wrapping_add(x); g(n) }",
            expect: Expect::Gapped {
                category: Category::Closure,
            },
        },
        // NEGATIVE control: a closure that mutates a PURELY INTERNAL local (never escapes, never a
        // capture — bound and mutated entirely within the closure's own body) must NOT be
        // misclassified as a captured-mutation `Closure` gap. It still gaps (Mycelium's body
        // grammar has no assignment-statement production at all, `MultiStmtBody`'s pre-existing
        // semicolon-terminated-statement refusal), but via the ordinary generic path — pinning
        // that the DN-109 D7 scan does not false-positive on a shadowed/local name.
        Case {
            name: "closure_purely_local_mutation_not_misclassified_as_closure_gap",
            rust: "fn f(n: u16) -> u16 { let g = |x: u16| { let mut acc = 0; acc += x; acc }; \
                   g(n) }",
            expect: Expect::Gapped {
                category: Category::MultiStmtBody,
            },
        },
        // T-A1 (DN-122 §13.2 WU-A; positive control): a single-param, param-only-sig foreign-trait
        // impl of the registered `Ord3` prelude trait (`mvp_prelude_trait_shape`) — receiverless
        // methods, every value-param `Self`, a primitive return — synthesizes the `[<SelfTy>]`
        // Mycelium trait-argument the Rust source itself never spells (see `emit_impl`'s MVP block).
        // `binop_operand_gated_forms_check_clean`-style live-oracle coverage of the SAME shape
        // (that it actually `myc check`s clean) is below, `mvp_cmp_emit_check_agreement`.
        Case {
            name: "mvp_cmp_eligible_synthesizes_trait_arg",
            rust: "impl Ord3 for u8 { fn cmp(a: Self, b: Self) -> u8 { a } }",
            expect: Expect::Emitted {
                item: "impl Ord3[Binary{8}] for Binary{8}",
                contains: "impl Ord3[Binary{8}] for Binary{8} {\n  fn cmp(a: Binary{8}, b: Binary{8}) => Binary{8} = a;\n};",
            },
        },
        // T-A2 (negative/honest-gap control): `Widen` is two-type/`Self`-receiver-needing (DN-122
        // §13.1's own adversarial narrowing) — it is emitted EXACTLY as before WU-A (no bracket
        // synthesis, no fabricated trait/`Self` body), still an honest `myc check`-time residual
        // (M-876/M-1076), never silently "fixed" by the MVP recognizer. Mirrors the pre-existing
        // `widen_binary_emits_width_cast_not_fabricated_from` assertion, pinned here specifically
        // against MVP-bracket leakage.
        Case {
            name: "mvp_widen_unaffected_by_mvp_recognizer",
            rust: "impl Widen<u16> for u8 { fn widen(self) -> u16 { u16::from(self) } }",
            expect: Expect::Emitted {
                item: "widen_free Binary{8} -> Binary{16}",
                contains: "width_cast(self, 0b0000_0000_0000_0000)",
            },
        },
        // T-A3 half 1 (emit<->check agreement, the transpile-time half): a `Ord3`-named impl whose
        // `cmp` method has a `self` RECEIVER (the exact shape `Widen`/`MycEq`/etc. all use) is
        // correctly recognized as INELIGIBLE (`has_self_receiver` excludes it) — emitted unchanged,
        // no `[<SelfTy>]` bracket. The live-oracle half (that the real checker ALSO refuses this
        // shape, confirming the exclusion was not overcautious) is `mvp_cmp_emit_check_agreement`.
        Case {
            name: "mvp_cmp_self_receiver_excluded_no_bracket",
            rust: "impl Ord3 for u8 { fn cmp(self, other: Self) -> u8 { self } }",
            expect: Expect::Emitted {
                item: "impl Ord3 for Binary{8}",
                contains: "impl Ord3 for Binary{8} {",
            },
        },
    ]
}

fn run(case: &Case) {
    let (myc, report) = transpile_source(case.rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("case `{}` failed to parse/transpile: {e}", case.name));
    match &case.expect {
        Expect::Emitted { item, contains } => {
            assert!(
                report.emitted_items.iter().any(|n| n == item),
                "case `{}`: expected `{item}` in emitted_items, got {:?}",
                case.name,
                report.emitted_items
            );
            assert!(
                myc.contains(contains),
                "case `{}`: expected .myc to contain `{contains}`, got:\n{myc}",
                case.name
            );
        }
        Expect::Gapped { category } => {
            assert!(
                report.emitted_items.is_empty(),
                "case `{}`: expected no emitted items, got {:?}",
                case.name,
                report.emitted_items
            );
            assert!(
                report.gaps.iter().any(|g| g.category == *category),
                "case `{}`: expected a gap of category {:?}, got {:?}",
                case.name,
                category.as_str(),
                report
                    .gaps
                    .iter()
                    .map(|g| g.category.as_str())
                    .collect::<Vec<_>>()
            );
        }
        Expect::EmittedAndGapped {
            item,
            contains,
            sub_gap_category,
        } => {
            assert!(
                report.emitted_items.iter().any(|n| n == item),
                "case `{}`: expected `{item}` in emitted_items, got {:?}",
                case.name,
                report.emitted_items
            );
            assert!(
                myc.contains(contains),
                "case `{}`: expected .myc to contain `{contains}`, got:\n{myc}",
                case.name
            );
            assert!(
                report.gaps.iter().any(|g| g.category == *sub_gap_category),
                "case `{}`: expected a sub-gap of category {:?}, got {:?}",
                case.name,
                sub_gap_category.as_str(),
                report
                    .gaps
                    .iter()
                    .map(|g| g.category.as_str())
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn emit_fixture_corpus() {
    for case in cases() {
        run(&case);
    }
}

/// Regression guard (High finding, G2/DN-34 §4, extended by DN-41/M-873 follow-on): the
/// never-silent gap mechanism means a *gapped* item's `.myc` text is never emitted at all — pin
/// that down directly for the bool-`Self` widen shape (which still has no `width_cast` witness
/// path — `Self` doesn't map to `Binary{N}`) so a future change that started emitting a
/// partial/fallback body for this case would fail loudly here, not just leave `emitted_items`
/// empty while still leaking fabricated text into the `.myc` output.
#[test]
fn widen_bool_from_call_produces_no_fabricated_myc_text() {
    let rust = "impl Widen<u32> for bool { fn widen(self) -> u32 { u32::from(self) } }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.is_empty(),
        "expected the bool Widen impl to be fully gapped, got emitted_items={:?}",
        report.emitted_items
    );
    assert!(
        !myc.contains("from("),
        "emitted .myc text must never contain a fabricated `from(...)` call (from is not a \
         Mycelium builtin — G2/DN-34 §4), got:\n{myc}"
    );
}

/// The DN-41 companion of the guard above: a `Binary{N}`->`Binary{M}` widen must emit a **real**
/// `width_cast(self, ..)` call — never a fabricated `from(...)` call, and never left gapped now
/// that the faithful mapping exists.
#[test]
fn widen_binary_emits_width_cast_not_fabricated_from() {
    let rust = "impl Widen<u16> for u8 { fn widen(self) -> u16 { u16::from(self) } }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report
            .emitted_items
            .iter()
            .any(|n| n.starts_with("widen_free Binary{8} -> Binary{16}")),
        "expected Binary widen free-fn via width_cast, got emitted_items={:?}",
        report.emitted_items
    );
    assert!(
        !myc.contains("from("),
        "emitted .myc text must never contain a fabricated `from(...)` call (from is not a \
         Mycelium builtin — G2/DN-34 §4), got:\n{myc}"
    );
    assert!(
        myc.contains("width_cast(self, 0b0000_0000_0000_0000)"),
        "expected a real `width_cast(self, ..)` call with a 16-bit zero witness, got:\n{myc}"
    );
}

/// DN-41 companion: `Narrow::narrow` is fallible and has no `= expr` surface, so it must stay an
/// honest gap whose reason cites DN-41 — never a fabricated `try_from`/`?`-shaped emission.
#[test]
fn narrow_gap_cites_dn41_and_produces_no_fabricated_myc_text() {
    let rust = "impl Narrow<u8> for u16 { fn narrow(self) -> Result<u8, NarrowError> { u8::try_from(self) } }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.is_empty(),
        "expected the Narrow impl to be fully gapped, got emitted_items={:?}",
        report.emitted_items
    );
    assert!(
        !myc.contains("try_from") && !myc.contains("width_cast"),
        "narrow bodies must never be fabricated (no try_from-shaped or width_cast emission), \
         got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| { g.reason.contains("DN-41") || g.reason.contains("non-prelude trait") }),
        "expected Narrow gap to cite DN-41 or non-prelude residual, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// Never-silent guard (G2/VR-5): a string literal that cannot be faithfully re-escaped (a control
/// char with no Mycelium `\xNN`/`\u{..}` form) is gapped, and its raw byte NEVER leaks into the
/// emitted `.myc` text — a future change that started emitting the raw control byte (or a fabricated
/// `\x07` escape Mycelium's lexer would reject) would fail loudly here.
#[test]
fn string_control_char_never_leaks_raw_byte() {
    let rust = "fn f(x: u8) -> u8 { g(x, \"\\x07\") }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.is_empty(),
        "expected the control-char string body to be fully gapped, got emitted_items={:?}",
        report.emitted_items
    );
    assert!(
        !myc.contains('\u{7}') && !myc.contains("\\x07"),
        "gapped control-char string must never leak a raw byte or a fabricated `\\x07` escape \
         (StrLit has no `\\xNN` form), got:\n{myc}"
    );
}

/// DN-99 #72 enabler-landed pin (M-1035/ENB-12, the first enabler-driven trx win): once the L1
/// checker admits a `Bytes` scrutinee in `match` (with the required wildcard/default arm for the
/// open domain), a string-literal match pattern EMITS and `myc check`-cleans. This pins the
/// faithful lowering (`&str`→`Bytes`, `"yes"` verbatim, `true`/`false`→`True`/`False`, `_` default)
/// and — its VR-5/G2 twin — that a string-literal match WITHOUT a default stays gapped never-
/// silently (a non-exhaustive `Bytes` match is check-failing surface we must not emit).
#[test]
fn string_literal_pattern_emits_with_l1_enabler() {
    // With the default arm: emits the faithful, check-clean surface.
    let rust = "fn classify(s: &str) -> bool { match s { \"yes\" => true, _ => false } }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "classify"),
        "the string-pattern fn must now emit (enabler landed), got emitted={:?} gaps={:?}",
        report.emitted_items,
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
    assert!(
        myc.contains("match s { \"yes\" => True, _ => False }"),
        "the faithful check-clean string-pattern surface must be emitted, got:\n{myc}"
    );

    // Without a default arm: `Bytes` is an open domain, so an emission would be non-exhaustive and
    // check-FAIL — it must stay gapped never-silently with a reason naming the open-domain default
    // requirement (VR-5/G2), never fake-emitted.
    let no_default = "fn c(s: &str) -> bool { match s { \"yes\" => true, \"no\" => false } }";
    let (myc2, report2) = transpile_source(no_default, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report2.emitted_items.iter().any(|n| n == "c"),
        "a defaultless string-literal match must stay gapped (would be non-exhaustive), got {:?}",
        report2.emitted_items
    );
    assert!(
        !myc2.contains("match s"),
        "the non-exhaustive (check-failing) surface must NEVER be emitted, got:\n{myc2}"
    );
    assert!(
        report2
            .gaps
            .iter()
            .any(|g| g.reason.contains("without a wildcard/default arm")
                && g.reason.contains("open value domain")),
        "the defaultless-match gap must cite the open-`Bytes` default requirement, got {:?}",
        report2.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// DN-132 P1 (M-1089): a named-field **struct pattern** on an in-file struct desugars to the
/// grammar's positional `Ctor` surface -- declaration-order placement (OQ-5) regardless of the
/// *pattern's* field order, a wildcard `_` at every field the pattern does not name (OQ-4,
/// regardless of whether the pattern spells `..` -- SS5.2 point 4), and the sub-pattern of a
/// renamed/nested field binding recursively mapped.
#[test]
fn struct_pattern_desugars_to_positional_ctor() {
    let cases = [
        // All fields named, in declaration order, no rest.
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { x, y } => x, } }",
            "Foo(x, y)",
        ),
        // `..` rest: an unmentioned field is a wildcard.
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { x, .. } => x, } }",
            "Foo(x, _)",
        ),
        // No `..` but still one field unmentioned -- SS5.2 point 4: the transpiler accepts either
        // spelling and emits the identical positional wildcard (only syntactically-valid Rust,
        // where `rustc` already enforces `..` for a genuinely partial pattern, ever reaches here).
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { x } => x, } }",
            "Foo(x, _)",
        ),
        // Field-order canonicalization (OQ-5): pattern spells `y, x`, out of declaration order.
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { y, x } => x, } }",
            "Foo(x, y)",
        ),
        // Field rename (`field: binding`) -- the sub-pattern binds a different local name.
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { x: a, y: b } => a, } }",
            "Foo(a, b)",
        ),
        // A nested/literal sub-pattern at a named field recurses through `map_pattern`.
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { x: 0, y } => y, _ => 0 } }",
            "Foo(0, y)",
        ),
        // `Self::Ctor { .. }` inside an `impl` -- the ctor-name resolution takes only the path's
        // last segment (the identical convention `Pat::Path`/`Pat::TupleStruct` already use), so
        // the `Self::` qualifier is transparent.
        (
            "struct Foo { x: u8, y: u8 } impl Foo { fn f(self) -> u8 { match self { Self { x, .. } => x, } } }",
            "Foo(x, _)",
        ),
        // M-1093/DN-134: an **enum struct-variant** pattern -- the DN-132 SS5.1 component-seam
        // boundary M-1089's own test used to pin as an honest gap (`struct_layouts` walked
        // `Item::Struct` only). Now that the shared, collision-safe population also walks
        // `Item::Enum` `Fields::Named` variants (`transpile.rs::struct_layouts`), this arm
        // resolves it exactly like a plain struct -- "composes automatically once that
        // population lands, with no further edit here" (M-1089's own doc, confirmed).
        (
            "enum E { A { x: u8, y: u8 } } fn f(v: E) -> u8 { match v { E::A { x, .. } => x, _ => 0 } }",
            "A(x, _)",
        ),
    ];
    for (rust, needle) in cases {
        let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
            .unwrap_or_else(|e| panic!("case `{rust}` failed to parse/transpile: {e}"));
        assert!(
            myc.contains(needle),
            "case `{rust}`: expected .myc to contain `{needle}`, got:\n{myc}\ngaps={:?}",
            report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
        );
    }
}

/// DN-132 P1 (M-1089): the never-silent gap paths (VR-5/G2) -- a struct pattern is only ever
/// desugared when its constructor + every named field is *confirmed* resolvable; anything short of
/// that refuses rather than emitting a guessed/partial-arity `Ctor`.
#[test]
fn struct_pattern_never_silently_gaps() {
    let cases = [
        // No confirmed in-file layout at all (an undeclared/foreign constructor name).
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Bar { x, .. } => x, _ => 0 } }",
            "no confirmed in-file layout",
        ),
        // A field name absent from the resolved layout -- never a silent wildcard/drop.
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { z, .. } => 0, _ => 1 } }",
            "not a declared field",
        ),
        // A duplicate field name within one pattern (defensive: `syn` parses this even though
        // `rustc` itself would reject it -- DN-132 OQ-4c).
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { x, x, .. } => 0, _ => 1 } }",
            "more than once",
        ),
        // A positional field-index member (`0: a`) on a struct-pattern -- out of DN-132 P1 scope.
        (
            "struct Foo(u8, u8); fn f(v: Foo) -> u8 { match v { Foo { 0: a, 1: _b } => a, } }",
            "positional field-index member",
        ),
    ];
    for (rust, needle) in cases {
        let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
            .unwrap_or_else(|e| panic!("case `{rust}` failed to parse/transpile: {e}"));
        assert!(
            !report.emitted_items.iter().any(|n| n == "f"),
            "case `{rust}`: `f` must stay gapped, got emitted={:?} myc:\n{myc}",
            report.emitted_items
        );
        assert!(
            report.gaps.iter().any(|g| g.reason.contains(needle)),
            "case `{rust}`: expected a gap whose reason contains `{needle}`, got {:?}",
            report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
        );
    }
}

/// **The verify-first proof** (mitigation #14) for DN-132 P1 (M-1089): every struct-pattern shape
/// [`struct_pattern_desugars_to_positional_ctor`] proves the *text* of is run through the REAL
/// `myc-check` oracle here, proving the emitted positional `Ctor` pattern actually **type-checks**
/// (the property the whole DN-132 P1 deliverable is for -- it reuses the Maranget usefulness pass
/// unchanged, so a real `myc check` pass is the honest confirmation of that claim, not just a
/// substring match). Skips gracefully (never fails) when `myc-check` is not built, exactly like
/// `binop_operand_gated_forms_check_clean` above.
#[test]
fn struct_pattern_forms_check_clean_against_real_toolchain() {
    let Some(bin) = super::vet::find_myc_check() else {
        eprintln!(
            "emit: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text assertions \
             above still cover the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-struct-pattern-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let rust_snippets = [
        // All fields named, declaration order.
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { x, y } => x, } }",
            "f",
        ),
        // `..` rest -- an unmentioned field wildcards.
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { x, .. } => x, } }",
            "f",
        ),
        // Field-order canonicalization (OQ-5): pattern spells `y, x`.
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { y, x } => x, } }",
            "f",
        ),
        // Field rename (`field: binding`).
        (
            "struct Foo { x: u8, y: u8 } fn f(v: Foo) -> u8 { match v { Foo { x: a, y: b } => a, } }",
            "f",
        ),
        // Bare `Self { .. }` inside an `impl` -- the pattern-side `Self` resolution. Impl blocks
        // are recorded under `impl <Type>`, not the bare method name (see
        // `inherent_impl_no_self_name_collision_is_mangled_and_checks_clean`'s precedent).
        (
            "struct Foo { x: u8, y: u8 } impl Foo { fn f(self) -> u8 { match self { Self { x, .. } => x, } } }",
            "impl Foo",
        ),
        // A three-field struct, only one field bound, `..` for the rest.
        (
            "struct P3 { a: u8, b: u8, c: u8 } fn f(v: P3) -> u8 { match v { P3 { b, .. } => b, } }",
            "f",
        ),
    ];
    for (i, (rust, item)) in rust_snippets.iter().enumerate() {
        let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            report.emitted_items.iter().any(|n| n == item),
            "case {i} (`{rust}`) failed to emit `{item}`: gaps={:?}",
            report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
        );
        let path = dir.join(format!("case_{i}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");

        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "case {i} (`{rust}`) must check CLEAN with the real myc-check oracle — emitted:\n{myc}\n\
             diagnostic={:?}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// DN-134 SS3 (M-1093): a named-field **struct-variant construction** in expression position
/// (`E::A { x: .., y: .. }`, `Self::Variant { .. }`) desugars to the grammar's positional `Ctor`
/// call, arguments placed at their DECLARATION index regardless of the literal's own field-write
/// order -- the construction twin of `struct_pattern_desugars_to_positional_ctor` (M-1089), now
/// sharing the same `struct_layouts` population.
#[test]
fn struct_variant_construction_desugars_to_positional_ctor() {
    let cases = [
        // Declaration-order field-value write.
        (
            "enum E { A { x: u8, y: u8 } } fn f() -> E { E::A { x: 1, y: 2 } }",
            "A(1, 2)",
        ),
        // Field-order canonicalization: written `y, x`, still emitted `A(1, 2)` (x=1, y=2).
        (
            "enum E { A { x: u8, y: u8 } } fn f() -> E { E::A { y: 2, x: 1 } }",
            "A(1, 2)",
        ),
        // `Self::Variant { .. }` inside an `impl` -- the identical ctor-name-resolution
        // convention `Expr::Struct`'s bare-`Self` arm and `Pat::Struct`'s already use; the
        // `Self::` qualifier's ENUM segment is transparent (only the variant's own last segment
        // matters), so no special-case is needed for the qualified form either.
        (
            "enum E { A { x: u8, y: u8 } } impl E { fn f() -> E { Self::A { x: 3, y: 4 } } }",
            "A(3, 4)",
        ),
        // A single-field variant.
        (
            "enum E { A { x: u8 } } fn f() -> E { E::A { x: 7 } }",
            "A(7)",
        ),
        // A three-field variant, matching the `std-sys-host` shape's field count class.
        (
            "enum E { A { a: u8, b: u8, c: u8 } } fn f() -> E { E::A { c: 3, a: 1, b: 2 } }",
            "A(1, 2, 3)",
        ),
    ];
    for (rust, needle) in cases {
        let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
            .unwrap_or_else(|e| panic!("case `{rust}` failed to parse/transpile: {e}"));
        assert!(
            myc.contains(needle),
            "case `{rust}`: expected .myc to contain `{needle}`, got:\n{myc}\ngaps={:?}",
            report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
        );
    }
}

/// DN-134 SS3 (M-1093): the never-silent gap/refusal paths (VR-5/G2) for struct-variant
/// construction -- an unresolved ctor, a missing field, an extra/unknown field, and a duplicate
/// field-value binding all refuse rather than emitting a guessed/partial-arity `Ctor`.
#[test]
fn struct_variant_construction_never_silently_gaps() {
    let cases = [
        // No confirmed in-file layout at all -- the enum/variant is undeclared in this file (the
        // honest DN-113/DN-134-OQ-2 cross-nodule-resolvability shape: `TimeErr` isn't declared
        // here, exactly like `std-sys-host`'s real `TimeErr::ClockUnavailable { reason }`, which
        // is imported from `std.time`, not declared in the same file).
        (
            "fn f() -> u8 { let x = TimeErr::ClockUnavailable { reason: 1 }; 0 }",
            "not an in-file single-ctor struct or enum struct-variant",
        ),
        // Missing field.
        (
            "enum E { A { x: u8, y: u8 } } fn f() -> E { E::A { x: 1 } }",
            "gives no value for the field at position",
        ),
        // Extra/unknown field -- previously silently dropped (no check existed at all before
        // this leaf); now an explicit refusal.
        (
            "enum E { A { x: u8, y: u8 } } fn f() -> E { E::A { x: 1, y: 2, z: 3 } }",
            "not a declared field",
        ),
        // Duplicate field-value binding.
        (
            "enum E { A { x: u8, y: u8 } } fn f() -> E { E::A { x: 1, x: 2, y: 3 } }",
            "more than once",
        ),
        // `..rest` struct-update on a variant construction -- the pre-existing "no record-update
        // surface" gap, exercised on the enum-variant shape too (was previously only reachable
        // for plain structs since variants never resolved a layout at all).
        (
            "enum E { A { x: u8, y: u8 } } fn f(o: E) -> E { E::A { x: 1, ..o } }",
            "struct-update syntax",
        ),
    ];
    for (rust, needle) in cases {
        let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
            .unwrap_or_else(|e| panic!("case `{rust}` failed to parse/transpile: {e}"));
        assert!(
            !report.emitted_items.iter().any(|n| n == "f"),
            "case `{rust}`: `f` must stay gapped, got emitted={:?} myc:\n{myc}",
            report.emitted_items
        );
        assert!(
            report.gaps.iter().any(|g| g.reason.contains(needle)),
            "case `{rust}`: expected a gap whose reason contains `{needle}`, got {:?}",
            report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
        );
    }
}

/// **CRITICAL regression (PR #1548 strict review — empirically reproduced against the compiled
/// transpiler; the exact #1535/DN-134 build-blocking hazard).** `struct A` and
/// `enum Foo1 { A { .. }, Baz { odd: Instant } }` share the bare ctor name `A`. `Baz`'s
/// `odd: Instant` field is unmappable (an undeclared external type), so `Foo1` fails WHOLE-ENUM
/// resolvability for a reason entirely UNRELATED to the colliding `A` variant. Before the fix,
/// `struct_layouts`'s collision loop skipped registering `Foo1`'s variants into its `seen`/
/// `ambiguous` bookkeeping whenever the WHOLE owning enum was unresolvable — so `Foo1::A`'s bare
/// name was never flagged as colliding with struct `A`'s, and `emit::struct_layout` (which
/// resolves by bare ctor name only, with no per-enum scoping) would silently bind `Foo1::A {
/// foo, bar }`'s construction to struct `A`'s real layout: a wrong-index bind (G2) recorded as a
/// clean success. This test fails against the pre-fix code (both `f` and `g` wrongly emit, `g`
/// bound to the wrong layout) and passes with the fix (both gap, never a silent wrong bind).
#[test]
fn struct_vs_unrelated_enum_variant_collision_registers_despite_sibling_gap() {
    let rust = "struct A { foo: u8, bar: u8 } \
                enum Foo1 { A { foo: u8, bar: u8 }, Baz { odd: Instant } } \
                fn f() -> A { A { foo: 1, bar: 2 } } \
                fn g() -> Foo1 { Foo1::A { foo: 3, bar: 4 } }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "fixture").unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "g"),
        "`Foo1::A {{ .. }}` must NEVER silently resolve against the unrelated struct `A`'s layout \
         just because `Foo1` (as a WHOLE) fails resolvability for a reason unrelated to the `A` \
         variant (`Baz`'s unmappable `Instant` field) — emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "struct `A`'s own construction must also gap once its bare name collides with `Foo1::A` \
         — never a partial refusal that leaves one interpretation silently reachable — \
         emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("A(3, 4)") && !myc.contains("A(1, 2)"),
        "neither construction may silently emit a positional `A(..)` bound to either \
         interpretation's layout — got:\n{myc}"
    );
}

/// **CRITICAL regression, twin of the above** (PR #1548 strict review). `Foo1::Bar` and
/// `Foo2::Bar` share the bare ctor name `Bar`, declared on two UNRELATED enums. `Foo1`'s sibling
/// variant `Baz` has an unmappable field (`Instant`, undeclared), excluding `Foo1` as a WHOLE
/// from resolvability for a reason entirely unrelated to `Bar`. Before the fix, `Foo1`'s `Bar`
/// variant was skipped from collision registration entirely (the whole-enum-unresolvable guard
/// covered the whole loop body, not just the `out`-insertion), so `Foo2::Bar`'s real layout
/// stayed unflagged and `Foo1::Bar`'s construction site would silently resolve against it —
/// bare-name-only lookup, no per-enum scoping — a wrong-index bind (G2) between two entirely
/// unrelated enums. Fails against the pre-fix code (both `g1`/`g2` wrongly emit, `g1` cross-bound
/// to `Foo2`'s layout) and passes with the fix (both gap, refused as ambiguous).
#[test]
fn two_enums_same_named_variant_one_excluded_by_unmappable_sibling_never_cross_binds() {
    let rust = "enum Foo1 { Bar { reason: String }, Baz { odd: Instant } } \
                enum Foo2 { Bar { reason: String } } \
                fn g1() -> Foo1 { Foo1::Bar { reason: 1 } } \
                fn g2() -> Foo2 { Foo2::Bar { reason: 2 } }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "fixture").unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "g1"),
        "`Foo1::Bar {{ .. }}` must NEVER silently resolve against `Foo2::Bar`'s layout merely \
         because `Foo1` fails whole-enum resolvability for a reason unrelated to `Bar` (`Baz`'s \
         unmappable `Instant` field) — emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        !report.emitted_items.iter().any(|n| n == "g2"),
        "`Foo2::Bar`'s own construction must also gap once its bare name collides with \
         `Foo1::Bar` — never a partial refusal leaving one interpretation silently reachable — \
         emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
}

/// **THE key soundness test** (DN-134 SS4 stress-#8, SS3 step 1(b) -- the cross-leaf finding from
/// the M-1089 pattern-emit review that made the shared `struct_layouts` population's
/// collision-safety a BUILD-BLOCKING DoD item, not an OQ). A file declaring both a plain `struct
/// A` and an unrelated `enum E { A { .. } }` sharing the SAME bare name must NEVER let `E::A`'s
/// construction silently resolve against struct `A`'s layout (a wrong-index bind, G2) -- nor may
/// struct `A`'s own construction silently keep resolving once the name is ambiguous (this
/// transpiler's resolution side has no qualifier to tell the two apart -- see
/// `transpile.rs::struct_layouts`'s collision-safety doc). BOTH must gap, never-silently, never a
/// wrong emission.
#[test]
fn struct_and_variant_same_bare_name_collision_never_silently_binds_wrong() {
    let rust = "struct A { foo: u8, bar: u8 } \
                enum E { A { foo: u8 } } \
                fn f() -> A { A { foo: 1, bar: 2 } } \
                fn g() -> E { E::A { foo: 3 } }";
    let (myc, report) =
        transpile_source(rust, "fixture.rs", "fixture").unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "the ambiguous struct `A`'s own construction must gap once its bare name collides with \
         `E::A`, not silently keep resolving — emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        !report.emitted_items.iter().any(|n| n == "g"),
        "`E::A {{ .. }}` must NEVER silently resolve against the unrelated struct `A`'s layout \
         — emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("A(3)") && !myc.contains("A(1, 2)"),
        "neither the wrong-bind shape (`A(3)`, `E::A` bound to struct `A`'s 2-field layout) nor \
         a coincidentally-matching struct-side emission may appear — got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .filter(|g| g.reason.contains(
                "not an in-file single-ctor struct or enum \
                                             struct-variant"
            ))
            .count()
            >= 2,
        "both `f` and `g` must each carry their own honest \"no confirmed layout\" gap (never a \
         silent wrong bind) — gaps={:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// **The verify-first proof** (mitigation #14) for DN-134 SS3 (M-1093): every struct-variant
/// construction shape [`struct_variant_construction_desugars_to_positional_ctor`] proves the
/// *text* of is run through the REAL `myc-check` oracle here, proving the emitted positional
/// `Ctor` call actually **type-checks** — the construction-side twin of
/// `struct_pattern_forms_check_clean_against_real_toolchain` (M-1089). Skips gracefully (never
/// fails) when `myc-check` is not built.
#[test]
fn struct_variant_construction_forms_check_clean_against_real_toolchain() {
    let Some(bin) = super::vet::find_myc_check() else {
        eprintln!(
            "emit: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text assertions \
             above still cover the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-struct-variant-ctor-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    // Field values are typed fn PARAMETERS (shorthand `E::A { x, y }`), not literal integers --
    // a bare `Int` literal has no representation family under the real checker without a
    // declared default paradigm (verify-first, mitigation #14: `expr_env_type`'s own doc, and
    // `struct_pattern_forms_check_clean_against_real_toolchain`'s sibling oracle test uses the
    // identical bound-variable convention for the same reason). Arity/declaration-order
    // correctness is asserted TEXTUALLY by the sibling
    // `struct_variant_construction_desugars_to_positional_ctor` test; this oracle proves each
    // canonicalized positional `Ctor` shape actually type-checks.
    let rust_snippets = [
        // Declaration-order write.
        (
            "enum E { A { x: u8, y: u8 } } fn f(x: u8, y: u8) -> E { E::A { x, y } }",
            "f",
        ),
        // Field-order canonicalization: written `y, x`.
        (
            "enum E { A { x: u8, y: u8 } } fn f(x: u8, y: u8) -> E { E::A { y, x } }",
            "f",
        ),
        // `Self::Variant { .. }` inside an `impl`.
        (
            "enum E { A { x: u8, y: u8 } } impl E { fn f(x: u8, y: u8) -> E { Self::A { x, y } } }",
            "impl E",
        ),
        // Single-field variant -- matches `std-sys-host`'s `TimeErr::ClockUnavailable { reason }`
        // shape's field-count class.
        ("enum E { A { x: u8 } } fn f(x: u8) -> E { E::A { x } }", "f"),
        // Three-field variant, a wider arity case for measure.
        (
            "enum E { A { a: u8, b: u8, c: u8 } } fn f(a: u8, b: u8, c: u8) -> E { E::A { c, a, b } }",
            "f",
        ),
    ];
    for (i, (rust, item)) in rust_snippets.iter().enumerate() {
        let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            report.emitted_items.iter().any(|n| n == item),
            "case {i} (`{rust}`) failed to emit `{item}`: gaps={:?}",
            report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
        );
        let path = dir.join(format!("case_{i}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");

        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "case {i} (`{rust}`) must check CLEAN with the real myc-check oracle — emitted:\n{myc}\n\
             diagnostic={:?}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// The #72 co-poison fix, UPDATED (M-1037 residual): a Rust ownership/identity-conversion method
/// must never desugar to a fabricated bare call (`unknown function/constructor/prim <name>`) —
/// G2/VR-5. Mapped rows (`to_owned`/`clone`/`to_string`(Bytes)/accessors) emit identity when
/// their receiver gate matches; residuals (`into`/`to_vec`/non-Bytes `to_string`/user types) gap
/// with EXPLAIN. **M-1037 residual:** `expr_env_type` types string/bool/char literals, so
/// `"a".to_owned()` is now sound identity (fixed Rust type `&'static str` → `Bytes`) — the
/// prior whole-fn gap for the string-literal match arm body is closed without fabricating.
#[test]
fn conversion_noop_method_gaps_never_fabricates_unknown_prim() {
    // A bare `.to_owned()` on a `&str` (maps to the builtin scalar `Bytes`) is sound identity —
    // emit the receiver unchanged, never a fabricated `to_owned(...)` call.
    let rust = "fn f(s: &str) -> String { s.to_owned() }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "a `.to_owned()` on a builtin-scalar (`Bytes`) receiver must emit identity (not gap), \
         got emitted_items={:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        !myc.contains("to_owned("),
        "the fabricated `to_owned(...)` bare call must NEVER be emitted, got:\n{myc}"
    );
    assert!(
        myc.contains("(s)"),
        "expected the identity passthrough `(s)`, got:\n{myc}"
    );

    // M-1037 residual: string-literal match arms with `.to_owned()` bodies — both arms now
    // identity-emit (literal typed as Bytes; bare `s` as Bytes). Must emit the whole fn, never
    // fabricate `to_owned(`.
    let real =
        "fn m(s: &str) -> String { match s { \"A\" => \"a\".to_owned(), _ => s.to_owned() } }";
    let (myc2, report2) = transpile_source(real, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report2.emitted_items.iter().any(|n| n == "m"),
        "string-literal `.to_owned()` arm bodies must identity-emit the whole fn (M-1037 residual), \
         got emitted_items={:?} gaps={:?}",
        report2.emitted_items,
        report2.gaps
    );
    assert!(
        !myc2.contains("to_owned("),
        "no fabricated `to_owned(...)` may leak even inside a string-match, got:\n{myc2}"
    );

    // M-1037: `.deref()` on a bare `&str` identifier (`Bytes`) is sound identity — must emit, not gap.
    let deref = "fn g(s: &str) -> &str { s.deref() }";
    let (myc3, report3) = transpile_source(deref, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report3.emitted_items.iter().any(|n| n == "g"),
        "`.deref()` on builtin `&str` receiver must emit identity (M-1037), got {:?}",
        report3.emitted_items
    );
    assert!(
        !myc3.contains("deref("),
        "the fabricated `deref(...)` bare call must NEVER be emitted, got:\n{myc3}"
    );

    // A `.to_owned()` on a USER-NAMED-TYPE receiver must NEVER fire the builtin-scalar identity
    // row — the receiver's `ToOwned` impl is not foreclosed by the orphan rule (only a *foreign*
    // type's is), and its `Owned` associated type need not even equal `Self` (std's own
    // `str -> String`/`[T] -> Vec<T>` are exactly this shape), so assuming identity would be an
    // unchecked guess (G2/VR-5). Mirrors `src/tests/prim_map.rs`'s
    // `clone_on_user_named_type_receiver_never_fires_identity_and_gaps` for the `clone` row.
    let user_type = "fn snap(t: Ticket) -> Ticket { t.to_owned() }";
    let (myc4, report4) = transpile_source(user_type, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        !report4.emitted_items.iter().any(|n| n == "snap"),
        "a `.to_owned()` on a user-named-type receiver (`Ticket`, not a builtin scalar) must \
         NEVER emit as identity, got emitted_items={:?}, gaps={:?}, myc=\n{myc4}",
        report4.emitted_items,
        report4.gaps
    );
    assert!(
        !myc4.contains("fn snap"),
        "no `fn snap` declaration of any shape may ever be emitted, got:\n{myc4}"
    );
    assert!(
        report4.gaps.iter().any(|g| g
            .reason
            .contains("ownership/identity-conversion no-op method")),
        "expected `snap` to gap via the `is_unmappable_conversion_method` catch-all, got {:?}",
        report4.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// M-1037 — generalized never-fabricates pin: mapped conversion/accessor methods and explicit
/// unmapped methods (`ne`/`fetch_add`/`contains`/`into`/`to_vec`) must not leak fabricated bare-call
/// surface.
#[test]
fn conversion_and_unmapped_methods_never_fabricate_unknown_prim() {
    for (rust, forbidden_call) in [
        (
            "fn atom(x: u64) -> u64 { x.fetch_add(1, std::sync::atomic::Ordering::Relaxed) }",
            "fetch_add(",
        ),
        (
            "fn has(s: String, c: char) -> bool { s.contains(c) }",
            "contains(",
        ),
        // M-1037 residual: into / to_vec / non-Bytes to_string never fabricate.
        ("fn i(s: &str) -> String { s.into() }", "into("),
        ("fn v(x: &[u8]) -> Vec<u8> { x.to_vec() }", "to_vec("),
        ("fn t(x: u64) -> String { x.to_string() }", "to_string("),
    ] {
        let (myc, _report) = transpile_source(rust, "fixture.rs", "fixture")
            .unwrap_or_else(|e| panic!("failed `{rust}`: {e}"));
        assert!(
            !myc.contains(forbidden_call),
            "`{rust}` leaked fabricated `{forbidden_call}`, got:\n{myc}"
        );
    }
    // Composed `.ne` on known `Binary{N}` operands must emit (not gap) without `ne(` fabrication.
    let ne_ok = "fn ne_u(a: u64, b: u64) -> bool { a.ne(&b) }";
    let (myc_ne, report_ne) =
        transpile_source(ne_ok, "fixture.rs", "fixture").unwrap_or_else(|e| panic!("failed: {e}"));
    assert!(
        report_ne.emitted_items.iter().any(|n| n == "ne_u"),
        "composed `.ne` on u64 should emit, got {:?}",
        report_ne.emitted_items
    );
    assert!(
        myc_ne.contains("match eq") && !myc_ne.contains("ne("),
        "expected composed `eq` lowering, got:\n{myc_ne}"
    );
}

/// The sharpened `MultiStmtBody` reason (this leaf, E33-1 M-1006 phase-1) names the *kind* of the
/// offending interior statement — a nested item (local `static`/`const`/`fn`), a macro invocation,
/// or a semicolon-terminated statement expression — so the gap report is precise, not generic
/// (G2). Each is a genuinely design-blocked form (no local-item / no macro / value-discard has no
/// grammar surface); this pins the diagnostic text, not any emission.
#[test]
fn multi_stmt_body_reason_names_the_statement_kind() {
    let cases = [
        // A local `static` item statement (the real `mono_nanos` shape).
        (
            "fn f(x: u8) -> u8 { static Z: u8 = 0; x }",
            "nested item declaration",
        ),
        // A macro-invocation statement (the real `rejection_sample_u64` `debug_assert!` shape).
        (
            "fn f(x: u8) -> u8 { debug_assert!(x > 0); x }",
            "macro-invocation statement",
        ),
        // A semicolon-terminated (value-discarding) statement expression.
        (
            "fn f(x: u8) -> u8 { g(x); x }",
            "semicolon-terminated (value-discarding) statement expression",
        ),
    ];
    for (rust, needle) in cases {
        let (_, report) = transpile_source(rust, "fixture.rs", "fixture")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            report
                .gaps
                .iter()
                .any(|g| g.category == Category::MultiStmtBody && g.reason.contains(needle)),
            "case `{rust}`: expected a MultiStmtBody gap whose reason mentions `{needle}`, got {:?}",
            report
                .gaps
                .iter()
                .map(|g| (g.category.as_str(), g.reason.as_str()))
                .collect::<Vec<_>>()
        );
    }
}

use super::vet::find_myc_check;

/// **The verify-first proof** (mitigation #14) for trx2 Lane C Deliverable 1: every operand-gated
/// rewrite in `Expr::Binary` (`and`/`or` for `&`/`|`, the `eq`/`lt`-composed forms for `!=`/`>`) is
/// run through the REAL `myc-check` oracle here, not just asserted as a substring match (the
/// `emit_fixture_corpus` cases above prove the *text*; this proves the text actually **type-checks**
/// with zero imports — the property the whole deliverable is for). Skips gracefully (never fails)
/// when `myc-check` is not built, exactly like `src/tests/vet.rs`'s `live_myc_check_classifies_clean_and_broken`.
#[test]
fn binop_operand_gated_forms_check_clean() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "emit: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text assertions \
             above still cover the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-binop-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    // Every rewrite this deliverable makes, in ONE nodule (mirrors the real driver: one file, no
    // cross-nodule imports) — `and`/`or`/`eq`/`lt` must all resolve as bare-call prims with no
    // `use`, and the composed `!=`/`>` match expressions must type as `Bool`.
    let rust_snippets = [
        "fn f_and(a: u16, b: u16) -> u16 { a & b }",
        "fn f_or(a: u16, b: u16) -> u16 { a | b }",
        "fn f_ne(a: u16, b: u16) -> bool { a != b }",
        "fn f_gt(a: u16, b: u16) -> bool { a > b }",
        // `^` (unchanged glyph) rides along as a negative control — it must ALSO check clean
        // (it already did before this deliverable; this pins that it still does).
        "fn f_xor(a: u16, b: u16) -> u16 { a ^ b }",
        // D3 operand-type-inference depth (DN-34 §8.16 residual): a `&`-reference-wrapped operand
        // must ALSO check clean, proving the extended `expr_env_type` gate composes into a real,
        // myc-check-clean body, not just matching test-fixture text.
        "fn f_and_ref(a: u16, b: u16) -> u16 { &a & b }",
        // D3 arithmetic-operator-emission residual (this leaf, the Add-glyph unblock): the
        // `add_u`/`sub_u`/`mul_u`-composed unsigned arithmetic ops must resolve as bare-call
        // prims with no `use` and type-check the fn's declared return width — the exact repro
        // this leaf closes (`fn add2(a: u64, b: u64) -> u64 { a + b }` failed `myc check` with
        // `` `add` does not accept argument types [Binary(..), Binary(..)] `` before this fix).
        "fn f_add_u(a: u16, b: u16) -> u16 { a + b }",
        "fn f_sub_u(a: u16, b: u16) -> u16 { a - b }",
        "fn f_mul_u(a: u16, b: u16) -> u16 { a * b }",
        // ONESHOT C3 — mask-lit / !=0 / Bool not residuals (std-fs metadata poison).
        "fn f_and_lit(a: u32) -> u32 { a & 0o400 }",
        "fn f_ne_zero(a: u32) -> bool { a != 0 }",
        "fn f_mask_ne_zero(a: u32) -> bool { a & 0o400 != 0 }",
        "fn f_bool_not(b: bool) -> bool { !b }",
        "fn f_bool_ne(a: bool, b: bool) -> bool { a != b }",
    ];
    for (i, rust) in rust_snippets.iter().enumerate() {
        let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            !report.emitted_items.is_empty(),
            "case {i} (`{rust}`) failed to emit at all: gaps={:?}",
            report.gaps
        );
        let path = dir.join(format!("case_{i}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");

        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "case {i} (`{rust}`) must check CLEAN with the real myc-check oracle — emitted:\n{myc}\n\
             diagnostic={:?}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// **The DN-118 Phase 1 verify-first live-oracle proof** (mitigation #14): the closure-EMIT pass's
/// `lambda` output — for a move/`Copy`-capture closure, the `closure_move_copy_capture_emits_lambda`
/// fixture above — is run through the REAL `myc-check` oracle, WHOLE-NODULE (one file, mirroring the
/// real driver), not just asserted as a substring match. This is the property the whole Phase 1
/// closure-EMIT pass exists to prove: the `apply$Fn` synthetic-`Env` gap the facility hit
/// (`elaborate_lower_rule`'s ad-hoc single-function `Env`, a `lower`-rule-only mechanism, DN-118's
/// header) is CLOSED here because mono's whole-program defunctionalization (RFC-0024 §4A, M-704)
/// resolves the generated `apply$Fn$Binary16$Binary16` dispatcher itself when the whole nodule is
/// checked — exactly as DN-118 Phase 0's standalone verify-first probe (`myc check` + `myc run`
/// against a hand-written `.myc`) already confirmed. Skips gracefully (never fails) when
/// `myc-check` is not built.
#[test]
fn closure_move_copy_capture_checks_clean() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "emit: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text assertion \
             (`closure_move_copy_capture_emits_lambda`) still covers the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-closure-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    // The move/Copy-capture closure case (`closure_move_copy_capture_emits_lambda`'s Rust source) —
    // the shape whose transpiled `.myc` must resolve `apply$Fn$Binary16$Binary16` whole-program.
    let rust = "fn make_masker(n: u16) -> u16 { let f = |x: u16| x & n; f(n) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
    assert!(
        !report.emitted_items.is_empty(),
        "closure case failed to emit at all: gaps={:?}",
        report.gaps
    );
    assert!(
        report.gaps.is_empty(),
        "closure case must have zero gaps (the `apply$Fn` gap must be fully closed): {:?}",
        report.gaps
    );
    let path = dir.join("closure_case.myc");
    std::fs::write(&path, &myc).expect("write closure case .myc");

    let checker = crate::vet::MycChecker {
        command: vec![bin.display().to_string()],
        cwd: None,
    };
    let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
    assert_eq!(
        rec.class,
        crate::vet::VetClass::Clean,
        "closure case must check CLEAN with the real myc-check oracle (the apply$Fn dispatcher \
         must resolve whole-program) — emitted:\n{myc}\ndiagnostic={:?}",
        rec.diagnostic
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **The verify-first live-oracle proof** (mitigation #14) for DN-51 §2 D3/§6's transpiler flip:
/// the narrow-cast `truncate` emission (`cast_narrow_binary_emits_truncate` above) is run through
/// the REAL `myc-check` oracle, not just asserted as a substring match — the property that matters
/// is that the emitted `truncate(x, <M-bit zero witness>)` call genuinely type-checks, mirroring
/// `binop_operand_gated_forms_check_clean`'s pattern. Skips gracefully (never fails) when
/// `myc-check` is not built.
#[test]
fn cast_narrow_truncate_emission_checks_clean() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "emit: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text assertion \
             (`cast_narrow_binary_emits_truncate`) still covers the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-truncate-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    // The narrow-cast case (`u32 as u16`, the FLAG-truncate-not-emittable arm this task flips),
    // plus the widen/identity siblings alongside it in one nodule — pinning that `truncate` and
    // `width_cast` coexist cleanly in the same file (no cross-nodule imports, matching the real
    // driver's one-file-per-input shape).
    let rust_snippets = [
        "fn f_narrow(x: u32) -> u16 { x as u16 }",
        "fn f_widen(x: u16) -> u32 { x as u32 }",
        "fn f_identity(x: u32) -> u32 { x as u32 }",
        // A narrow all the way down to a single bit — the boundary `M = 1` case.
        "fn f_narrow_to_bit(x: u32) -> u8 { x as u8 }",
    ];
    for (i, rust) in rust_snippets.iter().enumerate() {
        let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            !report.emitted_items.is_empty(),
            "case {i} (`{rust}`) failed to emit at all: gaps={:?}",
            report.gaps
        );
        let path = dir.join(format!("case_{i}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");

        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "case {i} (`{rust}`) must check CLEAN with the real myc-check oracle — emitted:\n{myc}\n\
             diagnostic={:?}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// **The verify-first live-oracle proof** (mitigation #14) for P4/P5 (DN-99 §8 ENB-6 / M-1029 /
/// ADR-028): every signed-int / usize / isize / char numeric-type-idiom emission this leaf added
/// runs through the REAL `myc-check` oracle, mirroring `binop_operand_gated_forms_check_clean`'s
/// pattern. **Non-vacuity (this leaf's verify-first finding):** every one of these Rust snippets
/// was a hard GAP before this leaf (`i8..i128`/`isize`/`usize`/`char` all refused in `map_type`,
/// so the whole containing fn never emitted at all) — this test both proves the new emission is
/// `myc check`-clean AND (via `report.emitted_items`) that it is a *real* emission, not a
/// coincidental no-op. Skips gracefully (never fails) when `myc-check` is not built.
#[test]
fn signed_numeric_idiom_check_clean() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "emit: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text assertions \
             elsewhere still cover the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-p4p5-numeric-idiom-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let rust_snippets = [
        // i32 arithmetic — routes to the landed `add_s`/`sub_s`/`mul_s` (ADR-028: overflow-checked
        // two's-complement, distinct from the unsigned `_u` family).
        "fn f_add(a: i32, b: i32) -> i32 { a + b }",
        "fn f_sub(a: i32, b: i32) -> i32 { a - b }",
        "fn f_mul(a: i32, b: i32) -> i32 { a * b }",
        // i64 arithmetic — a second width, pinning the width-parametric emission is not an i32-only
        // special case.
        "fn f_add64(a: i64, b: i64) -> i64 { a + b }",
        // i32 comparison — the signed order `lt_s`, bridged `Binary{1}` -> `Bool` (mirrors the
        // unsigned `Gt` composition already proven by `binop_operand_gated_forms_check_clean`).
        "fn f_lt(a: i32, b: i32) -> bool { a < b }",
        "fn f_gt(a: i32, b: i32) -> bool { a > b }",
        "fn f_ne(a: i32, b: i32) -> bool { a != b }",
        // Unary negation — the landed `neg_s`.
        "fn f_neg(a: i32) -> i32 { -a }",
        // `isize` — same `Binary{64}` mapping as `i64`, but sourced from the DISTINCT `isize` Rust
        // type (pins that `type_is_signed_int` recognizes `isize`, not just the fixed-width `iN`s).
        "fn f_neg_isize(a: isize) -> isize { -a }",
        // `usize` — a plain identity fn (the realistic index/count-parameter shape); proves the
        // UNSIGNED `Binary{64}` mapping alone (no `_s` routing — `usize` is never marked signed).
        "fn f_usize_identity(i: usize) -> usize { i }",
        // `char` — a plain identity fn; proves the `Binary{32}` codepoint mapping.
        "fn f_char_identity(c: char) -> char { c }",
    ];
    for (i, rust) in rust_snippets.iter().enumerate() {
        let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            !report.emitted_items.is_empty(),
            "case {i} (`{rust}`) failed to emit at all: gaps={:?}",
            report.gaps
        );
        let path = dir.join(format!("case_{i}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");

        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "case {i} (`{rust}`) must check CLEAN with the real myc-check oracle — emitted:\n{myc}\n\
             diagnostic={:?}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression guard (HIGH finding, PR #1299 review, fix 1a) for the `Stmt::Local` shadow-
/// invalidation bug: a `let` that **shadows** an existing name with an RHS of *unknown* type left
/// the shadowed name's *stale* prior type in `local_env`, so `Expr::Binary`'s operand-type gate
/// could keep firing using a type that no longer applies to the (now-shadowed) name. Repro: `let x
/// = a;` (RHS is the known `Binary{16}` param `a`, so `x` is recorded as `Binary{16}`), then `let x
/// = true;` shadows `x` with a bool-literal RHS (unknown type to this module — never a `Binary{N}`
/// guess). The tail `x != b` must fall back to the plain `!=` glyph (the shadowed `x`'s type is
/// invalidated), never the `eq`/`lt`-composed form the gate would wrongly emit using the *old*
/// binding.
#[test]
fn let_shadow_with_unknown_type_invalidates_stale_binary_env_entry() {
    let rust = "fn f(a: u16, b: u16) -> bool { let x = a; let x = true; x != b }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` to emit, got emitted_items={:?}",
        report.emitted_items
    );
    assert!(
        myc.contains("x != b"),
        "expected the shadowed `x != b` tail to fall back to the plain glyph (the shadow \
         invalidates x's known-Binary{{16}} type from the OLD `let x = a;` binding), got:\n{myc}"
    );
    assert!(
        !myc.contains("match eq(x, b)"),
        "the operand-type gate must NOT fire on the shadowed `x` using the stale OLD binding's \
         type — `let x = true;` shadows it with an unknown-type RHS, got:\n{myc}"
    );
}

/// Regression guard (HIGH finding, PR #1299 review, fix 1b) for the match-arm pattern-binding gap:
/// a name a match arm's pattern **binds** (here `Wrap::A(x)`'s `u32` payload `x`) must never
/// inherit an outer local's type through `env` — the outer `x: u16` (`Binary{16}`) parameter must
/// not leak onto the pattern-bound `x`, which is a *different* binding (the enum payload,
/// `Binary{32}`). Before the fix this mis-fired `and(x, b)` using the outer `Binary{16}` — a real
/// `myc check` width-mismatch failure once the pattern-bound `x` (actually `Binary{32}`) is
/// resolved against `b: Binary{16}`. The arm must fall back to the plain `&` glyph.
#[test]
fn match_arm_pattern_bound_name_invalidates_outer_binary_env_entry() {
    let rust = "enum Wrap { A(u32), B } fn f(x: u16, b: u16, w: Wrap) -> u16 { match w { \
                Wrap::A(x) => x & b, Wrap::B => b } }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "f"),
        "expected `f` to emit, got emitted_items={:?}",
        report.emitted_items
    );
    assert!(
        myc.contains("x & b"),
        "expected the `Wrap::A(x) => x & b` arm to fall back to the plain glyph (the \
         pattern-bound `x` is a distinct Binary{{32}} payload, not the outer u16 param), \
         got:\n{myc}"
    );
    assert!(
        !myc.contains("and(x, b)"),
        "the operand-type gate must NOT fire using the outer `x: u16` param's type for the \
         pattern-bound `x` (a real Binary{{32}} payload vs Binary{{16}} `b` — a genuine \
         width-mismatch myc-check failure if emitted), got:\n{myc}"
    );
}

/// **The verify-first live-oracle proof** (mitigation #14) for both PR #1299 review fixes above:
/// runs the two repros' emitted `.myc` through the REAL `myc-check` oracle. Honest finding
/// (never a silently-skipped false-green, G2): neither repro's *fixed* (fallen-back-to-glyph)
/// emission is actually `myc check`-clean — but for a completely different, PRE-EXISTING and
/// separately-tracked reason than the bug being fixed here. `!=`/`&` in the un-gated (operand-type
/// unknown) fallback path desugar to the bare word calls `ne`/`band`, which are not resolvable
/// prims with no import (exactly the failure mode this module's `Expr::Binary` doc already
/// documents for every other un-gated `!=`/`&` case, e.g. `bitand_non_binary_operand_keeps_glyph`
/// above) — this is orthogonal to, and unaffected by, the type-env shadow/pattern-binding fixes.
/// What this test proves is the *negative* the fixes exist for: the diagnostic is the KNOWN
/// `ne`/`band` gap, never a mismatched-width `and`/`eq` prim-call failure the pre-fix bug would
/// have risked (or, worse, a coincidentally-succeeding wrong-type `Clean` result). Skips
/// gracefully (never fails) when `myc-check` is not built.
#[test]
fn shadow_and_pattern_bound_fixes_fall_back_to_known_gap_not_wrong_prim_call() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "emit: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text \
             assertions above still cover the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-shadow-pattern-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    // (case name, rust source, the un-gated glyph word this fallback desugars to — the honest,
    // pre-existing gap the diagnostic must name; NOT the mismatched-width prim the pre-fix bug
    // would have wrongly emitted).
    let cases = [
        (
            "let_shadow",
            "fn f(a: u16, b: u16) -> bool { let x = a; let x = true; x != b }",
            "`ne`",
        ),
        (
            "match_arm_pattern_bound",
            "enum Wrap { A(u32), B } fn f(x: u16, b: u16, w: Wrap) -> u16 { match w { \
             Wrap::A(x) => x & b, Wrap::B => b } }",
            "`band`",
        ),
    ];
    for (name, rust, expected_gap_word) in cases {
        let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("case `{name}` (`{rust}`) failed to parse/transpile: {e}"));
        assert!(
            !report.emitted_items.is_empty(),
            "case `{name}` (`{rust}`) failed to emit at all: gaps={:?}",
            report.gaps
        );
        // Never the wrong-type prim call the pre-fix bug would have risked.
        assert!(
            !myc.contains("eq(x, b)") && !myc.contains("and(x, b)"),
            "case `{name}`: must never emit the mismatched-type prim-call form the shadow/\
             pattern-binding bug would have produced, got:\n{myc}"
        );

        let path = dir.join(format!("{name}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");
        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::CheckError,
            "case `{name}` (`{rust}`) was expected to hit the KNOWN pre-existing {expected_gap_word} \
             gap (never silently `Clean` on a wrong-type basis) — emitted:\n{myc}\ndiagnostic={:?}",
            rec.diagnostic
        );
        assert!(
            rec.diagnostic.contains(expected_gap_word),
            "case `{name}`: expected the diagnostic to name the known pre-existing \
             {expected_gap_word} gap, got: {}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// **DN-34 §8.13/8.14 "D4" — the live-oracle proof for inherent-impl no-`self` associated-fn
/// mangling.** Two different tuple-struct types each declare a same-named, receiver-less
/// constructor (`Foo::new`/`Bar::new`) in ONE nodule — exactly the shape that regressed the
/// gap-close-2 Phase-0 re-measure (`Duration::from_nanos`/`MonoInstant::from_nanos`,
/// `Task::new`/`TaskCtx::new`/`Deadlock::new`). Before the fix both emit a bare `fn new(...)`,
/// which `mycelium-l1`'s M-664 inherent-impl desugar lifts to the SAME flat top-level name —
/// `myc check` real-oracle-verified `duplicate function`. After the fix each is mangled
/// `{Type}__new`, so the combined nodule is myc-check **Clean**. Skips gracefully (never fails)
/// when `myc-check` is not built.
#[test]
fn inherent_impl_no_self_name_collision_is_mangled_and_checks_clean() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "emit: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text \
             assertions above still cover the emitted shape."
        );
        return;
    };

    // A parameterized constructor (never a bare literal — a bare integer literal has no
    // representation family in v0 with no `default paradigm` in scope, orthogonal to what this
    // test is proving).
    let rust = "pub struct Foo(u32);\n\
                impl Foo {\n\
                    pub fn new(x: u32) -> Self { Foo(x) }\n\
                }\n\
                pub struct Bar(u32);\n\
                impl Bar {\n\
                    pub fn new(x: u32) -> Self { Bar(x) }\n\
                }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the two-`new` fixture: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "impl Foo")
            && report.emitted_items.iter().any(|n| n == "impl Bar"),
        "both impl blocks must emit (mangling must not turn either into a gap): {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    let foo_mangled = crate::reserved::mangled_inherent_fn_name("Foo", "new");
    let bar_mangled = crate::reserved::mangled_inherent_fn_name("Bar", "new");
    assert!(
        myc.contains(&foo_mangled),
        "expected the mangled name `{foo_mangled}` in the emitted text, got:\n{myc}"
    );
    assert!(
        myc.contains(&bar_mangled),
        "expected the mangled name `{bar_mangled}` in the emitted text, got:\n{myc}"
    );
    assert!(
        !myc.contains("fn new("),
        "the bare, colliding name `fn new(` must never be emitted once mangling applies, got:\n{myc}"
    );

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-inherent-mangle-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("case.myc");
    std::fs::write(&path, &myc).expect("write case .myc");
    let checker = crate::vet::MycChecker {
        command: vec![bin.display().to_string()],
        cwd: None,
    };
    let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
    assert_eq!(
        rec.class,
        crate::vet::VetClass::Clean,
        "the mangled two-`new` nodule must check CLEAN with the real myc-check oracle (a \
         `duplicate function` here would mean the mangling regressed) — emitted:\n{myc}\n\
         diagnostic={:?}",
        rec.diagnostic
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `self`-receiving inherent-impl method is deliberately **not** mangled (see
/// `emit::mangled_inherent_fn_name`'s doc for the full scope rationale — mangling those would also
/// require rewriting every `visit_method_call` call site to the identical mangled name, a larger,
/// separately-scoped change). Pins that boundary: `as_ref`-shaped same-named `self`-methods across
/// two types stay bare (so a caller's un-qualified `.method()` desugar still resolves) — the
/// residual flat-namespace collision risk for that case is real and undocumented-away, not silently
/// "fixed" by this narrower change (VR-5: no overclaiming).
#[test]
fn self_receiving_inherent_method_is_left_unmangled() {
    let rust = "pub struct Foo(u32);\n\
                impl Foo {\n\
                    pub fn get(&self) -> u32 { self.0 }\n\
                }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the self-method fixture: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "impl Foo"),
        "impl Foo must emit: {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps
    );
    assert!(
        myc.contains("fn get(")
            && !myc.contains(&crate::reserved::mangled_inherent_fn_name("Foo", "get")),
        "a `self`-receiving method must keep its bare name (never mangled), got:\n{myc}"
    );
}

// --- G-β Rank A — never-fabricate free-fn method-call when inherent not emitted ---------------
//
// When an inherent method body fails to emit (e.g. M-1037 residual `.to_vec()`), the declaration
// is never recorded in `local_mangled`/`bare_fn_names`. A later free-fn call site
// `recv.method(...)` must **gap** rather than desugar to bare `method(recv, …)` which file-poisons
// myc-check with `unknown function/constructor/prim method` (G2/VR-5). Symmetric happy path:
// a successfully-emitted self method remains callable as the registered free-fn desugar.

/// **Poison stop (Rank A):** `Source::read_to_end` body gaps on `.to_vec()` (M-1037 residual);
/// free fn `read_all` calls `.read_to_end()` — must gap the free fn and never emit bare
/// `read_to_end(` that would file-poison myc-check.
#[test]
fn method_call_to_unemitted_inherent_gaps_never_fabricates_free_fn() {
    let rust = "\
pub struct Source {\n\
    data: Vec<u8>,\n\
    pos: usize,\n\
}\n\
impl Source {\n\
    fn read_to_end(&mut self) -> Vec<u8> {\n\
        let bytes = self.data[self.pos..].to_vec();\n\
        self.pos = self.data.len();\n\
        bytes\n\
    }\n\
}\n\
pub fn read_all(mut src: Source) -> Vec<u8> {\n\
    src.read_to_end()\n\
}\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile G-β poison fixture: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "read_all"),
        "`read_all` must gap (callee `read_to_end` was never recorded as emitted): \
         emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("read_to_end("),
        "emitted .myc must NEVER contain bare `read_to_end(` (G-β Rank A poison stop), got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("no proven-emitted free-fn referent")
                || g.reason.contains("read_to_end")),
        "expected a gap citing the unregistered method / body residual, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
    // The method body residual itself must still be reported (never silent about why the
    // declaration failed).
    assert!(
        report.gaps.iter().any(|g| g.reason.contains("to_vec")),
        "expected the impl-body `.to_vec()` residual to surface, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// **Happy path (Rank A):** a self-receiving inherent method that **does** emit (simple field
/// projection) is registered bare; a later free fn calling `.method()` desugars to the
/// registered free-fn name (not mangled on first claim).
#[test]
fn method_call_to_emitted_inherent_desugars_to_registered_free_fn() {
    let rust = "\
pub struct Foo {\n\
    x: u8,\n\
}\n\
impl Foo {\n\
    pub fn get(&self) -> u8 { self.x }\n\
}\n\
pub fn call(f: Foo) -> u8 { f.get() }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("failed to parse/transpile G-β happy-path fixture: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "call"),
        "`call` must emit (registered bare `get` is a proven free-fn referent): emitted={:?} \
         gaps={:?}",
        report.emitted_items,
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
    assert!(
        myc.contains("= get(f)") || myc.contains("= get(f);") || myc.contains("get(f)"),
        "expected free-fn desugar `get(f)` for the registered inherent, got:\n{myc}"
    );
    assert!(
        !myc.contains(&crate::reserved::mangled_inherent_fn_name("Foo", "get")),
        "first bare self-method claim stays un-mangled at the call site too, got:\n{myc}"
    );
}

// --- DN-133 (M-1094) — qualified/associated-function call-site emission ------------------------
//
// `visit_call`'s resolution-gated mangled-call arm: `Type::method(...)` emits the mangled
// `Type__method(args)` call ONLY when that exact declaration is a PROVEN-emitted no-`self`
// inherent-impl associated fn (same-file, or a resolved M-1084 cross-nodule sibling); otherwise an
// honest gap, never a fabricated bare-last-segment call (the D4 lesson, G2/VR-5).

/// The core positive case: a receiver-less associated fn (`Foo::new`) is declared EARLIER in the
/// file, and a LATER free fn calls it qualified (`Foo::new(x)`) — this file's own single
/// left-to-right pass already recorded the real emission by the time the call is reached, so it
/// resolves to the mangled `Foo__new(x)` call (never the fabricated bare `new(x)`).
#[test]
fn qualified_call_to_same_file_associated_fn_resolves_and_mangles() {
    let rust = "pub struct Foo(u32);\n\
                impl Foo {\n\
                    pub fn new(x: u32) -> Self { Foo(x) }\n\
                }\n\
                pub fn make(x: u32) -> Foo { Foo::new(x) }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the same-file fixture: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "make"),
        "`make` must emit (the qualified call must resolve, not gap): {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
    let foo_mangled = crate::reserved::mangled_inherent_fn_name("Foo", "new");
    assert!(
        myc.contains(&format!("{foo_mangled}(x)")),
        "expected the mangled call `{foo_mangled}(x)` in the emitted text, got:\n{myc}"
    );
    assert!(
        !myc.contains("= new(x)") && !myc.contains(" new(x)"),
        "must never emit the fabricated bare-last-segment call `new(x)`, got:\n{myc}"
    );
}

/// `Self::method(...)` resolves via the already-threaded enclosing impl type: a SECOND
/// receiver-less method in the same impl block calls an EARLIER sibling method (declaration
/// order within the impl) via `Self::new(...)`.
#[test]
fn qualified_call_via_self_resolves_within_own_impl() {
    let rust = "pub struct Foo(u32);\n\
                impl Foo {\n\
                    pub fn new(x: u32) -> Self { Foo(x) }\n\
                    pub fn default_new() -> Self { Self::new(0u32) }\n\
                }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the `Self::` fixture: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "impl Foo"),
        "impl Foo must emit: {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
    let new_m = crate::reserved::mangled_inherent_fn_name("Foo", "new");
    let default_m = crate::reserved::mangled_inherent_fn_name("Foo", "default_new");
    assert!(
        myc.contains(&format!("{new_m}(")) && myc.contains(&format!("{default_m}(")),
        "expected both mangled decls in the emitted text, got:\n{myc}"
    );
    // The `Self::new(0u32)` call site itself must resolve to the mangled name, not gap.
    assert!(
        !report.gaps.iter().any(|g| g
            .reason
            .contains("did not resolve to a known-emitted associated fn")),
        "the `Self::new(...)` call must resolve, not gap: gaps={:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// A FORWARD reference — the call site appears BEFORE the impl block in file order — stays gapped:
/// this file's single left-to-right pass has not yet observed `Foo::new`'s real emission at the
/// point `make`'s body is emitted, so the same-file tier correctly does NOT resolve it (never a
/// prediction, only an observed fact — VR-5/G2). A real, honest boundary, not a bug: closing it
/// would require a second file-local pass (out of this leaf's scope; the M-1084 cross-nodule tier
/// is exactly this kind of two-pass mechanism, applied across FILES).
#[test]
fn qualified_call_forward_reference_still_gaps() {
    let rust = "pub struct Foo(u32);\n\
                pub fn make(x: u32) -> Foo { Foo::new(x) }\n\
                impl Foo {\n\
                    pub fn new(x: u32) -> Self { Foo(x) }\n\
                }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the forward-reference fixture: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "make"),
        "`make` must stay gapped (forward reference, never resolved): emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        report.gaps.iter().any(|g| g
            .reason
            .contains("did not resolve to a known-emitted associated fn")),
        "expected the DN-133 resolution-gap reason, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// An unresolved type (never declared/impl'd anywhere in this file) gaps — never a guess.
#[test]
fn qualified_call_to_unknown_type_gaps() {
    let rust = "fn make(x: u32) -> u32 { Bar::create(x) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the unknown-type fixture: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "make"),
        "`make` must stay gapped: emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        report.gaps.iter().any(|g| g
            .reason
            .contains("did not resolve to a known-emitted associated fn")),
        "expected the DN-133 resolution-gap reason, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// A primitive/std associated fn (no emitted decl — e.g. `i128::try_from`) always gaps: it falls
/// out naturally from the resolution gate (no impl ever mangles a primitive), not a special case.
#[test]
fn primitive_associated_fn_call_always_gaps() {
    let rust = "fn conv(x: u32) -> u32 { i128::try_from(x) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the primitive fixture: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "conv"),
        "`conv` must stay gapped: emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        report.gaps.iter().any(|g| g
            .reason
            .contains("did not resolve to a known-emitted associated fn")),
        "expected the DN-133 resolution-gap reason, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// A `self`-receiving method's bare name is NEVER mangled (`self_receiving_inherent_method_is_left_unmangled`
/// above), so a UFCS-style qualified call naming one (`Foo::get(v)`) never resolves either — no
/// false positive just because the type/method names happen to match a real declaration.
#[test]
fn qualified_call_naming_a_self_receiving_method_still_gaps() {
    let rust = "pub struct Foo(u32);\n\
                impl Foo {\n\
                    pub fn get(&self) -> u32 { self.0 }\n\
                }\n\
                pub fn call_it(v: Foo) -> u32 { Foo::get(v) }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the self-method-UFCS fixture: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "call_it"),
        "`call_it` must stay gapped (a `self`-receiving method is never mangled, so its \
         qualified-call form never resolves): emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
}

/// Regression: a cross-*module* free-function path (`a::b::c()`, 3+ segments) is unaffected by
/// this DN-133 arm — it stays exactly the pre-existing gap (out of this leaf's scope, DN-133 §2
/// sub-kind 3; routes through the Import/symtab free-fn resolver instead).
#[test]
fn cross_module_free_fn_path_gap_is_unchanged() {
    let rust = "fn f() -> u32 { a::b::c() }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the free-fn-path fixture: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "f"),
        "`f` must stay gapped: emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.reason.contains("no established Mycelium surface form")),
        "expected the pre-existing qualified-call gap reason (unchanged), got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// **The verify-first proof** (mitigation #14) for DN-133/M-1094: the resolved mangled call is run
/// through the REAL `myc-check` oracle (T-A3 — "emit iff check accepts"), proving the emitted
/// `Foo__new(x)` call actually type-checks against the mangled decl, exactly like
/// `inherent_impl_no_self_name_collision_is_mangled_and_checks_clean` proves the DECL side alone.
/// Skips gracefully (never fails) when `myc-check` is not built.
#[test]
fn qualified_call_resolved_mangled_check_clean_against_real_toolchain() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "emit: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text \
             assertions above still cover the emitted shape."
        );
        return;
    };

    let rust = "pub struct Foo(u32);\n\
                pub struct Bar(u32);\n\
                impl Foo {\n\
                    pub fn new(x: u32) -> Self { Foo(x) }\n\
                }\n\
                impl Bar {\n\
                    pub fn new(x: u32) -> Self { Bar(x) }\n\
                }\n\
                pub fn make_foo(x: u32) -> Foo { Foo::new(x) }\n\
                pub fn make_bar(x: u32) -> Bar { Bar::new(x) }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the two-type fixture: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "make_foo")
            && report.emitted_items.iter().any(|n| n == "make_bar"),
        "both qualified calls must resolve and emit: {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
    let foo_call = format!(
        "{}(x)",
        crate::reserved::mangled_inherent_fn_name("Foo", "new")
    );
    let bar_call = format!(
        "{}(x)",
        crate::reserved::mangled_inherent_fn_name("Bar", "new")
    );
    assert!(
        myc.contains(&foo_call) && myc.contains(&bar_call),
        "expected both mangled calls in the emitted text (never cross-wired to the wrong type), \
         got:\n{myc}"
    );

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-qualcall-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("case.myc");
    std::fs::write(&path, &myc).expect("write case .myc");
    let checker = crate::vet::MycChecker {
        command: vec![bin.display().to_string()],
        cwd: None,
    };
    let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
    assert_eq!(
        rec.class,
        crate::vet::VetClass::Clean,
        "the resolved mangled-call nodule must check CLEAN with the real myc-check oracle \
         (T-A3: emit iff check accepts) — emitted:\n{myc}\ndiagnostic={:?}",
        rec.diagnostic
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// --- Regression (this leaf): a generic-argument self type must NEVER reach `{Type}__{method}` --
//
// DN-131 (M-1088/M-1101) taught `emit_impl` to accept an impl-level generic parameter instead of
// gapping the whole block — but the D4 mangler (above) was never updated for it: `self_ty_text` for
// a generic self type maps to bracketed text (`map.rs`'s `PathArguments::AngleBracketed` arm,
// `"{name}[{args}]"`), and splicing that into `{Type}__{method}` produced an INVALID Mycelium
// identifier (`Foo[T]__method` — brackets in an identifier position) — a HARD PARSE FAILURE the
// moment `myc check` (or any Mycelium parser) reads the emission, poisoning the whole containing
// file under the vet loop's file-gated `checked_fraction` (G2 violation). Reachable from BOTH an
// impl-level generic (`impl<T> Foo<T>`) and a concrete monomorphized inherent impl with no
// impl-level generics of its own (`impl Foo<Concrete>`) — both fixtures below pin the fix for each
// shape. `self_ty_is_generic_application` gaps the affected method instead (never a fabricated
// base-name-only mangle either — see that function's doc for the real cross-instantiation collision
// hazard that would reintroduce).

/// The exact regression shape named by the task: `impl<T> DeclaredTime<T> { pub fn new(..) }`
/// (mirroring `mycelium-std-time`'s real `DeclaredTime<T>` — DN-131 unblocked this impl-level
/// generic from its prior blanket gap). Before the fix, `new` (no `self` receiver) would mangle to
/// the invalid identifier `DeclaredTime[T]__new`; after the fix it is an honest per-method gap —
/// the struct's own `type_item` still emits (DN-131 didn't regress that), but the impl's sole
/// method gaps rather than producing unparseable text.
#[test]
fn impl_level_generic_self_type_ctor_gaps_never_emits_invalid_identifier() {
    let rust = "pub struct DeclaredTime<T>(T);\n\
                impl<T> DeclaredTime<T> {\n\
                    pub fn new(inner: T) -> Self { DeclaredTime(inner) }\n\
                }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the generic-self-type fixture: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "DeclaredTime"),
        "the struct's own type_item must still emit (DN-131 didn't regress this): {:?} \
         (gaps={:?})",
        report.emitted_items,
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
    assert!(
        report
            .emitted_items
            .iter()
            .any(|n| n.contains("impl") && n.contains("DeclaredTime")),
        "the impl should emit with DN-140 mangling: {:?}",
        report.emitted_items
    );
    assert!(
        !myc.contains("[T]__") && !myc.contains("]__"),
        "the emitted .myc text must NEVER contain the invalid bracketed-identifier shape \
         `Foo[T]__method` (a hard parse failure, G2), got:\n{myc}"
    );
    assert!(
        myc.contains("DeclaredTime_u5B_T_u5D_"),
        "generic self type text must be escaped in the mangled fn name, got:\n{myc}"
    );
}

/// The second reachable shape: a CONCRETE monomorphized inherent impl (`impl Foo<Concrete>`) whose
/// impl block itself declares NO generic parameters — only the self type's own generic argument is
/// generic-application-shaped. This is a real, legitimate Rust pattern (a type-specific inherent
/// impl of a generic type, e.g. `impl Approx<f64> { .. }` in `mycelium-std-math`) and was NOT
/// caught by the pre-existing impl-level-generics gap (that check only looks at `item.generics`,
/// which is empty here) — it is a distinct trigger from the impl-level-generic case above, so this
/// fixture pins it separately.
#[test]
fn concrete_generic_instantiation_self_type_ctor_gaps_never_emits_invalid_identifier() {
    let rust = "pub struct Wrapper<T>(T);\n\
                pub struct Inner(u32);\n\
                impl Wrapper<Inner> {\n\
                    pub fn new(inner: Inner) -> Self { Wrapper(inner) }\n\
                }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle").unwrap_or_else(|e| {
        panic!("failed to parse/transpile the concrete-instantiation fixture: {e}")
    });
    assert!(
        report.emitted_items.iter().any(|n| n == "Wrapper")
            && report.emitted_items.iter().any(|n| n == "Inner"),
        "both struct type_items must still emit: {:?} (gaps={:?})",
        report.emitted_items,
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
    assert!(
        report
            .emitted_items
            .iter()
            .any(|n| n.starts_with("impl Wrapper")),
        "the impl should emit with DN-140 mangling: {:?}",
        report.emitted_items
    );
    assert!(
        !myc.contains("[Inner]__") && !myc.contains("]__"),
        "the emitted .myc text must NEVER contain the invalid bracketed-identifier shape \
         `Wrapper[Inner]__new` (a hard parse failure, G2), got:\n{myc}"
    );
    assert!(
        myc.contains("Wrapper_u5B_Inner_u5D_"),
        "concrete generic self type must be escaped in mangled fn name, got:\n{myc}"
    );
}

/// **Decl/call consistency** (mitigation #14's "check both sides"): a qualified call
/// `DeclaredTime::new(x)` naming the now-gapped generic-self-type constructor must ALSO gap —
/// never resolve to a stale/mismatched mangled name. This is automatic by construction (the decl
/// side never calls `record_local_mangled_assoc_fn` for the gapped method, so the call side's
/// `local_mangled_assoc_fn_known` lookup correctly misses) rather than requiring a matching change
/// on the call side (`emit/calls/qualified_assoc.rs`) — this test pins that the automatic
/// consistency actually holds, not just that it should in theory.
#[test]
fn qualified_call_to_generic_self_type_ctor_gaps_consistently() {
    let rust = "pub struct DeclaredTime<T>(T);\n\
                impl<T> DeclaredTime<T> {\n\
                    pub fn new(inner: T) -> Self { DeclaredTime(inner) }\n\
                }\n\
                pub fn make(x: u32) -> DeclaredTime<u32> { DeclaredTime::new(x) }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the call-site fixture: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "make"),
        "`make` must stay gapped (the constructor it calls was never validly mangled/recorded): \
         emitted={:?} myc:\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("[T]__") && !myc.contains("]__") && !myc.contains("[u32]"),
        "the emitted .myc text must never contain an invalid bracketed identifier or a \
         fabricated mismatched call, got:\n{myc}"
    );
    assert!(
        report.gaps.iter().any(|g| g
            .reason
            .contains("did not resolve to a known-emitted associated fn")),
        "expected the DN-133 resolution-gap reason on the call site, got {:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
}

/// **The verify-first proof** (mitigation #14) — the live-oracle, real-toolchain regression proof:
/// before this leaf's fix, the emitted text for `impl<T> DeclaredTime<T> { pub fn new(..) }`
/// contained the invalid identifier `DeclaredTime[T]__new`, which `myc-check` rejects with a
/// **`parse-error`** (not a `check-error` — see this leaf's report for the exact repro/diagnostic).
/// After the fix, the method gaps instead, so the emitted `.myc` (just the struct's `type_item`)
/// must check CLEAN — proving the hard-parse-failure regression is closed, not just that the Rust
/// text assertions above look right. Skips gracefully (never fails) when `myc-check` is not built.
#[test]
fn generic_self_type_ctor_gap_checks_clean_never_parse_error() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "emit: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text \
             assertions above still cover the emitted shape."
        );
        return;
    };

    let rust = "pub struct DeclaredTime<T>(T);\n\
                impl<T> DeclaredTime<T> {\n\
                    pub fn new(inner: T) -> Self { DeclaredTime(inner) }\n\
                }\n";
    let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
        .unwrap_or_else(|e| panic!("failed to parse/transpile the generic-self-type fixture: {e}"));

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-generic-self-mangle-regression-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("case.myc");
    std::fs::write(&path, &myc).expect("write case .myc");
    let checker = crate::vet::MycChecker {
        command: vec![bin.display().to_string()],
        cwd: None,
    };
    let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
    assert_ne!(
        rec.class,
        crate::vet::VetClass::ParseError,
        "a generic-self-type inherent-impl constructor must NEVER produce a hard parse failure \
         (the DN-34 §8.13/8.14 D4 regression this leaf fixes) — emitted:\n{myc}\ngaps={:?}\n\
         diagnostic={:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>(),
        rec.diagnostic
    );
    assert_eq!(
        rec.class,
        crate::vet::VetClass::Clean,
        "the gapped-method nodule (just the struct's own type_item) must check CLEAN with the \
         real myc-check oracle — emitted:\n{myc}\ndiagnostic={:?}",
        rec.diagnostic
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// --- trx2 A1: `Expr::Cast` fidelity matrix (DN-34 §8.18) ---------------------------------------
//
// Rust `as` is lossy/wrapping/saturating/rounding by design; Mycelium's conversion prims are
// checked/refusing by design. This data-driven table pins that fidelity boundary at the
// gap-reason level (which the `cases()` table's `Expect` cannot express — it asserts a `Category`,
// not the FLAG reason). Drives `emit_expr` directly so a case can seed the operand's `TypeEnv`
// type precisely (a bare in-scope identifier is the only shape whose type the emitter resolves
// without guessing — see `expr_env_type`).

/// The expected outcome for one `Expr::Cast` fidelity case.
enum CastExpect {
    /// Emits this exact `.myc` text (faithful, `myc check`-clean).
    Emits(&'static str),
    /// Gaps with a reason containing this substring (the never-silent, honest refusal — G2/VR-5).
    GapReasonContains(&'static str),
}

/// One cast case: the operand-name -> mapped-type-text env seed, the Rust cast source, the outcome.
struct CastCase {
    name: &'static str,
    env: &'static [(&'static str, &'static str)],
    src: &'static str,
    expect: CastExpect,
}

fn cast_cases() -> Vec<CastCase> {
    use CastExpect::{Emits, GapReasonContains};
    vec![
        // WIDEN (`u16 as u32`, Binary{16} -> Binary{32}, M >= N): the one decidable-faithful slice —
        // `width_cast` zero-extends (unsigned), matching Rust's unsigned widening exactly (DN-41 §3).
        CastCase {
            name: "widen_u16_as_u32_emits_width_cast",
            env: &[("x", "Binary{16}")],
            src: "x as u32",
            expect: Emits("width_cast(x, 0b0000_0000_0000_0000_0000_0000_0000_0000)"),
        },
        // IDENTITY (`x as u32` where x is already Binary{32}, M == N): width_cast is identity here —
        // still faithful, still emitted.
        CastCase {
            name: "identity_u32_as_u32_emits_width_cast",
            env: &[("x", "Binary{32}")],
            src: "x as u32",
            expect: Emits("width_cast(x, 0b0000_0000_0000_0000_0000_0000_0000_0000)"),
        },
        // NARROW (`u32 as u16`, Binary{32} -> Binary{16}, M < N): Rust WRAPS (low 16 bits);
        // `width_cast` would REFUSE on overflow (not faithful), but `truncate` (DN-51 §2 D3, now
        // landed — maintainer-authorized DN-39 post-freeze promotion) unconditionally keeps the low
        // `M` bits — an exact match, so this now emits rather than FLAGging.
        CastCase {
            name: "narrow_u32_as_u16_emits_truncate",
            env: &[("x", "Binary{32}")],
            src: "x as u16",
            expect: Emits("truncate(x, 0b0000_0000_0000_0000)"),
        },
        // FLOAT->INT (`f64 as i32`): operand is `Float`, so this is CU-3 territory regardless of the
        // (signed) target — Rust saturates, `flt.to_bin` refuses; no faithful prim, gap CU-3.
        CastCase {
            name: "float_to_int_f64_as_i32_gaps_cu3",
            env: &[("x", "Float")],
            src: "x as i32",
            expect: GapReasonContains("PENDING-DESIGN(CU-3-fidelity)"),
        },
        // INT->FLOAT (`i64 as f64`): the target is a float, so this routes to CU-3 regardless of the
        // operand. (`i64` does not map to any `Binary{N}` — signed magnitude, `map_type` gaps — so it
        // is absent from the env; the target-float route gives the CU-3 gap, not the unknown-operand
        // one.) Rust rounds; `bin.to_flt` errs |n| > 2^53; no faithful prim, gap CU-3.
        CastCase {
            name: "int_to_float_i64_as_f64_gaps_cu3",
            env: &[],
            src: "x as f64",
            expect: GapReasonContains("PENDING-DESIGN(CU-3-fidelity)"),
        },
        // UNKNOWN OPERAND (`foo() as u32`): the operand is a call, not a bare in-scope identifier, so
        // its type is unknown — refuse rather than guess it (VR-5), and no float is involved.
        CastCase {
            name: "unknown_operand_call_gaps_never_guesses",
            env: &[],
            src: "foo() as u32",
            expect: GapReasonContains("operand type unknown"),
        },
    ]
}

#[test]
fn expr_cast_fidelity() {
    for c in cast_cases() {
        let expr: syn::Expr = syn::parse_str(c.src)
            .unwrap_or_else(|e| panic!("case `{}`: failed to parse `{}`: {e}", c.name, c.src));
        let mut env = TypeEnv::new();
        for (k, v) in c.env {
            env.insert((*k).to_string(), (*v).to_string());
        }
        match (c.expect, emit_expr(&expr, None, &env)) {
            (CastExpect::Emits(want), Ok(text)) => {
                assert_eq!(text, want, "case `{}`: emitted text mismatch", c.name)
            }
            (CastExpect::Emits(want), Err(g)) => panic!(
                "case `{}`: expected emit `{want}`, got gap: {}",
                c.name, g.reason
            ),
            (CastExpect::GapReasonContains(sub), Err(g)) => assert!(
                g.reason.contains(sub),
                "case `{}`: gap reason did not contain `{sub}`; got: {}",
                c.name,
                g.reason
            ),
            (CastExpect::GapReasonContains(sub), Ok(text)) => panic!(
                "case `{}`: expected a gap containing `{sub}`, got emit `{text}`",
                c.name
            ),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// DN-122 §13 (M-1080; WU-A) — the MVP foreign-trait-impl live-oracle proof (T-A1/T-A2/T-A3).
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// **T-A1 (positive control) + T-A3 (emit<->check agreement), against the REAL toolchain.** The
/// fixture-corpus cases above (`mvp_cmp_eligible_synthesizes_trait_arg`,
/// `mvp_widen_unaffected_by_mvp_recognizer`, `mvp_cmp_self_receiver_excluded_no_bracket`) prove the
/// emitted *text*; this proves the emitter's eligibility judgment agrees with what `myc check`
/// actually accepts — never a `[<SelfTy>]` bracket for a shape the checker would refuse, and never
/// a missed bracket for a shape that would otherwise check clean. Skips gracefully (never fails)
/// when `myc-check` is not built, exactly like `src/tests/vet.rs`'s live-oracle tests.
#[test]
fn mvp_cmp_emit_check_agreement() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "emit: DN-122/M-1080 MVP live oracle test skipped — no runnable myc-check (set \
             MYC_CHECK_CMD or build `cargo build -p mycelium-check --bin myc-check`). The \
             fixture-corpus text assertions above still cover the emitted shape."
        );
        return;
    };

    struct AgreementCase {
        name: &'static str,
        rust: &'static str,
        /// Whether the emitted `.myc` carries the MVP-synthesized `[<SelfTy>]` bracket for `Ord3`.
        expect_bracket: bool,
        /// Whether the real `myc-check` oracle accepts the emitted file clean.
        expect_clean: bool,
    }
    let cases = [
        // T-A1: single-param, param-only-sig, receiverless — MVP-eligible, checks clean.
        AgreementCase {
            name: "eligible_cmp",
            rust: "impl Ord3 for u8 { fn cmp(a: Self, b: Self) -> u8 { a } }",
            expect_bracket: true,
            expect_clean: true,
        },
        // T-A2: `Widen` (two-type/`Self`-receiver-needing) — unaffected by the MVP recognizer,
        // stays an honest `myc check`-time residual (M-876/M-1076), exactly as before WU-A.
        AgreementCase {
            name: "widen_stays_a_residual",
            rust: "impl Widen<u16> for u8 { fn widen(self) -> u16 { u16::from(self) } }",
            expect_bracket: false,
            expect_clean: true,
        },
        // T-A3: `self`-receiver `Ord3` impl — correctly excluded (no bracket); the checker refuses
        // it too (`cmp_used` still seeds the prelude trait since the impl NAMES `Ord3`, so the
        // checker's own arity/shape enforcement — not an "unknown trait" gap — is what fires here;
        // either way, never a silent accept).
        AgreementCase {
            name: "self_receiver_excluded_and_checker_agrees",
            rust: "impl Ord3 for u8 { fn cmp(self, other: Self) -> u8 { self } }",
            expect_bracket: false,
            expect_clean: false,
        },
        // T-A3: wrong arity (`Ord3::cmp` takes exactly 2 params in the prelude) — excluded, and the
        // checker's `register_instances` arity guard refuses it too.
        AgreementCase {
            name: "wrong_arity_excluded_and_checker_agrees",
            rust: "impl Ord3 for u8 { fn cmp(a: Self) -> u8 { a } }",
            expect_bracket: false,
            expect_clean: false,
        },
    ];

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-mvp-cmp-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    for (i, case) in cases.iter().enumerate() {
        let (myc, report) = transpile_source(case.rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("case `{}` failed to parse/transpile: {e}", case.name));
        assert!(
            !report.emitted_items.is_empty(),
            "case `{}` failed to emit at all: gaps={:?}",
            case.name,
            report.gaps
        );
        let has_bracket = myc.contains("Ord3[Binary{8}] for Binary{8}");
        assert_eq!(
            has_bracket, case.expect_bracket,
            "case `{}`: MVP-bracket presence mismatch; emitted:\n{myc}",
            case.name
        );

        let path = dir.join(format!("case_{i}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");
        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        let is_clean = rec.class == crate::vet::VetClass::Clean;
        assert_eq!(
            is_clean, case.expect_clean,
            "case `{}`: emit<->check agreement violated — emitter's bracket judgment ({}) must \
             agree with the real checker's verdict; diagnostic={:?}\nemitted:\n{myc}",
            case.name, case.expect_bracket, rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------------------
// DN-128 (M-1086) — std-derive lowering library, struct scope. Regression + negative-assertion
// tests beyond the data-driven `cases()` fixtures above (a nested-composition case and the
// "never fabricates" negative checks need multi-item sources / substring-absence assertions the
// `Case`/`Expect` shape does not carry).
// ---------------------------------------------------------------------------------------------

/// A gapped `derive(Debug)`/`derive(Default)` (the primitive-field case) must NEVER leak a
/// fabricated `impl Show[...]`/`impl Init[...]` fragment into the `.myc` text (G2 — mirrors
/// `widen_bool_from_call_produces_no_fabricated_myc_text`'s pattern). **DN-138 WU-4 note:** a
/// narrow `u8`/`Binary{8}` field is NO LONGER a gapping fixture (WU-4's `width_cast`/literal-zero
/// unblock composes it — see `derive_forms_check_clean_against_real_toolchain`'s `Narrow` case);
/// `f64`/`Float` is the fixture that stays a genuine, disclosed gap for EVERY row (ADR-040).
#[test]
fn derive_gap_never_leaks_partial_impl_text() {
    let (myc_debug, _) = transpile_source("#[derive(Debug)]\nstruct Pair(f64, bool);", "f.rs", "f")
        .expect("parses/transpiles");
    assert!(
        !myc_debug.contains("impl Show"),
        "a gapped derive(Debug) must never emit a partial `impl Show`, got:\n{myc_debug}"
    );
    let (myc_default, _) =
        transpile_source("#[derive(Default)]\nstruct Pair(f64, bool);", "f.rs", "f")
            .expect("parses/transpiles");
    assert!(
        !myc_default.contains("impl Init"),
        "a gapped derive(Default) must never emit a partial `impl Init`, got:\n{myc_default}"
    );
}

/// `derive(Clone)`/`derive(Copy)` (DN-128 §6.1's satisfied no-op) must never emit ANY impl text —
/// there is no Mycelium `Clone`/`Copy` trait to implement, so the honest answer is "nothing to
/// generate", not a stand-in impl.
#[test]
fn derive_clone_copy_never_emits_an_impl() {
    let (myc, _) = transpile_source("#[derive(Clone, Copy)]\nstruct Flag(bool);", "f.rs", "f")
        .expect("parses/transpiles");
    assert!(
        !myc.contains("impl "),
        "derive(Clone)/derive(Copy) must never emit any impl block, got:\n{myc}"
    );
}

/// The DN-128 §2 "structural fold over fields" shape composes end-to-end for a struct whose
/// fields are themselves ANOTHER derived (fieldless) struct in the SAME file — the one
/// field-eligible case [`crate::emit::derives::field_derive_kind`]'s docs describe as mechanically sound and not
/// merely `Declared`-hopeful. Both `Debug` and `Default` are exercised; the live-oracle half
/// (that this text is real `myc check`-clean, not just textually plausible) is
/// `derive_forms_check_clean_against_real_toolchain` below.
#[test]
fn derive_composes_end_to_end_over_a_same_file_nested_derived_field() {
    let rust = "#[derive(Debug, Default)]\nstruct Inner;\n\
                #[derive(Debug, Default)]\nstruct Outer(Inner, Inner);";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture").expect("parses/transpiles");
    for name in ["Inner", "Outer"] {
        assert!(
            report.emitted_items.iter().any(|n| n == name),
            "expected `{name}` in emitted_items, got {:?}",
            report.emitted_items
        );
    }
    assert!(
        myc.contains("impl Show[Inner] for Inner"),
        "expected Inner's derived Show impl, got:\n{myc}"
    );
    assert!(
        myc.contains("impl Init[Inner] for Inner"),
        "expected Inner's derived Init impl, got:\n{myc}"
    );
    assert!(
        myc.contains(
            "impl Show[Outer] for Outer {\n  fn render(x: Outer) => Bytes =\n    \
             match x { Outer(p0, p1) => bytes_concat(bytes_concat(bytes_concat(bytes_concat(\"Outer(\", \
             render(p0)), \", \"), render(p1)), \")\") };\n};"
        ),
        "expected Outer's field-walked Show impl body, got:\n{myc}"
    );
    assert!(
        myc.contains(
            "impl Init[Outer] for Outer {\n  fn init() => Outer =\n    Outer(init(), init());\n};"
        ),
        "expected Outer's field-walked Init impl body, got:\n{myc}"
    );
    // Neither struct's `derive` sub-gaps land as a real `DeriveAttr` gap for THIS composition
    // (only the pre-existing sub_gap machinery for OTHER, unrelated constructs would) — a
    // successful compose adds no sub-gap at all (see `lower_struct_derives` docs).
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "a fully-eligible nested derive must not record any DeriveAttr gap, got {:?}",
        report.gaps
    );
}

/// **DN-136 §8 invariant witness #3 (mixed derive, per-derive-independence across the set).** A
/// derive list mixing a COMPOSABLE rule (`Debug`) with an UNRECOGNIZED one (`Serialize`) must
/// compose the eligible derive AND sub-gap the rest — the item still emits BOTH the struct's own
/// `type` declaration and the composed `impl Show`, never gapping the whole item just because a
/// sibling derive in the same list didn't compose. Pins the `lower_struct_derives`
/// (`crate::emit`, the DN-136/P1-a driver) orchestration this axis's migration must not move into
/// a row (DN-136 §3 item 2 / §7 / §8 point 2(e)).
#[test]
fn derive_mixed_set_composes_eligible_and_sub_gaps_the_rest_item_still_emits() {
    let rust = "#[derive(Debug, Serialize)]\nstruct OsEntropy;";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture").expect("parses/transpiles");
    assert!(
        report.emitted_items.iter().any(|n| n == "OsEntropy"),
        "the item must still emit despite a sibling derive being unrecognized, got {:?}",
        report.emitted_items
    );
    assert!(
        myc.contains("type OsEntropy = OsEntropy;"),
        "the struct's own type decl must still emit, got:\n{myc}"
    );
    assert!(
        myc.contains("impl Show[OsEntropy] for OsEntropy"),
        "the composable Debug->Show impl must still compose despite the sibling Serialize \
         derive being unrecognized, got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr && g.reason.contains("Serialize")),
        "the unrecognized Serialize derive must still be recorded as a sub-gap, got {:?}",
        report.gaps
    );
}

/// **The verify-first proof** (mitigation #14) for DN-128 (M-1086): every derive shape the fixture
/// corpus above proves the *text* of is run through the REAL `myc-check` oracle here — the fieldless
/// `Debug`/`Default` cases, and the same-file nested-composition case
/// ([`derive_composes_end_to_end_over_a_same_file_nested_derived_field`]'s text). Skips gracefully
/// (never fails) when `myc-check` is not built, exactly like `struct_pattern_forms_check_clean_
/// against_real_toolchain` above.
#[test]
fn derive_forms_check_clean_against_real_toolchain() {
    let Some(bin) = super::vet::find_myc_check() else {
        eprintln!(
            "emit: live oracle test skipped — no runnable myc-check (set MYC_CHECK_CMD or build \
             `cargo build -p mycelium-check --bin myc-check`). The fixture-corpus text assertions \
             above still cover the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-derive-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let rust_snippets = [
        // Fieldless `Debug` -- the primary "make sure this case emits clean" deliverable.
        ("#[derive(Debug)]\nstruct OsEntropy;", "OsEntropy"),
        // Fieldless `Default`.
        ("#[derive(Default)]\nstruct OsEntropy;", "OsEntropy"),
        // Fieldless struct deriving BOTH in one attribute list.
        ("#[derive(Debug, Default)]\nstruct OsEntropy;", "OsEntropy"),
        // The same-file nested-composition case (both items, both derives).
        (
            "#[derive(Debug, Default)]\nstruct Inner;\n\
             #[derive(Debug, Default)]\nstruct Outer(Inner, Inner);",
            "Outer",
        ),
        // DN-136 Phase-2 (DERIVE-COMPLETION) -- fieldless `PartialEq`, alone.
        ("#[derive(PartialEq)]\nstruct OsEntropy;", "OsEntropy"),
        // `PartialEq` + `Eq` together (the common real-Rust pair; must compose exactly once --
        // `derive_eq_recognizes_only_partialeq_avoids_duplicate_fn` below pins the "exactly once"
        // half directly).
        ("#[derive(PartialEq, Eq)]\nstruct OsEntropy;", "OsEntropy"),
        // Fieldless `PartialOrd`, alone.
        ("#[derive(PartialOrd)]\nstruct OsEntropy;", "OsEntropy"),
        // `PartialOrd` + `Ord` together (the common real-Rust pair).
        ("#[derive(PartialOrd, Ord)]\nstruct OsEntropy;", "OsEntropy"),
        // Fieldless `Hash`.
        ("#[derive(Hash)]\nstruct OsEntropy;", "OsEntropy"),
        // The realistic full derive stack real Rust code commonly writes on one struct.
        (
            "#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]\n\
             struct OsEntropy;",
            "OsEntropy",
        ),
        // The same-file nested-composition case for the Phase-2 derives (mirrors the Debug/
        // Default case above).
        (
            "#[derive(PartialEq, PartialOrd, Hash)]\nstruct Inner;\n\
             #[derive(PartialEq, PartialOrd, Hash)]\nstruct Outer(Inner, Inner);",
            "Outer",
        ),
        // DN-138 (increment 1, the DeriveAttr-class scalar/Bytes/Bool unblock) -- the exact corpus
        // shape (`u64`/`String`/`bool` fields, the real `CheckError`/`CtorInfo`/`EvaluatorOpts`
        // non-`Vec` field mix) now composes all four field-gating rows over ONE struct with zero
        // DeriveAttr gaps.
        (
            "#[derive(Debug, Default, PartialEq, PartialOrd)]\nstruct Rec(u64, String, bool);",
            "Rec",
        ),
        // DN-138 WU-4 (increment 2): a NARROW scalar (`u8`, not the seeded `Binary{64}`) now
        // composes for the trait-dispatched rows too, via a `width_cast` up to `Binary{64}`
        // (`Show`/`Ord3`) or a literal-zero-at-width (`Init`) -- no longer just `PartialEq`.
        (
            "#[derive(Debug, Default, PartialEq, PartialOrd)]\nstruct Narrow(u8);",
            "Narrow",
        ),
        // DN-138 WU-4 (increment 2): `Hash` now composes over a `ScalarBinary` field too, via the
        // new `bin_to_bytes` raw-byte prim (`hash_blake3(bin_to_bytes(p))`).
        (
            "#[derive(Hash)]\nstruct HashableRec(String, bool, u64);",
            "HashableRec",
        ),
        // DN-138 WU-4 (increment 2) -- THE headline corpus-payoff shape: a `Vec<T>` field, the one
        // increment-1 explicitly left gapped in every named corpus struct (`CheckError`/`CtorInfo`/
        // `EvaluatorOpts`). All five field-gating derives compose over a `Vec<u64>` + `String`
        // struct with zero DeriveAttr gaps -- `Show`/`PartialOrd` route the `Vec` field through a
        // per-element auxiliary fn (`show_vec_Binary_64`/`ord_vec_Binary_64`), `Default` seeds
        // `Nil`, `PartialEq`/`Hash` route through `eq_vec_Binary_64`/`hash_vec_Binary_64`.
        (
            "#[derive(Debug, Default, PartialEq, PartialOrd, Hash)]\nstruct WithVec(Vec<u64>, String);",
            "WithVec",
        ),
        // DN-138 WU-4 -- a `Vec` of a `UserNamed` element type (mirrors the real corpus's
        // `CtorInfo.fields: Vec<Ty>` shape once `Ty` itself is derivable) composes too, chaining
        // through the ELEMENT's own same-file derived instance/fns.
        (
            "#[derive(Debug, Default, PartialEq)]\nstruct Elem(u64);\n\
             #[derive(Debug, Default, PartialEq)]\nstruct WithVecOfUser(Vec<Elem>);",
            "WithVecOfUser",
        ),
        // ONESHOT C2 -- unit enum PartialEq + Debug (std-fs Fallibility/FileKind residual).
        (
            "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\nenum Fallibility { Total, OptionFallible, ResultFallible }",
            "Fallibility",
        ),
        // ONESHOT C2 -- reserved-keyword unit variants (Exact -> Exact_kw) + parent struct eq.
        (
            "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\nenum GuaranteeTag { Exact, Proven, Empirical, Declared }\n\
             #[derive(Debug, PartialEq, Eq)]\nstruct MatrixRow(String, GuaranteeTag);",
            "MatrixRow",
        ),
        // ONESHOT C2 -- Bool logical or (std-fs OpenOptions::wants_write residual).
        (
            "fn wants_write(a: bool, b: bool) -> bool { a || b }",
            "wants_write",
        ),
        (
            "fn wants_both(a: bool, b: bool) -> bool { a && b }",
            "wants_both",
        ),
        // DN-138 WU-4 / ORACLE-R1 A5 -- a `u128` field is WIDER than the seeded `Binary{64}`
        // instance: `PartialEq` (width-generic `eq` prim), `Default` (literal zero at own width),
        // and `Debug` (Declared opaque `"<Binary{128}>"` placeholder — never a narrowing
        // width_cast) all compose cleanly; `PartialOrd` still honestly GAPs (see
        // `derive_debug_and_partialord_gap_a_wide_scalar_never_a_runtime_throwing_width_cast`).
        (
            "#[derive(Debug, Default, PartialEq)]\nstruct Wide(u128);",
            "Wide",
        ),
    ];
    for (i, (rust, item)) in rust_snippets.iter().enumerate() {
        let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("failed to parse/transpile `{rust}`: {e}"));
        assert!(
            report.emitted_items.iter().any(|n| n == item),
            "case {i} (`{rust}`) failed to emit `{item}`: gaps={:?}",
            report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
        );
        let path = dir.join(format!("case_{i}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");

        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "case {i} (`{rust}`) must check CLEAN with the real myc-check oracle — emitted:\n{myc}\n\
             diagnostic={:?}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------------------
// DN-136 Phase-2 (DERIVE-COMPLETION, M-1097/M-1098/M-1099) — the `PartialEq`/`PartialOrd`/`Hash`
// additive rows (`emit/derives/{eq,ord,hash}.rs`). Mirrors the DN-128 (M-1086) test shape above
// (fixture-corpus text assertions here; the live-oracle half rides the `rust_snippets` cases
// added to `derive_forms_check_clean_against_real_toolchain` above).
// ---------------------------------------------------------------------------------------------

/// ONESHOT C2 — unit-enum `derive(PartialEq)` co-emits `fn eq_<Enum>` so nested struct eq can
/// resolve (the `eq_Fallibility` / `eq_FileKind` / `eq_GuaranteeTag` residual).
#[test]
fn derive_eq_unit_enum_composes() {
    let (myc, report) = transpile_source(
        "#[derive(PartialEq, Eq)]\nenum Fallibility { Total, OptionFallible, ResultFallible }",
        "f.rs",
        "f",
    )
    .expect("parses/transpiles");
    assert!(
        report.emitted_items.iter().any(|n| n == "Fallibility"),
        "expected Fallibility in emitted_items, got {:?}",
        report.emitted_items
    );
    assert!(
        myc.contains("fn eq_Fallibility(a: Fallibility, b: Fallibility) => Binary{1} =")
            && myc.contains("Total => match b { Total => 0b1, _ => 0b0 }")
            && myc.contains("OptionFallible => match b { OptionFallible => 0b1, _ => 0b0 }")
            && myc.contains("ResultFallible => match b { ResultFallible => 0b1, _ => 0b0 }"),
        "expected structural unit-enum eq arms, got:\n{myc}"
    );
    // Exactly one eq_Fallibility (Eq must not double-emit).
    assert_eq!(
        myc.matches("fn eq_Fallibility").count(),
        1,
        "expected exactly one eq_Fallibility, got:\n{myc}"
    );
}

/// ONESHOT C4 — single-variant unit enum must NOT emit an unreachable `_ => 0b0` arm
/// (std-rand `RngAlgo = Xoshiro256PlusPlus` file-poison after C2; myc-check W7).
#[test]
fn derive_eq_single_variant_unit_enum_is_trivially_true() {
    let (myc, report) = transpile_source(
        "#[derive(PartialEq, Eq, Debug)]\nenum RngAlgo { Xoshiro256PlusPlus }",
        "f.rs",
        "f",
    )
    .expect("parses/transpiles");
    assert!(
        report.emitted_items.iter().any(|n| n == "RngAlgo"),
        "expected RngAlgo emitted, got {:?}",
        report.emitted_items
    );
    assert!(
        myc.contains("fn eq_RngAlgo(a: RngAlgo, b: RngAlgo) => Binary{1} =\n    0b1;"),
        "single-variant unit enum eq must be trivially-true (no unreachable `_`), got:\n{myc}"
    );
    assert!(
        !myc.contains("_ => 0b0"),
        "must not emit unreachable wildcard arm for single-variant enum, got:\n{myc}"
    );
}

/// ONESHOT C4 — single-variant *payload* enum: field-binding match without a wildcard.
#[test]
fn derive_eq_single_variant_payload_enum_no_wildcard() {
    let (myc, _report) = transpile_source(
        "#[derive(PartialEq, Eq)]\nenum BoxU8 { V(u8) }",
        "f.rs",
        "f",
    )
    .expect("parses/transpiles");
    assert!(
        myc.contains("fn eq_BoxU8(a: BoxU8, b: BoxU8) => Binary{1} =")
            && myc.contains("V(p0) => match b { V(q0) => eq(p0, q0) }")
            && !myc.contains("_ => 0b0"),
        "single-variant payload eq must bind fields without `_ => 0b0`, got:\n{myc}"
    );
}

/// ONESHOT C2 — unit-enum `derive(Debug)` co-emits `impl Show[T]` so parent struct Show over
/// enum fields does not poison after eq lands.
#[test]
fn derive_debug_unit_enum_composes_show() {
    let (myc, report) = transpile_source(
        "#[derive(Debug)]\nenum FileKind { File, Directory, Symlink, Other }",
        "f.rs",
        "f",
    )
    .expect("parses/transpiles");
    assert!(report.emitted_items.iter().any(|n| n == "FileKind"));
    assert!(
        myc.contains("impl Show[FileKind] for FileKind {")
            && myc.contains("File => \"File\"")
            && myc.contains("Directory => \"Directory\""),
        "expected unit-enum Show arms, got:\n{myc}"
    );
}

/// ONESHOT C2 — nested enum field: struct PartialEq calls `eq_<Enum>` that the enum's own
/// PartialEq co-emits in the same file.
#[test]
fn derive_eq_enum_nested_in_struct_same_file() {
    let (myc, _report) = transpile_source(
        "#[derive(PartialEq, Eq)]\nenum FileKind { File, Directory }\n\
         #[derive(PartialEq, Eq)]\nstruct Metadata(FileKind, u64);",
        "f.rs",
        "f",
    )
    .expect("parses/transpiles");
    assert!(
        myc.contains("fn eq_FileKind(") && myc.contains("fn eq_Metadata("),
        "expected both enum and struct eq helpers, got:\n{myc}"
    );
    assert!(
        myc.contains("eq_FileKind(p0, q0)"),
        "struct eq must call nested eq_FileKind, got:\n{myc}"
    );
}

/// ONESHOT C2 — Rust `||`/`&&` emit total Bool match folds (not Binary `or`/`and` prims).
#[test]
fn bool_logical_or_and_emit_match_not_bit_prim() {
    let (or_myc, _) =
        transpile_source("fn f(a: bool, b: bool) -> bool { a || b }", "f.rs", "f").expect("parses");
    assert!(
        or_myc.contains("match (a) { True => True, False => (b) }")
            || or_myc.contains("match (a) { True => True, False => (b)}"),
        "expected Bool-or match fold, got:\n{or_myc}"
    );
    assert!(
        !or_myc.contains("||") && !or_myc.contains(" or("),
        "must not emit || glyph or bit-or call, got:\n{or_myc}"
    );
    let (and_myc, _) =
        transpile_source("fn f(a: bool, b: bool) -> bool { a && b }", "f.rs", "f").expect("parses");
    assert!(
        and_myc.contains("match (a) { True => (b), False => False }"),
        "expected Bool-and match fold, got:\n{and_myc}"
    );
    assert!(
        !and_myc.contains("&&"),
        "must not emit && glyph, got:\n{and_myc}"
    );
}

/// Fieldless `derive(PartialEq)` composes the trivially-true `fn eq_T` — mirrors the fieldless
/// `Debug`/`Default` fixture-corpus cases' shape (`cases()` above), pinned directly here since
/// this axis has no dedicated fixture-corpus entry point of its own.
#[test]
fn derive_eq_fieldless_composes() {
    let (myc, report) = transpile_source("#[derive(PartialEq)]\nstruct OsEntropy;", "f.rs", "f")
        .expect("parses/transpiles");
    assert!(
        report.emitted_items.iter().any(|n| n == "OsEntropy"),
        "expected OsEntropy in emitted_items, got {:?}",
        report.emitted_items
    );
    assert!(
        myc.contains("fn eq_OsEntropy(a: OsEntropy, b: OsEntropy) => Binary{1} =\n    0b1;"),
        "expected the fieldless eq fn body, got:\n{myc}"
    );
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "a fully-eligible (fieldless) derive must not record any DeriveAttr gap, got {:?}",
        report.gaps
    );
}

/// A gapped `derive(PartialEq)` (the primitive-field case) must NEVER leak a fabricated `fn eq_*`
/// fragment into the `.myc` text — mirrors `derive_gap_never_leaks_partial_impl_text` (DN-128)
/// exactly, for the new axis (G2).
#[test]
fn derive_eq_gap_never_leaks_partial_fn_text() {
    // DN-138 note: `u8`/`bool` fields alone are NO LONGER a gapping fixture for `PartialEq` — the
    // seeded `Show`/`Init`/`Ord3` unblock (DN-138) also routes `PartialEq` to the width-generic
    // `eq` prim (any `Binary{N}`) and an inline `Bool` match, so BOTH now compose (see
    // `derive_eq_composes_over_scalar_bytes_bool_fields_dn138` below). **DN-138 WU-4 update:** a
    // `Vec<u8>` field is ALSO no longer a gapping fixture (`ScalarBinary` is `PartialEq`-eligible
    // at any width, so `VecOf(ScalarBinary)` composes too — see `WithVec` in
    // `derive_forms_check_clean_against_real_toolchain`). `Vec<f64>` (`VecOf(Float)` — the ONE
    // element kind with no equality route at all, ADR-040) is the fixture that stays a genuine,
    // disclosed gap.
    let (myc, report) = transpile_source(
        "#[derive(PartialEq)]\nstruct Pair(Vec<f64>, bool);",
        "f.rs",
        "f",
    )
    .expect("parses/transpiles");
    assert!(
        !myc.contains("fn eq_"),
        "a gapped derive(PartialEq) must never emit a partial `fn eq_*`, got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr && g.reason.contains("PartialEq")),
        "expected a DeriveAttr gap citing PartialEq, got {:?}",
        report.gaps
    );
}

/// **ADR-040 §2.4 (NaN semantics).** A `Float` field REFUSES the whole `derive(PartialEq)` with a
/// gap reason that names the real cause (NaN/ADR-040), not the generic no-ambient-instance one —
/// the DN-136 Phase-2 worklist's explicit "not just ineligible-repr fields" requirement.
#[test]
fn derive_eq_float_field_refused_with_adr040_citation() {
    let (myc, report) = transpile_source("#[derive(PartialEq)]\nstruct Sample(f64);", "f.rs", "f")
        .expect("parses/transpiles");
    assert!(
        !myc.contains("fn eq_Sample"),
        "a Float-field derive(PartialEq) must never fabricate an equality fn, got:\n{myc}"
    );
    let gap = report
        .gaps
        .iter()
        .find(|g| g.category == Category::DeriveAttr && g.reason.contains("Sample"))
        .unwrap_or_else(|| {
            panic!(
                "expected a DeriveAttr gap for Sample, got {:?}",
                report.gaps
            )
        });
    assert!(
        gap.reason.contains("NaN") && gap.reason.contains("ADR-040"),
        "expected the Float-field gap to cite NaN/ADR-040 specifically, got: {}",
        gap.reason
    );
}

/// **The verified duplicate-fn-avoidance property.** `#[derive(PartialEq, Eq)]` on one struct
/// composes the `fn eq_*` body EXACTLY ONCE (never twice — the empirically-verified
/// `myc-check "duplicate function"` collision `eq.rs`'s module doc documents) and records the
/// bare `Eq` name as an unrecognized sub-gap, exactly like any other never-built derive name
/// falling through the driver's catch-all — never silently absorbed (G2).
#[test]
fn derive_eq_recognizes_only_partialeq_avoids_duplicate_fn() {
    let (myc, report) =
        transpile_source("#[derive(PartialEq, Eq)]\nstruct OsEntropy;", "f.rs", "f")
            .expect("parses/transpiles");
    let occurrences = myc.matches("fn eq_OsEntropy").count();
    assert_eq!(
        occurrences, 1,
        "expected exactly one `fn eq_OsEntropy` (never a duplicate), got {occurrences} in:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr && g.reason.contains("Eq")),
        "expected the bare `Eq` name to fall through as an unrecognized sub-gap, got {:?}",
        report.gaps
    );
}

/// Fieldless `derive(PartialOrd)` composes the trivially-equal `impl Ord3[T] for T`.
#[test]
fn derive_ord_fieldless_composes() {
    let (myc, report) = transpile_source("#[derive(PartialOrd)]\nstruct OsEntropy;", "f.rs", "f")
        .expect("parses/transpiles");
    assert!(
        report.emitted_items.iter().any(|n| n == "OsEntropy"),
        "expected OsEntropy in emitted_items, got {:?}",
        report.emitted_items
    );
    assert!(
        myc.contains(
            "impl Ord3[OsEntropy] for OsEntropy {\n  fn cmp(a: OsEntropy, b: OsEntropy) => \
             Binary{8} =\n    0b00000001;\n};"
        ),
        "expected the fieldless Ord3 impl body, got:\n{myc}"
    );
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "a fully-eligible (fieldless) derive must not record any DeriveAttr gap, got {:?}",
        report.gaps
    );
}

/// A gapped `derive(PartialOrd)` (the primitive-field case) must NEVER leak a fabricated
/// `impl Ord3[...]` fragment into the `.myc` text. **DN-138 WU-4 update:** a narrow `u8` field is
/// NO LONGER a gapping fixture (WU-4's `width_cast` unblock composes it — see the `Narrow` case in
/// `derive_forms_check_clean_against_real_toolchain`); `Vec<f64>` (`VecOf(Float)`, still ineligible
/// for every row) is the fixture that stays a genuine, disclosed gap.
#[test]
fn derive_ord_gap_never_leaks_partial_impl_text() {
    let (myc, report) = transpile_source(
        "#[derive(PartialOrd)]\nstruct Pair(Vec<f64>, bool);",
        "f.rs",
        "f",
    )
    .expect("parses/transpiles");
    assert!(
        !myc.contains("impl Ord3"),
        "a gapped derive(PartialOrd) must never emit a partial `impl Ord3`, got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr && g.reason.contains("PartialOrd")),
        "expected a DeriveAttr gap citing PartialOrd, got {:?}",
        report.gaps
    );
}

/// **ADR-040 §2.4 (NaN semantics).** A `Float` field REFUSES the whole `derive(PartialOrd)`,
/// citing the real cause (no order position under IEEE-754's partial order).
#[test]
fn derive_ord_float_field_refused_with_adr040_citation() {
    let (myc, report) = transpile_source("#[derive(PartialOrd)]\nstruct Sample(f64);", "f.rs", "f")
        .expect("parses/transpiles");
    assert!(
        !myc.contains("impl Ord3[Sample]"),
        "a Float-field derive(PartialOrd) must never fabricate an Ord3 impl, got:\n{myc}"
    );
    let gap = report
        .gaps
        .iter()
        .find(|g| g.category == Category::DeriveAttr && g.reason.contains("Sample"))
        .unwrap_or_else(|| {
            panic!(
                "expected a DeriveAttr gap for Sample, got {:?}",
                report.gaps
            )
        });
    assert!(
        gap.reason.contains("NaN") && gap.reason.contains("ADR-040"),
        "expected the Float-field gap to cite NaN/ADR-040 specifically, got: {}",
        gap.reason
    );
}

/// **The verified duplicate-instance-avoidance property** — the `Ord3` analogue of
/// `derive_eq_recognizes_only_partialeq_avoids_duplicate_fn`. `#[derive(PartialOrd, Ord)]`
/// composes `impl Ord3[T] for T` EXACTLY ONCE (never the RFC-0019 §4.5 "overlapping instance"
/// collision `ord.rs`'s module doc documents).
#[test]
fn derive_ord_recognizes_only_partialord_avoids_duplicate_impl() {
    let (myc, report) =
        transpile_source("#[derive(PartialOrd, Ord)]\nstruct OsEntropy;", "f.rs", "f")
            .expect("parses/transpiles");
    let occurrences = myc.matches("impl Ord3[OsEntropy]").count();
    assert_eq!(
        occurrences, 1,
        "expected exactly one `impl Ord3[OsEntropy]` (never a duplicate), got {occurrences} \
         in:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr && g.reason.contains("Ord")),
        "expected the bare `Ord` name to fall through as an unrecognized sub-gap, got {:?}",
        report.gaps
    );
}

/// Fieldless `derive(Hash)` composes the type-name-discriminated `fn hash_T`.
#[test]
fn derive_hash_fieldless_composes() {
    let (myc, report) = transpile_source("#[derive(Hash)]\nstruct OsEntropy;", "f.rs", "f")
        .expect("parses/transpiles");
    assert!(
        report.emitted_items.iter().any(|n| n == "OsEntropy"),
        "expected OsEntropy in emitted_items, got {:?}",
        report.emitted_items
    );
    assert!(
        myc.contains("fn hash_OsEntropy(a: OsEntropy) => Bytes =\n    hash_blake3(\"OsEntropy\");"),
        "expected the fieldless hash fn body, got:\n{myc}"
    );
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "a fully-eligible (fieldless) derive must not record any DeriveAttr gap, got {:?}",
        report.gaps
    );
}

/// A gapped `derive(Hash)` (the primitive-field case) must NEVER leak a fabricated `fn hash_*`
/// fragment into the `.myc` text. **DN-138 WU-4 update:** a `u8`/`ScalarBinary` field is NO LONGER
/// a gapping fixture (the new `bin_to_bytes` prim unblocks it, any width — see `HashableRec` in
/// `derive_forms_check_clean_against_real_toolchain`); `Vec<f64>` (`VecOf(Float)`, still
/// ineligible) is the fixture that stays a genuine, disclosed gap.
#[test]
fn derive_hash_gap_never_leaks_partial_fn_text() {
    let (myc, report) =
        transpile_source("#[derive(Hash)]\nstruct Pair(Vec<f64>, bool);", "f.rs", "f")
            .expect("parses/transpiles");
    assert!(
        !myc.contains("fn hash_"),
        "a gapped derive(Hash) must never emit a partial `fn hash_*`, got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr && g.reason.contains("Hash")),
        "expected a DeriveAttr gap citing Hash, got {:?}",
        report.gaps
    );
}

// ---------------------------------------------------------------------------------------------
// DN-138 (increment 1) — the DeriveAttr-class scalar/`Bytes`/`Bool` field unblock. The seeded
// `Show`/`Init`/`Ord3` primitive instances (`crates/mycelium-l1/src/checkty.rs`'s
// `PRELUDE_INSTANCE_SEEDS`) plus the `field_derive_kind` classifier (this file's `mod.rs`) let the
// five field-gating rows compose over real scalar/`Bytes`/`Bool` fields for the first time — a
// struct composed ONLY of such fields is now FULLY unblocked, not just structurally emitted with
// every derive gapped. `Vec[T]` fields (in every named corpus struct) still gap — increment 2.
// ---------------------------------------------------------------------------------------------

/// **The DeriveAttr-class unblock, end to end.** A struct with a `u64`/`String`/`bool` field set —
/// the exact non-`Vec` field mix the real corpus structs (`CheckError`/`CtorInfo`/`EvaluatorOpts`)
/// carry — composes `Debug`/`Default`/`PartialEq`/`PartialOrd` with ZERO `DeriveAttr` gaps.
/// Before DN-138, `field_derive_eligible`'s boolean gate refused EVERY field here (`Binary{64}`,
/// `Bytes`, `Bool` were all in the disallowed/bracketed set), so all four derives gapped entirely.
#[test]
fn derive_composes_over_scalar_bytes_bool_fields_dn138() {
    let (myc, report) = transpile_source(
        "#[derive(Debug, Default, PartialEq, PartialOrd)]\nstruct Rec(u64, String, bool);",
        "f.rs",
        "f",
    )
    .expect("parses/transpiles");
    assert!(
        report.emitted_items.iter().any(|n| n == "Rec"),
        "expected `Rec` in emitted_items, got {:?}",
        report.emitted_items
    );
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "a scalar/Bytes/Bool-only struct must compose ALL FOUR derives with zero DeriveAttr gaps \
         (DN-138 increment 1), got {:?}",
        report.gaps
    );
    assert!(
        myc.contains("impl Show[Rec] for Rec"),
        "expected a composed Show impl, got:\n{myc}"
    );
    assert!(
        myc.contains("impl Init[Rec] for Rec"),
        "expected a composed Init impl, got:\n{myc}"
    );
    assert!(
        myc.contains("fn eq_Rec("),
        "expected a composed eq_Rec fn, got:\n{myc}"
    );
    assert!(
        myc.contains("impl Ord3[Rec] for Rec"),
        "expected a composed Ord3 impl, got:\n{myc}"
    );
}

/// **DN-138 §3's heterogeneity finding — the WIDTH half is CLOSED by WU-4.** `PartialEq` was
/// always width-generic (the bare `eq` prim, RFC-0032 D1). Before WU-4, `Debug` (trait-dispatched
/// through the ONE seeded `Binary{64}` instance) gapped an identical NARROW `Binary{8}` field;
/// WU-4 closes that via a `width_cast` wrapper (`show.rs`/`ord.rs`) — so BOTH now compose over the
/// same narrow scalar, over the SAME struct, with zero DeriveAttr gaps.
#[test]
fn derive_eq_and_debug_both_compose_over_a_narrow_scalar_dn138_wu4() {
    let (myc, report) = transpile_source("#[derive(PartialEq)]\nstruct Narrow(u8);", "f.rs", "f")
        .expect("parses/transpiles");
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "PartialEq must compose over a narrow Binary{{8}} scalar (no width restriction for the \
         prim-routed eq call), got {:?}",
        report.gaps
    );
    assert!(
        myc.contains("eq(p0, q0)"),
        "expected the bare eq prim call, got:\n{myc}"
    );

    // The SAME field, `Debug` — WU-4's `width_cast` wrapper now ALSO composes (no longer a gap).
    let (debug_myc, debug_report) =
        transpile_source("#[derive(Debug)]\nstruct Narrow(u8);", "f.rs", "f")
            .expect("parses/transpiles");
    assert!(
        !debug_report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "Debug must now compose over a narrow Binary{{8}} scalar too (WU-4's width_cast unblock), \
         got {:?}",
        debug_report.gaps
    );
    assert!(
        debug_myc.contains("width_cast(p0,"),
        "expected the width_cast-wrapped render call, got:\n{debug_myc}"
    );
}

/// **HIGH (post-landing review + ORACLE-R1 A5):** a WIDE `ScalarBinary` (`u128`/`i128` ->
/// `Binary{128}`) must NEVER compose via a NARROWING `width_cast` down to the seeded
/// `Binary{64}` instance — that cast overflows at eval for any value `>= 2^64`. **Debug** now
/// composes with a Declared opaque `"<Binary{128}>"` placeholder (structure shown, payload not
/// decimal-rendered — never fabricated Display, G2/VR-5; clears same-file parent `UserNamed`
/// `render` file-poison). **PartialOrd** still honestly GAPs (no total order surface without a
/// wide Ord3 seed). Never a partial/wrong-but-plausible-looking narrowing impl (G2).
#[test]
fn derive_debug_and_partialord_gap_a_wide_scalar_never_a_runtime_throwing_width_cast() {
    let (debug_myc, debug_report) =
        transpile_source("#[derive(Debug)]\nstruct Wide(u128);", "f.rs", "f")
            .expect("parses/transpiles");
    assert!(
        !debug_myc.contains("width_cast"),
        "Debug must never emit a NARROWING width_cast for a wide (u128) scalar, got:\n{debug_myc}"
    );
    assert!(
        debug_myc.contains("impl Show[Wide] for Wide"),
        "Debug must compose Show with opaque wide-Binary placeholder, got:\n{debug_myc}"
    );
    assert!(
        debug_myc.contains("\"<Binary{128}>\""),
        "expected Declared opaque Binary{{128}} placeholder in Show body, got:\n{debug_myc}"
    );
    assert!(
        !debug_report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr && g.reason.contains("Debug")),
        "Debug must no longer DeriveAttr-gap a wide scalar (A5 opaque compose), got {:?}",
        debug_report.gaps
    );

    let (ord_myc, ord_report) =
        transpile_source("#[derive(PartialOrd)]\nstruct Wide(u128);", "f.rs", "f")
            .expect("parses/transpiles");
    assert!(
        !ord_myc.contains("width_cast"),
        "PartialOrd must never emit a NARROWING width_cast for a wide (u128) scalar, got:\n{ord_myc}"
    );
    assert!(
        ord_report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr && g.reason.contains("PartialOrd")),
        "expected a DeriveAttr gap citing PartialOrd for the wide scalar field, got {:?}",
        ord_report.gaps
    );

    // The companion positive control: PartialEq (width-generic prim, no cast at all) and Default
    // (a literal zero at the field's own width, no cast either) both still compose CLEAN for the
    // identical wide field -- this asymmetry is real and intentional, not a regression (pinned
    // directly here, and via the live oracle in `derive_forms_check_clean_against_real_toolchain`'s
    // `Wide` case).
    let (_, eq_report) = transpile_source("#[derive(PartialEq)]\nstruct Wide(u128);", "f.rs", "f")
        .expect("parses/transpiles");
    assert!(
        !eq_report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "PartialEq must still compose over a wide (u128) scalar (width-generic eq prim, no \
         width_cast at all), got {:?}",
        eq_report.gaps
    );
    let (_, default_report) =
        transpile_source("#[derive(Default)]\nstruct Wide(u128);", "f.rs", "f")
            .expect("parses/transpiles");
    assert!(
        !default_report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "Default must still compose over a wide (u128) scalar (a literal zero at its own width, \
         no width_cast at all), got {:?}",
        default_report.gaps
    );
}

/// ORACLE-R1 A5: `derive(Debug)` on a wide-Binary leaf (`WallInstant`-shape) plus a same-file
/// `UserNamed` parent (`ManualClock`-shape) both compose Show; parent `render(field)` resolves.
/// Hand-written `Default` calling `Type::from_nanos(0)` rewrites the bare `0` to a BinLit via
/// recorded mangled-assoc param widths (post-Show residual).
#[test]
fn oracle_r1_a5_wide_show_and_call_arg_lit_zero() {
    let rust = r#"
        #[derive(Debug, Clone, Copy)]
        struct WallInstant { nanos: i128 }
        impl WallInstant {
            pub const fn from_nanos(nanos: i128) -> Self { WallInstant { nanos } }
        }
        #[derive(Debug, Clone)]
        struct ManualClock { wall: WallInstant }
        impl Default for ManualClock {
            fn default() -> Self {
                ManualClock { wall: WallInstant::from_nanos(0) }
            }
        }
    "#;
    let (myc, report) =
        transpile_source(rust, "std_time_like.rs", "std.time").expect("parses/transpiles");
    assert!(
        myc.contains("impl Show[WallInstant] for WallInstant"),
        "expected WallInstant Show, got:\n{myc}"
    );
    assert!(
        myc.contains("\"<Binary{128}>\""),
        "expected opaque wide Binary placeholder, got:\n{myc}"
    );
    assert!(
        myc.contains("impl Show[ManualClock] for ManualClock"),
        "expected ManualClock Show (parent of WallInstant), got:\n{myc}"
    );
    assert!(
        myc.contains("render(p0)") || myc.contains("render(p1)"),
        "ManualClock Show should dispatch render on UserNamed field, got:\n{myc}"
    );
    // Init body: bare 0 rewritten to BinLit (Q6-safe).
    assert!(
        myc.contains("impl Init[ManualClock]") || myc.contains("fn init()"),
        "expected Default→Init, got:\n{myc}\ngaps={:?}",
        report.gaps.iter().map(|g| &g.reason).collect::<Vec<_>>()
    );
    assert!(
        !myc.contains("from_nanos(0)")
            && !myc.contains("from_nanos(0,")
            && !myc.contains("_from_nanos(0)"),
        "bare decimal 0 must be rewritten to BinLit in assoc-fn call args, got:\n{myc}"
    );
}

/// `Hash` composes over `Bytes`/`Bool` fields (no scalar field present).
#[test]
fn derive_hash_composes_over_bytes_and_bool_fields() {
    let (myc, report) = transpile_source(
        "#[derive(Hash)]\nstruct HashableRec(String, bool);",
        "f.rs",
        "f",
    )
    .expect("parses/transpiles");
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "a Bytes/Bool-only struct must compose Hash with zero DeriveAttr gaps, got {:?}",
        report.gaps
    );
    assert!(
        myc.contains("fn hash_HashableRec("),
        "expected a composed hash_HashableRec fn, got:\n{myc}"
    );
    assert!(
        myc.contains("hash_blake3(p0)"),
        "expected the direct hash_blake3 route for the Bytes field, got:\n{myc}"
    );
}

/// **DN-138 WU-4 unblock:** `Hash` now composes over a scalar field too, via the new
/// `bin_to_bytes` raw-byte prim (`hash_blake3(bin_to_bytes(p))`) — previously an honest,
/// disclosed gap (no such prim existed).
#[test]
fn derive_hash_composes_over_a_scalar_field_dn138_wu4() {
    let (myc, report) = transpile_source("#[derive(Hash)]\nstruct Rec(u64);", "f.rs", "f")
        .expect("parses/transpiles");
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "Hash must now compose over a scalar field (the new bin_to_bytes prim), got {:?}",
        report.gaps
    );
    assert!(
        myc.contains("hash_blake3(bin_to_bytes(p0))"),
        "expected the bin_to_bytes-routed hash call, got:\n{myc}"
    );
}

/// **The same-file nested-composition case, for all three Phase-2 rows at once** — mirrors
/// `derive_composes_end_to_end_over_a_same_file_nested_derived_field` (DN-128) exactly, pinning
/// the field-walked body text for `Eq`/`Ord3`/`Hash` together.
#[test]
fn derive_eq_ord_hash_compose_end_to_end_over_a_same_file_nested_derived_field() {
    let rust = "#[derive(PartialEq, PartialOrd, Hash)]\nstruct Inner;\n\
                #[derive(PartialEq, PartialOrd, Hash)]\nstruct Outer(Inner, Inner);";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture").expect("parses/transpiles");
    for name in ["Inner", "Outer"] {
        assert!(
            report.emitted_items.iter().any(|n| n == name),
            "expected `{name}` in emitted_items, got {:?}",
            report.emitted_items
        );
    }
    assert!(
        myc.contains("fn eq_Inner(a: Inner, b: Inner) => Binary{1} =\n    0b1;"),
        "expected Inner's derived eq fn, got:\n{myc}"
    );
    assert!(
        myc.contains(
            "fn eq_Outer(a: Outer, b: Outer) => Binary{1} =\n    match a { Outer(p0, p1) => \
             match b { Outer(q0, q1) => and(eq_Inner(p0, q0), eq_Inner(p1, q1)) } };"
        ),
        "expected Outer's field-walked eq fn body, got:\n{myc}"
    );
    assert!(
        myc.contains(
            "impl Ord3[Outer] for Outer {\n  fn cmp(a: Outer, b: Outer) => Binary{8} =\n    \
             match a { Outer(p0, p1) => match b { Outer(q0, q1) => match cmp(p0, q0) { \
             0b00000001 => cmp(p1, q1), other => other } } };\n};"
        ),
        "expected Outer's field-walked Ord3 impl body, got:\n{myc}"
    );
    assert!(
        myc.contains(
            "fn hash_Outer(a: Outer) => Bytes =\n    match a { Outer(p0, p1) => \
             hash_blake3(bytes_concat(bytes_concat(\"Outer\", hash_Inner(p0)), hash_Inner(p1))) \
             };"
        ),
        "expected Outer's field-walked hash fn body, got:\n{myc}"
    );
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr),
        "a fully-eligible nested derive set must not record any DeriveAttr gap, got {:?}",
        report.gaps
    );
}

/// **Mixed derive set (DN-136 §8 invariant witness, Phase-2 twin of
/// `derive_mixed_set_composes_eligible_and_sub_gaps_the_rest_item_still_emits`).** `PartialEq`
/// composes AND the unrecognized `Serialize` sub-gaps -- the item still emits both.
#[test]
fn derive_eq_mixed_set_composes_eligible_and_sub_gaps_the_rest_item_still_emits() {
    let rust = "#[derive(PartialEq, Serialize)]\nstruct OsEntropy;";
    let (myc, report) = transpile_source(rust, "fixture.rs", "fixture").expect("parses/transpiles");
    assert!(
        report.emitted_items.iter().any(|n| n == "OsEntropy"),
        "the item must still emit despite a sibling derive being unrecognized, got {:?}",
        report.emitted_items
    );
    assert!(
        myc.contains("type OsEntropy = OsEntropy;"),
        "the struct's own type decl must still emit, got:\n{myc}"
    );
    assert!(
        myc.contains("fn eq_OsEntropy"),
        "the composable PartialEq->eq fn must still compose despite the sibling Serialize \
         derive being unrecognized, got:\n{myc}"
    );
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.category == Category::DeriveAttr && g.reason.contains("Serialize")),
        "the unrecognized Serialize derive must still be recorded as a sub-gap, got {:?}",
        report.gaps
    );
}

// ---------------------------------------------------------------------------------------------
// DN-136/P1-a (M-1096) — the emit hook-dispatch interfaces-first refactor's byte-identical
// differential (the DoD gate, DN-136 §8 / §3 "the migration is mechanical and behavior-
// preserving ... the cases() corpus and the differential harness emit byte-identical text
// before/after"). This is the one place the whole `cases()` corpus is asserted BYTE-IDENTICAL
// (not the substring `contains` checks the fixtures above use) against a golden snapshot
// captured from the PRE-refactor emitter (`origin/dev@642851ac`, before the P1-a handler-table
// migration touched `map_pattern_inner`/`lower_struct_derives`/`visit_call`).
// ---------------------------------------------------------------------------------------------

/// One case's captured emission outcome — deliberately independent of [`crate::gap::Gap`]'s own
/// `Serialize`-only derive (that type has no `Deserialize`), so this snapshot struct is the
/// stable, round-trippable golden format on its own terms.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct CaseSnapshot {
    /// The exact `.myc` text `transpile_source` produced for the whole fixture (every emitted
    /// item, joined) — the primary byte-identical signal.
    myc: String,
    emitted_items: Vec<String>,
    /// `(category.as_str(), reason, item_name)` per gap, in report order — catches an accidental
    /// message-text drift during the handler-table move, not just an emitted-text drift.
    gaps: Vec<(String, String, Option<String>)>,
}

fn snapshot_case(case: &Case) -> CaseSnapshot {
    let (myc, report) = transpile_source(case.rust, "fixture.rs", "fixture")
        .unwrap_or_else(|e| panic!("case `{}` failed to parse/transpile: {e}", case.name));
    CaseSnapshot {
        myc,
        emitted_items: report.emitted_items,
        gaps: report
            .gaps
            .iter()
            .map(|g| {
                (
                    g.category.as_str().to_string(),
                    g.reason.clone(),
                    g.item_name.clone(),
                )
            })
            .collect(),
    }
}

const EMIT_HOOK_GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/tests/fixtures/emit_hook_golden.json"
);

/// **One-off golden-snapshot generator — NOT part of the regression gate (`#[ignore]`).**
///
/// Run manually (`cargo test -p mycelium-transpile --lib generate_emit_hook_golden_snapshot -- \
/// --ignored --exact`) to (re)write `src/tests/fixtures/emit_hook_golden.json`. This was run
/// exactly ONCE, against the PRE-refactor emitter (`origin/dev@642851ac`, before the P1-a
/// handler-table migration), to capture the golden reference
/// [`emit_hook_refactor_byte_identical_differential`] below checks the post-refactor emitter
/// against. **Never re-run this to "fix" a failing differential** — a red differential means the
/// migration changed behavior (a regression to find and fix in the migration, not the snapshot);
/// only re-run it when a *separately reviewed, intentional* emitter behavior change lands (VR-5 —
/// regenerating the oracle to match a regression would silently launder it).
#[test]
#[ignore = "one-off golden-snapshot generator for the DN-136/P1-a byte-identical differential; \
            run manually with --ignored, never as part of the regression gate"]
fn generate_emit_hook_golden_snapshot() {
    use std::collections::BTreeMap;
    let snapshot: BTreeMap<&'static str, CaseSnapshot> = cases()
        .iter()
        .map(|case| (case.name, snapshot_case(case)))
        .collect();
    let json = serde_json::to_string_pretty(&snapshot).expect("serialize golden snapshot");
    std::fs::write(EMIT_HOOK_GOLDEN_PATH, json).expect("write golden snapshot fixture");
}

/// **The DN-136/P1-a Definition-of-Done gate.** Re-derives every `cases()` fixture's emission
/// through the (post-refactor) emitter and asserts it is BYTE-IDENTICAL to the golden snapshot
/// captured from the pre-refactor emitter (see [`generate_emit_hook_golden_snapshot`]'s doc). A
/// case present in `cases()` but absent from the golden snapshot (or vice versa) is itself a
/// failure — the corpus must not silently grow/shrink without a deliberate re-snapshot (G2).
#[test]
fn emit_hook_refactor_byte_identical_differential() {
    use std::collections::BTreeMap;
    let golden: BTreeMap<String, CaseSnapshot> =
        serde_json::from_str(include_str!("fixtures/emit_hook_golden.json"))
            .expect("golden snapshot fixture parses as JSON");
    let live = cases();
    assert_eq!(
        golden.len(),
        live.len(),
        "cases() corpus size ({}) drifted from the golden snapshot ({}) — regenerate the \
         snapshot deliberately (see generate_emit_hook_golden_snapshot's doc), never silently",
        live.len(),
        golden.len()
    );
    for case in &live {
        let expected = golden.get(case.name).unwrap_or_else(|| {
            panic!(
                "case `{}` is not in the golden snapshot — a case was added/renamed without a \
                 deliberate re-snapshot",
                case.name
            )
        });
        let actual = snapshot_case(case);
        assert_eq!(
            &actual, expected,
            "case `{}`: emit-hook-refactored output differs from the pre-refactor (DN-136 \
             P1-a) golden snapshot — the handler-table migration must be byte-identical \
             (mechanical move only, never a behavior change)",
            case.name
        );
    }
}

/// **The verify-first live-oracle proof** (mitigation #14) for DN-131/M-1101 (bounded
/// inherent-impl type-parameter emission): every impl-level generic-parameter shape this leaf
/// newly emits — unbounded, single-bound, multi-bound — runs through the REAL `myc-check`
/// oracle, mirroring `signed_numeric_idiom_check_clean`'s pattern. Each snippet is a full
/// one-nodule program (trait + type + bounded impl + a concrete-instantiating `fn`), matching
/// the shape `crates/mycelium-l1/tests/check.rs::impl_slot_bound_is_accepted` already pins at
/// the kernel/L1 level — this test is the transpiler-emission twin: it proves the .myc TEXT
/// this leaf's `emit_impl` produces from REAL Rust source also checks clean, not just that the
/// kernel accepts hand-written .myc of the same shape. **Non-vacuity:** every one of these Rust
/// snippets was a hard GAP before this leaf (the impl carried a non-empty `generics.params`, so
/// the whole impl always refused) — `report.emitted_items` confirms the impl is a REAL
/// emission, not a coincidental no-op. Skips gracefully (never fails) when `myc-check` is not
/// built.
#[test]
fn bounded_impl_generic_emission_check_clean() {
    let Some(bin) = find_myc_check() else {
        eprintln!(
            "emit: DN-131/M-1101 live oracle test skipped — no runnable myc-check (set \
             MYC_CHECK_CMD or build `cargo build -p mycelium-check --bin myc-check`). The \
             fixture-corpus text assertions in `cases()` still cover the emitted shape."
        );
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "mycelium-transpile-emit-dn131-impl-generics-oracle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let rust_snippets = [
        // Unbounded impl-level generic (DN-103's own slot; DN-131 §3's backward-compatible
        // identity case — `bounds: []`).
        "struct Bx<A>(A);\nimpl<T> Bx<T> { fn dup(self) -> Bx<T> { self } }\n\
         fn mk(x: u8) -> Bx<u8> { Bx(x) }",
        // A single bounded impl-level type parameter — the leaf's headline capability. `Cmp` is
        // declared in-nodule and never called from `dup`, pinning the DN-131 §7 "dead bound"
        // case (registry-validated by `check_bounds`, costs nothing at monomorphization).
        "trait Cmp<A> { fn cmp(a: A, b: A) -> bool; }\nstruct Bx<A>(A);\n\
         impl<T: Cmp> Bx<T> { fn dup(self) -> Bx<T> { self } }\n\
         fn mk(x: u8) -> Bx<u8> { Bx(x) }",
        // A multi-bound impl-level type parameter (`T: A + B`, DN-131 §3's `parse_bound` reuse).
        "trait Cmp<A> { fn cmp(a: A, b: A) -> bool; }\ntrait Ord2<A> { fn lt(a: A, b: A) -> bool; }\n\
         struct Bx<A>(A);\nimpl<T: Cmp + Ord2> Bx<T> { fn dup(self) -> Bx<T> { self } }\n\
         fn mk(x: u8) -> Bx<u8> { Bx(x) }",
    ];
    for (i, rust) in rust_snippets.iter().enumerate() {
        let (myc, report) = transpile_source(rust, "fixture.rs", "oracle")
            .unwrap_or_else(|e| panic!("case {i} (`{rust}`) failed to parse/transpile: {e}"));
        assert!(
            report.emitted_items.iter().any(|n| n.starts_with("impl")),
            "case {i} (`{rust}`) failed to emit the bounded/unbounded impl at all: \
             emitted_items={:?} gaps={:?}",
            report.emitted_items,
            report.gaps
        );
        let path = dir.join(format!("case_{i}.myc"));
        std::fs::write(&path, &myc).expect("write case .myc");

        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "fixture.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "case {i} (`{rust}`) must check CLEAN with the real myc-check oracle — emitted:\n{myc}\n\
             diagnostic={:?}",
            rec.diagnostic
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- ORACLE-R1 A2: guarantee-lattice co-emit (eval.rs Strength poison) -------------------------

/// Free-fn `strength_of` references `Strength` + `GuaranteeStrength` without declaring either.
/// Co-emit both lattice types (Exact→Exact_kw) so myc-check never sees `unknown type Strength`.
#[test]
fn lattice_co_emit_strength_of_no_unknown_type_poison() {
    let rust = r#"
        pub fn strength_of(s: Strength) -> GuaranteeStrength {
            match s {
                Strength::Exact => GuaranteeStrength::Exact,
                Strength::Proven => GuaranteeStrength::Proven,
                Strength::Empirical => GuaranteeStrength::Empirical,
                Strength::Declared => GuaranteeStrength::Declared,
            }
        }
    "#;
    let (myc, report) = transpile_source(rust, "eval.rs", "l1.eval")
        .unwrap_or_else(|e| panic!("transpile failed: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "co-emit:Strength"),
        "expected co-emitted Strength; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        report
            .emitted_items
            .iter()
            .any(|n| n == "co-emit:GuaranteeStrength"),
        "expected co-emitted GuaranteeStrength; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        report.emitted_items.iter().any(|n| n == "strength_of"),
        "expected strength_of emitted; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        myc.contains("type Strength = Exact_kw | Proven_kw | Empirical_kw | Declared_kw;"),
        "missing Strength co-emit type:\n{myc}"
    );
    assert!(
        myc.contains("type GuaranteeStrength = Exact_kw | Proven_kw | Empirical_kw | Declared_kw;"),
        "missing GuaranteeStrength co-emit type:\n{myc}"
    );
    assert!(
        myc.contains("fn strength_of(s: Strength) => GuaranteeStrength"),
        "missing strength_of signature:\n{myc}"
    );
    // Co-emits must precede the free-fn (type must be in scope before use).
    let s_pos = myc.find("type Strength =").expect("Strength type position");
    let fn_pos = myc.find("fn strength_of").expect("strength_of position");
    assert!(
        s_pos < fn_pos,
        "co-emitted Strength must precede strength_of:\n{myc}"
    );

    // Real-oracle gate when myc-check is available.
    if let Some(bin) = find_myc_check() {
        let dir = std::env::temp_dir().join(format!(
            "myc-a2-strength-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("strength_of.myc");
        std::fs::write(&path, &myc).expect("write myc");
        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "eval.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "strength_of + lattice co-emit must not file-poison with unknown type Strength; \
             myc=\n{myc}\ndiagnostic={:?}",
            rec.diagnostic
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// When `Strength` is declared in-file, co-emit only the missing lattice peer (GuaranteeStrength).
#[test]
fn lattice_co_emit_skips_in_file_strength() {
    let rust = r#"
        pub enum Strength { Exact, Proven, Empirical, Declared }
        pub fn strength_of(s: Strength) -> GuaranteeStrength {
            match s {
                Strength::Exact => GuaranteeStrength::Exact,
                Strength::Proven => GuaranteeStrength::Proven,
                Strength::Empirical => GuaranteeStrength::Empirical,
                Strength::Declared => GuaranteeStrength::Declared,
            }
        }
    "#;
    let (myc, report) = transpile_source(rust, "eval.rs", "l1.eval")
        .unwrap_or_else(|e| panic!("transpile failed: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "co-emit:Strength"),
        "must not co-emit Strength when it is declared in-file; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        report
            .emitted_items
            .iter()
            .any(|n| n == "co-emit:GuaranteeStrength"),
        "expected co-emitted GuaranteeStrength only; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    // In-file enum still emits (DN-140 variant renames).
    assert!(
        myc.contains("type Strength = Exact_kw | Proven_kw | Empirical_kw | Declared_kw;"),
        "in-file Strength enum must emit:\n{myc}"
    );
}

// ---- ORACLE-R1 A4: private const co-emit (eval.rs DEFAULT_FUEL / DEFAULT_DEPTH) ----------------

/// `impl Default` for a budget opts struct references private `DEFAULT_FUEL` / `DEFAULT_DEPTH`
/// consts. Co-emit them as zero-arg BinLit fns and rewrite use sites to calls so myc-check never
/// sees `unknown name DEFAULT_FUEL` (post-A2 residual on eval.rs Init).
#[test]
fn const_co_emit_default_fuel_depth_init_no_unknown_name_poison() {
    let rust = r#"
        const DEFAULT_FUEL: u64 = 1_000_000;
        const DEFAULT_DEPTH: u32 = RecursionBudget::DEFAULT_DEPTH_LIMIT;
        pub struct EvaluatorOpts {
            pub fuel: u64,
            pub depth: u32,
        }
        impl Default for EvaluatorOpts {
            fn default() -> Self {
                EvaluatorOpts {
                    fuel: DEFAULT_FUEL,
                    depth: DEFAULT_DEPTH,
                }
            }
        }
    "#;
    let (myc, report) = transpile_source(rust, "eval.rs", "l1.eval")
        .unwrap_or_else(|e| panic!("transpile failed: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "DEFAULT_FUEL"),
        "expected co-emitted DEFAULT_FUEL; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        report.emitted_items.iter().any(|n| n == "DEFAULT_DEPTH"),
        "expected co-emitted DEFAULT_DEPTH; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        myc.contains("fn DEFAULT_FUEL() => Binary{64} ="),
        "missing DEFAULT_FUEL zero-arg fn:\n{myc}"
    );
    assert!(
        myc.contains("fn DEFAULT_DEPTH() => Binary{32} ="),
        "missing DEFAULT_DEPTH zero-arg fn:\n{myc}"
    );
    assert!(
        myc.contains("DEFAULT_FUEL()") && myc.contains("DEFAULT_DEPTH()"),
        "Init body must call co-emitted consts, not bare names:\n{myc}"
    );
    assert!(
        !myc.contains("EvaluatorOpts(DEFAULT_FUEL, DEFAULT_DEPTH)"),
        "must not leave bare const names in Init (file poison):\n{myc}"
    );
    // Co-emitted fns must precede Init (name must be in scope before use).
    let fuel_pos = myc
        .find("fn DEFAULT_FUEL()")
        .expect("DEFAULT_FUEL fn position");
    let init_pos = myc.find("fn init()").expect("init position");
    assert!(
        fuel_pos < init_pos,
        "co-emitted DEFAULT_FUEL must precede init:\n{myc}"
    );

    if let Some(bin) = find_myc_check() {
        let dir = std::env::temp_dir().join(format!(
            "myc-a4-fuel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("eval_opts.myc");
        std::fs::write(&path, &myc).expect("write myc");
        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "eval.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "DEFAULT_FUEL/DEFAULT_DEPTH co-emit must not file-poison with unknown name; \
             myc=\n{myc}\ndiagnostic={:?}",
            rec.diagnostic
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A const whose initializer is not a decidable integer stays a whole-item gap (never invents
/// a value — VR-5).
#[test]
fn const_non_decidable_initializer_still_gapped() {
    let rust = "const BAD: u64 = foo() + 1;";
    let (myc, report) =
        transpile_source(rust, "f.rs", "f").unwrap_or_else(|e| panic!("transpile failed: {e}"));
    assert!(
        !report.emitted_items.iter().any(|n| n == "BAD"),
        "must not emit non-decidable const; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        report.gaps.iter().any(|g| {
            g.item_name.as_deref() == Some("BAD")
                && g.reason.contains("decidable non-negative integer")
        }),
        "expected honest gap for non-decidable const; gaps={:?}\nmyc=\n{myc}",
        report
            .gaps
            .iter()
            .map(|g| (&g.item_name, &g.reason))
            .collect::<Vec<_>>()
    );
}

// ---- L2-C: std-io Source/Sink named-field structs must emit (not M-1006 false-gap) --------------

/// Minimal std-io residual shape: named-field `Substrate`/`Source`/`Sink` (fields are only
/// `Vec<u8>` / nested in-file types / `usize`) plus a free-fn `read_all(src: Source)` that used to
/// emit while the structs stayed gapped under a false M-1006 "Vec is a user dep" classification →
/// `unknown type Source` file poison. After the L2-C fix, the three structs emit positionally
/// (named fields dropped + EXPLAIN sub-gap) so the free-fn's `Source` type is declared in-file.
#[test]
fn source_sink_named_field_structs_emit_not_unknown_type_poison() {
    let rust = r#"
        pub struct Substrate {
            data: Vec<u8>,
            pos: usize,
        }
        pub struct Source {
            substrate: Substrate,
        }
        pub struct Sink {
            buffer: Vec<u8>,
        }
        // Identity free-fn so the signature alone is the residual class (body residuals are
        // orthogonal). Real std-io `read_all` still has body gaps; the poison was the *type*.
        pub fn identity_src(src: Source) -> Source {
            src
        }
    "#;
    let (myc, report) = transpile_source(rust, "io.rs", "std.io.io")
        .unwrap_or_else(|e| panic!("transpile failed: {e}"));

    // `Substrate` is a Mycelium reserved word (DN-140) → emits as `Substrate_kw`.
    for (raw, emitted) in [
        ("Substrate", "Substrate_kw"),
        ("Source", "Source"),
        ("Sink", "Sink"),
    ] {
        assert!(
            report.emitted_items.iter().any(|n| n == emitted),
            "expected `{raw}` emitted as `{emitted}` (not M-1006 false-gap); emitted={:?}\nmyc=\n{myc}",
            report.emitted_items
        );
        assert!(
            !report.gaps.iter().any(|g| {
                g.item_name.as_deref() == Some(raw)
                    && g.category == Category::Struct
                    && g.reason.contains("not resolvable in-file")
            }),
            "`{raw}` must not be whole-item gapped under M-1006 resolvability; gaps={:?}\nmyc=\n{myc}",
            report
                .gaps
                .iter()
                .filter(|g| g.item_name.as_deref() == Some(raw))
                .map(|g| (&g.category, &g.reason))
                .collect::<Vec<_>>()
        );
    }

    // Positional emission of the product (named fields dropped — EXPLAIN via NamedFieldDrop).
    // Substrate rewrites to Substrate_kw (reserved-word, DN-140).
    assert!(
        myc.contains("type Substrate_kw = Substrate_kw(Vec[Binary{8}], Binary{64});")
            || myc.contains("type Substrate_kw = Substrate_kw("),
        "missing Substrate_kw type emission:\n{myc}"
    );
    assert!(
        myc.contains("type Source = Source(Substrate_kw);"),
        "missing Source type emission (field type Substrate → Substrate_kw):\n{myc}"
    );
    assert!(
        myc.contains("type Sink = Sink(Vec[Binary{8}]);"),
        "missing Sink type emission:\n{myc}"
    );
    assert!(
        report.emitted_items.iter().any(|n| n == "identity_src"),
        "expected identity_src free-fn emitted with Source in signature; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        myc.contains("fn identity_src(src: Source)") && myc.contains("=> Source"),
        "missing identity_src signature referencing Source:\n{myc}"
    );
    // Types must precede the free-fn that names them (declaration-before-use).
    let src_ty = myc.find("type Source =").expect("Source type position");
    let fn_pos = myc.find("fn identity_src").expect("identity_src position");
    assert!(
        src_ty < fn_pos,
        "Source type must precede identity_src:\n{myc}"
    );

    // Real-oracle gate when myc-check is available: no `unknown type Source` file poison.
    if let Some(bin) = super::vet::find_myc_check() {
        let dir = std::env::temp_dir().join(format!(
            "myc-l2c-source-sink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("io.myc");
        std::fs::write(&path, &myc).expect("write myc");
        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "io.rs", 4, 4);
        let diag = rec.diagnostic.as_str();
        assert!(
            !diag.contains("unknown type `Source`")
                && !diag.contains("unknown type Source")
                && !diag.contains("unknown type `Sink`")
                && !diag.contains("unknown type `Substrate`")
                && !diag.contains("unknown type `Substrate_kw`"),
            "Source/Sink/Substrate type emission must not file-poison with unknown type; \
             class={:?} diagnostic={:?}\nmyc=\n{myc}",
            rec.class,
            rec.diagnostic
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---- G-α Rank-1 / L2-A: ambient Result / Option co-emit ---------------------------------------

/// Free-fn returning `Result[…]` co-emits `type Result[A, E] = Ok(A) | Err(E);` once (with EXPLAIN
/// citing lib/std/result.myc) so myc-check never sees `unknown type Result` (std-io read_all poison).
#[test]
fn ambient_result_co_emit_on_result_return() {
    let rust = r#"
        pub fn read_all(flag: u8) -> Result<u8, u8> {
            Ok(flag)
        }
    "#;
    let (myc, report) = transpile_source(rust, "io.rs", "std.io.io")
        .unwrap_or_else(|e| panic!("transpile failed: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "co-emit:Result"),
        "expected co-emit:Result; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        report.emitted_items.iter().any(|n| n == "read_all"),
        "expected read_all emitted; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        myc.contains("type Result[A, E] = Ok(A) | Err(E);"),
        "missing ambient Result type shape (lib/std/result.myc):\n{myc}"
    );
    assert!(
        myc.contains("lib/std/result.myc") || myc.contains("ambient Result"),
        "EXPLAIN must cite ambient Result / lib/std/result.myc:\n{myc}"
    );
    // Exactly one type Result definition line (never double-emit). Count line-starts only so an
    // EXPLAIN citation cannot false-double the tally.
    let type_result_lines = myc
        .lines()
        .filter(|l| {
            l.trim_start()
                .starts_with("type Result[A, E] = Ok(A) | Err(E);")
        })
        .count();
    assert_eq!(
        type_result_lines, 1,
        "Result ambient must appear once as a type item:\n{myc}"
    );
    // Combinators must NOT be fabricated as ambient.
    assert!(
        !myc.contains("fn is_ok[") && !myc.contains("fn map[A, B, E]"),
        "must not fabricate Result combinators as ambient:\n{myc}"
    );
    // Co-emit precedes the free-fn.
    let t_pos = myc
        .find("type Result[A, E] = Ok(A) | Err(E);")
        .expect("Result type position");
    let fn_pos = myc.find("fn read_all").expect("read_all position");
    assert!(
        t_pos < fn_pos,
        "ambient Result must precede read_all:\n{myc}"
    );

    if let Some(bin) = super::vet::find_myc_check() {
        let dir = std::env::temp_dir().join(format!(
            "myc-g-alpha-result-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("io.myc");
        std::fs::write(&path, &myc).expect("write myc");
        let checker = crate::vet::MycChecker {
            command: vec![bin.display().to_string()],
            cwd: None,
        };
        let rec = checker.vet_file(&path, "io.rs", 1, 1);
        assert_eq!(
            rec.class,
            crate::vet::VetClass::Clean,
            "read_all + ambient Result must not file-poison with unknown type Result; \
             myc=\n{myc}\ndiagnostic={:?}",
            rec.diagnostic
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Option-only surface co-emits Option ambient, not Result.
#[test]
fn ambient_option_co_emit_on_option_return() {
    let rust = r#"
        pub fn maybe(flag: u8) -> Option<u8> {
            Some(flag)
        }
    "#;
    let (myc, report) = transpile_source(rust, "opt.rs", "std.opt")
        .unwrap_or_else(|e| panic!("transpile failed: {e}"));
    assert!(
        report.emitted_items.iter().any(|n| n == "co-emit:Option"),
        "expected co-emit:Option; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        !report.emitted_items.iter().any(|n| n == "co-emit:Result"),
        "must not co-emit Result when only Option is mentioned; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        myc.contains("type Option[A] = Some(A) | None;"),
        "missing ambient Option type shape (lib/std/option.myc):\n{myc}"
    );
    assert!(
        !myc.contains("type Result[A, E] = Ok(A) | Err(E);"),
        "must not ambient-emit Result when unused:\n{myc}"
    );
}

/// No Result/Option in surface ⇒ no ambient co-emit.
#[test]
fn ambient_result_option_skipped_when_unused() {
    let rust = r#"
        pub fn id(x: u8) -> u8 { x }
    "#;
    let (myc, report) = transpile_source(rust, "id.rs", "std.id")
        .unwrap_or_else(|e| panic!("transpile failed: {e}"));
    assert!(
        !report
            .emitted_items
            .iter()
            .any(|n| n.starts_with("co-emit:Result") || n.starts_with("co-emit:Option")),
        "no ambient Result/Option when unused; emitted={:?}\nmyc=\n{myc}",
        report.emitted_items
    );
    assert!(
        !myc.contains("type Result[A, E] = Ok(A) | Err(E);")
            && !myc.contains("type Option[A] = Some(A) | None;"),
        "must not inject ambient types when unused:\n{myc}"
    );
}

/// In-file `type Result[…]` definition (via a Rust enum named Result) must not double-define.
/// When the body already defines Result, ambient co-emit is skipped.
#[test]
fn ambient_result_skips_when_already_defined() {
    // Force a body that both defines Result-like surface and mentions Result[…] — the ambient
    // gate keys off `type Result[` already present in the assembled body.
    // Direct unit test of the pure preamble helper (emit path for a real enum named Result would
    // also define `type Result = …` without type args, which body_defines_type_head also catches).
    let body_with_def = "type Result[A, E] = Ok(A) | Err(E);\n\nfn f() => Result[Binary{8}, Binary{8}] = Ok(0b8'0);";
    let preamble = crate::emit::ambient_result_option_preamble(body_with_def);
    assert!(
        preamble.is_empty(),
        "must not re-co-emit Result when already defined; got {preamble:?}"
    );
    let body_needs = "fn f() => Result[Binary{8}, Binary{8}] = Ok(0b8'0);";
    let preamble2 = crate::emit::ambient_result_option_preamble(body_needs);
    assert_eq!(
        preamble2.len(),
        1,
        "must co-emit Result when mentioned and undefined; got {preamble2:?}"
    );
    assert_eq!(preamble2[0].0, "Result");
}
