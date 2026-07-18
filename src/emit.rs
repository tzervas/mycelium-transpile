//! The `.myc` emitter (M-873).
//!
//! Every emission path here is a `match` over a `syn` node, and every fallback/uncovered arm
//! returns `Err(GapReason)` rather than emitting a placeholder or dropping the construct — the
//! driver (`transpile.rs`) is responsible for turning every `Err` into a recorded [`Gap`] (never
//! silent, G2). Nothing in this module ever writes a partial or best-guess `.myc` fragment for a
//! construct it isn't confident about; "confident" here means "traced to a specific grammar
//! production in `docs/spec/grammar/mycelium.ebnf`", cited in the comments below.
//!
//! **Guarantee: `Declared`.** All emitted text is heuristic, unvalidated by any Mycelium
//! parser/typechecker (see crate docs).

use crate::gap::{guarded, Category, GapReason};
use crate::map::{map_type, tokens_to_string};
use crate::reserved::{declared_rewrite_comment, valid_ident, ValidIdent};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use syn::{
    Attribute, Block, Expr, Fields, FieldsNamed, FnArg, GenericArgument, GenericParam, Generics,
    ImplItem, ItemConst, ItemEnum, ItemFn, ItemImpl, ItemStruct, ItemTrait, Lit, Pat,
    PathArguments, ReturnType, Signature, Stmt, TraitBoundModifier, TraitItem, TypeParamBound,
};

// DN-136/P1-a — the emit hook-dispatch axes (Alt B: static per-axis handler tables generalizing
// the landed `prim_map::TABLE` pattern). Each submodule owns one dispatch axis's additive rows;
// the driver methods below (`map_pattern_inner`/`lower_struct_derives`/`EmitVisitor::visit_call`)
// consult the table FIRST, then their own unchanged base/fallback logic — see each submodule's
// own doc for its axis's ordered-pass-preservation invariant (DN-136 §3/§7).
mod calls;
mod derives;
mod macros;
mod patterns;

/// One struct's positional field layout — the M-1006 field-projection input (Lever 1): its field
/// slots in declaration order, `Some(name)` for a named field, `None` for a tuple (unnamed) position.
/// The emitted constructor's name is the struct's own type name (see [`emit_struct`]), so a
/// `self.<field>` access desugars to `match self { <Ty>(_, x, _) => x }` at the field's position.
type StructLayout = Vec<Option<String>>;

/// A name -> mapped-type-text environment threaded through the expression emitters (M-1000/M-1001
/// follow-on, trx2 Lane C Deliverable 1): maps a **local name in scope** (a fn/method parameter,
/// `self`, or a `let`-bound local whose type is trivially known — see the `Stmt::Local` handling in
/// [`emit_block_as_expr_inner`]) to its [`map_type`]-produced type-ref text (e.g. `"Binary{16}"`,
/// `"Bool"`). Populated at a body's two entry points ([`emit_fn`]/[`emit_impl`]) from the already-
/// mapped [`MappedSig::params`] (which already carries `(name, mapped_type_text)` — no re-mapping
/// needed), so this environment is Declared-grade in exactly the same sense the rest of this module
/// is: a heuristic textual record, not a real type-checker's substitution. It exists so
/// `Expr::Binary`'s operator emission (see the `and`/`or`/`ne`/`gt` cases below) can tell, **without
/// ever guessing**, when an operand is a *known* `Binary{N}` value — the gate that decides between
/// the WORD/prim-composed surface (real, myc-check-clean per the verify-first probes cited below) and
/// the glyph fallback (unchanged, still Declared-heuristic). A name absent from the map is simply
/// "not known" — never treated as "known to be something else" (VR-5: absence, not a wrong guess).
pub(crate) type TypeEnv = HashMap<String, String>;

/// If `e` is a **bare, single-segment identifier** naming a local whose type is present in `env`,
/// return that local's mapped type text (a clone of the `env` entry); or a **structurally
/// transparent** wrapper around such an expression (`(e)`, `&e`/`&mut e`). `None` for any other
/// expression shape (a call, a field access, a literal, …) or for a name not in scope. Deliberately
/// narrow: the transpiler has no general expression-typing pass, so only cases this can decide
/// *without guessing* are answered; everything else is simply absent (VR-5).
///
/// (D3 operand-type-inference depth, DN-34 §8.16 residual — trx2 follow-on.) The addition past the
/// original bare-identifier case is decidable on the expression's own syntax, not an inference
/// guess: `Expr::Paren`/`Expr::Reference` are recursed through because this module's own `emit_expr`
/// treats them identically — `Expr::Paren` emits its inner text unchanged but wrapped in `( )`, and
/// `Expr::Reference` is **erased** outright (value semantics, ADR-003; see that arm's doc) — so the
/// *type* of `(e)`/`&e`/`&mut e` is exactly the type of `e` by this module's own emission contract,
/// not a new claim.
///
/// **Verify-first-rejected extension (recorded, not guessed away — VR-5/mitigation #14):** typing an
/// integer literal by its explicit unsigned Rust suffix (`5u16`) was tried and does NOT belong here.
/// The suffix itself is decidable, but composing the literal into a prim call (`eq(a, 5)`) does not
/// `myc check`-clean regardless — the real toolchain refuses a bare decimal `Int` operand with
/// `"a bare integer literal has no representation family (no cross-family defaulting, Q6)"`
/// (empirically confirmed against `target/debug/myc-check`; `docs/spec/grammar/mycelium.ebnf`'s
/// literal-elaboration comment does not hold in the shipped checker). Fixing that needs the literal's
/// *own emission* to change to a width-correct `BinLit` spelling — exactly the **"typed-literal
/// form"** DN-34 §8.13/§8.14 already surveyed and explicitly left undecided ("a design decision, not
/// a faithful drop-in (not implemented, VR-5)"). Inventing that spelling decision here would be
/// exactly the guess G2/VR-5 forbid, so this module still only ever emits an `Int` literal as a bare
/// decimal digit string (`Expr::Lit`'s arm, unchanged) and never claims one as a known `Binary{N}`
/// operand for the gate below.
pub(crate) fn expr_env_type(e: &Expr, env: &TypeEnv) -> Option<String> {
    // Routed through `crate::visit::ExprVisitor` (M-1041 Scope-A): a narrow visitor overriding
    // only the shapes this probe cares about (`visit_path`/`visit_paren`/`visit_reference`/
    // `visit_lit`), inheriting `fallback -> None` for every other `Expr` shape.
    //
    // **M-1037 residual — typed literals:** string / bool / char *literals* have a fixed Rust
    // type independent of context (`&'static str` / `bool` / `char`), so their mapped Mycelium
    // types (`Bytes` / `Bool` / `Binary{32}`) are decidable without TypeEnv. That unlocks
    // identity conversion rows (`to_owned`/`clone`/`to_string`/accessors) on literal receivers
    // that previously fell through to the unmappable-conversion gap (the #72 `"MAP-I".to_owned()`
    // arm-body residual). Integer/float literals are deliberately NOT typed here — their Rust
    // type is unconstrained until inference (defaults to `i32`/`f64` but can be any width), so
    // a guessed Binary{N}/Float would be VR-5-unsafe.
    struct EnvTypeVisitor<'a> {
        env: &'a TypeEnv,
    }
    impl crate::visit::ExprVisitor for EnvTypeVisitor<'_> {
        type Output = Option<String>;

        fn fallback(&mut self, _expr: &Expr) -> Self::Output {
            None
        }

        fn visit_path(&mut self, _expr: &Expr, p: &syn::ExprPath) -> Self::Output {
            if p.qself.is_some() || p.path.segments.len() != 1 {
                return None;
            }
            let name = p.path.segments.last()?.ident.to_string();
            self.env.get(&name).cloned()
        }

        fn visit_paren(&mut self, _expr: &Expr, p: &syn::ExprParen) -> Self::Output {
            expr_env_type(&p.expr, self.env)
        }

        fn visit_reference(&mut self, _expr: &Expr, r: &syn::ExprReference) -> Self::Output {
            expr_env_type(&r.expr, self.env)
        }

        fn visit_lit(&mut self, _expr: &Expr, l: &syn::ExprLit) -> Self::Output {
            match &l.lit {
                Lit::Str(_) => Some("Bytes".to_string()),
                Lit::Bool(_) => Some("Bool".to_string()),
                Lit::Char(_) => Some("Binary{32}".to_string()),
                // Lit::Int / Lit::Float / Lit::Byte / Lit::ByteStr: unconstrained or non-scalar
                // mapped types — leave unresolved (never guess a width).
                _ => None,
            }
        }
    }
    crate::visit::walk_expr(e, &mut EnvTypeVisitor { env })
}

/// [`expr_env_type`] narrowed to the `Binary{N}` case (via [`binary_width`]) — the gate
/// `Expr::Binary`'s `&`/`|`/`!=`/`>` emission below reads directly.
fn expr_env_binary_width(e: &Expr, env: &TypeEnv) -> Option<u32> {
    expr_env_type(e, env).and_then(|t| binary_width(&t))
}

/// ONESHOT C3: whether a `!` operand is known to be Bool (so logical-not match composition is
/// honest). Resolves via TypeEnv, Bool literals, and same-file method/call return types recorded
/// in [`EmitCtx::local_fn_ret`]. Never guesses for an unresolved shape (VR-5) — those keep the
/// bare `!` glyph (correct for Binary; residual for still-unknown Bool).
fn unary_not_operand_is_bool(e: &Expr, env: &TypeEnv, self_ty: Option<&str>) -> bool {
    if expr_env_type(e, env).as_deref() == Some("Bool") {
        return true;
    }
    match e {
        Expr::Paren(p) => unary_not_operand_is_bool(&p.expr, env, self_ty),
        Expr::Reference(r) => unary_not_operand_is_bool(&r.expr, env, self_ty),
        Expr::MethodCall(m) => {
            let method = m.method.to_string();
            if local_fn_ret_ty(&method).as_deref() == Some("Bool") {
                return true;
            }
            if let Some(st) = self_ty {
                let mangled = mangled_inherent_fn_name(st, &method);
                if local_fn_ret_ty(&mangled).as_deref() == Some("Bool") {
                    return true;
                }
            }
            // Receiver-typed mangle (when `self` is a typed binding, not only the impl `self_ty`).
            if let Some(recv_ty) = expr_env_type(&m.receiver, env) {
                let mangled = mangled_inherent_fn_name(&recv_ty, &method);
                if local_fn_ret_ty(&mangled).as_deref() == Some("Bool") {
                    return true;
                }
            }
            false
        }
        Expr::Call(c) => {
            let Expr::Path(p) = c.func.as_ref() else {
                return false;
            };
            if p.qself.is_some() || p.path.segments.len() != 1 {
                return false;
            }
            let name = p.path.segments.last().unwrap().ident.to_string();
            local_fn_ret_ty(&name).as_deref() == Some("Bool")
        }
        _ => false,
    }
}

/// Recover the mapped field-type text (`Binary{N}` / `Binary{N}!s`) for a field access
/// `self.<field>` or a single-arm product projection `match self { Ty(p0, …) => p0 }`, using
/// the per-file field-*type* map (not the name-only layout — names are not `Binary{N}` text).
/// Express gap-close residual post-#1645: lit-zero rewrite needs this so signed field compares
/// to a bare `0` rewrite to equal-width `BinLit` instead of poisoning the file on Q6.
fn match_field_type_text(e: &Expr, self_ty: Option<&str>) -> Option<String> {
    if let Expr::Field(f) = e {
        let base_ty = match f.base.as_ref() {
            Expr::Path(p) if p.path.is_ident("self") => self_ty.map(|s| s.to_string()),
            _ => None,
        }?;
        // Gate on resolvable layout (constructor exists) AND the parallel type map.
        let layout = struct_layout(&base_ty)?;
        let types = struct_field_types(&base_ty)?;
        let pos = match &f.member {
            syn::Member::Named(id) => {
                let n = id.to_string();
                layout
                    .iter()
                    .position(|f| f.as_deref() == Some(n.as_str()))?
            }
            syn::Member::Unnamed(idx) => {
                let i = idx.index as usize;
                if i < layout.len() {
                    i
                } else {
                    return None;
                }
            }
        };
        return types.get(pos)?.clone();
    }
    let Expr::Match(m) = e else {
        return None;
    };
    if m.arms.len() != 1 {
        return None;
    }
    let arm = &m.arms[0];
    let Pat::TupleStruct(ts) = &arm.pat else {
        return None;
    };
    // Only the "first-field projection" shape: body is the first binder of a single-ctor product.
    // Used when source already match-projects (rare) or when a paren-wrapped match is compared.
    let ty_name = ts
        .path
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .or_else(|| self_ty.map(|s| s.to_string()))?;
    let _layout = struct_layout(&ty_name)?;
    let types = struct_field_types(&ty_name)?;
    types.first()?.clone()
}

/// Recover `Binary{N}` width from field access / match-first-field (unsigned or signed marker).
fn match_first_field_binary_width(e: &Expr, self_ty: Option<&str>) -> Option<u32> {
    match_field_type_text(e, self_ty).and_then(|t| binary_width(&t))
}

/// Recover signed `Binary{N}` width (`!s` marker) from field access / match-first-field.
fn match_first_field_signed_binary_width(e: &Expr, self_ty: Option<&str>) -> Option<u32> {
    match_field_type_text(e, self_ty).and_then(|t| signed_binary_width(&t))
}

/// P4/P5 (DN-99 §8 ENB-6): [`expr_env_type`] narrowed to the **signed**-marked `Binary{N}` case
/// (via [`signed_binary_width`]) — `Expr::Binary`'s signed-op gate (`add_s`/`sub_s`/`mul_s`/
/// `lt_s`) and `Expr::Unary`'s `neg_s` gate read this directly. `None` for an unmarked (unsigned)
/// `Binary{N}` entry, a non-`Binary` type, or a name absent from `env` — signedness is never
/// guessed (VR-5); it is only ever known via [`map_signature`]'s `signed_param_names` bookkeeping
/// reaching `env` through [`sig_type_env`]'s marker.
fn expr_env_signed_binary_width(e: &Expr, env: &TypeEnv) -> Option<u32> {
    expr_env_type(e, env).and_then(|t| signed_binary_width(&t))
}

/// If `e` is a struct-literal expression (`Ty { .. }` / `Self { .. }`) naming an **in-file struct
/// that actually emits** (the same [`struct_layout`] resolvability gate `Expr::Struct`'s own
/// emission arm already uses — see that arm's docs), return that struct's type name as the local's
/// known type text. `None` for every other expression shape, an unresolvable `Self`, or a struct
/// that itself does not resolve/emit (never records a type this module cannot back up — VR-5).
fn known_struct_literal_ty(e: &Expr, self_ty: Option<&str>) -> Option<String> {
    // Routed through `crate::visit::ExprVisitor` (M-1041 Scope-A): a narrow visitor overriding
    // only `visit_struct`, inheriting `fallback -> None` for every other shape -- behaviorally
    // identical to the pre-refactor `let Expr::Struct(se) = e else { return None }` this replaced.
    struct StructLitVisitor<'a> {
        self_ty: Option<&'a str>,
    }
    impl crate::visit::ExprVisitor for StructLitVisitor<'_> {
        type Output = Option<String>;

        fn fallback(&mut self, _expr: &Expr) -> Self::Output {
            None
        }

        fn visit_struct(&mut self, _expr: &Expr, se: &syn::ExprStruct) -> Self::Output {
            if se.qself.is_some() || se.rest.is_some() {
                return None;
            }
            let raw = se.path.segments.last()?.ident.to_string();
            let sty = if raw == "Self" {
                self.self_ty?.to_string()
            } else {
                raw
            };
            struct_layout(&sty).map(|_| sty)
        }
    }
    crate::visit::walk_expr(e, &mut StructLitVisitor { self_ty })
}

/// Per-file emit context installed by `transpile::transpile_source` for the item loop (see
/// [`with_emit_ctx`]): the M-1006 **resolvability set** (gates named-field-record emission), the
/// **struct layouts** (drives field-projection / struct-literal desugaring), and — gap-close-2's
/// Import lever (DN-34 §8.19/§8.20) — the batch-scoped **cross-nodule symbol table** plus this
/// file's own **pub-needed set** (names at least one sibling file in the batch resolved a `use`
/// against, so this file must emit them `pub` for the referencing `use` to be the checker-accepted
/// form — DN-113/M-1060's own `pub`-gated `resolve_imports`; see `symtab.rs` module docs). All are
/// file/batch-scoped analyses computed before the item loop runs. `None` (direct `emit_*` unit
/// tests / non-opted-in callers, and every *single-file* transpile) disables all of them — a
/// named-field record then emits unconditionally, a `self.<field>` projection gaps for want of
/// layout info, and no item is ever marked `pub` (byte-identical to pre-symtab behavior).
struct EmitCtx {
    resolvable: HashSet<String>,
    layouts: HashMap<String, StructLayout>,
    /// Parallel to [`layouts`]: positional mapped field *types* (`Binary{128}`, or
    /// `Binary{128}!s` when the Rust field was a signed integer). Used by the binary lit-zero
    /// rewrite so `self.nanos < 0` can recover width/signedness — layouts alone only store
    /// field *names* and cannot drive `binary_width` (express residual post-#1645).
    field_types: HashMap<String, Vec<Option<String>>>,
    symtab: crate::symtab::SymbolTable,
    pub_needed: HashSet<String>,
    /// DN-133 (M-1094) tier (i): mangled inherent-impl associated-fn names (`{Type}__{method}`,
    /// `mangled_inherent_fn_name`) actually emitted so far in THIS file's own single
    /// left-to-right item pass — see [`record_local_mangled_assoc_fn`]/
    /// [`local_mangled_assoc_fn_known`]. A qualified/associated-fn call site
    /// (`EmitVisitor::visit_call`) is only ever reached AFTER every earlier item in the same
    /// file has already been dispatched, so this map is exactly "what a call here could
    /// legitimately reference" — an observed fact, never a forward reference or a syntactic
    /// prediction (VR-5/G2, the D4 lesson). Starts empty every file; mutated in place as items
    /// are emitted (unlike the other fields here, which are precomputed before the item loop).
    ///
    /// **ORACLE-R1 A5:** values are the callee's parameter mapped-type Binary widths
    /// (`Some(N)` for `Binary{N}`, `None` otherwise), in declaration order — so a later call
    /// site can rewrite a bare decimal lit arg (`from_nanos(0)`) to an equal-width `BinLit`
    /// instead of file-poisoning `myc check` with Q6 "bare integer literal has no
    /// representation family" (the post-Show residual on `ManualClock`'s `impl Default` →
    /// `impl Init` body).
    local_mangled: HashMap<String, Vec<Option<u32>>>,
    /// ONESHOT C3: mapped return-type text for each local inherent / bare fn recorded above
    /// (`"Bool"`, `"Binary{32}"`, …). Lets `!method(self)` resolve as Bool logical-not when the
    /// callee was already emitted earlier in this file (std-fs `is_readonly` residual) — never a
    /// guess for a name not yet recorded (VR-5).
    local_fn_ret: HashMap<String, String>,
    /// ONESHOT C3: in-file type names for which `fn eq_<T>` was co-emitted (derive(PartialEq)
    /// product/enum — C2). Expression-level `==`/`!=` on those user types routes through the
    /// co-emitted comparator (kernel `eq` is Binary/Ternary-only) instead of the glyph that
    /// poisons as T-Op on Data types (std-fs `Metadata::is_dir` residual after the Binary !=0
    /// close).
    local_eq_types: HashSet<String>,
    /// DN-133 tier (ii): for each locally `use`-imported type NAME in this file (an
    /// `Item::Use` leaf's [`crate::symtab::CandidateKind::Name`]), the ordered cross-nodule
    /// symbol-table lookup key(s) ([`crate::symtab::SymbolTable::candidate_lookup_keys`]) that
    /// head would resolve through — the SAME precedence `transpile::dispatch_use` already
    /// applies to a plain `use` (DRY, one resolution policy). Consumed by
    /// [`cross_nodule_resolve_mangled`] to try each key's sibling `emitted` set for a
    /// `{Type}__{method}` mangled decl name. Empty in single-file/non-batch mode (no sibling to
    /// ever ask, byte-identical no-op) — see `transpile::imported_type_keys`'s doc for the
    /// currently-honest scope of this tier (the M-1084 symtab indexes per-TOP-LEVEL-ITEM
    /// emitted names, not yet each mangled per-method name, so a genuinely cross-file
    /// associated fn does not resolve through this tier today — a real, FLAGged residual, not a
    /// silently-assumed close).
    imported_type_keys: HashMap<String, Vec<String>>,
    /// DN-140 §8②/⑤: first original Rust name recorded for each emitted identifier spelling in
    /// this nodule — catches sentinel/escape self-collisions (never a silent overwrite).
    ident_emission_sources: HashMap<String, String>,
    /// Bare (un-mangled) inherent method names already emitted in this file — second occurrence
    /// of the same short name forces D4 mangling (express gap-close / `as_nanos` collision).
    bare_fn_names: HashSet<String>,
    /// Names successfully resolved by a `use` in this file's item loop (batch mode only — see
    /// [`record_imported_name`]). Used by the ORACLE-R1 A2 lattice co-emit gate so a type that
    /// already arrives via a resolved sibling import is **not** re-declared (duplicate type
    /// declaration would poison `myc check`).
    imported_names: HashSet<String>,
    /// Guarantee-lattice types (`Strength` / `GuaranteeStrength`) requested for co-emission —
    /// referenced in this file's signatures but neither declared in-file nor successfully
    /// imported. Drained after the item loop into preamble `type` items (ORACLE-R1 A2).
    lattice_co_emits: HashSet<String>,
    /// ORACLE-R1 A4: surface names of private consts co-emitted as zero-arg `fn NAME() =>
    /// Binary{N} = <BinLit>` (no const item production in the grammar). `visit_path` rewrites a
    /// bare path `NAME` to `NAME()` so Init/default bodies never file-poison with
    /// `unknown name DEFAULT_FUEL`. Values live in [`const_int_values`] for same-file path RHS.
    const_zero_arg_fns: HashSet<String>,
    /// Integer values of co-emitted consts (and known workspace-floor path RHS) keyed by bare
    /// name — used only to resolve a later const's path RHS honestly (never fabricated).
    const_int_values: HashMap<String, u128>,
}

thread_local! {
    /// See [`EmitCtx`]. Emitting a named-field record positionally is only safe for `checked_fraction`
    /// when every type it references *resolves in-file* (else it introduces a reference — `ContentRef`
    /// → the out-of-corpus `ContentHash` — that poisons the file's `myc check`); field projection is
    /// only safe when the `self` type is an *emitted* in-file struct (else the `match Ty(...)` names an
    /// absent constructor). Both gates read this context (VR-5/G2 — never emit a reference we cannot
    /// confirm resolves).
    static EMIT_CTX: RefCell<Option<EmitCtx>> = const { RefCell::new(None) };
}

/// Install the per-file emit context for the duration of `f`, then clear it (RAII-free — the
/// transpiler never unwinds across this boundary in practice; the budget thread-local in `gap.rs`
/// takes the same shape). Used by `transpile::transpile_source_with_ctx`.
pub(crate) fn with_emit_ctx<R>(
    resolvable: HashSet<String>,
    layouts: HashMap<String, StructLayout>,
    field_types: HashMap<String, Vec<Option<String>>>,
    symtab: crate::symtab::SymbolTable,
    pub_needed: HashSet<String>,
    imported_type_keys: HashMap<String, Vec<String>>,
    f: impl FnOnce() -> R,
) -> R {
    EMIT_CTX.with(|c| {
        *c.borrow_mut() = Some(EmitCtx {
            resolvable,
            layouts,
            field_types,
            symtab,
            pub_needed,
            local_mangled: HashMap::new(),
            local_fn_ret: HashMap::new(),
            local_eq_types: HashSet::new(),
            imported_type_keys,
            ident_emission_sources: HashMap::new(),
            bare_fn_names: HashSet::new(),
            imported_names: HashSet::new(),
            lattice_co_emits: HashSet::new(),
            const_zero_arg_fns: HashSet::new(),
            const_int_values: HashMap::new(),
        })
    });
    let r = f();
    EMIT_CTX.with(|c| *c.borrow_mut() = None);
    r
}

// ---- ORACLE-R1 A4: private integer const co-emit (DEFAULT_FUEL / DEFAULT_DEPTH) ----------------
//
// Mycelium's `item` production has no `const` form (only use/default/type/trait/impl/fn/object/
// lower/derive). Leaving a bare `DEFAULT_FUEL` path through from `impl Default` → `impl Init`
// file-poisons myc-check with `unknown name DEFAULT_FUEL` (eval.rs residual post-A2). Hand-port
// precedent (`lib/compiler/parse.myc` `max_expr_depth()`, `lib/compiler/ambient.myc`, …) co-emits
// private numeric floors as **zero-arg fns returning a BinLit** at the const's Binary{N} width.
// Use sites rewrite `NAME` → `NAME()` (G2/VR-5: Declared co-emit + EXPLAIN; value taken only from
// an integer literal or a known workspace-floor path — never a fabricated number).

/// Public workspace-floor associated-const last segments whose integer value is pinned in source
/// (and in hand-ports). Last-segment match only — used solely when a private const's RHS is a
/// path like `RecursionBudget::DEFAULT_DEPTH_LIMIT` (eval.rs `DEFAULT_DEPTH`). Declared table,
/// not rustc const-eval (VR-5: value is the documented floor `4096`, Exact when source matches).
const KNOWN_PATH_CONST_VALUES: &[(&str, u128)] = &[("DEFAULT_DEPTH_LIMIT", 4096)];

/// Unsigned integer types this pass will co-emit. Signed/`bool`/`str`/… stay whole-item gaps
/// (no fabricated two's-complement / string encoding).
fn const_unsigned_binary_width(ty: &syn::Type) -> Option<u32> {
    let syn::Type::Path(tp) = ty else {
        return None;
    };
    if tp.qself.is_some() {
        return None;
    }
    let seg = tp.path.segments.last()?;
    if !matches!(seg.arguments, PathArguments::None) {
        return None;
    }
    match seg.ident.to_string().as_str() {
        "u8" => Some(8),
        "u16" => Some(16),
        "u32" => Some(32),
        "u64" | "usize" => Some(64),
        "u128" => Some(128),
        _ => None,
    }
}

/// MSB-first `BinLit` of exactly `width` bits for `value`, nibble-grouped like
/// [`zero_bin_literal`]. `None` when `value` does not fit in `width` bits (never truncate — VR-5).
pub(crate) fn u128_bin_literal(value: u128, width: u32) -> Option<String> {
    if width == 0 || width > 128 {
        return None;
    }
    if width < 128 {
        let max = (1u128 << width) - 1;
        if value > max {
            return None;
        }
    }
    // width == 128: every u128 fits.
    let mut s = String::with_capacity(2 + width as usize + width as usize / 4);
    s.push_str("0b");
    for i in 0..width {
        if i > 0 && i % 4 == 0 {
            s.push('_');
        }
        let bit = (value >> (width - 1 - i)) & 1;
        s.push(if bit == 1 { '1' } else { '0' });
    }
    Some(s)
}

fn lookup_const_int_value(name: &str) -> Option<u128> {
    EMIT_CTX.with(|c| match &*c.borrow() {
        Some(ctx) => ctx.const_int_values.get(name).copied(),
        None => None,
    })
}

fn is_const_zero_arg_fn(name: &str) -> bool {
    EMIT_CTX.with(|c| match &*c.borrow() {
        Some(ctx) => ctx.const_zero_arg_fns.contains(name),
        None => false,
    })
}

fn record_const_zero_arg_fn(name: &str, value: u128) {
    EMIT_CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.const_zero_arg_fns.insert(name.to_string());
            ctx.const_int_values.insert(name.to_string(), value);
        }
    });
}

/// Extract a non-negative integer from a const initializer when it is honest and decidable:
/// a decimal/hex/bin `Lit::Int`, a paren-wrapped form of those, a same-file co-emitted const
/// path, or a last-segment match against [`KNOWN_PATH_CONST_VALUES`]. Everything else → `None`
/// (caller whole-gaps the const — never guesses a value).
fn try_const_int_value(expr: &Expr) -> Option<u128> {
    match expr {
        Expr::Lit(el) => match &el.lit {
            Lit::Int(i) => i.base10_parse::<u128>().ok(),
            _ => None,
        },
        Expr::Paren(p) => try_const_int_value(&p.expr),
        Expr::Path(p) if p.qself.is_none() => {
            let last = p.path.segments.last()?.ident.to_string();
            if let Some(v) = lookup_const_int_value(&last) {
                return Some(v);
            }
            KNOWN_PATH_CONST_VALUES
                .iter()
                .find(|(n, _)| *n == last)
                .map(|(_, v)| *v)
        }
        _ => None,
    }
}

/// ORACLE-R1 A4: co-emit a top-level unsigned integer `const` as a zero-arg fn whose body is a
/// width-exact `BinLit` (hand-port `max_expr_depth` shape). Gaps when the type is not a plain
/// unsigned integer, the initializer is not a decidable integer, or the value does not fit the
/// mapped width — never a fabricated const item or a silent wrong number (G2/VR-5).
pub fn emit_const(item: &ItemConst) -> Result<Emitted, GapReason> {
    let raw_name = item.ident.to_string();
    let Some(width) = const_unsigned_binary_width(&item.ty) else {
        return Err(GapReason::new(
            Category::Other,
            format!(
                "top-level `const {raw_name}` — co-emit (ORACLE-R1 A4) only covers plain unsigned \
                 integer types (`u8`/`u16`/`u32`/`u64`/`u128`/`usize`); other types have no const \
                 item production and no faithful zero-arg-fn encoding (gap, never fabricate)"
            ),
        ));
    };
    let Some(value) = try_const_int_value(&item.expr) else {
        return Err(GapReason::new(
            Category::Other,
            format!(
                "top-level `const {raw_name}` — initializer is not a decidable non-negative integer \
                 literal or a known workspace-floor path (e.g. `RecursionBudget::DEFAULT_DEPTH_LIMIT`); \
                 no const item production in the grammar, and co-emit refuses to invent a value \
                 (ORACLE-R1 A4 / VR-5)"
            ),
        ));
    };
    let Some(body) = u128_bin_literal(value, width) else {
        return Err(GapReason::new(
            Category::Other,
            format!(
                "top-level `const {raw_name}` — value {value} does not fit in Binary{{{width}}} \
                 (ORACLE-R1 A4 refuses silent truncation; VR-5)"
            ),
        ));
    };

    let fn_vi = valid_ident(&raw_name);
    register_ident_emission(&fn_vi, "const co-emit zero-arg fn name")?;
    let fn_name = fn_vi.text.clone();
    let mut ident_doc = Vec::new();
    push_rewrite_doc(&fn_vi, &mut ident_doc);

    // Register under both original and emitted spellings so visit_path rewrites either form.
    record_const_zero_arg_fn(&raw_name, value);
    if fn_name != raw_name {
        record_const_zero_arg_fn(&fn_name, value);
    }
    record_bare_fn_name(&fn_name);

    let mut sub_gaps = Vec::new();
    let non_doc = non_doc_attrs(&item.attrs);
    if !non_doc.is_empty() {
        sub_gaps.push(GapReason::new(
            Category::DeriveAttr,
            format!(
                "dropped non-doc attribute(s) on const `{raw_name}`: {}",
                non_doc.join(" ")
            ),
        ));
    }

    let mut myc = String::new();
    for d in doc_lines(&item.attrs) {
        myc.push_str(&d);
        myc.push('\n');
    }
    myc.push_str(
        "// Declared: co-emitted private const as zero-arg fn returning a BinLit — Mycelium has no \
         const item production (`item` covers use/default/type/trait/impl/fn/object/lower/derive \
         only); hand-port precedent `max_expr_depth()` (ORACLE-R1 A4; G2/VR-5: value from the Rust \
         initializer or a known workspace-floor path, never fabricated). Use sites rewrite \
         `NAME` → `NAME()`.\n",
    );
    for d in &ident_doc {
        myc.push_str(d);
        myc.push('\n');
    }
    // Decimal in a trailing comment only (EXPLAIN); the body is the paradigm-safe BinLit.
    myc.push_str(&format!(
        "fn {fn_name}() => Binary{{{width}}} = {body}; // {value}"
    ));

    Ok(Emitted {
        name: fn_name,
        myc,
        sub_gaps,
    })
}

// ---- ORACLE-R1 A2: guarantee-lattice co-emit ---------------------------------------------------
//
// `Strength` (mycelium-l1 AST) and `GuaranteeStrength` (mycelium-core) are isomorphic unit enums
// over the reserved lattice keywords `Exact|Proven|Empirical|Declared`. When a free fn like
// `strength_of` references them but the defining enum is **not** in this file and **not**
// available via a resolved batch `use`, a bare passthrough type name poisons the whole file's
// `myc check` with `unknown type Strength` (file-level checked_fraction → 0). Co-emitting the
// same sum type `emit_enum` would produce for that unit enum — with DN-140 `*_kw` variant
// renames — restores a checkable surface without fabricating language prims (G2/VR-5: Declared
// co-emit, EXPLAIN comment; never silent). Hand-port precedent: `lib/compiler/*.myc` redeclares
// `type Strength = GExact | …` in every nodule that needs it.

/// Unit-enum names that are the surface/kernel guarantee lattice (same four reserved variants).
const LATTICE_TYPE_NAMES: &[&str] = &["Strength", "GuaranteeStrength"];

/// The lattice's four reserved-word variants (DN-140 rewrites each to `*_kw` on emission).
const LATTICE_VARIANTS: &[&str] = &["Exact", "Proven", "Empirical", "Declared"];

/// Record a name successfully resolved by `transpile::dispatch_use` so lattice co-emit will not
/// redeclare it.
pub(crate) fn record_imported_name(name: &str) {
    EMIT_CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.imported_names.insert(name.to_string());
        }
    });
}

/// Whether `name` is already available in this file (declared resolvable, imported, or already
/// queued for lattice co-emit).
fn lattice_name_available(name: &str) -> bool {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => false,
        Some(ctx) => {
            ctx.resolvable.contains(name)
                || ctx.imported_names.contains(name)
                || ctx.lattice_co_emits.contains(name)
        }
    })
}

/// If `name` is a lattice type and not already available, queue a co-emitted `type` item.
fn request_lattice_co_emit(name: &str) {
    if !LATTICE_TYPE_NAMES.contains(&name) {
        return;
    }
    if lattice_name_available(name) {
        return;
    }
    EMIT_CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.lattice_co_emits.insert(name.to_string());
        }
    });
}

/// Walk a Rust type for bare user-type deps; queue lattice co-emits for any missing lattice names.
fn note_lattice_deps_from_ty(ty: &syn::Type) {
    let mut deps = Vec::new();
    // `field_type_user_deps` returns false when the type is unmappable — nothing to co-emit then
    // (the surrounding item will gap on map_type for other reasons).
    let _ = crate::map::field_type_user_deps(ty, &mut deps);
    for d in deps {
        request_lattice_co_emit(&d);
    }
}

/// Queue lattice co-emits for every user type mentioned in a free-fn / method signature.
fn note_lattice_deps_from_sig(sig: &Signature) {
    for input in &sig.inputs {
        if let FnArg::Typed(pt) = input {
            note_lattice_deps_from_ty(&pt.ty);
        }
    }
    if let ReturnType::Type(_, ty) = &sig.output {
        note_lattice_deps_from_ty(ty);
    }
}

/// Drain the lattice co-emit set into ordered `(emitted_name, myc_chunk)` pairs. Must be called
/// **inside** [`with_emit_ctx`] (before the context is cleared) so DN-140 variant renames can
/// register against the same per-file ident-emission map the item loop used.
pub(crate) fn drain_lattice_co_emits() -> Vec<(String, String)> {
    let names: Vec<String> = EMIT_CTX.with(|c| match c.borrow_mut().as_mut() {
        Some(ctx) => {
            let mut v: Vec<String> = ctx.lattice_co_emits.drain().collect();
            v.sort();
            v
        }
        None => Vec::new(),
    });
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        // Register type name (non-reserved) + each variant via valid_ident so Exact→Exact_kw is
        // the same rewrite strength_of's match arms already apply (DN-140).
        let type_vi = valid_ident(&name);
        if let Err(g) = register_ident_emission(&type_vi, "lattice co-emit type name") {
            // Collision with a prior emission of a different original → refuse this co-emit
            // (never silent overwrite). The referencing item stays emitted; myc-check may still
            // see the type missing — that residual is FLAGged by the collision gap path if the
            // driver surfaces it. For A2 the lattice names are free in eval.rs.
            let _ = g;
            continue;
        }
        let mut doc = Vec::new();
        push_rewrite_doc(&type_vi, &mut doc);
        let mut ctors = Vec::with_capacity(LATTICE_VARIANTS.len());
        let mut variant_ok = true;
        for v in LATTICE_VARIANTS {
            let vi = valid_ident(v);
            if register_ident_emission(&vi, "lattice co-emit variant").is_err() {
                variant_ok = false;
                break;
            }
            push_rewrite_doc(&vi, &mut doc);
            ctors.push(vi.text);
        }
        if !variant_ok {
            continue;
        }
        let mut myc = String::new();
        myc.push_str(
            "// Declared: co-emitted guarantee-lattice type — referenced in this file but not \
             declared here and not available via a resolved batch `use` (ORACLE-R1 A2; never \
             silent unknown-type file poison — G2/VR-5). Variants use DN-140 `*_kw` renames for \
             the reserved lattice keywords (Exact/Proven/Empirical/Declared).\n",
        );
        for d in &doc {
            myc.push_str(d);
            myc.push('\n');
        }
        myc.push_str(&format!("type {} = {};", type_vi.text, ctors.join(" | ")));
        out.push((type_vi.text, myc));
    }
    out
}

// ---- G-α Rank-1: ambient Result / Option co-emit (oracle self-containment) ---------------------
//
// myc-check seeds Bool/Unit unconditionally and Vec conditionally (`CONDITIONAL_PRELUDE_TYPE_NAMES`);
// **Result and Option are not ambient.** Emission already maps Rust `Result<A,E>` / `Option<A>` to
// surface `Result[A, E]` / `Option[A]`, so free-fns like std-io `read_all => Result[Vec[…], IoError]`
// file-poison with `unknown type Result` under the live oracle. Live-oracle tests historically
// injected the same type shape `lib/std/result.myc` / `lib/std/option.myc` declare. Co-emit that
// **type shape only** (never combinators — VR-5: no fabricated `map`/`and_then`/…) once per file
// when the assembled body mentions the type head and does not already define it (G2/EXPLAIN).

/// True when `body` already carries a top-level `type {name}[…]` / `type {name} =` definition
/// (including a prior ambient co-emit or a batch type co-include).
fn body_defines_type_head(body: &str, name: &str) -> bool {
    // Match the surface forms this transpiler and `lib/std/{result,option}.myc` emit:
    //   `type Result[A, E] = …`  /  `type Option[A] = …`  /  unit-sum `type Foo = A | B`
    body.contains(&format!("type {name}[")) || body.contains(&format!("type {name} ="))
}

/// True when `body` mentions the generic type head `{name}[…]` (the only form emission produces
/// for Result/Option). Avoids bare-word false positives in prose comments that name the concept.
fn body_mentions_type_head(body: &str, name: &str) -> bool {
    body.contains(&format!("{name}["))
}

/// G-α Rank-1 / L2-A: if the assembled body mentions `Result[…]` / `Option[…]` and does not already
/// define those type heads, return ordered `(emitted_name, myc_chunk)` pairs for a one-shot ambient
/// co-emit. Shape is **byte-identical** to `lib/std/result.myc` / `lib/std/option.myc` type lines;
/// combinators are intentionally omitted (Declared ambient for oracle self-containment — never a
/// silent stdlib substitute). Pure over body text — no emit-ctx mutation.
pub(crate) fn ambient_result_option_preamble(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if body_mentions_type_head(body, "Result") && !body_defines_type_head(body, "Result") {
        out.push((
            "Result".to_string(),
            // EXPLAIN deliberately does NOT embed the full `type Result[…] = …` spelling (that
            // would make naive substring "once" checks / future define-gates double-count).
            "// Declared: ambient Result type co-emit — oracle self-containment (G-α Rank-1 / \
             L2-A). Type shape cites lib/std/result.myc (Ok/Err sum); combinators are NOT \
             fabricated (VR-5/G2). Checker does not seed Result as prelude (unlike Bool/Unit \
             unconditional, Vec conditional).\n\
             type Result[A, E] = Ok(A) | Err(E);"
                .to_string(),
        ));
    }
    if body_mentions_type_head(body, "Option") && !body_defines_type_head(body, "Option") {
        out.push((
            "Option".to_string(),
            "// Declared: ambient Option type co-emit — oracle self-containment (G-α Rank-1 / \
             L2-A). Type shape cites lib/std/option.myc (Some/None sum); combinators are NOT \
             fabricated (VR-5/G2). Checker does not seed Option as prelude (unlike Bool/Unit \
             unconditional, Vec conditional).\n\
             type Option[A] = Some(A) | None;"
                .to_string(),
        ));
    }
    out
}

/// Re-export for call-site resolution (DN-140 §7).
pub(crate) use crate::reserved::mangled_inherent_fn_name;

/// DN-140: map `raw` to a legal emitted identifier and register per-unit self-collision state.
fn resolve_surface_ident(raw: &str, position: &str) -> Result<String, GapReason> {
    let vi = valid_ident(raw);
    register_ident_emission(&vi, position)?;
    Ok(vi.text)
}

fn register_ident_emission(vi: &ValidIdent, position: &str) -> Result<(), GapReason> {
    let Some(r) = &vi.rewrite else {
        return Ok(());
    };
    EMIT_CTX.with(|c| {
        let mut slot = c.borrow_mut();
        let Some(ctx) = slot.as_mut() else {
            return Ok(());
        };
        if let Some(prev) = ctx.ident_emission_sources.get(&vi.text) {
            if prev != &r.original {
                return Err(GapReason::new(
                    Category::ReservedWord,
                    format!(
                        "identifier emission collision at {position}: `{prev}` and `{}` both map to \
                         emitted `{emitted}` — DN-140 §8②/⑤ per-unit self-collision GAP, never a silent \
                         overwrite (G2)",
                        r.original,
                        emitted = vi.text,
                    ),
                ));
            }
        } else {
            ctx.ident_emission_sources
                .insert(vi.text.clone(), r.original.clone());
        }
        Ok(())
    })
}

fn push_rewrite_doc(vi: &ValidIdent, doc: &mut Vec<String>) {
    if let Some(line) = declared_rewrite_comment(vi) {
        doc.push(line);
    }
}

/// Whether a named-field record named `name` may be emitted under the M-1006 resolvability gate.
/// Context off (`None`) ⇒ always allowed; on ⇒ allowed iff `name` is resolvable in-file.
///
/// **`name` must be the Rust source ident** (e.g. `Substrate`), not a DN-140 `valid_ident`
/// rewrite (`Substrate_kw`). [`crate::transpile::resolvable_type_names`] keys the set by source
/// idents; checking the rewritten form false-gaps every reserved-word named-field struct even
/// when its fields fully resolve (std-io `Substrate` — L2-C residual; G2/VR-5).
fn named_field_emit_allowed(name: &str) -> bool {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => true,
        Some(ctx) => ctx.resolvable.contains(name),
    })
}

/// The positional field layout of the in-file struct `name`, when known **and** the struct is
/// resolvable (i.e. emitted — so its constructor exists to desugar against). `None` disables the
/// field-projection / struct-literal desugaring for `name` (context off, `name` not an in-file
/// single-ctor struct, or `name` not emitted — where a `match name(...) => …` would reference an
/// absent ctor and poison the file's check).
pub(crate) fn struct_layout(name: &str) -> Option<StructLayout> {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => None,
        Some(ctx) if ctx.resolvable.contains(name) => ctx.layouts.get(name).cloned(),
        Some(_) => None,
    })
}

/// Positional mapped field *types* for the in-file struct `name` (parallel to [`struct_layout`]),
/// when known and resolvable. Entries are `map_type` text, with a trailing `"!s"` when the Rust
/// field was a signed integer (same internal marker [`sig_type_env`] uses). `None` when context
/// is off or the type is not resolvable/emitted.
fn struct_field_types(name: &str) -> Option<Vec<Option<String>>> {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => None,
        Some(ctx) if ctx.resolvable.contains(name) => ctx.field_types.get(name).cloned(),
        Some(_) => None,
    })
}

/// The M-1006 Lever 1 field-projection text for reading position `pos` of `sty` off `base`: a
/// `match` binding exactly that position and wildcarding the rest, parenthesized so it composes
/// as an operand subexpression (`(match self { Ty(p0, _, ..) => p0 })`). Shared by
/// [`EmitVisitor::visit_field`] (`base == "self"`, an ordinary field READ) and (DN-125/M-1081)
/// [`reconstruct_positional`] (reading every UNCHANGED field while rebuilding `sty` with one
/// position replaced) — kept as one function so the two call sites can never emit a differently-
/// shaped projection for what is semantically the same operation (DRY, house rule #5).
fn field_projection_text(sty: &str, layout: &StructLayout, base: &str, pos: usize) -> String {
    let bind = format!("p{pos}");
    let pats: Vec<String> = (0..layout.len())
        .map(|i| {
            if i == pos {
                bind.clone()
            } else {
                "_".to_string()
            }
        })
        .collect();
    format!("(match {base} {{ {sty}({}) => {bind} }})", pats.join(", "))
}

/// The gap-close-2 cross-nodule `pub`-propagation gate: `"pub "` when `name` is in this file's
/// pub-needed set (at least one sibling in the batch resolved a `use` against it — see [`EmitCtx`]
/// docs), else `""`. Context off ⇒ always `""` (byte-identical to pre-symtab emission).
pub(crate) fn pub_prefix(name: &str) -> &'static str {
    EMIT_CTX.with(|c| match &*c.borrow() {
        Some(ctx) if ctx.pub_needed.contains(name) => "pub ",
        _ => "",
    })
}

/// Resolve `name` in the batch sibling named by `module_key` (dot-joined Rust module-path
/// segments) against the installed cross-nodule symbol table (see [`EmitCtx`] docs). `None` when
/// the context is off (single-file mode — no batch, no siblings) or the lookup misses.
pub(crate) fn cross_nodule_resolve(module_key: &str, name: &str) -> Option<String> {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => None,
        Some(ctx) => ctx.symtab.resolve(module_key, name).map(str::to_owned),
    })
}

/// Is `module_key` a batch sibling at all (regardless of whether a particular name resolves)? Used
/// by `transpile::dispatch_use` to word an honest "not a batch sibling" vs "sibling gapped this
/// name" reason. `false` when the context is off.
pub(crate) fn cross_nodule_has_module(module_key: &str) -> bool {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => false,
        Some(ctx) => ctx.symtab.has_module(module_key),
    })
}

/// L2-B: does `module_key` have a baseline single-line type def for `name`? Used by
/// `transpile::dispatch_use` to choose co-include vs full-path `use`.
pub(crate) fn cross_nodule_has_type_def(module_key: &str, name: &str) -> bool {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => false,
        Some(ctx) => ctx.symtab.type_def(module_key, name).is_some(),
    })
}

/// G-α L2-B non-type: does `module_key` have a baseline single-line free-fn def for `name`?
pub(crate) fn cross_nodule_has_fn_def(module_key: &str, name: &str) -> bool {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => false,
        Some(ctx) => ctx.symtab.fn_def(module_key, name).is_some(),
    })
}

/// G-α L2-B non-type: baseline free-fn line for `name` in `module_key`, when present.
pub(crate) fn cross_nodule_fn_def(module_key: &str, name: &str) -> Option<String> {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => None,
        Some(ctx) => ctx.symtab.fn_def(module_key, name).map(str::to_owned),
    })
}

/// L2-B: transitive type-def co-include set for seed `(module_key, name)` pairs.
/// Empty when the context is off or no seed has a type def in the table.
pub(crate) fn cross_nodule_type_def_closure(seeds: &[(String, String)]) -> Vec<(String, String)> {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => Vec::new(),
        Some(ctx) => ctx.symtab.type_def_closure(seeds),
    })
}

/// G-α L2-B non-type: free-fn co-include lines for seed `(module_key, name)` pairs.
pub(crate) fn cross_nodule_fn_def_lines(seeds: &[(String, String)]) -> Vec<(String, String)> {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => Vec::new(),
        Some(ctx) => ctx.symtab.fn_def_lines(seeds),
    })
}

/// Whether `name` is already available in this file (declared resolvable, imported, or lattice
/// co-emitted) — L2-B skips re-co-including a name the consumer already has.
pub(crate) fn name_already_available(name: &str) -> bool {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => false,
        Some(ctx) => {
            ctx.resolvable.contains(name)
                || ctx.imported_names.contains(name)
                || ctx.lattice_co_emits.contains(name)
        }
    })
}

/// DN-133 (M-1094) tier (i): record that this file's own single-pass emission just successfully
/// produced the mangled inherent-impl associated-fn `mangled_name` (`mangled_inherent_fn_name`'s
/// `{Type}__{method}` form) — called once, from `emit_impl`'s success path, right after it renames
/// such a method, so a LATER call site in the SAME file can resolve against it (see
/// [`local_mangled_assoc_fn_known`]). `param_tys` is the mapped signature params
/// (`(name, Binary{N}|…)`); their Binary widths power ORACLE-R1 A5 call-arg lit rewrite. No-op
/// when the context is off (`None` — direct `emit_impl` unit tests never install a context, so
/// this degrades to always-absent, matching every OTHER `EmitCtx`-gated behavior's off-mode).
fn record_local_mangled_assoc_fn(mangled_name: &str, param_tys: &[(String, String)], ret_ty: &str) {
    let widths: Vec<Option<u32>> = param_tys.iter().map(|(_, ty)| binary_width(ty)).collect();
    EMIT_CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.local_mangled.insert(mangled_name.to_string(), widths);
            // Return-type bookkeeping for UnOp::Not / Bool `!=` composition (ONESHOT C3).
            // Empty/`unit`-shaped returns are still recorded honestly — consumers that need a
            // specific type (`Bool`) match exactly; never invent a default.
            if !ret_ty.is_empty() {
                ctx.local_fn_ret
                    .insert(mangled_name.to_string(), ret_ty.to_string());
            }
        }
    });
}

/// ONESHOT C3: mapped return type of a local inherent/bare fn already recorded this file.
/// `None` when unknown or emit context is off — never a fabricated type (VR-5).
fn local_fn_ret_ty(name: &str) -> Option<String> {
    EMIT_CTX.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|ctx| ctx.local_fn_ret.get(name).cloned())
    })
}

/// ONESHOT C3: record that `fn eq_<ty_name>` was co-emitted for this file (derive PartialEq).
fn record_local_eq_type(ty_name: &str) {
    EMIT_CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.local_eq_types.insert(ty_name.to_string());
        }
    });
}

/// ONESHOT C3: whether `fn eq_<ty_name>` is known to have been co-emitted this file.
fn local_eq_type_known(ty_name: &str) -> bool {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => false,
        Some(ctx) => ctx.local_eq_types.contains(ty_name),
    })
}

/// ONESHOT C3: recover a **user-named** (non-builtin) type for an expression used in `==`/`!=`,
/// so we can route through a co-emitted `eq_<T>`. Builtins (`Bool`/`Bytes`/`Binary{N}`) return
/// `None` — those have their own arms. Never guesses (VR-5).
fn expr_user_named_type(e: &Expr, env: &TypeEnv, self_ty: Option<&str>) -> Option<String> {
    let is_user = |t: &str| -> bool {
        t != "Bool"
            && t != "Bytes"
            && binary_width(t).is_none()
            && signed_binary_width(t).is_none()
            && !t.starts_with("Vec[")
            && !t.starts_with("Option[")
            && !t.starts_with("Result[")
    };
    if let Some(t) = expr_env_type(e, env) {
        if is_user(&t) {
            return Some(t);
        }
    }
    if let Some(t) = match_field_type_text(e, self_ty) {
        // Strip signed marker if present (user types never carry it, but be safe).
        let bare = t.strip_suffix("!s").unwrap_or(&t);
        if is_user(bare) {
            return Some(bare.to_string());
        }
    }
    match e {
        Expr::Paren(p) => expr_user_named_type(&p.expr, env, self_ty),
        Expr::Reference(r) => expr_user_named_type(&r.expr, env, self_ty),
        // `Type::Variant` / `Type::assoc` — the head segment is the type name when this is a
        // unit-variant path used in `kind == FileKind::File`.
        Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 2 => {
            let head = p.path.segments.first()?.ident.to_string();
            if is_user(&head) && local_eq_type_known(&head) {
                Some(head)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// DN-133 tier (i): whether `mangled_name` was already recorded via
/// [`record_local_mangled_assoc_fn`] — an EARLIER item in this same file's own left-to-right pass
/// really did emit it. `false` when the context is off.
pub(crate) fn local_mangled_assoc_fn_known(mangled_name: &str) -> bool {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => false,
        Some(ctx) => ctx.local_mangled.contains_key(mangled_name),
    })
}

/// ORACLE-R1 A5: per-parameter Binary widths for a known local mangled assoc fn.
/// `None` when the name is unknown or the emit context is off.
fn local_mangled_param_binary_widths(mangled_name: &str) -> Option<Vec<Option<u32>>> {
    EMIT_CTX.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|ctx| ctx.local_mangled.get(mangled_name).cloned())
    })
}

/// Rewrite a bare decimal integer literal to an equal-width Mycelium `BinLit` when it fits in
/// `width` bits (shared by comparison lit-zero rewrite and ORACLE-R1 A5 call-arg rewrite).
/// `None` when `e` is not an int lit, the digits do not parse, or the value does not fit.
fn int_lit_as_bin_literal(e: &Expr, width: u32) -> Option<String> {
    let Expr::Lit(el) = e else {
        return None;
    };
    let Lit::Int(i) = &el.lit else {
        return None;
    };
    let digits = i.base10_digits();
    let Ok(v) = digits.parse::<u128>() else {
        return None;
    };
    if v == 0 {
        return Some(zero_bin_literal(width));
    }
    let mut bits = format!("{v:b}");
    if bits.len() as u32 > width {
        return None;
    }
    while (bits.len() as u32) < width {
        bits.insert(0, '0');
    }
    let mut s = String::from("0b");
    for (i, c) in bits.chars().enumerate() {
        if i > 0 && i % 4 == 0 {
            s.push('_');
        }
        s.push(c);
    }
    Some(s)
}

/// Express gap-close (2026-07-16): bare top-level fn names already used in this file's emit
/// (inherent methods left un-mangled on first occurrence). Second use forces D4 mangling.
pub(crate) fn bare_fn_name_taken(name: &str) -> bool {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => false,
        Some(ctx) => ctx.bare_fn_names.contains(name),
    })
}

pub(crate) fn record_bare_fn_name(name: &str) {
    EMIT_CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.bare_fn_names.insert(name.to_string());
        }
    });
}

/// Claim a bare top-level fn name for this file's emission. Returns `true` if this is the **first**
/// claim (caller should emit the body) and `false` if the name is already taken (caller must **not**
/// re-emit — would file-poison myc-check with `duplicate function`). Used by derive aux helpers
/// (`show_vec_*` / eventual `eq_vec_*`) so two structs with the same `Vec[ELEM]` field shape share
/// one helper (std-io `Substrate`+`Sink` both need `show_vec_Binary_8_` after L2-C type emission).
/// Context off ⇒ always `true` (single-item tests have no cross-struct collision surface).
pub(crate) fn claim_bare_fn_name(name: &str) -> bool {
    EMIT_CTX.with(|c| match c.borrow_mut().as_mut() {
        None => true,
        Some(ctx) => {
            if ctx.bare_fn_names.contains(name) {
                false
            } else {
                ctx.bare_fn_names.insert(name.to_string());
                true
            }
        }
    })
}

/// DN-133 tier (ii): resolve `mangled_name` via the M-1084 cross-nodule symbol table, using
/// `head`'s own resolved `use`-import candidate key(s) (see [`EmitCtx::imported_type_keys`]).
/// `false` when the context is off, `head` was not imported via a resolvable `use` in this file,
/// or no candidate key's sibling module has `mangled_name` in its own emitted-name set — which,
/// honestly, is EVERY case today: that set is populated from `GapReport::emitted_items`, which
/// records an inherent `impl` block under its own coarse `"impl {Type}"` name (`emit_impl`'s
/// `Emitted::name`), not each individual mangled method it contains. So this tier is currently a
/// safe no-op for a genuinely cross-file/cross-phylum associated fn — never a false positive
/// (VR-5/G2) — pending a follow-up that also indexes each mangled per-method name in the batch
/// symbol table (FLAGged in this leaf's report, not silently assumed closed).
fn cross_nodule_resolve_mangled(head: &str, mangled_name: &str) -> bool {
    EMIT_CTX.with(|c| match &*c.borrow() {
        None => false,
        Some(ctx) => match ctx.imported_type_keys.get(head) {
            None => false,
            Some(keys) => keys
                .iter()
                .any(|k| ctx.symtab.resolve(k, mangled_name).is_some()),
        },
    })
}

/// The `.myc` text (+ any dropped sub-features, e.g. attributes) for one successfully emitted
/// top-level item.
pub struct Emitted {
    pub name: String,
    pub myc: String,
    /// Sub-features of this *otherwise-emitted* item that were still dropped (e.g. a
    /// `#[derive(..)]`, or — for an `impl` block — a method that individually failed to map).
    /// Recorded so the item can be simultaneously "emitted" (its core structure landed) and
    /// "in gaps" (something about it is honestly flagged) — both is allowed; only "neither" is
    /// forbidden (see `GapReport` docs).
    pub sub_gaps: Vec<GapReason>,
}

// ---------------------------------------------------------------------------------------------
// Shared helpers: doc/attr extraction, generic-parameter mapping, fn-signature mapping.
// ---------------------------------------------------------------------------------------------

/// Extract `///`/`//!` doc-comment lines (represented by `syn` as `#[doc = "..."]` attributes),
/// rendered as plain `//` line comments (grammar: "line comments start with '//' ... ignored by
/// the grammar" — doc comments have no first-class surface form, so this is the closest honest
/// mapping: preserved as prose, not as a structured doc construct).
pub fn doc_lines(attrs: &[Attribute]) -> Vec<String> {
    let mut lines = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("doc") {
            if let syn::Meta::NameValue(nv) = &attr.meta {
                if let Expr::Lit(syn::ExprLit {
                    lit: Lit::Str(s), ..
                }) = &nv.value
                {
                    lines.push(format!("//{}", s.value()));
                }
            }
        }
    }
    lines
}

/// Every non-doc attribute on an item, rendered as text — these are always dropped (KNOWN HARD
/// GAP: derive/`#[...]` attributes have no confirmed Mycelium surface), recorded via a
/// [`Category::DeriveAttr`] sub-gap rather than silently discarded.
pub fn non_doc_attrs(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|a| !a.path().is_ident("doc"))
        .map(tokens_to_string)
        .collect()
}

/// [`non_doc_attrs`] narrowed to exclude `#[derive(...)]` as well (DN-128/M-1086) — used by
/// [`emit_struct`] and [`emit_enum`], whose derive lists are classified/lowered separately (see
/// the "DN-128 std-derive lowering library" section below; ONESHOT C2 enum half) rather than
/// bulk-dropped. Every OTHER non-doc attribute (`#[repr(C)]`, an unrecognized macro attribute, …)
/// still falls through to the same unconditional-drop `Category::DeriveAttr` sub-gap
/// `non_doc_attrs` backs at fn/impl-method sites.
fn non_doc_non_derive_attrs(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|a| !a.path().is_ident("doc") && !a.path().is_ident("derive"))
        .map(tokens_to_string)
        .collect()
}

/// Heuristic `#[cfg(test)]` detection (Declared: a token-text `contains("test")` check, not a
/// real `cfg` predicate evaluator).
pub fn is_cfg_test(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().is_ident("cfg") && tokens_to_string(a).contains("test"))
}

/// Map a `Generics` list to Mycelium's bare `type_params ::= '[' Ident (',' Ident)* ']'` —
/// confirmed to allow *only* unbounded type identifiers (grammar comment: "a fn generic over
/// both is `[T]{N}`"; bounds live on individual `fn` params via `RFC-0019 §4.1`, not on the
/// type-param list itself in this fragment). A lifetime, a bounded type param, or a const
/// generic each has no confirmed slot here.
fn plain_type_params(generics: &Generics) -> Result<Vec<String>, GapReason> {
    if generics.where_clause.is_some() {
        return Err(GapReason::new(
            Category::WhereClause,
            "a `where` clause has no Mycelium equivalent",
        ));
    }
    let mut names = Vec::new();
    for p in &generics.params {
        match p {
            GenericParam::Type(tp) => {
                if !tp.bounds.is_empty() {
                    return Err(GapReason::new(
                        Category::GenericBound,
                        format!(
                            "type parameter `{}` carries a bound — type_params/fn generics are \
                             bare identifiers only in this grammar fragment",
                            tp.ident
                        ),
                    ));
                }
                // Same emit-verbatim exposure as fn parameters: an UNUSED type-param name never
                // reaches map_type's guard, so guard at the declaration site too.
                let name = resolve_surface_ident(&tp.ident.to_string(), "type parameter")?;
                names.push(name);
            }
            GenericParam::Lifetime(lt) => {
                return Err(GapReason::new(
                    Category::GenericBound,
                    format!(
                        "lifetime parameter `{}` has no grammar surface",
                        lt.lifetime
                    ),
                ));
            }
            GenericParam::Const(cp) => {
                return Err(GapReason::new(
                    Category::GenericBound,
                    format!(
                        "const generic parameter `{}` — correspondence with Mycelium's width \
                         const_params (`{{N}}`) is not confirmed",
                        cp.ident
                    ),
                ));
            }
        }
    }
    Ok(names)
}

/// DN-131 (Accepted; M-1088/M-1101 build) — map an **inherent**-impl `Generics` list to
/// Mycelium's impl-slot `type_param ::= Ident (':' bound)?` grammar (RFC-0019 §4.1, already
/// landed for `fn` generics via `parse_type_params_bounded`/`check_bounds`). Each returned
/// entry is the impl-slot's own type-param text — `"T"` for an unbounded parameter or
/// `"T: A + B"` for a bounded one — ready to join into the impl's own `[...]` list. Unlike
/// [`plain_type_params`] (bare identifiers only, used by `fn`/`enum`/`struct`/`trait`
/// declaration-head sites this leaf does not touch), this function is the impl-slot's own
/// bounded surface (DN-131 §3): the bound rides through unchanged, redistributed by DN-103's
/// Phase-0 desugar onto each lifted method and discharged by the already-landed `check_bounds` +
/// dictionary-free monomorphizer — zero new discharge logic.
///
/// Scope (never-silent, G2): a lifetime parameter or a const-generic parameter gaps exactly as
/// `plain_type_params` does. A bound is emitted only when it is a **plain trait name** — no
/// type arguments (`T: Into<u8>`), no `?`-relaxed modifier (`T: ?Sized`), no higher-ranked
/// `for<'a>` binder, no parenthesized trait — matching the DN-131 v1 surface this leaf builds
/// (`bound ::= Ident type_args? ('+' Ident type_args?)*` technically allows bound type
/// arguments too, but this leaf scopes to the plain-name case the DN-136 worklist specs and
/// gaps a bound-type-arg shape explicitly rather than guessing a mapping, VR-5).
fn bounded_impl_type_params(generics: &Generics) -> Result<Vec<String>, GapReason> {
    let mut names = Vec::with_capacity(generics.params.len());
    for p in &generics.params {
        match p {
            GenericParam::Type(tp) => {
                // Same emit-verbatim exposure as `plain_type_params`: an UNUSED type-param
                // name never reaches `map_type`'s guard, so guard at the declaration site too.
                let tp_name = resolve_surface_ident(&tp.ident.to_string(), "impl type parameter")?;
                if tp.bounds.is_empty() {
                    names.push(tp_name);
                    continue;
                }
                let mut bound_names = Vec::with_capacity(tp.bounds.len());
                for b in &tp.bounds {
                    let TypeParamBound::Trait(tb) = b else {
                        return Err(GapReason::new(
                            Category::GenericBound,
                            format!(
                                "impl type parameter `{}` carries a bound with no confirmed \
                                 mapping (a lifetime bound or another non-trait bound form) — \
                                 DN-131 v1 covers plain trait-name bounds only",
                                tp.ident
                            ),
                        ));
                    };
                    if tb.paren_token.is_some()
                        || tb.lifetimes.is_some()
                        || !matches!(tb.modifier, TraitBoundModifier::None)
                    {
                        return Err(GapReason::new(
                            Category::GenericBound,
                            format!(
                                "impl type parameter `{}` bound `{}` is parenthesized, \
                                 `?`-relaxed, or carries a higher-ranked `for<..>` binder — no \
                                 confirmed mapping (DN-131 v1 covers plain trait-name bounds \
                                 only)",
                                tp.ident,
                                tokens_to_string(&tb.path)
                            ),
                        ));
                    }
                    let seg = tb.path.segments.last().ok_or_else(|| {
                        GapReason::new(
                            Category::GenericBound,
                            format!(
                                "impl type parameter `{}` bound has an empty trait path",
                                tp.ident
                            ),
                        )
                    })?;
                    if !matches!(seg.arguments, PathArguments::None) {
                        return Err(GapReason::new(
                            Category::GenericBound,
                            format!(
                                "impl type parameter `{}` bound `{}` carries generic arguments \
                                 — DN-131 v1 emits plain trait-name bounds only",
                                tp.ident,
                                tokens_to_string(&tb.path)
                            ),
                        ));
                    }
                    let bound =
                        resolve_surface_ident(&seg.ident.to_string(), "impl type parameter bound")?;
                    bound_names.push(bound);
                }
                names.push(format!("{tp_name}: {}", bound_names.join(" + ")));
            }
            GenericParam::Lifetime(lt) => {
                return Err(GapReason::new(
                    Category::GenericBound,
                    format!(
                        "lifetime parameter `{}` has no grammar surface",
                        lt.lifetime
                    ),
                ));
            }
            GenericParam::Const(cp) => {
                return Err(GapReason::new(
                    Category::GenericBound,
                    format!(
                        "const generic parameter `{}` — correspondence with Mycelium's width \
                         const_params (`{{N}}`) is not confirmed",
                        cp.ident
                    ),
                ));
            }
        }
    }
    Ok(names)
}

// ---------------------------------------------------------------------------------------------
// DN-41 `width_cast` conversion-body emission (M-873 follow-on).
//
// `docs/notes/DN-41-Width-Cast-Prim.md` §2 ratifies a real surface prim
// `width_cast(value: Binary{N}, into: Binary{M}) -> Binary{M}`: widen (M>N) zero-extends
// (`Exact`); same-width is identity; narrow (M<N) is a checked, never-silent refuse
// (`EvalError::Overflow`) — §3 fixes the **width-witness ABI**: `M` is carried by the *second
// operand's* `Binary{M}` width alone (its bits are unused), exactly as `lib/std/text.myc`'s own
// `width_cast(i, bytes_len(b))` call threads a width through an in-scope `Binary{32}` value.
//
// A Rust `impl Widen<To> for From { fn widen(self) -> To { To::from(self) } }` body — the actual
// shape in `mycelium-std-cmp` — has no confirmed mapping for the qualified `To::from(self)` call
// (see `emit_expr`'s `Expr::Call` qualified-path arm); previously that always gapped the whole
// impl. When `From`/`To` both map to `Binary{N}`/`Binary{M}` (unsigned widening), this is now a
// **real, faithful** emission instead: `width_cast(self, <Binary{M} witness>)`. The witness is a
// synthesized all-zero `BinLit` of exactly `M` bits — confirmed as a legitimate `Binary{M}`-typed
// value by the grammar (`literal ::= BinLit | ...`, `BinLit ::= '0b' ('0'|'1'|'_')+`) and
// RFC-0020 §"Representation-tagged literals" ("[a BinLit's] width/dimension is determined by the
// literal's content (bit-count for BinLit)") — and DN-41 §3 explicitly says the witness's *bits*
// are ignored, so an all-zero witness is exactly as valid as any other same-width value already
// in scope. This is a synthesized witness, not one reused from the call site (the widen body has
// no other `Binary{M}` value in scope to reuse) — `Declared`, not `Exact`, because no Mycelium
// checker in this crate confirms the emitted text type-checks (see module docs).
//
// `Narrow::narrow` bodies are the DN-41 §2 fallible case (`Result<To, NarrowError>`, refusing on
// an out-of-range/non-representable value) — a single `= expr` `fn_item` body has no
// Result-returning surface in this grammar fragment, so those stay an honest, explicitly-cited
// gap rather than a forced/fabricated emission.

/// Parse a `map_type`-produced `Binary{N}` type-ref string back to its width `N`. Only matches
/// the exact `Binary{<digits>}` shape `map_type` emits for unsigned OR signed integers (`Binary`
/// is sign-free, ADR-028 — P4/P5, DN-99 §8 ENB-6) — never a guess for any other text (e.g. `Bool`,
/// a bare ident) that happens to not match. Deliberately returns `None` for a P4/P5 `"!s"`-marked
/// [`TypeEnv`] entry (the trailing marker breaks the `strip_suffix('}')` match) — see
/// [`sig_type_env`]'s doc for why that opacity is load-bearing for `Expr::Cast`, and
/// [`signed_binary_width`] for the marker-aware counterpart.
pub(crate) fn binary_width(ty_text: &str) -> Option<u32> {
    // Accept plain `Binary{N}` and the signed env marker `Binary{N}!s` (sig_type_env).
    let plain = ty_text.strip_suffix("!s").unwrap_or(ty_text);
    plain
        .strip_prefix("Binary{")
        .and_then(|rest| rest.strip_suffix('}'))
        .and_then(|digits| digits.parse::<u32>().ok())
}

/// P4/P5 (DN-99 §8 ENB-6): the marker-aware counterpart of [`binary_width`] — parses a
/// [`sig_type_env`]-produced `"Binary{N}!s"` marked entry back to its width `N`. Returns `None`
/// for an UNMARKED `Binary{N}` (unsigned) or any non-matching text; never a guess.
fn signed_binary_width(ty_text: &str) -> Option<u32> {
    ty_text.strip_suffix("!s").and_then(binary_width)
}

/// True iff `ty` is a bare (single-segment, no-generic) Rust float type `f32`/`f64`. Used by the
/// [`Expr::Cast`] fidelity gate to recognize a **cast target** that is a float **at the syn level**,
/// before (and independent of) [`map_type`] — because `map_type` maps `f64 -> Float` but *gaps*
/// `f32`, yet BOTH make the cast a float-crossing `as` whose faithful form is the reified lossy
/// swap, not a checked prim (CU-3, ADR-040 §2.4/§5). A non-path / qualified / generic / non-float
/// path type is not a float here (never a guess — VR-5).
fn type_is_float(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(tp)
    if tp.qself.is_none()
        && tp.path.segments.last().is_some_and(|s| {
            matches!(s.arguments, PathArguments::None)
                && matches!(s.ident.to_string().as_str(), "f32" | "f64")
        }))
}

/// True iff `ty` is a bare (single-segment, no-generic) Rust **signed**-integer-family type
/// (`i8`/`i16`/`i32`/`i64`/`i128`/`isize`). P4/P5 (DN-99 §8 ENB-6 / M-1029 / ADR-028): `map_type`
/// now maps every one of these to the SAME `Binary{N}` text as its unsigned counterpart (`Binary`
/// is sign-free, ADR-028) — so signedness can no longer be read back off the *mapped* type text.
/// This probe reads it off the ORIGINAL `syn::Type` instead, at the one place it is still known
/// (a fn/method parameter's declared Rust type, in [`map_signature`]; or a struct field's declared
/// type when seeding the per-file field-type map for lit-zero rewrite) — purely transpile-time
/// bookkeeping that is never itself emitted into `.myc` text (mirrors [`type_is_float`]'s shape;
/// never a guess — VR-5).
pub(crate) fn type_is_signed_int(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(tp)
    if tp.qself.is_none()
        && tp.path.segments.last().is_some_and(|s| {
            matches!(s.arguments, PathArguments::None)
                && matches!(
                    s.ident.to_string().as_str(),
                    "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
                )
        }))
}

/// Synthesize an all-zero `BinLit` witness of exactly `width` bits, grouped in nibbles
/// (`0b0000_0000_0000_0000` for width 16) matching the corpus's own `BinLit` style (e.g.
/// `lib/std/text.myc`'s `0b0000_0000_0000_0000_0000_0000_1000_0000`). The witness's bits are
/// ignored by `width_cast` (DN-41 §3) — only its bit-count (= its `Binary{width}` type, per
/// RFC-0020) is observed, so an all-zero pattern is a faithful, unconditionally-valid witness for
/// any target width.
fn zero_bin_literal(width: u32) -> String {
    let mut s = String::with_capacity(2 + width as usize + width as usize / 4);
    s.push_str("0b");
    for i in 0..width {
        if i > 0 && i % 4 == 0 {
            s.push('_');
        }
        s.push('0');
    }
    s
}

/// Whether `method` is a Rust **ownership/identity-conversion no-op** whose bare-call desugar would
/// fabricate a non-existent Mycelium prim. In value-semantic Mycelium (ADR-003) these are identity
/// or unmapped conversions with no free-function/prim referent, so `recv.method()` → `method(recv)`
/// is a check-failing fabrication (`unknown function/constructor/prim `method``) — the caller gaps
/// them, never-silently, instead of emitting (G2/VR-5). The set is deliberately conservative: only
/// the canonical `ToOwned`/`Clone`/`ToString`/`Into`/`AsRef`/`Borrow`/`Deref` accessors whose sole
/// effect is ownership/representation identity, never an operation that computes a value.
fn is_unmappable_conversion_method(method: &str) -> bool {
    // Methods with a `prim_map` identity row (`clone`/`to_owned`/`to_string`(Bytes)/M-1037
    // accessors) are handled there first when their gate matches; this catch-all covers gate
    // misses (user types, unresolved receivers, non-Bytes `to_string`) and deliberately withheld
    // conversions (`into`/`to_vec`/mutable accessors).
    matches!(
        method,
        "to_owned"
            | "to_string"
            | "to_vec"
            | "clone"
            | "into"
            | "as_str"
            | "as_ref"
            | "as_slice"
            | "as_mut"
            | "borrow"
            | "borrow_mut"
            | "deref"
            | "deref_mut"
    )
}

/// Method-specific EXPLAIN for residual conversion gaps (M-1037 residual). Prefer a precise
/// never-silent reason over the shared generic ownership-no-op text when the method has its own
/// verify-first finding (into target undetermined; to_vec not identity; to_string non-Bytes needs
/// Show/render).
fn conversion_gap_reason(method: &str) -> String {
    match method {
        "into" => {
            "Rust `.into()` (Into::into) target type is determined by call-site expected-type \
             inference; this per-expression emitter has no expected-type context, so identity \
             when source/target coincide is undecidable without guessing the target — gapped \
             rather than fabricating bare `into(recv)` (M-1037 residual, G2/VR-5; ADR-003)"
                .to_string()
        }
        "to_vec" => {
            "Rust `.to_vec()` allocates a new owned Seq (not a value-semantic identity); no \
             verified bare-call Seq-copy prim is wired in this pipeline — gapped rather than \
             fabricating bare `to_vec(recv)` (M-1037 residual, G2/VR-5)"
                .to_string()
        }
        "to_string" => {
            "Rust `.to_string()` on a non-`Bytes` receiver needs Show/render (DN-127), but \
             single-file myc-check does not guarantee a Show impl is in scope — and `render(recv)` \
             checks as `unknown function/constructor/prim render` without one. Bytes receivers \
             use the prim_map identity row; every other receiver is gapped rather than fabricating \
             bare `to_string(recv)` (M-1037 residual, G2/VR-5)"
                .to_string()
        }
        other => format!(
            "Rust ownership/identity-conversion no-op method `.{other}()` has no \
             Mycelium free-function/prim referent (value semantics — ADR-003); \
             desugaring it to a bare `{other}(recv)` would fabricate an unknown \
             prim (`unknown function/constructor/prim `{other}`` — verified \
             against the oracle), so it is gapped, never fake-emitted (G2/VR-5)"
        ),
    }
}

/// If `trait_name`/`method` identify a `Widen::widen` method whose `Self`/target both map to
/// `Binary{N}`/`Binary{M}` (unsigned widening) with `M > N`, return the faithful `width_cast`
/// body. `None` for every other shape (bool/float/signed self types, non-`Widen` impls, or a
/// `Widen` impl whose recorded target arg isn't a plain `Binary{M}` text) — the caller falls back
/// to the general per-expression emitter, which gaps `To::from(self)` honestly (no fabrication,
/// VR-5).
fn try_width_cast_widen_body(
    trait_name: Option<&str>,
    method: &str,
    self_ty_text: &str,
    trait_targs: &[String],
) -> Option<String> {
    if trait_name != Some("Widen") || method != "widen" {
        return None;
    }
    let n = binary_width(self_ty_text)?;
    let m = binary_width(trait_targs.first()?)?;
    if m <= n {
        // Not an actual widen (or an unresolvable width relationship) — leave it to the general
        // path rather than emit a `width_cast` that DN-41 would treat as identity/narrow for a
        // trait that promises "Total — never fails" widening. Never guessed (VR-5).
        return None;
    }
    Some(format!("width_cast(self, {})", zero_bin_literal(m)))
}

/// Reject `async`/`unsafe`/`extern "ABI"` fn modifiers — `fn_item`/`fn_sig` in the grammar carry
/// no such modifier slot.
fn check_fn_modifiers(sig: &Signature) -> Result<(), GapReason> {
    if sig.asyncness.is_some() || sig.unsafety.is_some() || sig.abi.is_some() {
        return Err(GapReason::new(
            Category::Other,
            "`async`/`unsafe`/`extern \"ABI\"` fn modifier has no grammar surface",
        ));
    }
    Ok(())
}

struct MappedSig {
    params: Vec<(String, String)>,
    ret: String,
    type_params: Vec<String>,
    /// P4/P5 (DN-99 §8 ENB-6): the subset of `params`' names whose ORIGINAL Rust type was a
    /// signed-integer-family type ([`type_is_signed_int`]) — the signedness bookkeeping that
    /// `map_type`'s sign-free `Binary{N}` output can no longer carry (ADR-028). Rendering
    /// (`render_fn`/`render_fn_sig`) never reads this field — only [`sig_type_env`] does, to build
    /// the internal (never-emitted) [`TypeEnv`] marker `Expr::Binary`/`Expr::Unary`'s signed-gate
    /// reads. Never includes `"self"` (a receiver's type is a struct/`Self`, never numeric).
    signed_param_names: HashSet<String>,
    /// DN-125 (M-1081) value-threading: non-empty exactly when this signature had a `&mut self`
    /// receiver and/or one or more top-level `&mut T` parameters — Alt A, Rank 1 (the by-value
    /// receiver/param + rebind lowering). `ret` already reflects the threaded tuple/type (see
    /// [`map_signature`]'s receiver/param arms); body emission must go through
    /// [`emit_mutating_block_as_expr`] instead of [`emit_block_as_expr`] whenever this is
    /// non-empty. Ordered: the receiver first (if `&mut self`), then each `&mut T` parameter in
    /// declaration order.
    threaded: Vec<ThreadedBinding>,
    /// DN-125 §5.1: `Some(mapped type text)` when, IN ADDITION to the threaded binding(s) above,
    /// the ORIGINAL (pre-lowering) Rust return type carries a genuine extra value the body must
    /// still produce (e.g. `fn incr(&mut self, by: u64) -> u64`) — `None` when the original
    /// return was `()` or the `&mut Self` builder-chain shape ([`is_mut_self_return`]), in which
    /// case the threaded binding(s) alone constitute the whole return value.
    threaded_extra_ret: Option<String>,
}

/// DN-125 (M-1081) — one value-threaded `&mut self`/`&mut T` binding: the Mycelium name it keeps
/// (unchanged from the Rust source — `"self"` for a receiver, else the parameter's own name), its
/// mapped (erased-to-value) Mycelium type text, and — when resolvable — the in-file struct layout
/// that enables FIELD-level reassignment (`self.<field> = ..`) reconstruction in
/// [`emit_mutating_block_as_expr`]. `layout` is only ever consulted for `name == "self"` (field
/// projection, `visit_field`/[`field_projection_text`], is wired for the `self` base only); a
/// non-`self` threaded binding supports only WHOLE-VALUE reassignment (`*name = ..`), so its
/// `layout` is carried for completeness but never read.
#[derive(Clone)]
struct ThreadedBinding {
    name: String,
    ty: String,
    layout: Option<StructLayout>,
}

/// Build the body's initial [`TypeEnv`] from a mapped signature's `params` — the two body-emission
/// entry points ([`emit_fn`]/[`emit_impl`]) call this once, before descending into the body, so
/// `Expr::Binary`'s operand-type gate can see every fn/method parameter's already-mapped type text
/// with **no re-mapping** (`MappedSig::params` already carries `(name, mapped_type_text)` —
/// `map_signature`'s doc). For a method, `self` is already present in `params` (the `FnArg::Receiver`
/// arm of `map_signature` pushes `("self".to_string(), ty)`), so this one function covers both the
/// free-fn and impl-method cases without a separate `self`-insertion step.
///
/// **P4/P5 signed marker (DN-99 §8 ENB-6):** a name in `sig.signed_param_names` gets a `"!s"`
/// suffix appended to its stored value (e.g. `"Binary{32}!s"`) — an internal-only marker, never
/// emitted as `.myc` text (the actual signature text is rendered straight from `sig.params` by
/// `render_fn`/`render_fn_sig`, which never consult this env). [`signed_binary_width`] is the sole
/// reader that understands the marker; every *other* consumer of a `TypeEnv` entry
/// (`binary_width`, `receiver_gate_matches`, `Expr::Cast`'s `operand_width`) parses the UNMARKED
/// `Binary{N}` shape only, so a marked entry safely fails to match them (`None`, not a wrong
/// answer — VR-5) rather than being silently treated as an ordinary unsigned `Binary{N}`. That is
/// deliberate for `Expr::Cast` in particular: `width_cast`'s widen is an unconditional
/// zero-extend (DN-41 §3), which is faithful for an unsigned source but WRONG for a signed one
/// (Rust sign-extends); opacity-by-construction is what keeps a signed-source widen an honest gap
/// instead of a silently-wrong zero-extend.
fn sig_type_env(sig: &MappedSig) -> TypeEnv {
    sig.params
        .iter()
        .map(|(name, ty)| {
            if sig.signed_param_names.contains(name) {
                (name.clone(), format!("{ty}!s"))
            } else {
                (name.clone(), ty.clone())
            }
        })
        .collect()
}

/// Map a fn signature's generics/params/return type. `self_ty` is `Some(name)` inside an
/// impl/trait body (the concrete or best-effort `Self` substitution); `None` for a top-level fn,
/// where a `self` parameter or bare `Self` type is therefore always a gap.
fn map_signature(
    generics: &Generics,
    inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>,
    output: &ReturnType,
    self_ty: Option<&str>,
) -> Result<MappedSig, GapReason> {
    let type_params = plain_type_params(generics)?;
    let mut params = Vec::with_capacity(inputs.len());
    let mut signed_param_names = HashSet::new();
    let mut threaded: Vec<ThreadedBinding> = Vec::new();
    for arg in inputs {
        match arg {
            FnArg::Receiver(r) => {
                let ty = self_ty.ok_or_else(|| {
                    GapReason::new(
                        Category::Other,
                        "`self` parameter with no enclosing impl/trait context",
                    )
                })?;
                if r.reference.is_some() && r.mutability.is_some() {
                    // DN-125 (M-1081), Alt A Rank 1: value-thread `&mut self` instead of the
                    // pre-DN-125 hard gap — take the receiver BY VALUE (identical to the existing
                    // `&self` erasure just below) and record it as threaded so the return type
                    // (below) widens to carry the mutated value back out; the call-site rebind is
                    // the driver's/caller's job (`emit_mutating_block_as_expr`'s body-level half,
                    // and the corpus-level `x.f(a)` -> `x = f(x, a)` desugar this DN scopes to
                    // in-body statement position — see that fn's module doc). `layout` is
                    // `None` when `ty` isn't an emitted in-file single-ctor struct — value-
                    // threading the WHOLE receiver still works then (a body ending in a full
                    // reconstruction/replacement), only FIELD-level reassignment needs the layout.
                    threaded.push(ThreadedBinding {
                        name: "self".to_string(),
                        ty: ty.to_string(),
                        layout: struct_layout(ty),
                    });
                }
                params.push(("self".to_string(), ty.to_string()));
            }
            FnArg::Typed(pt) => {
                let name = match &*pt.pat {
                    Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => {
                        pi.ident.to_string()
                    }
                    _ => {
                        return Err(GapReason::new(
                            Category::Other,
                            "non-identifier parameter pattern (destructuring param) has no \
                             `param ::= Ident ':' type_ref` equivalent",
                        ))
                    }
                };
                // A parameter name is emitted verbatim into `param ::= Ident ':' type_ref`, and
                // an UNUSED param's body references never pass through Expr::Path — so the
                // reserved-word guard must fire here, not only at use sites (PR #1207 review).
                let name = resolve_surface_ident(&name, "fn parameter")?;
                // DN-125 (M-1081) S2: a top-level `&mut T` PARAMETER value-threads exactly like
                // the receiver above — erase to the referent's value type and record it as
                // threaded, rather than the blanket `&mut T` gap `map_type`'s `visit_reference`
                // still applies to every OTHER (nested) `&mut T` position (a return type, a
                // generic argument, a struct field) — deliberately UNCHANGED there. That
                // untouched gap is exactly what closes the DN-125 §6.2 interior-&mut-return
                // narrowing "for free": a `&mut self` method returning `&mut Field` still fails
                // to map its return type below (an interior mutable borrow is never faithfully a
                // value), so it still gaps as a whole — never silently value-threaded.
                if let syn::Type::Reference(r) = &*pt.ty {
                    if r.mutability.is_some() {
                        let ty = map_type(&r.elem, self_ty)?;
                        if type_is_signed_int(&r.elem) {
                            signed_param_names.insert(name.clone());
                        }
                        threaded.push(ThreadedBinding {
                            name: name.clone(),
                            ty: ty.clone(),
                            layout: struct_layout(&ty),
                        });
                        params.push((name, ty));
                        continue;
                    }
                }
                let ty = map_type(&pt.ty, self_ty)?;
                // P4/P5 (DN-99 §8 ENB-6): record signedness off the ORIGINAL `syn::Type` — the
                // one place it is still legible before `map_type` erases it onto the shared,
                // sign-free `Binary{N}` text (ADR-028).
                if type_is_signed_int(&pt.ty) {
                    signed_param_names.insert(name.clone());
                }
                params.push((name, ty));
            }
        }
    }
    let (ret, threaded_extra_ret) = if threaded.is_empty() {
        // Unchanged pre-DN-125 path.
        let ret = match output {
            ReturnType::Default => {
                return Err(GapReason::new(
                    Category::Other,
                    "function has no return type (implicit `()`) — no unit value is \
                     representable in this grammar fragment",
                ))
            }
            ReturnType::Type(_, ty) => map_type(ty, self_ty)?,
        };
        (ret, None)
    } else {
        // DN-125 §5.1 return-type composition: the threaded binding(s) alone, OR — when the
        // source genuinely returns an extra value — a tuple of the threaded binding(s) plus that
        // value.
        match output {
            ReturnType::Default => (thread_ret_text(&threaded), None),
            ReturnType::Type(_, ty) => {
                if is_mut_self_return(ty, self_ty) {
                    // DN-125 §1/§4 "builder methods": `-> &mut Self` returns the receiver ITSELF
                    // for chaining, not an interior reference into self — value-semantically
                    // identical to the `()` case (the mutated receiver alone), never gapped as
                    // an interior-`&mut`-return residual (§6.2 is about returning a reference
                    // INTO self, e.g. `get_mut`, not the receiver's own value).
                    (thread_ret_text(&threaded), None)
                } else {
                    // A genuine extra return value. `map_type` still applies its EXISTING,
                    // UNCHANGED `&mut T` gap here for any other reference-shaped return — the
                    // §6.2 interior-&mut-return narrowing, falling out of code this DN does not
                    // touch.
                    let extra = map_type(ty, self_ty)?;
                    (thread_ret_text_with_extra(&threaded, &extra), Some(extra))
                }
            }
        }
    };
    Ok(MappedSig {
        params,
        ret,
        type_params,
        signed_param_names,
        threaded,
        threaded_extra_ret,
    })
}

/// DN-125 §5.1: the threaded-binding-only return-type text — a single type when there is exactly
/// one threaded binding, else a tuple of all of them in order.
fn thread_ret_text(threaded: &[ThreadedBinding]) -> String {
    if threaded.len() == 1 {
        threaded[0].ty.clone()
    } else {
        format!(
            "({})",
            threaded
                .iter()
                .map(|t| t.ty.clone())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// [`thread_ret_text`] plus one extra (non-threaded) return value appended to the tuple.
fn thread_ret_text_with_extra(threaded: &[ThreadedBinding], extra: &str) -> String {
    let mut parts: Vec<String> = threaded.iter().map(|t| t.ty.clone()).collect();
    parts.push(extra.to_string());
    format!("({})", parts.join(", "))
}

/// DN-125 §1/§4: whether `ty` is `&mut Self` or `&mut <self_ty>` — the receiver's OWN type
/// returned by (mutable) reference, the "builder method" chaining shape (`fn set_x(&mut self, ..)
/// -> &mut Self { .. ; self }`). This is NOT an interior-`&mut`-return (§6.2's `get_mut`/
/// `iter_mut`/`IndexMut` residual, which returns a reference into a *different*, unrelated part of
/// self) — it is exactly the receiver's own mutated value, so it value-threads like the `()` case.
/// `false` for every other shape (including `&mut` to any OTHER named type) — never a guess: only
/// a syntactic match against the enclosing `self_ty` name (or the literal `Self` keyword).
fn is_mut_self_return(ty: &syn::Type, self_ty: Option<&str>) -> bool {
    let Some(sty) = self_ty else {
        return false;
    };
    let syn::Type::Reference(r) = ty else {
        return false;
    };
    if r.mutability.is_none() {
        return false;
    }
    let syn::Type::Path(tp) = &*r.elem else {
        return false;
    };
    let Some(seg) = tp.path.segments.last() else {
        return false;
    };
    matches!(seg.arguments, PathArguments::None) && (seg.ident == "Self" || seg.ident == sty)
}

fn render_fn(name: &str, sig: &MappedSig, body: &str, doc: &[String], pub_prefix: &str) -> String {
    let params_str = sig
        .params
        .iter()
        .map(|(n, t)| format!("{n}: {t}"))
        .collect::<Vec<_>>()
        .join(", ");
    let type_params_text = if sig.type_params.is_empty() {
        String::new()
    } else {
        format!("[{}]", sig.type_params.join(", "))
    };
    let mut out = String::new();
    for d in doc {
        out.push_str(d);
        out.push('\n');
    }
    out.push_str(&format!(
        "{pub_prefix}fn {name}{type_params_text}({params_str}) => {} = {body};",
        sig.ret
    ));
    out
}

fn render_fn_sig(name: &str, sig: &MappedSig) -> String {
    let params_str = sig
        .params
        .iter()
        .map(|(n, t)| format!("{n}: {t}"))
        .collect::<Vec<_>>()
        .join(", ");
    let type_params_text = if sig.type_params.is_empty() {
        String::new()
    } else {
        format!("[{}]", sig.type_params.join(", "))
    };
    format!("fn {name}{type_params_text}({params_str}) => {}", sig.ret)
}

// ---------------------------------------------------------------------------------------------
// Function bodies: a `let`-chain + tail expression maps to Mycelium's nested `let ... in ...`;
// anything else (early return, loops, multiple non-`let` statements, no tail expr) is a
// MultiStmtBody gap — a KNOWN HARD GAP named in the kickoff brief.
// ---------------------------------------------------------------------------------------------

/// Emit one plain `let`-binding statement's `(name, value)` pair, extending `local_env` in place
/// with the RHS's decidable type — the two shapes [`expr_env_type`]/[`known_struct_literal_ty`]
/// cover (a bare-identifier alias, or an in-file struct literal), exactly mirroring
/// [`emit_block_as_expr_inner`]'s pre-DN-125 inline logic (this is a pure extraction, no behavior
/// change). Shared by [`emit_block_as_expr_inner`] and (DN-125/M-1081)
/// [`emit_mutating_block_as_expr_inner`] so the plain `let`-binding rules never drift between the
/// two body-emission paths (DRY, house rule #5).
fn emit_local_binding(
    local: &syn::Local,
    self_ty: Option<&str>,
    local_env: &mut TypeEnv,
) -> Result<(String, String), GapReason> {
    let name = match &local.pat {
        Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => pi.ident.to_string(),
        _ => {
            return Err(GapReason::new(
                Category::MultiStmtBody,
                "`let` binding uses an unsupported pattern (only simple `let x = e;` is \
                 supported)",
            ))
        }
    };
    let init = local.init.as_ref().ok_or_else(|| {
        GapReason::new(Category::MultiStmtBody, "`let` binding has no initializer")
    })?;
    if init.diverge.is_some() {
        return Err(GapReason::new(
            Category::MultiStmtBody,
            "`let ... else` has no Mycelium equivalent",
        ));
    }
    let value = emit_expr(&init.expr, self_ty, local_env)?;
    // See `emit_block_as_expr_inner`'s original doc (preserved verbatim in intent): only the two
    // decidable RHS shapes extend `local_env`; a shadowed stale entry is removed, never kept
    // (VR-5).
    match expr_env_type(&init.expr, local_env)
        .or_else(|| known_struct_literal_ty(&init.expr, self_ty))
    {
        Some(ty) => {
            local_env.insert(name.clone(), ty);
        }
        None => {
            local_env.remove(&name);
        }
    }
    Ok((name, value))
}

pub fn emit_block_as_expr(
    block: &Block,
    self_ty: Option<&str>,
    env: &TypeEnv,
) -> Result<String, GapReason> {
    guarded(|| emit_block_as_expr_inner(block, self_ty, env))
}

/// The recursion-guarded body of [`emit_block_as_expr`] (RFC-0041 §4.7 W1 — see
/// `crate::gap::guarded`). Every recursive call back into a guarded entry point uses the *public*
/// wrapper name (`emit_expr`, `emit_block_as_expr` is not itself re-entered here), so each
/// recursion step consumes one budget frame.
fn emit_block_as_expr_inner(
    block: &Block,
    self_ty: Option<&str>,
    env: &TypeEnv,
) -> Result<String, GapReason> {
    let stmts = &block.stmts;
    if stmts.is_empty() {
        return Err(GapReason::new(
            Category::MultiStmtBody,
            "empty function body (no expression)",
        ));
    }
    let (lets, tail) = stmts.split_at(stmts.len() - 1);
    let tail_expr = match &tail[0] {
        Stmt::Expr(e, None) => e,
        _ => {
            return Err(GapReason::new(
                Category::MultiStmtBody,
                "function body's last statement is not a trailing expression (implicit unit \
                 return, or a semicolon-terminated final statement)",
            ))
        }
    };
    let mut bindings = Vec::with_capacity(lets.len());
    // The type environment as extended by the `let`-chain processed so far (trx2 Lane C
    // Deliverable 1) — starts as a clone of the caller's `env` (the fn/method's own
    // params + `self`) and gains one entry per local **only** when that local's type is
    // trivially known (see the two cases below); every other local is simply absent from
    // `local_env`, never guessed (VR-5), so `Expr::Binary`'s operand-type gate treats it
    // exactly like any other not-known expression.
    let mut local_env = env.clone();
    for s in lets {
        match s {
            Stmt::Local(local) => {
                bindings.push(emit_local_binding(local, self_ty, &mut local_env)?);
            }
            // A non-`let`, non-tail statement — name the actual kind so the gap reason is precise
            // (never-silent, G2). syn's `Stmt` is a plain 4-variant enum (`Local` handled above).
            Stmt::Item(_) => {
                return Err(GapReason::new(
                    Category::MultiStmtBody,
                    "function body contains a nested item declaration (e.g. a local \
                     `static`/`const`/`fn`) — this grammar fragment has no local-item production; \
                     only simple `let x = e;` bindings plus a trailing expression map",
                ))
            }
            Stmt::Macro(_) => {
                return Err(GapReason::new(
                    Category::MultiStmtBody,
                    "function body contains a macro-invocation statement (e.g. \
                     `debug_assert!`/`println!`) — no macro system in this grammar fragment",
                ))
            }
            Stmt::Expr(_, _) => {
                return Err(GapReason::new(
                    Category::MultiStmtBody,
                    "function body has a semicolon-terminated (value-discarding) statement \
                     expression before the tail — a `let`-chain body maps only simple `let x = e;` \
                     bindings plus a single trailing expression",
                ))
            }
        }
    }
    let mut result = emit_expr(tail_expr, self_ty, &local_env)?;
    for (name, value) in bindings.into_iter().rev() {
        result = format!("let {name} = {value} in {result}");
    }
    Ok(result)
}

// ---------------------------------------------------------------------------------------------
// DN-125 (M-1081) — value-threaded `&mut self`/`&mut T` method/fn bodies.
//
// A mutating body's threaded binding(s) (`self` and/or a `&mut T` param, see `map_signature`) are
// rebound via NESTED `let <name> = <new-value> in <rest>` shadowing — Mycelium's own lexical
// scoping then implements DN-125 §5.2's sequential rebind (`x = h(x); …; x = k(x)`) for free: each
// later statement's occurrences of `<name>` resolve to the nearest enclosing `let`, i.e. the most
// recently threaded value, exactly mirroring Rust's own sequential in-place mutation.
//
// Deliberately NARROW (DN-125 §6.1 — never guess on an unprovable/aliased shape): only a FLAT
// sequence of `self.<field> (=|+=|-=|..) <rhs>;` / `*<param> (=|+=|-=|..) <rhs>;` re-assignment
// statements is recognized, optionally followed by one trailing value expression when the
// method's original return type carried an extra value. This transpiler has no DN-33 static
// uniqueness analysis to consult (that analysis is itself `Declared`/unbuilt, DN-125 §5.3) — so
// the "conservative confident-uniqueness check" §6.1 calls for is implemented by EXCLUSION: any
// body shape outside this flat sequence (control flow, an early return, a call chaining another
// mutation) is refused outright rather than risked.
//
// **Correction (re-review of PR #1527, closing an aliasing hole this doc previously claimed shut
// by construction):** the flat-sequence grammar DOES admit a plain `let <name> = <rhs>;`
// statement, and this emitter's `try_threaded_assign`/`threaded_deref_lhs` matching is purely
// name-based (it has no scope-tracking), so a `let` whose RHS is a bare reference to a DIFFERENT
// threaded `&mut` binding genuinely DOES introduce a second live alias to the shadowed name — a
// prior version of this doc's claim that this could not happen was wrong (see
// `crates/mycelium-transpile/src/tests/mut_thread.rs`'s
// `let_binding_aliasing_another_threaded_param_refuses_rather_than_mis_thread` for the repro).
// The REAL guarantee this module upholds is narrower: every body it DOES accept is provably safe
// to rebind because `aliased_threaded_binding` explicitly detects and REFUSES exactly this one
// aliasing shape (a bare-path `let <threaded-name> = <other-threaded-name>;`) before it can reach
// the fold — the narrowness of what we accept, now correctly including this exclusion, is what
// keeps every accepted body provably safe to rebind (never-silent G2/VR-5). An ordinary
// independent-value shadow (`let y = <literal>;`, `let y = *other;`, `let y = some_call();`, …)
// remains fully supported via the pre-existing synthetic-carrier routing.
// ---------------------------------------------------------------------------------------------

fn emit_mutating_block_as_expr(
    block: &Block,
    self_ty: Option<&str>,
    env: &TypeEnv,
    threaded: &[ThreadedBinding],
    want_extra: bool,
) -> Result<String, GapReason> {
    guarded(|| emit_mutating_block_as_expr_inner(block, self_ty, env, threaded, want_extra))
}

/// The recursion-guarded body of [`emit_mutating_block_as_expr`] — see that fn's + this module's
/// doc. `want_extra` mirrors `MappedSig::threaded_extra_ret.is_some()`: whether the body must ALSO
/// produce a genuine trailing value beyond the threaded binding(s).
fn emit_mutating_block_as_expr_inner(
    block: &Block,
    self_ty: Option<&str>,
    env: &TypeEnv,
    threaded: &[ThreadedBinding],
    want_extra: bool,
) -> Result<String, GapReason> {
    let stmts = &block.stmts;
    if stmts.is_empty() {
        return Err(GapReason::new(
            Category::MultiStmtBody,
            "empty function body (no expression)",
        ));
    }
    // CRITICAL fix (strict review of PR #1527, DN-125/M-1081): a plain `let` binding whose
    // pattern name SHADOWS a threaded `&mut` binding's own name (only reachable for a `&mut T`
    // PARAMETER — Rust forbids `let self`) is, in the common case, a genuinely new, ordinarily-
    // scoped local (Rust lexical shadowing) with NO effect on the referent. (**Correction, later
    // re-review:** this is only true when the RHS is an independent value — a bare-path RHS
    // naming a DIFFERENT threaded binding is instead a genuine aliasing rebind, refused outright
    // by `aliased_threaded_binding` in the `Stmt::Local` arm below before it ever reaches this
    // shadow-routing fix; the shadow_risk/synthetic-carrier machinery here only ever runs on the
    // already-excluded-from-aliasing, safe-to-shadow case.) Naively folding both the threaded
    // reassignment(s) AND this unrelated same-named local into ONE nested `let <name> = .. in ..`
    // chain (the pre-fix behavior) let the shadow silently intercept the fold's tail reference,
    // returning the shadow's value instead of the actually-threaded one — a silent-corruption bug
    // that still `myc check`-cleaned. Fix: for exactly the threaded names a `let` in THIS body
    // shadows, route the tail reference through a synthetic internal alias
    // (`synth_thread_name`, `__myc_thread_<name>`) that a source-level `let <name> = ..` can never
    // intercept, seeded before the first statement and re-captured immediately after every
    // threaded reassignment (`fold_threaded_tail`'s doc has the full nesting argument). A body
    // with no such shadow is completely unaffected — `shadow_risk` is empty and every emission is
    // byte-identical to pre-fix (no unnecessary verbosity).
    let shadow_risk = shadowed_threaded_names(block, threaded);
    if !shadow_risk.is_empty() {
        // Never-silent collision guard (VR-5): if the source already spells the exact synthetic
        // carrier name this fix needs, routing through it would defeat the whole point — refused
        // outright (Category::Other) rather than risked. Astronomically unlikely for real Rust
        // source (the `__myc_thread_` prefix is an internal convention, not a reserved word), but
        // checked rather than assumed.
        for name in threaded
            .iter()
            .map(|t| &t.name)
            .filter(|n| shadow_risk.contains(*n))
        {
            let synth = synth_thread_name(name);
            let collides = env.contains_key(&synth)
                || threaded.iter().any(|t| t.name == synth)
                || block.stmts.iter().any(|s| {
                    matches!(
                        s,
                        Stmt::Local(l)
                            if local_binding_simple_name(l).as_deref() == Some(synth.as_str())
                    )
                });
            if collides {
                return Err(GapReason::new(
                    Category::Other,
                    format!(
                        "source already uses the internal synthetic carrier name `{synth}` — \
                         `{name}`'s DN-125 shadow-safe value-threading needs this name \
                         internally and refuses to risk a collision with a source binding of \
                         the same spelling (VR-5)",
                    ),
                ));
            }
        }
    }
    // Seed a synthetic capture for every shadow-risked threaded binding BEFORE any statement is
    // processed, so the tail always has a well-defined synthetic value even when the body never
    // explicitly reassigns that binding at all (it then simply carries the original parameter
    // through, unaffected by any later same-named `let` shadow).
    let mut bindings: Vec<(String, String)> = threaded
        .iter()
        .filter(|t| shadow_risk.contains(&t.name))
        .map(|t| (synth_thread_name(&t.name), t.name.clone()))
        .collect();
    let mut local_env = env.clone();
    let mut touched: HashSet<String> = HashSet::new();

    for (idx, s) in stmts.iter().enumerate() {
        let is_final = idx + 1 == stmts.len();
        match s {
            Stmt::Local(local) => {
                // Aliasing-rebind hole (re-review of PR #1527, DN-125/M-1081 follow-up): a `let`
                // that shadows a threaded name is not always the harmless "genuinely new local"
                // the CRITICAL fix above assumes — if its RHS is itself another threaded `&mut`
                // binding, the shadow makes the bare name alias a DIFFERENT live reference, and
                // this emitter's purely-name-based `try_threaded_assign`/`threaded_deref_lhs`
                // matching has no way to notice the rebind, so it would keep attributing
                // subsequent `*<name> = ..` reassignments to the ORIGINAL threaded binding —
                // silently mutating the wrong one (see `aliased_threaded_binding`'s doc for the
                // full repro). Refused outright rather than risked (never-silent G2/VR-5).
                if let Some(shadowed) = local_binding_simple_name(local) {
                    if threaded.iter().any(|t| t.name == shadowed) {
                        if let Some(alias) = aliased_threaded_binding(local, &shadowed, threaded) {
                            return Err(GapReason::new(
                                Category::Other,
                                format!(
                                    "a `let {shadowed} = ..` binding rebinds threaded `&mut` \
                                     name `{shadowed}` to alias another threaded binding \
                                     (`{alias}`) — refused rather than risk mis-threading a \
                                     subsequent `*{shadowed} = ..`/`{shadowed}.<field> = ..` \
                                     reassignment onto the wrong referent (DN-125 \
                                     aliasing-rebind hole, never-silent G2/VR-5)"
                                ),
                            ));
                        }
                    }
                }
                bindings.push(emit_local_binding(local, self_ty, &mut local_env)?);
            }
            Stmt::Expr(e, semi) => {
                if let Some((name, value)) = try_threaded_assign(e, self_ty, &local_env, threaded)?
                {
                    touched.insert(name.clone());
                    bindings.push((name.clone(), value));
                    if shadow_risk.contains(&name) {
                        // Re-capture the just-updated value under the synthetic carrier,
                        // immediately after the reassignment it belongs to (see this fn's
                        // CRITICAL-fix doc + `fold_threaded_tail`'s nesting argument) — a later
                        // same-named capture shadows an earlier one exactly like the real
                        // `<name>` chain does, so the LAST reassignment always wins, unaffected
                        // by any later unrelated `let <name> = ..` shadow.
                        bindings.push((synth_thread_name(&name), name));
                    }
                    continue;
                }
                if is_final && semi.is_none() {
                    // Bare-name shortcut: the tail is literally one of the threaded bindings' own
                    // name — Rust's explicit "return the (already mutated) receiver/arg" tail
                    // (DN-125 §1/§4's builder-method shape spelled with an explicit `self` at the
                    // end rather than via `-> &mut Self`). No extra value; nothing left to do.
                    if let Expr::Path(p) = e {
                        if p.qself.is_none() && p.path.segments.len() == 1 {
                            let nm = p.path.segments[0].ident.to_string();
                            if threaded.iter().any(|t| t.name == nm) {
                                return Ok(fold_threaded_tail(
                                    bindings,
                                    threaded,
                                    None,
                                    &shadow_risk,
                                ));
                            }
                        }
                    }
                    if want_extra {
                        let tail_text = emit_expr(e, self_ty, &local_env)?;
                        return Ok(fold_threaded_tail(
                            bindings,
                            threaded,
                            Some(tail_text),
                            &shadow_risk,
                        ));
                    }
                    // No assignment statement touched the (sole) threaded binding at all — the
                    // tail expression may be its whole replacement value (e.g. a full `Self { .. }`
                    // literal reconstruction written directly, DN-125 §5.1's `{ self with n = .. }`
                    // illustration, spelled the way this grammar fragment's existing struct-literal
                    // desugar, M-1006 Lever 1, already supports). Deliberately NARROW (never guess,
                    // VR-5): this is only accepted when the tail is SYNTACTICALLY a struct literal
                    // whose resolved type is EXACTLY the threaded binding's own type — an arbitrary
                    // well-typed-but-unrelated tail expression (e.g. a call to some other `()`-typed
                    // fn) is refused rather than silently mistaken for "the new self", which could
                    // otherwise (rarely, if the unrelated expression happened to type-check as the
                    // same shape) emit a semantically WRONG rebind instead of merely a check-failing
                    // one — the case DN-125 §6.1 exists to rule out.
                    if threaded.len() == 1
                        && !touched.contains(&threaded[0].name)
                        && known_struct_literal_ty(e, self_ty).as_deref()
                            == Some(threaded[0].ty.as_str())
                    {
                        let tail_text = emit_expr(e, self_ty, &local_env)?;
                        let name = &threaded[0].name;
                        if shadow_risk.contains(name) {
                            bindings.push((synth_thread_name(name), tail_text));
                        } else {
                            bindings.push((name.clone(), tail_text));
                        }
                        return Ok(fold_threaded_tail(bindings, threaded, None, &shadow_risk));
                    }
                    return Err(GapReason::new(
                        Category::Other,
                        "mutating method's tail expression is neither a threaded-binding \
                         field/whole-value re-assignment nor (with the method's original return \
                         type already `()`/self-chain, and either multiple threaded bindings or \
                         one already assigned) an extra value — value-threading only supports a \
                         flat sequence of `self.<field> = ..`/`*<param> = ..` re-assignments plus, \
                         when the source return type is non-unit, one trailing value expression \
                         (DN-125 §5, conservative scope per §6.1)",
                    ));
                }
                return Err(GapReason::new(
                    Category::Other,
                    "mutating method body statement is neither a `let`, a supported \
                     `self.<field>`/`*<param>` re-assignment, nor (in tail position) the \
                     method's own return value — value-threading deliberately refuses any shape \
                     outside a flat re-assignment sequence rather than risk an unsound rebind \
                     (DN-125 §6.1, never-silent G2/VR-5)",
                ));
            }
            Stmt::Item(_) => {
                return Err(GapReason::new(
                    Category::MultiStmtBody,
                    "function body contains a nested item declaration — unsupported in a \
                     value-threaded mutating body exactly as in the ordinary body form",
                ))
            }
            Stmt::Macro(_) => {
                return Err(GapReason::new(
                    Category::MultiStmtBody,
                    "function body contains a macro-invocation statement — unsupported in a \
                     value-threaded mutating body exactly as in the ordinary body form",
                ))
            }
        }
    }
    // Every statement was consumed as a `let`/threaded re-assignment — no genuine tail expression
    // (the source fn's body ended in a semicolon-terminated re-assignment, or is a `()`-typed fn
    // whose last statement never needed one).
    if want_extra {
        return Err(GapReason::new(
            Category::Other,
            "mutating method's original return type expects an extra value but the body has no \
             trailing value expression",
        ));
    }
    Ok(fold_threaded_tail(bindings, threaded, None, &shadow_risk))
}

/// Best-effort extraction of a `let` binding's simple `Pat::Ident` name — `None` for any other
/// pattern shape (destructuring, etc.), which `emit_local_binding` itself refuses when the body
/// is actually processed. Used only by the CRITICAL-fix shadow-detection pre-scan below (a
/// non-`Pat::Ident` pattern can't textually collide with a threaded binding's bare name anyway).
fn local_binding_simple_name(local: &syn::Local) -> Option<String> {
    match &local.pat {
        Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => Some(pi.ident.to_string()),
        _ => None,
    }
}

/// Aliasing-rebind hole fix (re-review of PR #1527, DN-125/M-1081 follow-up — see the
/// `Stmt::Local` arm in `emit_mutating_block_as_expr_inner` for the corruption this closes). If
/// `local` is a plain `let <shadowed_name> = <rhs>;` whose RHS is a bare, single-segment path
/// naming a DIFFERENT threaded binding, returns that binding's name. Only a bare-`Path` RHS is
/// checked: every threaded binding's Rust-source type is a `&mut T` reference, which is **not**
/// `Copy` — so a bare `let <name> = <other-threaded-name>;` can only be a MOVE of that same
/// reference (an alias), never a deref-copy of its pointee. Any other RHS shape (a literal, a
/// deref `*other`, a method call, a struct literal, …) produces a genuinely independent value —
/// exactly the shape the pre-existing synthetic-carrier shadow fix already handles safely, so
/// this check does not fire for it (never over-refuse the already-safe case). A self-referential
/// `let y = y;` (RHS names the SAME binding being shadowed) is deliberately excluded too — it
/// re-states the current threaded value under its own name and introduces no second alias.
fn aliased_threaded_binding<'a>(
    local: &syn::Local,
    shadowed_name: &str,
    threaded: &'a [ThreadedBinding],
) -> Option<&'a str> {
    let init = local.init.as_ref()?;
    let Expr::Path(p) = &*init.expr else {
        return None;
    };
    if p.qself.is_some() || p.path.segments.len() != 1 {
        return None;
    }
    let rhs_name = p.path.segments[0].ident.to_string();
    threaded
        .iter()
        .find(|t| t.name == rhs_name && t.name != shadowed_name)
        .map(|t| t.name.as_str())
}

/// CRITICAL fix (DN-125/M-1081, strict review of PR #1527): the set of threaded-binding names
/// this body's `let`-bindings SHADOW anywhere in the flat statement sequence — see
/// `emit_mutating_block_as_expr_inner`'s doc for the full corruption this pre-scan exists to
/// prevent. Only a plain `let <name> = ..;` (simple `Pat::Ident`) counts; any other pattern shape
/// is a separate, pre-existing gap (`emit_local_binding`) the moment it is actually processed.
fn shadowed_threaded_names(block: &Block, threaded: &[ThreadedBinding]) -> HashSet<String> {
    let mut out = HashSet::new();
    for s in &block.stmts {
        if let Stmt::Local(local) = s {
            if let Some(name) = local_binding_simple_name(local) {
                if threaded.iter().any(|t| t.name == name) {
                    out.insert(name);
                }
            }
        }
    }
    out
}

/// The synthetic internal carrier name used to route a shadow-risked threaded binding's true
/// final value safely through to the tail, immune to a source-level `let <name> = ..` shadow of
/// the same name (see `emit_mutating_block_as_expr_inner`'s CRITICAL-fix doc). `__myc_thread_`
/// is an internal convention, never emitted for an ordinary source binding — and
/// `emit_mutating_block_as_expr_inner` additionally refuses outright (never-silent, VR-5) rather
/// than risk a collision if the source itself already spells this exact name.
fn synth_thread_name(name: &str) -> String {
    format!("__myc_thread_{name}")
}

/// Fold the accumulated `(name, value)` re-assignment bindings into nested `let name = value in
/// ..` shadows (identical fold direction to [`emit_block_as_expr_inner`]'s, so sequential
/// re-assignments to the SAME name compose as sequential rebinds — see this section's module
/// doc), seeded with the threaded binding(s)' own reference (plus `extra`, if any) as the
/// innermost tail — a single bare reference when there is exactly one threaded binding and no
/// extra value, else a tuple. **CRITICAL-fix (DN-125/M-1081):** for a threaded binding whose name
/// is in `shadow_risk` (i.e. some plain `let` in this body shadows it), the seeded reference is
/// its synthetic carrier ([`synth_thread_name`]) rather than its bare source name — the carrier
/// is seeded/re-captured by `emit_mutating_block_as_expr_inner` so it always resolves to the
/// LAST threaded reassignment's value, never to an unrelated same-named `let` shadow that appears
/// later in the body (the silent-corruption bug this fix closes). A binding NOT in `shadow_risk`
/// is completely unaffected — same bare-name reference as before this fix, byte-identical output.
fn fold_threaded_tail(
    bindings: Vec<(String, String)>,
    threaded: &[ThreadedBinding],
    extra: Option<String>,
    shadow_risk: &HashSet<String>,
) -> String {
    let mut parts: Vec<String> = threaded
        .iter()
        .map(|t| {
            if shadow_risk.contains(&t.name) {
                synth_thread_name(&t.name)
            } else {
                t.name.clone()
            }
        })
        .collect();
    if let Some(e) = extra {
        parts.push(e);
    }
    let mut tail = if parts.len() == 1 {
        parts.into_iter().next().unwrap_or_default()
    } else {
        format!("({})", parts.join(", "))
    };
    for (name, value) in bindings.into_iter().rev() {
        tail = format!("let {name} = {value} in {tail}");
    }
    tail
}

/// If `lhs` is `EXACTLY <name>.<member>` where `<name>` is a bare, single-segment identifier
/// naming one of `threaded`'s bindings — return that binding + the member. Does NOT recurse
/// through nested field access (`self.inner.field`) or any other wrapper — only a single,
/// direct-on-the-threaded-name projection is a supported reassignment target (DN-125 §6.1's
/// narrow, structurally-safe scope).
fn threaded_field_lhs<'a>(
    e: &Expr,
    threaded: &'a [ThreadedBinding],
) -> Option<(&'a ThreadedBinding, syn::Member)> {
    let Expr::Field(f) = e else { return None };
    let Expr::Path(p) = &*f.base else { return None };
    if p.qself.is_some() || p.path.segments.len() != 1 {
        return None;
    }
    let name = p.path.segments[0].ident.to_string();
    threaded
        .iter()
        .find(|t| t.name == name)
        .map(|t| (t, f.member.clone()))
}

/// If `lhs` is `EXACTLY *<name>` where `<name>` is one of `threaded`'s bindings — return that
/// binding. The whole-value counterpart of [`threaded_field_lhs`]: supported for ANY threaded
/// binding (not just `self`), since it replaces the entire value rather than projecting a field.
fn threaded_deref_lhs<'a>(
    e: &Expr,
    threaded: &'a [ThreadedBinding],
) -> Option<&'a ThreadedBinding> {
    let Expr::Unary(u) = e else { return None };
    if !matches!(u.op, syn::UnOp::Deref(_)) {
        return None;
    }
    let Expr::Path(p) = &*u.expr else { return None };
    if p.qself.is_some() || p.path.segments.len() != 1 {
        return None;
    }
    let name = p.path.segments[0].ident.to_string();
    threaded.iter().find(|t| t.name == name)
}

/// The ten Rust compound-assignment operators desugar (syn 2, no separate `ExprAssignOp`) to
/// `Expr::Binary` with a `*Assign` [`syn::BinOp`] — this maps each to its PLAIN (non-assigning)
/// counterpart so the new field/whole value can be composed via a synthetic `Expr::Binary` node
/// re-using [`emit_expr`]'s existing, fully-tested binary-op emission (signed/unsigned prim
/// selection, bitwise word-forms, …) rather than duplicating any of that logic (DRY).
fn compound_to_plain_bin_op(op: &syn::BinOp) -> Option<syn::BinOp> {
    use syn::BinOp;
    Some(match op {
        BinOp::AddAssign(_) => BinOp::Add(Default::default()),
        BinOp::SubAssign(_) => BinOp::Sub(Default::default()),
        BinOp::MulAssign(_) => BinOp::Mul(Default::default()),
        BinOp::DivAssign(_) => BinOp::Div(Default::default()),
        BinOp::RemAssign(_) => BinOp::Rem(Default::default()),
        BinOp::BitXorAssign(_) => BinOp::BitXor(Default::default()),
        BinOp::BitAndAssign(_) => BinOp::BitAnd(Default::default()),
        BinOp::BitOrAssign(_) => BinOp::BitOr(Default::default()),
        BinOp::ShlAssign(_) => BinOp::Shl(Default::default()),
        BinOp::ShrAssign(_) => BinOp::Shr(Default::default()),
        _ => return None,
    })
}

/// Build a bare-identifier `syn::Expr::Path` node naming `name` — used to synthesize the "current
/// value" operand of a compound whole-value reassignment (`*y += v` needs `y`'s current value as
/// the synthetic binary's LHS; `y` textually, since `y` is already the value under this module's
/// `&mut T`-erasure model, never `*y`). `name` is always either the literal `"self"` or a Rust
/// identifier `map_signature` already accepted via `guard_ident`, so this never panics on
/// unparseable input in practice.
fn ident_path_expr(name: &str) -> Expr {
    let ident = syn::Ident::new(name, proc_macro2::Span::call_site());
    let mut segments = syn::punctuated::Punctuated::new();
    segments.push(syn::PathSegment {
        ident,
        arguments: syn::PathArguments::None,
    });
    Expr::Path(syn::ExprPath {
        attrs: vec![],
        qself: None,
        path: syn::Path {
            leading_colon: None,
            segments,
        },
    })
}

/// If `e` is a supported threaded-binding re-assignment (`self.<field> (=|OP=) rhs` or
/// `*<param> (=|OP=) rhs`), return `Some((binding-name, new-value-text))` — the caller folds this
/// into a `let <name> = <new-value> in ..` rebind. `Ok(None)` for any expression that is not one
/// of these two shapes at all (the caller then tries the tail-expression / generic-gap paths).
/// Once the LHS is confirmed to target a threaded binding, every subsequent failure (unresolvable
/// field, unsupported non-`self` field target, RHS emission failure) is a real `Err` — never
/// silently reinterpreted as "not an assignment" (G2).
fn try_threaded_assign(
    e: &Expr,
    self_ty: Option<&str>,
    env: &TypeEnv,
    threaded: &[ThreadedBinding],
) -> Result<Option<(String, String)>, GapReason> {
    let (lhs, rhs, plain_op): (&Expr, &Expr, Option<syn::BinOp>) = match e {
        Expr::Assign(a) => (&a.left, &a.right, None),
        Expr::Binary(b) if is_compound_assign_op(&b.op) => {
            let op = compound_to_plain_bin_op(&b.op).ok_or_else(|| {
                GapReason::new(
                    Category::Other,
                    "compound-assignment operator has no plain-operator counterpart for \
                     value-threading",
                )
            })?;
            (&b.left, &b.right, Some(op))
        }
        _ => return Ok(None),
    };

    if let Some((tb, member)) = threaded_field_lhs(lhs, threaded) {
        if tb.name != "self" {
            return Err(GapReason::new(
                Category::Other,
                format!(
                    "field assignment `{}.<field> = ..` on a threaded `&mut` parameter (not the \
                     method receiver) has no supported reconstruction — only whole-value \
                     re-assignment (`*{} = ..`) is supported for a non-`self` threaded binding \
                     (field-level projection is only wired for `self`, see `visit_field`)",
                    tb.name, tb.name
                ),
            ));
        }
        let sty = self_ty.ok_or_else(|| {
            GapReason::new(
                Category::Other,
                "`self` field assignment with no enclosing impl/trait `self` type",
            )
        })?;
        let layout = tb.layout.clone().ok_or_else(|| {
            GapReason::new(
                Category::Other,
                format!(
                    "field assignment `self.{} = ..` on `{sty}` — not an in-file single-ctor \
                     struct that emits (no constructor to rebuild)",
                    member_text(&member)
                ),
            )
        })?;
        let pos = match &member {
            syn::Member::Named(id) => {
                let n = id.to_string();
                layout.iter().position(|f| f.as_deref() == Some(n.as_str()))
            }
            syn::Member::Unnamed(idx) => {
                let i = idx.index as usize;
                (i < layout.len()).then_some(i)
            }
        }
        .ok_or_else(|| {
            GapReason::new(
                Category::Other,
                format!(
                    "field `{}` not found on struct `{sty}`",
                    member_text(&member)
                ),
            )
        })?;
        let new_field_val = match plain_op {
            None => emit_expr(rhs, self_ty, env)?,
            Some(op) => {
                let synth = Expr::Binary(syn::ExprBinary {
                    attrs: vec![],
                    left: Box::new(lhs.clone()),
                    op,
                    right: Box::new(rhs.clone()),
                });
                emit_expr(&synth, self_ty, env)?
            }
        };
        let recon = reconstruct_positional(sty, &layout, "self", pos, &new_field_val);
        return Ok(Some(("self".to_string(), recon)));
    }

    if let Some(tb) = threaded_deref_lhs(lhs, threaded) {
        let new_val = match plain_op {
            None => emit_expr(rhs, self_ty, env)?,
            Some(op) => {
                let synth = Expr::Binary(syn::ExprBinary {
                    attrs: vec![],
                    left: Box::new(ident_path_expr(&tb.name)),
                    op,
                    right: Box::new(rhs.clone()),
                });
                emit_expr(&synth, self_ty, env)?
            }
        };
        return Ok(Some((tb.name.clone(), new_val)));
    }

    Ok(None)
}

/// Build the positional constructor text for `sty` after replacing the field at `pos` with
/// `new_val_text`, reading every OTHER field via the existing self-field-access projection
/// ([`field_projection_text`]) against `base`'s CURRENT (pre-this-assignment) value.
fn reconstruct_positional(
    sty: &str,
    layout: &StructLayout,
    base: &str,
    pos: usize,
    new_val_text: &str,
) -> String {
    let args: Vec<String> = (0..layout.len())
        .map(|i| {
            if i == pos {
                new_val_text.to_string()
            } else {
                field_projection_text(sty, layout, base, i)
            }
        })
        .collect();
    format!("{sty}({})", args.join(", "))
}

/// Re-encode a Rust string value into a Mycelium `StrLit` (grammar `literal ::= … | StrLit`,
/// line 414; `StrLit ::= '"' (StrChar | EscapeSeq)* '"'`, line 430; M-910/M-911). `syn` hands us
/// the *decoded* string value, so re-escape it into Mycelium's deliberately-minimal escape set
/// (`EscapeSeq ::= '\' ('n' | 't' | '\\' | '"' | '0' | 'r')`, line 433). A control character with
/// no Mycelium escape is a never-silent gap, not a raw-byte injection: Mycelium has no `\xNN`/
/// `\u{..}` form (grammar §StrLit note, lines 424-428), so such a char *cannot* be faithfully
/// represented (G2/VR-5). Every other char — including non-ASCII like `μ` — is a valid `StrChar`
/// (`[^"\\\n\r]`, line 431) that lowers to its UTF-8 bytes (line 427), so it is emitted verbatim.
pub(crate) fn myc_string_literal(value: &str) -> Result<String, GapReason> {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if c.is_control() => {
                return Err(GapReason::new(
                    Category::Other,
                    format!(
                        "string literal contains control character U+{:04X} with no Mycelium \
                         escape — StrLit's escape set is exactly `\\n \\t \\\\ \\\" \\0 \\r` (no \
                         `\\xNN`/`\\u{{..}}` form; grammar §StrLit/EscapeSeq, M-910/M-911), so it \
                         cannot be faithfully represented",
                        c as u32
                    ),
                ))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

/// Whether `digits` (a `syn::LitFloat::base10_digits()` string — the suffix already stripped and
/// underscores removed by `syn`) is a well-formed Mycelium `FloatLit` (grammar lines 443-445:
/// `[0-9]+ '.' [0-9]+ Exponent?` or `[0-9]+ Exponent`; `Exponent ::= ('e' | 'E') ('+' | '-')?
/// [0-9]+`). Only an exact shape match returns `true` — a Rust-only form (a bare `1f64` → "1", a
/// trailing-dot `2.` → "2.") returns `false` and is gapped rather than reshaped, so the emitter
/// never synthesizes a literal the source did not already spell (VR-5). (`syn` normalizes `E`→`e`,
/// drops a `+` exponent sign, and strips underscores, all of which stay within this grammar.)
fn is_myc_float_literal(digits: &str) -> bool {
    let (mantissa, exp) = match digits.find(['e', 'E']) {
        Some(i) => (&digits[..i], Some(&digits[i + 1..])),
        None => (digits, None),
    };
    if let Some(e) = exp {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        if e.is_empty() || !e.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match mantissa.split_once('.') {
        // `[0-9]+ '.' [0-9]+` (Exponent already validated above if present).
        Some((int, frac)) => all_digits(int) && all_digits(frac),
        // `[0-9]+ Exponent` — a dot-less mantissa is a FloatLit *only* with an exponent (else it
        // is an `Int`, not a float — Mycelium's structural Int/float disambiguation, grammar
        // line 437).
        None => exp.is_some() && all_digits(mantissa),
    }
}

/// Translate one Rust expression. Exhaustive `match` over `syn::Expr` (itself `#[non_exhaustive]`
/// — the trailing `_` arm is therefore also the forward-compatibility catch-all); every arm not
/// explicitly handled falls to that final arm, which returns `Err`, never emits a placeholder.
///
/// **RFC-0041 §4.7 (W1):** guarded by the crate-wide recursion budget (`crate::gap::guarded`) —
/// mutually recurses with [`emit_block_as_expr`]/[`map_pattern`] over unbounded/attacker-controlled
/// input depth (e.g. deeply-parenthesized `Expr::Paren`), so each call consumes one budget frame
/// and refuses with a `Category::RecursionBudget` gap rather than risking a host-stack overflow.
pub fn emit_expr(expr: &Expr, self_ty: Option<&str>, env: &TypeEnv) -> Result<String, GapReason> {
    guarded(|| emit_expr_inner(expr, self_ty, env))
}

/// The recursion-guarded body of [`emit_expr`] (see [`emit_expr`]'s docs / `crate::gap::guarded`).
/// Recursive calls within this match use the public `emit_expr` name so each nested call re-enters
/// the guard.
fn emit_expr_inner(expr: &Expr, self_ty: Option<&str>, env: &TypeEnv) -> Result<String, GapReason> {
    // Routed through `crate::visit::ExprVisitor` (M-1041 Scope-A): the previous single ~19-arm
    // hand-written `match` now lives as `EmitVisitor`'s per-variant methods (below), reached via
    // the shared `crate::visit::walk_expr` dispatcher. Every method body is the unmodified
    // content of its former match arm (only bare `self_ty`/`env` references became
    // `self.self_ty`/`self.env` — the same values, now visitor fields instead of function
    // parameters), so this is a pure relocation, not a behavior change (verified: byte-identical
    // `cargo test -p mycelium-transpile`).
    let mut visitor = EmitVisitor { self_ty, env };
    crate::visit::walk_expr(expr, &mut visitor)
}

/// The `emit_expr_inner` translation, reified as a `crate::visit::ExprVisitor` (M-1041 Scope-A —
/// the DRY force-multiplier pilot). Each method below is the *unmodified* body of its former
/// match arm in the pre-refactor `emit_expr_inner` — only the outer dispatch moved to the shared
/// `crate::visit::walk_expr`, and every bare `self_ty`/`env` reference became
/// `self.self_ty`/`self.env` (fields instead of function parameters, same values). No emitted
/// `.myc` text and no `GapReason` message changed.
struct EmitVisitor<'a> {
    self_ty: Option<&'a str>,
    env: &'a TypeEnv,
}

impl crate::visit::ExprVisitor for EmitVisitor<'_> {
    type Output = Result<String, GapReason>;

    fn fallback(&mut self, expr: &Expr) -> Self::Output {
        Err(GapReason::new(
            Category::Other,
            format!("unsupported expression form `{}`", tokens_to_string(expr)),
        ))
    }

    fn visit_path(&mut self, expr: &Expr, p: &syn::ExprPath) -> Self::Output {
        if p.qself.is_some() {
            return self.fallback(expr);
        }
        // Declared mapping decision: a qualified path (`Type::Variant`, UFCS calls) is
        // reduced to its last segment — Mycelium constructor/value references are bare
        // identifiers within a nodule (matching `lib/std/cmp.myc`'s own style, e.g. `Lt`
        // rather than `Ordering.Lt`); this transpiler emits everything into one nodule, so
        // qualification carries no distinguishing information here.
        let seg = p
            .path
            .segments
            .last()
            .ok_or_else(|| GapReason::new(Category::Other, "empty path expression"))?;
        let name = seg.ident.to_string();
        let resolved = resolve_surface_ident(&name, "value/constructor reference")?;
        // ORACLE-R1 A4: a private const co-emitted as a zero-arg fn must be *called* — a bare
        // name is `unknown name DEFAULT_FUEL` at myc-check (no const binding in the surface).
        if is_const_zero_arg_fn(&name) || is_const_zero_arg_fn(&resolved) {
            return Ok(format!("{resolved}()"));
        }
        Ok(resolved)
    }

    fn visit_lit(&mut self, _expr: &Expr, l: &syn::ExprLit) -> Self::Output {
        match &l.lit {
            Lit::Bool(b) => Ok(if b.value { "True" } else { "False" }.to_string()),
            Lit::Int(i) => Ok(i.base10_digits().to_string()),
            // A Rust string literal maps to a Mycelium `StrLit` (grammar `literal ::= … | StrLit`,
            // line 414; M-910/M-911). `myc_string_literal` re-escapes into Mycelium's minimal
            // escape set and gaps (never-silent) on a char it cannot faithfully represent.
            Lit::Str(s) => myc_string_literal(&s.value()),
            // A Rust float literal maps to a Mycelium `FloatLit` (grammar `literal ::= … | FloatLit`,
            // line 414 / `FloatLit`, line 443; ADR-040/M-897) — but *only* when its `syn`-normalized
            // digit string is already a well-formed FloatLit AND denotes a finite binary64 value
            // (ADR-040 §2.4: a literal is a conversion boundary, out-of-range is a never-silent
            // refuse, so a non-finite `1e999` never lands on ±inf). A Rust-only shape or a
            // non-finite value is gapped rather than reshaped/forced (VR-5).
            Lit::Float(f) => {
                let digits = f.base10_digits();
                if !is_myc_float_literal(digits) {
                    Err(GapReason::new(
                        Category::Other,
                        format!(
                            "float literal `{digits}` has no faithful Mycelium `FloatLit` spelling \
                             (FloatLit is `[0-9]+ '.' [0-9]+ Exponent?` | `[0-9]+ Exponent`, no \
                             trailing-dot/bare-suffix form — grammar line 443; ADR-040/M-897)"
                        ),
                    ))
                } else if !f.base10_parse::<f64>().is_ok_and(f64::is_finite) {
                    Err(GapReason::new(
                        Category::Other,
                        format!(
                            "float literal `{digits}` is not a finite binary64 value — a literal \
                             is a conversion boundary, so out-of-range is a never-silent refuse, \
                             never a silent ±inf (ADR-040 §2.4 / FloatLit note, grammar line 439)"
                        ),
                    ))
                } else {
                    Ok(digits.to_string())
                }
            }
            _ => Err(GapReason::new(
                Category::Other,
                format!(
                    "unsupported literal kind `{}` (only bool/int/string/float literals map)",
                    tokens_to_string(l)
                ),
            )),
        }
    }

    fn visit_if(&mut self, _expr: &Expr, e: &syn::ExprIf) -> Self::Output {
        let else_branch = e.else_branch.as_ref().ok_or_else(|| {
            GapReason::new(
                Category::Other,
                "`if` without an `else` branch — if_expr requires both arms",
            )
        })?;
        if matches!(*e.cond, Expr::Let(_)) {
            return Err(GapReason::new(
                Category::Other,
                "`if let` has no Mycelium equivalent in this grammar fragment",
            ));
        }
        let cond = emit_expr(&e.cond, self.self_ty, self.env)?;
        let then_ = emit_block_as_expr(&e.then_branch, self.self_ty, self.env)?;
        let else_ = emit_expr(&else_branch.1, self.self_ty, self.env)?;
        Ok(format!("if {cond} then {then_} else {else_}"))
    }

    fn visit_match(&mut self, _expr: &Expr, m: &syn::ExprMatch) -> Self::Output {
        let scrutinee = emit_expr(&m.expr, self.self_ty, self.env)?;
        // M-1035/ENB-12: a string-literal arm implies a `Bytes` scrutinee, and `Bytes` is an
        // OPEN value domain — the L1 checker's W7 coverage rejects a non-exhaustive `Bytes`
        // match (`non-exhaustive match on Bytes: missing _`, verified against the oracle). So a
        // string-literal `match` is emittable-and-check-clean ONLY with a wildcard/default arm;
        // without one, emit nothing (gap the whole match) rather than a check-failing surface
        // that would regress `checked_fraction` (VR-5/G2). Non-string matches are unaffected.
        if m.arms.iter().any(|a| pattern_contains_str_lit(&a.pat))
            && !m
                .arms
                .iter()
                .any(|a| a.guard.is_none() && is_irrefutable_match_default(&a.pat))
        {
            return Err(GapReason::new(
                Category::Other,
                "string-literal `match` on a `Bytes` scrutinee without a wildcard/default arm \
                 (`_ => …`): `Bytes` is an open value domain, so the L1 checker rejects a \
                 non-exhaustive match (`non-exhaustive match on Bytes: missing _` — M-1035/ \
                 ENB-12 W7 coverage); emitting it would regress checked_fraction (VR-5/G2)",
            ));
        }
        let mut arms = Vec::with_capacity(m.arms.len());
        for arm in &m.arms {
            if arm.guard.is_some() {
                return Err(GapReason::new(
                    Category::Other,
                    "match-arm guard (`if ...`) has no Mycelium equivalent (arm grammar has \
                     no guard slot)",
                ));
            }
            let pat = map_pattern(&arm.pat, self.self_ty)?;
            // A match arm's pattern can **bind** names that shadow an outer local of the same
            // name with a completely different (and possibly narrower/wider) type — e.g. an
            // enum payload field bound by the pattern is not the outer parameter it shadows.
            // `env` must never let `Expr::Binary`'s operand-type gate keep firing on such a
            // name using the *outer* type, so strip every name this arm's pattern binds from a
            // per-arm copy of `env` before emitting the arm body (VR-5: absence, never a stale
            // guess — see `collect_pattern_bound_names`'s docs for why this is conservative).
            let arm_env = if self.env.is_empty() {
                self.env.clone()
            } else {
                let mut bound = HashSet::new();
                collect_pattern_bound_names(&arm.pat, &mut bound);
                if bound.is_empty() {
                    self.env.clone()
                } else {
                    let mut e = self.env.clone();
                    for name in &bound {
                        e.remove(name);
                    }
                    e
                }
            };
            let body = emit_expr(&arm.body, self.self_ty, &arm_env)?;
            arms.push(format!("{pat} => {body}"));
        }
        Ok(format!("match {scrutinee} {{ {} }}", arms.join(", ")))
    }

    fn visit_binary(&mut self, _expr: &Expr, b: &syn::ExprBinary) -> Self::Output {
        use syn::BinOp;
        // Express gap-close: a bare decimal lit on one side of a Binary comparison has no
        // paradigm (Q6) and wrong width. When the other operand is known Binary{N}, rewrite
        // the lit to an equal-width BinLit zero/value so `lt`/`eq` can fire cleanly.
        // ONESHOT C3: the same rewrite applies to bitwise `&`/`|`/`^` — a mask lit (`0o400`)
        // paired with a known-width field is the std-fs `Permissions::owner_read` residual
        // (`mode & 0o400 != 0`); leaving the decimal lit forces Q6/band/ne poison.
        let lit_as_bin =
            |e: &Expr, width: u32| -> Option<String> { int_lit_as_bin_literal(e, width) };
        let field_bin_w = |e: &Expr| -> Option<u32> {
            let Expr::Paren(p) = e else {
                return match_first_field_binary_width(e, self.self_ty);
            };
            match_first_field_binary_width(&p.expr, self.self_ty)
        };
        let field_signed_w = |e: &Expr| -> Option<u32> {
            let Expr::Paren(p) = e else {
                return match_first_field_signed_binary_width(e, self.self_ty);
            };
            match_first_field_signed_binary_width(&p.expr, self.self_ty)
        };
        let width_from_myc = |s: &str| -> Option<u32> {
            // Recover `Binary{N}` from emitted projection text.
            let i = s.find("Binary{")?;
            let rest = &s[i + "Binary{".len()..];
            let end = rest.find('}')?;
            rest[..end].parse().ok()
        };
        // ONESHOT C3: recover Binary{N} width through bitwise/arith chains so
        // `(self.mode & 0o400) != 0` sees the field's width on the Ne left (field_bin_w alone
        // only matches a bare field / match-projection, not a BitAnd of one). Width-preserving
        // ops only — never invent a width for an unmatched shape (VR-5).
        fn chain_bin_w(
            e: &Expr,
            env: &TypeEnv,
            field_bin_w: &dyn Fn(&Expr) -> Option<u32>,
        ) -> Option<u32> {
            if let Some(w) = expr_env_binary_width(e, env).or_else(|| field_bin_w(e)) {
                return Some(w);
            }
            match e {
                Expr::Paren(p) => chain_bin_w(&p.expr, env, field_bin_w),
                Expr::Reference(r) => chain_bin_w(&r.expr, env, field_bin_w),
                Expr::Binary(inner)
                    if matches!(
                        &inner.op,
                        BinOp::BitAnd(_)
                            | BinOp::BitOr(_)
                            | BinOp::BitXor(_)
                            | BinOp::Shl(_)
                            | BinOp::Shr(_)
                            | BinOp::Add(_)
                            | BinOp::Sub(_)
                            | BinOp::Mul(_)
                    ) =>
                {
                    chain_bin_w(&inner.left, env, field_bin_w)
                        .or_else(|| chain_bin_w(&inner.right, env, field_bin_w))
                }
                _ => None,
            }
        }
        let lw = chain_bin_w(&b.left, self.env, &field_bin_w);
        let rw = chain_bin_w(&b.right, self.env, &field_bin_w);
        let lhs_raw = emit_expr(&b.left, self.self_ty, self.env)?;
        let rhs_raw = emit_expr(&b.right, self.self_ty, self.env)?;
        let lw = lw.or_else(|| width_from_myc(&lhs_raw));
        let rw = rw.or_else(|| width_from_myc(&rhs_raw));
        let is_cmp = matches!(
            &b.op,
            BinOp::Eq(_) | BinOp::Ne(_) | BinOp::Lt(_) | BinOp::Gt(_) | BinOp::Le(_) | BinOp::Ge(_)
        );
        let is_bitwise = matches!(&b.op, BinOp::BitAnd(_) | BinOp::BitOr(_) | BinOp::BitXor(_));
        // Rewrite decimal/octal/hex int lits on comparisons AND bitwise ops (ONESHOT C3).
        let rewrite_lits = is_cmp || is_bitwise;
        let lhs = if rewrite_lits {
            if let Some(w) = rw {
                lit_as_bin(&b.left, w).unwrap_or_else(|| lhs_raw.clone())
            } else {
                lhs_raw.clone()
            }
        } else {
            lhs_raw.clone()
        };
        let rhs = if rewrite_lits {
            if let Some(w) = lw {
                lit_as_bin(&b.right, w).unwrap_or_else(|| rhs_raw.clone())
            } else {
                rhs_raw.clone()
            }
        } else {
            rhs_raw.clone()
        };
        // trx2 Lane C Deliverable 1 — operand-type-gated operator emission (VERIFY-FIRST,
        // mitigation #14; every claim below is a *measured* `myc check` result over the built
        // `target/debug/myc`, not a doc-derived guess — see the crate's `src/tests/emit.rs`
        // `binop_operand_gated` fixtures for the same probes committed as regression tests).
        //
        // The kernel's real bitwise/comparison surface (`crates/mycelium-l1/src/checkty.rs`
        // `prim_kernel_name`/`prim_sig`, `Π`) registers `and`/`or`/`xor`/`not`/`eq`/`lt` as
        // BARE-CALL builtin prims resolvable with **no import** (checkty.rs:7214-7264) — but
        // the PARSER's glyph→word desugar table (`crates/mycelium-l1/src/parse.rs::infix_op`)
        // does NOT send every glyph to its matching prim name: `&` desugars to word `"band"`
        // and `|` to `"bor"` (parse.rs:2383/2385) — names that exist ONLY as ordinary
        // `lib/std/math.myc` functions (`band`/`bor`, wrapping `and`/`or`), not as prims, so a
        // glyph emission with no `use std.math.band;` import (this transpiler emits one
        // import-less nodule — see `emit_expr`'s `Expr::Path` doc) fails `myc check` with
        // "unknown function/constructor/prim `band`"/`"bor"` — confirmed empirically. `^`
        // (BitXor) is the one glyph that already desugars to the CORRECT prim name (`"xor"`,
        // parse.rs:2384) and checks clean as-is — left unchanged below.
        //
        // `!=`/`>` are a *different* shape of the same problem, one level deeper: they desugar
        // to words `"ne"`/`"gt"` (parse.rs:2390/2392), but `ne`/`gt` are not prims at all —
        // they are ordinary (and, as committed today, non-`pub`) functions in
        // `lib/std/cmp.myc` (§CU-4). Confirmed empirically: `ne(a, b)`/`gt(a, b)` as a BARE
        // CALL fails identically to the `!=`/`>` glyphs ("unknown function/constructor/prim
        // `ne`"/`"gt"`) — because a glyph and its desugar-target word call parse to the exact
        // same `Expr::App` node (parse.rs's `op_call` doc: "`a + b` and `add(a, b)` are
        // structurally identical after parsing"), so respelling the *emitted text* from `!=`
        // to `ne(a, b)` changes NOTHING about whether it checks — both fail exactly alike, with
        // or without importing `std.cmp` (whose `ne`/`gt`/`cmp`/... are not `pub` in the
        // committed corpus, so even a real `use std.cmp.ne;` import would additionally fail).
        // This directly **contradicts** an initial-brief assumption that a `ne`/`gt` word-call
        // spelling would newly check-clean (VR-5/house-rule-#4: surfacing the disconfirming
        // finding, not implementing an assumption the codebase doesn't support). Emitting the
        // bare identifier form was therefore rejected as a no-op change.
        //
        // The real, verified fix for `!=`/`>`: compose them from the two comparison prims that
        // ARE bare-call-resolvable with no import (`eq`/`lt`, confirmed above) — exactly the
        // derivation `lib/std/cmp.myc`'s own `ne{N}`/`gt{N}` bodies use (cmp.myc:111-116:
        // `ne(a,b) = match eq(a,b) { 0b1 => False, _ => True }`; `gt(a,b) = match cmp(a,b) {
        // Gt=>True,... }`, and `cmp` itself is `match eq(a,b) {0b1=>Eq, _=>match lt(a,b)
        // {0b1=>Lt, _=>Gt}}` — so `gt` unfolds to "not eq, and not lt"). This is a faithful,
        // prim-composed body, not a fabrication — the same idiom this module already uses for
        // `try_width_cast_widen_body`'s synthesized `width_cast` call. Verified `myc
        // check`-clean end-to-end (both cases, no import) via the committed regression tests
        // below.
        //
        // Every case here is gated on **both operands resolving to a known `Binary{N}`** via
        // `expr_env_binary_width` (only a bare identifier already in `env` can ever resolve —
        // never a guess, VR-5); an unresolved operand keeps the prior, unchanged glyph
        // emission (Declared heuristic, exactly as before this deliverable).
        // Peer Binary known on both sides (original gate). Lit rewrite for comparisons AND
        // bitwise (ONESHOT C3 — mask-lit residual).
        let lit_rewrote = rewrite_lits
            && ((lw.is_some() && lit_as_bin(&b.right, lw.unwrap()).is_some())
                || (rw.is_some() && lit_as_bin(&b.left, rw.unwrap()).is_some()));
        // One known Binary side + a rewritten equal-width lit is enough for and/or/ne composition
        // (the rewritten lit *is* Binary{N} of that width).
        let both_known_binary = lw.is_some() && rw.is_some();
        let binary_with_lit = lit_rewrote && (lw.is_some() || rw.is_some());
        // ONESHOT C3: Bool-typed operands for logical `==`/`!=` (kernel `eq` is Binary/Ternary
        // only — confirmed myc-check T-Op on `Bool == Bool`). Compose the corpus `bool_eq`/
        // inverted shape (`lib/std/cmp.myc`), never a fabricated `bool_ne` prim.
        let left_ty = expr_env_type(&b.left, self.env);
        let right_ty = expr_env_type(&b.right, self.env);
        let both_known_bool =
            left_ty.as_deref() == Some("Bool") && right_ty.as_deref() == Some("Bool");
        // P4/P5 (DN-99 §8 ENB-6 / M-1029 / ADR-028; VERIFY-FIRST, mitigation #14 — every claim
        // below is a *measured* `myc check` result over the built `target/debug/myc-check`, not a
        // doc-derived guess, mirroring the Deliverable-1 probes above; see this crate's
        // `src/tests/emit.rs` `signed_*_check_clean` live-oracle fixtures).
        //
        // ADR-028: `add`/`sub`/`mul`/`neg` are bit-identical for signed/unsigned two's-complement,
        // but the kernel's OVERFLOW-CHECKED prims still split by signedness (`add_u`/`sub_u`/
        // `mul_u` detect UNSIGNED overflow; `add_s`/`sub_s`/`mul_s`/`neg_s` detect SIGNED/two's-
        // complement overflow — checkty.rs:8005-8040) — so a source-signed operand must route to
        // the `_s` family to report the semantically-correct overflow. `lt`'s ordering genuinely
        // differs by signedness (`lt` reads Binary as unsigned magnitude; `lt_s` is the signed/
        // two's-complement order, ADR-028's `bvslt`/`bvult` split) — confirmed `lt_s(a, b)`
        // resolves as a bare-call prim with no import, `myc check`-clean. `eq` is signedness-
        // agnostic (bit-pattern equality) — no `eq_s` exists or is needed, so `Ne`'s EXISTING
        // `both_known_binary`-gated composed form already applies unchanged to a signed operand
        // too (widened below to `both_known_signed_binary` so it still fires when `expr_env_
        // binary_width` is opaque to the `"!s"` marker — see `sig_type_env`'s doc). `Gt`'s signed
        // form composes `eq` + `lt_s` exactly as the existing unsigned `Gt` arm composes `eq` +
        // `lt` (same derivation, signed order). `Lt` (RFC-0032 D1's "canonical" bare glyph for
        // unsigned `lt`) has no established bare-glyph convention for the signed case — bare
        // `lt_s` also returns `Binary{1}`, so a signed `<` is bridged to `Bool` the same proven
        // way `Gt`'s composition already is (confirmed empirically: a bare `a < b`/`==` embedded
        // directly as a `Bool`-typed fn body does NOT check-clean regardless of signedness — a
        // PRE-EXISTING, orthogonal gap this leaf does not touch; the bridged form is required).
        //
        // Each signed arm is gated on **both operands resolving to a KNOWN SIGNED `Binary{N}`**
        // via `expr_env_signed_binary_width` — only a bare identifier the signature already
        // recorded as source-signed (`type_is_signed_int`, `map_signature`) can ever resolve;
        // never a guess (VR-5). An unresolved-signed operand (unsigned, or type unknown) falls
        // through unchanged to the existing unsigned-gated / plain-glyph arms below — Add/Sub/Mul
        // for an unsigned `Binary{N}` operand stay the PRE-EXISTING (already-broken, out of
        // scope) plain-glyph form; this leaf only adds new signed-specific coverage, never
        // regresses the unsigned path.
        // Signedness: bare params via env (`!s` marker) OR in-file struct fields via the
        // field-type map. A decimal lit has no env entry — so a signed field/param compared
        // to a rewritten zero must still route to `lt_s` (ADR-028 signed order). Using
        // unsigned `lt` for `self.nanos < 0` would mis-order high-bit payloads (G2/VR-5).
        let left_signed_w =
            expr_env_signed_binary_width(&b.left, self.env).or_else(|| field_signed_w(&b.left));
        let right_signed_w =
            expr_env_signed_binary_width(&b.right, self.env).or_else(|| field_signed_w(&b.right));
        let both_known_signed_binary = left_signed_w.is_some() && right_signed_w.is_some();
        // One known-signed Binary side + a lit rewritten to equal-width BinLit (the other side).
        let signed_lit_cmp = lit_rewrote && (left_signed_w.is_some() || right_signed_w.is_some());
        let use_signed_order = both_known_signed_binary || signed_lit_cmp;
        match &b.op {
            // RFC-0032 D1 (ratified): `==`/`<` glyphs are the canonical surface for `eq`/`lt`
            // — left unchanged (not part of this deliverable's operand-gated rewrite).
            // Glyph `==`/`<` stay when both sides are already Binary idents (fixture corpus /
            // D1). Bridge to `match eq/lt {… Bool}` only when a decimal lit was rewritten to a
            // BinLit (otherwise bare `x < 0` fails Q6 ambient).
            // ONESHOT C3: Bool `==` is NOT the Binary `eq` prim — compose corpus `bool_eq`.
            BinOp::Eq(_) if both_known_bool => Ok(format!(
                "match ({lhs}) {{ True => ({rhs}), False => match ({rhs}) {{ True => False, \
                 False => True }} }}"
            )),
            // ONESHOT C3: user-type `==` via co-emitted `eq_<T>` (C2 PartialEq) — kernel `eq`
            // rejects Data types (std-fs `kind == FileKind::File` residual).
            BinOp::Eq(_)
                if {
                    let lt = expr_user_named_type(&b.left, self.env, self.self_ty);
                    let rt = expr_user_named_type(&b.right, self.env, self.self_ty);
                    matches!((lt.as_deref(), rt.as_deref()), (Some(a), Some(b)) if a == b && local_eq_type_known(a))
                } =>
            {
                let ty = expr_user_named_type(&b.left, self.env, self.self_ty).unwrap();
                Ok(format!(
                    "(match eq_{ty}({lhs}, {rhs}) {{ 0b1 => True, _ => False }})"
                ))
            }
            BinOp::Eq(_) if lit_rewrote => Ok(format!(
                "(match eq({lhs}, {rhs}) {{ 0b1 => True, _ => False }})"
            )),
            BinOp::Eq(_) => Ok(format!("{lhs} == {rhs}")),
            BinOp::Lt(_) if use_signed_order => Ok(format!(
                "(match lt_s({lhs}, {rhs}) {{ 0b1 => True, _ => False }})"
            )),
            BinOp::Lt(_) if lit_rewrote => Ok(format!(
                "(match lt({lhs}, {rhs}) {{ 0b1 => True, _ => False }})"
            )),
            BinOp::Lt(_) => Ok(format!("{lhs} < {rhs}")),
            // ONESHOT C3: `!=` composes from `eq` whenever we have two known Binary sides OR a
            // Binary+rewritten-lit pair (the metadata `mode & mask != 0` residual — previously
            // only `both_known_binary` fired, so a known Binary vs `0` fell through to the
            // glyph → unknown prim `ne`). Bool `!=` composes the inverted `bool_eq` shape.
            BinOp::Ne(_) if both_known_bool => Ok(format!(
                "match ({lhs}) {{ True => match ({rhs}) {{ True => False, False => True }}, \
                 False => ({rhs}) }}"
            )),
            BinOp::Ne(_)
                if {
                    let lt = expr_user_named_type(&b.left, self.env, self.self_ty);
                    let rt = expr_user_named_type(&b.right, self.env, self.self_ty);
                    matches!((lt.as_deref(), rt.as_deref()), (Some(a), Some(b)) if a == b && local_eq_type_known(a))
                } =>
            {
                let ty = expr_user_named_type(&b.left, self.env, self.self_ty).unwrap();
                Ok(format!(
                    "(match eq_{ty}({lhs}, {rhs}) {{ 0b1 => False, _ => True }})"
                ))
            }
            BinOp::Ne(_) if both_known_binary || binary_with_lit || use_signed_order => Ok(
                format!("(match eq({lhs}, {rhs}) {{ 0b1 => False, _ => True }})"),
            ),
            BinOp::Ne(_) => Ok(format!("{lhs} != {rhs}")),
            BinOp::Gt(_) if use_signed_order => Ok(format!(
                "(match eq({lhs}, {rhs}) {{ 0b1 => False, _ => match lt_s({lhs}, {rhs}) {{ 0b1 \
                 => False, _ => True }} }})"
            )),
            BinOp::Gt(_) if both_known_binary || binary_with_lit => Ok(format!(
                "(match eq({lhs}, {rhs}) {{ 0b1 => False, _ => match lt({lhs}, {rhs}) {{ 0b1 \
                 => False, _ => True }} }})"
            )),
            BinOp::Gt(_) => Ok(format!("{lhs} > {rhs}")),
            // ONESHOT C2 — Rust `&&`/`||` are *logical* Bool connectives. The glyphs desugar
            // (parse.rs AmpAmp/PipePipe) to the words `"and"`/`"or"`, which are the Binary
            // bitwise prims (`bit.and`/`bit.or`, checkty prim_kernel_name) — so a bare `a || b`
            // on Bool fails `myc check` with T-Op
            // `` `or` does not accept argument types [Bool, Bool] `` (std-fs `OpenOptions::wants_write`
            // residual). There is no ambient `bool_or`/`bool_and` prim (lib/compiler redeclares
            // them per-nodule as match folds). Emit the same total match shape the corpus uses
            // (`lib/std/content.myc`, `lib/compiler/parse.myc`) — always, because Rust `&&`/`||`
            // are never Binary bitwise (`&`/`|` are). Short-circuit is not modelled (value-
            // semantics total evaluation; matches the corpus helpers — Declared vs Rust's
            // short-circuit, disclosed).
            BinOp::And(_) => Ok(format!(
                "match ({lhs}) {{ True => ({rhs}), False => False }}"
            )),
            BinOp::Or(_) => Ok(format!(
                "match ({lhs}) {{ True => True, False => ({rhs}) }}"
            )),
            // ONESHOT C3: bitwise `&`/`|` also fire when one side is a rewritten equal-width
            // mask lit (std-fs `mode & 0o400`).
            BinOp::BitAnd(_) if both_known_binary || binary_with_lit => {
                Ok(format!("and({lhs}, {rhs})"))
            }
            BinOp::BitAnd(_) => Ok(format!("{lhs} & {rhs}")),
            BinOp::BitOr(_) if both_known_binary || binary_with_lit => {
                Ok(format!("or({lhs}, {rhs})"))
            }
            BinOp::BitOr(_) => Ok(format!("{lhs} | {rhs}")),
            // `^` already desugars to the correct prim name (`"xor"`, parse.rs:2384) — no
            // rewrite needed; confirmed `myc check`-clean as a bare glyph.
            BinOp::BitXor(_) => Ok(format!("{lhs} ^ {rhs}")),
            BinOp::Shl(_) => Ok(format!("{lhs} << {rhs}")),
            BinOp::Shr(_) => Ok(format!("{lhs} >> {rhs}")),
            BinOp::Add(_) if both_known_signed_binary => Ok(format!("add_s({lhs}, {rhs})")),
            // D3 residual (this leaf): the UNSIGNED counterpart to the `add_s` arm above. The
            // bare `+` glyph desugars to the word `"add"` (`parse.rs::infix_op`), which is the
            // *ternary*-only prim family member (`prim_family` — checkty.rs:9975) — it never
            // resolves for `Binary{N}` operands, so `a + b` on two unsigned `Binary{N}` values
            // failed `myc check` with `` `add` does not accept argument types
            // [Binary(..), Binary(..)] `` (T-Op; RFC-0007 §4.4) — confirmed empirically on a
            // plain `fn add2(a: u64, b: u64) -> u64 { a + b }` transpilation (the exact repro this
            // leaf closes). `add_u` is the correctly-typed sibling: already registered in
            // `prim_family`/`prim_sig` (width-preserving `Binary{N}` arithmetic, RFC-0032 D2/
            // M-748) and mapped to the already-registered kernel prim `bit.add`
            // (`prim_kernel_name`, `mycelium-interp/src/prims.rs::prim_bit_add`) — so this is a
            // pure **emission** fix (CASE A: the prim exists end-to-end, checker + interpreter;
            // no kernel touch), mirroring the `add_s` arm's shape exactly. Confirmed
            // `myc check`-clean as a bare call with no import (`add2u_check_clean` fixture below).
            BinOp::Add(_) if both_known_binary => Ok(format!("add_u({lhs}, {rhs})")),
            BinOp::Add(_) => Ok(format!("{lhs} + {rhs}")),
            BinOp::Sub(_) if both_known_signed_binary => Ok(format!("sub_s({lhs}, {rhs})")),
            // Unsigned counterpart to `sub_s` above — same shape/rationale as `add_u`'s arm;
            // `sub_u` is likewise already registered (`prim_family`/`prim_sig` -> `bit.sub`,
            // `mycelium-interp/src/prims.rs::prim_bit_sub`). Confirmed `myc check`-clean.
            BinOp::Sub(_) if both_known_binary => Ok(format!("sub_u({lhs}, {rhs})")),
            BinOp::Sub(_) => Ok(format!("{lhs} - {rhs}")),
            BinOp::Mul(_) if both_known_signed_binary => Ok(format!("mul_s({lhs}, {rhs})")),
            // Unsigned counterpart to `mul_s` above — same shape/rationale; `mul_u` is likewise
            // already registered (`prim_family`/`prim_sig` -> `bit.mul`, RFC-0033 §4.1.2 CU-1's
            // never-silent unsigned multiply, `mycelium-interp/src/prims.rs::prim_bit_mul`).
            // Confirmed `myc check`-clean.
            BinOp::Mul(_) if both_known_binary => Ok(format!("mul_u({lhs}, {rhs})")),
            BinOp::Mul(_) => Ok(format!("{lhs} * {rhs}")),
            BinOp::Div(_) => Ok(format!("{lhs} / {rhs}")),
            BinOp::Rem(_) => Ok(format!("{lhs} % {rhs}")),
            // RFC-0025 §4.1: `<=`/`>=` glyphs are RETIRED; word forms `lte`/`gte` instead.
            // (Pre-existing: `lte`/`gte` have the identical not-a-prim/non-`pub`-stdlib-fn
            // gap `ne`/`gt` had — out of scope for this deliverable, which only covers
            // `& | ^ != >`; left unchanged.)
            BinOp::Le(_) => Ok(format!("lte({lhs}, {rhs})")),
            BinOp::Ge(_) => Ok(format!("gte({lhs}, {rhs})")),
            other => Err(GapReason::new(
                Category::Other,
                format!(
                    "unsupported/compound binary operator `{}`",
                    tokens_to_string(other)
                ),
            )),
        }
    }

    fn visit_unary(&mut self, _expr: &Expr, u: &syn::ExprUnary) -> Self::Output {
        let operand = emit_expr(&u.expr, self.self_ty, self.env)?;
        match &u.op {
            // P4/P5 (DN-99 §8 ENB-6 / ADR-028): a source-signed `Binary{N}` operand routes to
            // the landed `neg_s` prim (`crates/mycelium-l1/src/checkty.rs:8020`, DN-72/M-766 —
            // confirmed `myc check`-clean against the real toolchain, this leaf's verify-first
            // probe). Gated exactly like `Expr::Binary`'s signed arms — never a guess (VR-5); an
            // unresolved/unsigned operand keeps the prior, unchanged bare-glyph fallback.
            syn::UnOp::Neg(_) if expr_env_signed_binary_width(&u.expr, self.env).is_some() => {
                Ok(format!("neg_s({operand})"))
            }
            syn::UnOp::Neg(_) => Ok(format!("-{operand}")),
            // ONESHOT C3 — Rust `!` on Bool is *logical* not; the glyph desugars to word `"not"`
            // (parse.rs Bang → "not"), which is the Binary bitwise prim (`bit.not`) — so `!b` on
            // Bool fails T-Op `` `not` does not accept argument types [Bool] `` (std-fs
            // `Permissions::is_readonly` residual). There is no ambient `bool_not` prim; the
            // corpus defines it per-nodule as a total match (`lib/std/core.myc` `bool_not`).
            // Compose that match when the operand is known Bool (env / method-return bookkeeping);
            // keep the bare glyph for known Binary (already myc-check-clean) or unresolved.
            syn::UnOp::Not(_) if unary_not_operand_is_bool(&u.expr, self.env, self.self_ty) => Ok(
                format!("match ({operand}) {{ True => False, False => True }}"),
            ),
            syn::UnOp::Not(_) => Ok(format!("!{operand}")),
            _ => Err(GapReason::new(
                Category::Other,
                "unsupported unary operator (e.g. `*` deref has no equivalent in a \
                 value-semantic grammar)",
            )),
        }
    }

    /// **DN-136/P1-a (Alt B).** [`calls::lookup`] is consulted FIRST — a static, per-axis
    /// handler table (generalizing the landed `prim_map::TABLE` pattern) covering the bare and
    /// 2-segment qualified/associated-fn call-target shapes. A future call-shape leaf adds one
    /// file + one append-only `TABLE` row there, never touching this method. The remaining two
    /// shapes (a 3+-segment qualified path; a non-path call target) are not additive leaf
    /// targets today (DN-133 §2 sub-kind 3 routes the former through the Import/symtab resolver
    /// instead) — a table miss falls through to them unchanged, then to the guard/emit tail
    /// below, identical to the pre-refactor `match`'s own fallback shape (G2).
    fn visit_call(&mut self, _expr: &Expr, c: &syn::ExprCall) -> Self::Output {
        let func =
            match calls::lookup(c) {
                Some(handler) => (handler.resolve)(c, self.self_ty)?,
                None => match &*c.func {
                    Expr::Path(p) if p.qself.is_none() => {
                        // Any OTHER qualified path shape this arm does not (yet) resolve: a
                        // cross-*module* free-function path (`a::b::c()`, e.g.
                        // `mycelium_std_sys::time::mono_nanos()`, 3+ segments) routes through the
                        // Import/symtab free-fn resolver (M-1084's `use`-driven resolution), not this
                        // call-target path — out of DN-133's scope (§2 sub-kind 3). Mirroring
                        // `map::map_type`'s identical qualified-path decision, this stays an explicit
                        // gap rather than a fabricated call (G2/DN-34 §4).
                        return Err(GapReason::new(
                            Category::Other,
                            format!(
                            "qualified/associated-function call `{}` — no established Mycelium \
                             surface form for a Rust conversion-op body; emitting the bare \
                             last-segment name would fabricate a call (e.g. `from(...)` is not a \
                             Mycelium builtin)",
                            tokens_to_string(&*c.func)
                        ),
                        ));
                    }
                    _ => return Err(GapReason::new(
                        Category::Other,
                        "call target is not a simple path (e.g. a closure call) — no confirmed \
                         mapping",
                    )),
                },
            };
        // M-1001: a call to a function whose name is a reserved word (e.g. a Rust `.swap()`
        // method or a `to(..)` helper) would emit un-parseable text; gap it (VR-5/G2).
        let func = resolve_surface_ident(&func, "call target")?;
        // ORACLE-R1 A5: when the callee is a same-file mangled inherent assoc fn whose params
        // are known Binary{N}, rewrite bare decimal lit args to equal-width BinLit (so
        // `MonoInstant::from_nanos(0)` / `WallInstant::from_nanos_since_epoch(0)` in a hand-
        // written `Default` → `Init` body never Q6-poison the file after Show is clean).
        let param_widths = local_mangled_param_binary_widths(&func);
        let mut args = Vec::with_capacity(c.args.len());
        for (i, a) in c.args.iter().enumerate() {
            if let Some(w) = param_widths
                .as_ref()
                .and_then(|ws| ws.get(i).copied().flatten())
            {
                if let Some(bin) = int_lit_as_bin_literal(a, w) {
                    args.push(bin);
                    continue;
                }
            }
            args.push(emit_expr(a, self.self_ty, self.env)?);
        }
        Ok(format!("{func}({})", args.join(", ")))
    }

    fn visit_method_call(&mut self, _expr: &Expr, m: &syn::ExprMethodCall) -> Self::Output {
        // DN-135/M-1092 — the Result/Option combinator-directed match-inline (Alt A). Consulted
        // FIRST (before the `prim_map` forward-map and the generic desugar below), gated on a
        // CONFIRMED Result/Option receiver (never a guess — VR-5, the same no-guess discipline
        // `prim_map::receiver_gate_matches` uses for its own rows). `None` means "not applicable"
        // — falls straight through to the unchanged code below, exactly as if this pass did not
        // exist; see `try_inline_result_option_combinator`'s own doc for the full decline set.
        if let Some(result) = try_inline_result_option_combinator(m, self.self_ty, self.env) {
            return result;
        }
        // trx2 Lane C Deliverable 2 — forward-mapped kernel prim surface (`crate::prim_map`).
        // Consulted BEFORE the generic desugar below so a confirmed row wins; gated on the
        // receiver's *known* type (never a guess — VR-5) so an unrelated Rust type's
        // same-named method never triggers a wrong/misleading mapping. A row whose gate
        // doesn't match (receiver type unknown or doesn't match) falls straight through to the
        // unchanged generic desugar, exactly as if no row existed.
        let method_name = m.method.to_string();
        if let Some(row) = crate::prim_map::lookup(&method_name) {
            let receiver_ty = expr_env_type(&m.receiver, self.env);
            if crate::prim_map::receiver_gate_matches(row.receiver_gate, receiver_ty.as_deref()) {
                if !row.wired {
                    // PENDING-BACKEND: the mapping is known (a decided ruling — see
                    // `crate::prim_map` module docs for each row's citation) but the kernel/
                    // grammar backend is not landed — always an explicit gap, NEVER an
                    // emission (VR-5/G2: a forward-declared mapping is documentation, not a
                    // fabricated success).
                    return Err(GapReason::new(
                        row.pending_category,
                        format!(
                            "PENDING-BACKEND({}): {} forward-mapped, backend unwired — gated \
                             off (VR-5/G2). {}",
                            row.slug, row.myc_prim, row.citation
                        ),
                    ));
                }
                let recv = emit_expr(&m.receiver, self.self_ty, self.env)?;
                let mut args = vec![recv];
                for a in &m.args {
                    args.push(emit_expr(a, self.self_ty, self.env)?);
                }
                let call = format!("{}({})", row.myc_prim, args.join(", "));
                return Ok(if row.bridge_binary1_to_bool {
                    // The prim's own return is `Binary{1}`; Rust's method returns `bool` ->
                    // bridge to `Bool` the same proven way `Expr::Binary`'s `!=`/`>` composition
                    // does (see that arm's doc) — a bare call would fail `myc check`'s
                    // `Binary{1}` vs `Bool` mismatch (confirmed empirically).
                    format!("(match {call} {{ 0b1 => True, _ => False }})")
                } else {
                    call
                });
            }
        }
        // M-1037 — `.ne(other)` is `PartialEq::ne`; bare `ne(recv, other)` fabricates the same
        // non-prim `lib/std/cmp.myc` surface that `!=` already avoids (see `visit_binary`'s
        // composed-`eq` arm). When both operands resolve to a known `Binary{N}` (or Binary +
        // equal-width lit — ONESHOT C3), emit that faithful composition; Bool uses the inverted
        // `bool_eq` match. Otherwise gap never-silently (VR-5/G2).
        if method_name == "ne" && m.args.len() == 1 {
            let recv_w = expr_env_binary_width(&m.receiver, self.env);
            let arg_w = expr_env_binary_width(&m.args[0], self.env);
            let both_known_binary = recv_w.is_some() && arg_w.is_some();
            let both_known_signed_binary = expr_env_signed_binary_width(&m.receiver, self.env)
                .is_some()
                && expr_env_signed_binary_width(&m.args[0], self.env).is_some();
            let both_bool = expr_env_type(&m.receiver, self.env).as_deref() == Some("Bool")
                && expr_env_type(&m.args[0], self.env).as_deref() == Some("Bool");
            let arg_as_bin = recv_w.and_then(|w| int_lit_as_bin_literal(&m.args[0], w));
            let recv_as_bin = arg_w.and_then(|w| int_lit_as_bin_literal(&m.receiver, w));
            let lit_ok = arg_as_bin.is_some() || recv_as_bin.is_some();
            if both_bool || both_known_binary || both_known_signed_binary || lit_ok {
                let lhs = if let Some(bin) = recv_as_bin {
                    bin
                } else {
                    emit_expr(&m.receiver, self.self_ty, self.env)?
                };
                let rhs = if let Some(bin) = arg_as_bin {
                    bin
                } else {
                    emit_expr(&m.args[0], self.self_ty, self.env)?
                };
                if both_bool {
                    return Ok(format!(
                        "match ({lhs}) {{ True => match ({rhs}) {{ True => False, False => True }}, \
                         False => ({rhs}) }}"
                    ));
                }
                return Ok(format!(
                    "(match eq({lhs}, {rhs}) {{ 0b1 => False, _ => True }})"
                ));
            }
            return Err(GapReason::new(
                Category::Other,
                "Rust `.ne()` method has no faithful bare `ne(recv, arg)` referent (same \
                 class as `!=` — cmp.myc's `ne` is not a bare-call prim); operands did not \
                 both resolve to a known `Binary{N}` for the composed `eq` lowering, so it \
                 is gapped rather than fabricated (M-1037, G2/VR-5)",
            ));
        }
        // M-1037 — atomics / unmapped std methods that must never desugar to fabricated bare calls.
        if matches!(
            method_name.as_str(),
            "fetch_add" | "fetch_sub" | "fetch_and" | "fetch_or" | "fetch_xor"
        ) {
            return Err(GapReason::new(
                Category::Other,
                format!(
                    "PENDING-BACKEND(CU-8): atomic `.{method_name}()` needs a memory-model RFC \
                     surface — desugaring to `{method_name}(recv, …)` would fabricate an unknown \
                     prim (M-1037, G2/VR-5)"
                ),
            ));
        }
        if method_name == "contains" {
            return Err(GapReason::new(
                Category::Other,
                "Rust `.contains()` has no verified bare-call kernel prim mapping in this \
                 pipeline; desugaring to `contains(recv, …)` would fabricate an unknown prim — \
                 gapped never-silently (M-1037, G2/VR-5)",
            ));
        }
        if method_name.starts_with("saturating_") {
            return Err(GapReason::new(
                Category::Other,
                format!(
                    "Rust `.{method_name}` is saturating (silent clamp) — Mycelium has no \
                     saturating prim surface; desugaring to `{method_name}(…)` would fabricate \
                     an unknown prim or lie about overflow (G2/VR-5). Gap never-silently \
                     (express gap-close 2026-07-16)."
                ),
            ));
        }
        // A Rust **ownership/identity-conversion no-op method** (`ToOwned::to_owned`,
        // `Clone::clone`, `ToString::to_string`, `Into::into`, `AsRef`/`Borrow` accessors, …)
        // has NO Mycelium free-function or prim referent: Mycelium is value-semantic (ADR-003),
        // so these are either identity or an unmapped conversion — desugaring `recv.to_owned()`
        // to a bare `to_owned(recv)` FABRICATES a call to a non-existent prim (`myc check`:
        // `unknown function/constructor/prim to_owned`), which is exactly the never-silent
        // violation the house rules forbid (G2/VR-5). Gap it explicitly instead of emitting a
        // check-failing surface. Methods with a `prim_map` identity row fire above when their
        // receiver gate matches; this arm is the never-silent residual (gate miss, user type,
        // or deliberately withheld conversion — `into`/`to_vec`/non-Bytes `to_string`).
        // M-1037 residual: per-method EXPLAIN via [`conversion_gap_reason`].
        if is_unmappable_conversion_method(&method_name) {
            return Err(GapReason::new(
                Category::Other,
                conversion_gap_reason(&method_name),
            ));
        }
        // rotate_left/right: no kernel prim (FLAG-math-3); compose from bin.shl/bin.shr when
        // the receiver maps to Binary{N} and the shift arg is emit-able — never fabricate
        // `rotate_left` (G2; express gap-close 2026-07-16, unblocks std-rand rotl64).
        if method_name == "rotate_left" || method_name == "rotate_right" {
            let recv = emit_expr(&m.receiver, self.self_ty, self.env)?;
            if m.args.len() != 1 {
                return Err(GapReason::new(
                    Category::Other,
                    format!(
                        "`.{method_name}(k)` expects exactly one shift arg; got {}",
                        m.args.len()
                    ),
                ));
            }
            let k = emit_expr(&m.args[0], self.self_ty, self.env)?;
            let width = expr_env_type(&m.receiver, self.env)
                .and_then(|t| binary_width(&t))
                .or_else(|| self.self_ty.and_then(binary_width));
            let Some(n) = width else {
                return Err(GapReason::new(
                    Category::Other,
                    format!(
                        "`.{method_name}` composition needs a known Binary{{N}} receiver \
                         width (TypeEnv); gapped rather than fabricating `rotate_left` \
                         (FLAG-math-3 / G2)"
                    ),
                ));
            };
            let n_lit = zero_bin_literal(n);
            // Surface prims are `shl_u`/`shr_u`/`sub_u` (checkty), not bare `shl`/`shr`.
            let k_n = format!("width_cast({k}, {n_lit})");
            let n_minus_k = format!("sub_u({n_lit}, {k_n})");
            let body = if method_name == "rotate_left" {
                format!("or(shl_u({recv}, {k_n}), shr_u({recv}, {n_minus_k}))")
            } else {
                format!("or(shr_u({recv}, {k_n}), shl_u({recv}, {n_minus_k}))")
            };
            return Ok(body);
        }
        // Declared mapping decision: the grammar's `app_expr` has no postfix method-call
        // form (`primary ('(' args? ')')*` only) — desugar `recv.method(args)` to
        // `method(recv, args...)`, matching how `lib/std/cmp.myc`'s free functions
        // (`cmp`/`le`/`ge`/...) take the receiver as an ordinary first argument.
        //
        // D4 / express gap-close: if the receiver's TypeEnv type has a locally mangled
        // inherent method (always true for same-file inherent emits after 2026-07-16),
        // call the mangled name so declaration/call stay in sync.
        //
        // **G-β Rank A (never-fabricate free-fn method-call, 2026-07-17):** only emit the
        // bare/mangled free-fn form when that exact name is a **proven** same-file emission
        // ([`local_mangled_assoc_fn_known`] / [`bare_fn_name_taken`]) — the same resolution
        // discipline DN-133 applies to qualified `Type::method` calls. Emitting an
        // unregistered bare name (e.g. `read_to_end(src)` after `Source::read_to_end`'s body
        // gapped on `.to_vec()`) file-poisons `myc-check` with
        // `unknown function/constructor/prim read_to_end` and is exactly the G2/VR-5
        // fabrication the house rules forbid. Gap the call site honestly instead (the free
        // fn may then fail to emit rather than emit-and-poison the whole file).
        let rust_method = method_name.clone();
        let mut method_name = resolve_surface_ident(&method_name, "method call")?;
        let mut known_emitted = false;
        if let Some(recv_ty) = expr_env_type(&m.receiver, self.env) {
            let mangled = mangled_inherent_fn_name(&recv_ty, &method_name);
            if local_mangled_assoc_fn_known(&mangled) {
                method_name = mangled;
                known_emitted = true;
            }
        } else if let Some(st) = self.self_ty {
            let mangled = mangled_inherent_fn_name(st, &method_name);
            if local_mangled_assoc_fn_known(&mangled) {
                method_name = mangled;
                known_emitted = true;
            }
        }
        if !known_emitted {
            // Bare un-mangled inherent method (first claim of a self-receiver) is recorded
            // under its bare name — see `emit_impl`'s success path.
            known_emitted =
                local_mangled_assoc_fn_known(&method_name) || bare_fn_name_taken(&method_name);
        }
        if !known_emitted {
            return Err(GapReason::new(
                Category::Other,
                format!(
                    "method call `.{rust_method}(...)` has no proven-emitted free-fn referent in \
                     this file (neither a D4-mangled inherent `Type__{rust_method}` nor a bare \
                     inherent recorded by this file's left-to-right pass, nor a prim_map / \
                     combinator row). Emitting bare `{rust_method}(recv, …)` would fabricate an \
                     unknown prim and file-poison myc-check (G-β Rank A / G2/VR-5; residual when \
                     the method body itself gapped — e.g. M-1037 `.to_vec()` inside \
                     `read_to_end` — so the declaration was never registered)"
                ),
            ));
        }
        let recv = emit_expr(&m.receiver, self.self_ty, self.env)?;
        let mut args = vec![recv];
        for a in &m.args {
            args.push(emit_expr(a, self.self_ty, self.env)?);
        }
        Ok(format!("{method_name}({})", args.join(", ")))
    }

    fn visit_macro(&mut self, _expr: &Expr, m: &syn::ExprMacro) -> Self::Output {
        if m.mac.path.is_ident("format") || m.mac.path.is_ident("write") {
            macros::try_lower_expr_macro(&m.mac, self.self_ty, self.env)
        } else {
            Err(GapReason::new(
                Category::MacroInvocation,
                format!(
                    "expression-position macro `{}` — no macro system in this grammar fragment \
                     (`write!`/`format!` lower to pure `Bytes` per DN-127/M-1090 WU-3)",
                    m.mac
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident.to_string())
                        .unwrap_or_default()
                ),
            ))
        }
    }

    fn visit_paren(&mut self, _expr: &Expr, p: &syn::ExprParen) -> Self::Output {
        Ok(format!("({})", emit_expr(&p.expr, self.self_ty, self.env)?))
    }

    fn visit_reference(&mut self, _expr: &Expr, r: &syn::ExprReference) -> Self::Output {
        // Declared simplification: Mycelium is value-semantic (ADR-003) with no reference
        // type in this grammar fragment — `&expr`/`&mut expr` is treated as
        // reference-transparent and erased to its inner expression.
        emit_expr(&r.expr, self.self_ty, self.env)
    }

    fn visit_tuple(&mut self, _expr: &Expr, t: &syn::ExprTuple) -> Self::Output {
        if t.elems.len() >= 2 {
            let mut parts = Vec::with_capacity(t.elems.len());
            for e in &t.elems {
                parts.push(emit_expr(e, self.self_ty, self.env)?);
            }
            Ok(format!("({})", parts.join(", ")))
        } else if t.elems.is_empty() {
            Err(GapReason::new(
                Category::Other,
                "unit value `()` has no Mycelium literal",
            ))
        } else {
            Err(GapReason::new(
                Category::Other,
                "single-element tuple `(x,)` has no Mycelium equivalent (tuple type requires arity \
                 >= 2, M-826)",
            ))
        }
    }

    fn visit_array(&mut self, _expr: &Expr, a: &syn::ExprArray) -> Self::Output {
        // An explicit-element array `[e1, e2, …]` maps to a Mycelium `ListLit` (grammar line 415:
        // `ListLit ::= '[' (expr (',' expr)*)? ']'`, constructs a `Seq{T, N}` — RFC-0032 D3, the
        // `Seq`/`Vec` list-literal surface ratified in RFC-0040 §Vec-List-Literal-Desugaring). An
        // empty `[]` is a valid empty ListLit. Each element recurses through the guarded
        // `emit_expr`, so a non-expressible element gaps the whole array (never a partial list).
        let mut elems = Vec::with_capacity(a.elems.len());
        for e in &a.elems {
            elems.push(emit_expr(e, self.self_ty, self.env)?);
        }
        Ok(format!("[{}]", elems.join(", ")))
    }

    fn visit_repeat(&mut self, _expr: &Expr, _r: &syn::ExprRepeat) -> Self::Output {
        // An array-repeat `[x; N]` has no Mycelium surface: `ListLit` (grammar line 415) enumerates
        // its elements and carries no repeat/count form — so this is an explicit, cited gap rather
        // than a fabricated expansion (which would also require evaluating `N`).
        Err(GapReason::new(
            Category::Other,
            "array-repeat expression `[x; N]` has no Mycelium equivalent — `ListLit ::= '[' (expr \
             (',' expr)*)? ']'` (grammar line 415) enumerates its elements and has no repeat form",
        ))
    }

    fn visit_block(&mut self, expr: &Expr, b: &syn::ExprBlock) -> Self::Output {
        if b.label.is_none() {
            emit_block_as_expr(&b.block, self.self_ty, self.env)
        } else {
            self.fallback(expr)
        }
    }

    // M-1006 Lever 1 — field projection `self.<field>`. The grammar has NO projection surface
    // (`path ::= Ident ('.' Ident)*` is a namespace glyph; `self.0` cannot even lex), but reading
    // one field of a single-constructor product has a faithful equivalent: a `match` that binds
    // exactly that field. Only `self` has a statically-known type here (the impl's `self_ty` — the
    // transpiler tracks no other local types), so only `self.<field>` desugars; any other base
    // gaps. Gated (via `struct_layout`) on `self_ty` being an *emitted* in-file struct so the
    // `Ty(...)` constructor the `match` names actually exists (never poison the file's check).
    fn visit_field(&mut self, _expr: &Expr, fe: &syn::ExprField) -> Self::Output {
        let base_is_self = matches!(
            &*fe.base,
            Expr::Path(p) if p.qself.is_none() && p.path.is_ident("self")
        );
        if !base_is_self {
            return Err(GapReason::new(
                Category::Other,
                "field access on a non-`self` base — the transpiler tracks no local types, so \
                 the projection cannot be resolved to a constructor position (only \
                 `self.<field>` desugars to a `match`)",
            ));
        }
        let sty = self.self_ty.ok_or_else(|| {
            GapReason::new(
                Category::Other,
                "`self` field access with no enclosing impl/trait `self` type",
            )
        })?;
        let layout = struct_layout(sty).ok_or_else(|| {
            GapReason::new(
                Category::Other,
                format!(
                    "field projection `self.{}` on `{sty}` — not an in-file single-ctor struct \
                     that emits (an enum / external / non-resolvable type has no constructor to \
                     `match` against)",
                    member_text(&fe.member)
                ),
            )
        })?;
        let pos = match &fe.member {
            syn::Member::Named(id) => {
                let n = id.to_string();
                layout.iter().position(|f| f.as_deref() == Some(n.as_str()))
            }
            syn::Member::Unnamed(idx) => {
                let i = idx.index as usize;
                (i < layout.len()).then_some(i)
            }
        }
        .ok_or_else(|| {
            GapReason::new(
                Category::Other,
                format!(
                    "field `{}` not found on struct `{sty}`",
                    member_text(&fe.member)
                ),
            )
        })?;
        // Bind the accessed position to `p{pos}` (a guaranteed-valid, non-reserved ident),
        // wildcard the rest, and return the binding. Parenthesized so it composes as a binary /
        // application operand subexpression. (DN-125/M-1081: factored into
        // `field_projection_text` so `reconstruct_positional`'s OTHER-fields read uses the exact
        // same projection text this arm emits for a direct `self.<field>` read.)
        Ok(field_projection_text(sty, &layout, "self", pos))
    }

    // M-1006 Lever 1 — struct-literal construction `Ty { a: x, b: y }` / `Self { .. }` -> the
    // positional constructor call `Ty(x, y)` (arguments ordered by the struct's declaration
    // order). Gated on `Ty` being an emitted in-file struct. `..rest` (struct-update) and a
    // partial literal have no Mycelium surface -> explicit gap (never a fabricated field).
    //
    // DN-134 SS3 (M-1093, coordinated with M-1089's pattern-side twin): `sty` resolves *exactly*
    // the same way whether it names a plain in-file struct or — since `struct_layouts`
    // (`transpile.rs`) now also walks `Item::Enum` `Fields::Named` variants, collision-safe by
    // construction — an in-file enum's named-field STRUCT-VARIANT (`TimeErr::ClockUnavailable {
    // reason }`, `Self::Variant { .. }`). `struct_layout` cannot tell the two apart (bare-ctor-name
    // resolution only, no qualifier threading — see that fn's doc), and this arm doesn't need to:
    // the enum emitter already lowers a `Fields::Named` variant to the identical positional `Ctor`
    // surface a struct gets (`emit_enum`'s struct-variant arm, `emit.rs:3113` at the time of
    // writing), so ONE field-resolution loop below serves both — "no change to the loop itself"
    // (DN-134 SS3 step 2). The three bounds DN-134 §4 names for the construction side specifically
    // (as opposed to M-1089's pattern side, which faces none of them):
    // - **Cross-nodule resolvability (OQ-2):** `struct_layout`/`resolvable` are per-file — a
    //   variant declared in another file/nodule (e.g. `std-sys-host`'s own `TimeErr`, imported
    //   from `std.time`) is simply absent from `items` here, so it gaps via the same "not an
    //   in-file ... that emits" refusal below as any unresolved foreign struct — never a
    //   fabricated out-of-file reference (G2). Clean on the real port path once the nodule
    //   actually contains/imports the type (DN-113's cross-nodule resolution, out of this
    //   file-scoped transpiler's reach today).
    // - **DN-104 construction seal (OQ-3(b)):** a per-constructor `priv` seal is a Mycelium-side
    //   annotation this Rust->`.myc` transpiler never reads (Rust has no equivalent per-ctor
    //   visibility marker to translate FROM, and this transpiler never emits `priv` on anything it
    //   produces — `reserved.rs`'s `"priv"` entry is only a keyword-collision guard, not a
    //   seal-tracking mechanism). So there is no first-class "sealed ctor" signal to check here;
    //   the seal's construction-side enforcement is, today, entirely SUBSUMED by the cross-nodule
    //   bound above: a same-file variant construction is trivially "at home" (there is no
    //   smaller-than-file nodule boundary in this architecture), and a cross-file one already gaps
    //   unconditionally — so "constructing a sealed ctor from outside its home nodule" cannot
    //   arise as a DISTINCT reachable case through this transpiler; it is held, not built, and
    //   reported as such (VR-5 — no fabricated enforcement of a signal that isn't there).
    // - **Same-name struct/variant collision (the correctness mandate, DN-134 §4 stress-#8):**
    //   enforced entirely at the `struct_layouts` population (never a silently-shadowed `layouts`
    //   entry) — this arm just sees `struct_layout` return `None` for an ambiguous name, exactly
    //   like any other unresolved ctor.
    fn visit_struct(&mut self, expr: &Expr, se: &syn::ExprStruct) -> Self::Output {
        if se.qself.is_some() {
            return self.fallback(expr);
        }
        if se.rest.is_some() {
            return Err(GapReason::new(
                Category::Other,
                "struct-update syntax `..rest` has no Mycelium equivalent (no record-update \
                 surface)",
            ));
        }
        let seg = se
            .path
            .segments
            .last()
            .ok_or_else(|| GapReason::new(Category::Other, "empty struct-literal path"))?;
        let raw = seg.ident.to_string();
        let sty = if raw == "Self" {
            self.self_ty
                .ok_or_else(|| {
                    GapReason::new(
                        Category::Other,
                        "`Self { .. }` with no enclosing impl/trait `self` type",
                    )
                })?
                .to_string()
        } else {
            raw
        };
        let layout = struct_layout(&sty).ok_or_else(|| {
            GapReason::new(
                Category::Other,
                format!(
                    "struct literal `{sty} {{ .. }}` — not an in-file single-ctor struct or \
                     enum struct-variant that emits (no constructor to build; a cross-nodule \
                     variant is an honest DN-113/DN-134-OQ-2 resolvability gap, never a \
                     fabricated out-of-file reference — VR-5/G2)"
                ),
            )
        })?;
        // Single pass over the WRITTEN fields (mirrors `map_struct_pattern`'s DN-132 SS5.2 loop,
        // the pattern-side twin): resolves each field to its declaration position, catching a
        // **duplicate** field-value binding (never-silent, DN-134 SS3 step 3) as it goes, then
        // requires every layout position be filled exactly once — an unfilled position is a
        // **missing** field (VR-5, pre-existing check) and a written field matching no position is
        // an **extra/unknown** field (new, DN-134 SS3 step 3 — previously silently ignored: a
        // `Foo { a: 1, b: 2, bogus: 3 }` against a two-field layout would drop `bogus` unnoticed).
        let mut args: Vec<Option<String>> = vec![None; layout.len()];
        let mut seen_members: HashSet<String> = HashSet::new();
        for fv in &se.fields {
            let member_key = member_text(&fv.member);
            if !seen_members.insert(member_key.clone()) {
                return Err(GapReason::new(
                    Category::Other,
                    format!(
                        "struct literal `{sty}` names field `{member_key}` more than once — a \
                         duplicate field-value binding has no faithful Mycelium construction \
                         (VR-5/G2)"
                    ),
                ));
            }
            let pos = match &fv.member {
                syn::Member::Named(id) => {
                    let n = id.to_string();
                    layout
                        .iter()
                        .position(|slot| slot.as_deref() == Some(n.as_str()))
                }
                syn::Member::Unnamed(idx) => {
                    let i = idx.index as usize;
                    (i < layout.len() && layout[i].is_none()).then_some(i)
                }
            }
            .ok_or_else(|| {
                GapReason::new(
                    Category::Other,
                    format!(
                        "struct literal `{sty}` names field `{member_key}`, which is not a \
                         declared field of `{sty}`'s confirmed layout — an extra/unknown field \
                         is never silently dropped (VR-5/G2)"
                    ),
                )
            })?;
            args[pos] = Some(emit_expr(&fv.expr, self.self_ty, self.env)?);
        }
        let mut resolved = Vec::with_capacity(args.len());
        for (i, slot) in args.into_iter().enumerate() {
            resolved.push(slot.ok_or_else(|| {
                GapReason::new(
                    Category::Other,
                    format!(
                        "struct literal `{sty}` gives no value for the field at position \
                         {i} — a partial constructor has no Mycelium surface (VR-5)"
                    ),
                )
            })?);
        }
        Ok(format!("{sty}({})", resolved.join(", ")))
    }

    // A Rust `as` cast (`syn::Expr::Cast`). Rust `as` is **lossy / wrapping / saturating /
    // rounding by design**; Mycelium's conversion prims are **checked / refusing by design**, so
    // fidelity — not opportunistic emission — governs this arm: a checked prim is emitted **only**
    // where it matches Rust's `as` semantics *exactly*, and every other cast is a never-silent gap
    // rather than an unfaithful emission (G2/VR-5; trx2 A1, DN-34 §8.18).
    //
    // The one decidable-faithful slice is **`Binary{N} as Binary{M}` widening/identity** (`M >=
    // N`): DN-41's `bit.width_cast` zero-extends on the MSB side (verified `prim_width_cast`,
    // `mycelium-interp/src/prims.rs`), and `Binary` is sign-free unsigned magnitude (ADR-028), so
    // that exactly matches Rust's unsigned widening/identity. **Narrowing** (`M < N`) was NOT
    // faithful with `width_cast` alone: Rust `as` narrowing **wraps** (keeps the low `M` bits), but
    // `width_cast` **refuses** (`EvalError::Overflow`) on any set dropped high bit — a *checked*
    // narrow, not a wrapping one. DN-51 (**Accepted**) *names* the faithful wrapping form — an
    // explicit `truncate` op, "unconditionally drops the high `N - M` bits… total but lossy"
    // (DN-51 §2 D3) — and it is now **landed** (maintainer-authorized DN-39 post-freeze promotion:
    // `bit.truncate` registered in `crates/mycelium-core/src/prim.rs`'s Π table, implemented in
    // `crates/mycelium-interp/src/prims.rs::prim_truncate`, surfaced in
    // `crates/mycelium-l1/src/checkty.rs`). So a narrow now emits `truncate` — it matches Rust `as`
    // narrowing's wrap semantics *exactly* (DN-51 §2 D3: unconditional low-`M`-bits keep, never a
    // refusal), the same fidelity bar the widen/identity arm above already meets.
    // Any **float-crossing** cast (`Binary{N} as Float`, `Float as Binary{N}`, `Float as Float`)
    // is CU-3 territory: the CU-3 kernel prims are checked/refusing where Rust `as` rounds/
    // saturates (`flt.to_bin` refuses out-of-range vs Rust's saturation; `bin.to_flt` errs
    // `|n| > 2^53` vs Rust's rounding — ADR-040 §2.4), so the faithful form is the reified **lossy
    // swap** (ADR-040 §2.4/§5, explicitly *not* a prim), which the transpiler cannot emit yet — an
    // explicit `PENDING-DESIGN(CU-3-fidelity)` gap (`prim_map.rs` §CU-3 records the same exclusion:
    // no confirmed prim name, `as` has no `Call`/`MethodCall` shape to key on).
    fn visit_cast(&mut self, expr: &Expr, c: &syn::ExprCast) -> Self::Output {
        // The operand's Mycelium type — decidable for a bare in-scope identifier or a
        // structurally-transparent `(e)`/`&e` wrapper around one; `None` for a call/field/
        // literal/etc. (see `expr_env_type`'s doc — D3 operand-type-inference depth, DN-34
        // §8.16 residual, including why a suffixed literal was tried and rejected there).
        let operand_ty = expr_env_type(&c.expr, self.env);
        let operand_is_float = operand_ty.as_deref() == Some("Float");
        let operand_width = operand_ty.as_deref().and_then(binary_width);
        // The target's width iff it is an *unsigned* integer (`u8..u128` -> `Binary{M}`); signed /
        // platform-width / non-int targets yield `None` here (their own `map_type` gap is not
        // surfaced — the fidelity dispatch below produces the honest, cast-specific reason instead).
        let target_width = map_type(&c.ty, self.self_ty)
            .ok()
            .as_deref()
            .and_then(binary_width);
        // A float on *either* side (target `f32`/`f64` at the syn level, or a `Float` operand) makes
        // this a CU-3 float-crossing cast regardless of the other side's mapping.
        let target_is_float = type_is_float(&c.ty);

        if target_is_float || operand_is_float {
            // CU-3: no faithful prim — the lossy swap is the correct form and is not emittable yet.
            Err(GapReason::new(
                Category::Other,
                format!(
                    "PENDING-DESIGN(CU-3-fidelity): cast `{}` crosses the Binary/Float boundary — \
                     Rust `as` is lossy here (float->int *saturates*, int->float *rounds*), but the \
                     CU-3 kernel prims are checked/refusing (`flt.to_bin` refuses out-of-range; \
                     `bin.to_flt` errs |n| > 2^53 — ADR-040 §2.4), so no faithful prim exists. The \
                     faithful form is the reified lossy swap (ADR-040 §2.4/§5, NOT a prim), which \
                     the transpiler cannot emit yet — explicit gap (G2/VR-5)",
                    tokens_to_string(expr)
                ),
            ))
        } else if operand_ty.is_none() {
            // Operand type unknown — never guess it (VR-5).
            Err(GapReason::new(
                Category::Other,
                format!(
                    "cast `{}` — operand type unknown: `as` fidelity requires a known operand type, \
                     but the operand is not a bare in-scope identifier (or a `(..)`/`&..` wrapper \
                     around one) whose type this transpiler can resolve without guessing (no \
                     general expression-typing pass; VR-5)",
                    tokens_to_string(expr)
                ),
            ))
        } else if let (Some(n), Some(m)) = (operand_width, target_width) {
            // `Binary{N} as Binary{M}` — the decidable int->int slice.
            if m >= n {
                // Widen / identity: `width_cast` zero-extends (unsigned), matching Rust exactly.
                // Faithful + `myc check`-clean (DN-41 §3; reuses the `try_width_cast_widen_body`
                // witness form `width_cast(<value>, <M-bit zero BinLit>)`).
                let operand = emit_expr(&c.expr, self.self_ty, self.env)?;
                Ok(format!("width_cast({operand}, {})", zero_bin_literal(m)))
            } else {
                // Narrow: Rust wraps (low `M` bits); `truncate` unconditionally keeps the low `M`
                // bits (DN-51 §2 D3) — an exact semantic match, now landed (maintainer-authorized
                // DN-39 post-freeze promotion). Reuses the same width-witness ABI `width_cast`
                // uses (`zero_bin_literal(m)` — only the witness's width is read, its bits are
                // ignored, DN-41 §3), since `truncate` was built as `width_cast`'s sibling.
                let operand = emit_expr(&c.expr, self.self_ty, self.env)?;
                Ok(format!("truncate({operand}, {})", zero_bin_literal(m)))
            }
        } else {
            // Operand known but not `Binary{N}` (e.g. `Bool`, a user type), or the target is not an
            // unsigned-int `Binary{M}` (signed int / pointer / user type) and no float is involved.
            // No faithful, decidable cast form — explicit gap rather than a guess (VR-5).
            Err(GapReason::new(
                Category::Other,
                format!(
                    "cast `{}` has no faithful, decidable Mycelium form — the operand is not a known \
                     `Binary{{N}}` value and/or the target is not an unsigned `Binary{{M}}` integer \
                     (signed integers, pointers, and user types have no confirmed `as`-cast surface); \
                     left an explicit gap rather than a guessed conversion (VR-5)",
                    tokens_to_string(expr)
                ),
            ))
        }
    }

    /// DN-118 Phase 1 (the closure-EMIT pass). `syn::ExprClosure` (`|a, b| …`) has **no**
    /// arm here before this method — defunctionalization of an env-capturing closure is *already*
    /// done in the LANGUAGE (RFC-0024 §4A, M-704 `done`: `mono.rs`'s `ClosureSpecialization` lowers
    /// every escaping closure to a per-arrow `Fn$A$B` tag-sum + an `apply$A$B` dispatcher,
    /// whole-program, at `finish()`), so this transpiler does **not** build its own defunctionalizer
    /// — that would duplicate mono and re-hit the exact synthetic-`Env` limitation a *different*,
    /// unrelated mechanism (`elaborate_lower_rule`'s ad-hoc single-function `Env`, used only for
    /// `lower`-rule RHS elaboration) already hit (`crate::tests::facility_stage1_hygiene`
    /// fixture-4 doc, `apply$Fn$…` unresolved there — NOT a general `myc check`/`myc run`
    /// limitation; DN-118 Phase 0 verify-first reproduced the language side end-to-end clean: a
    /// whole-program `nodule` with a `lambda` capturing an outer `let`-binder both `myc
    /// check`-clean and runs to the expected value). This method instead **emits the Mycelium
    /// `lambda` surface** (`lambda_expr ::= 'lambda' '(' params? ')' '=>' expr`) and leaves the
    /// captured names as ordinary in-scope references in the body — mono resolves the whole
    /// program's capture set itself; this emitter never synthesizes an env record.
    fn visit_closure(&mut self, _expr: &Expr, c: &syn::ExprClosure) -> Self::Output {
        // `async`/`const`/`static` (movable) closures have no Mycelium `lambda` correspondence —
        // `lambda_expr` is plain, synchronous, and always moves its captures by value (there is no
        // reference type in this grammar fragment, ADR-003).
        if c.asyncness.is_some() || c.constness.is_some() || c.movability.is_some() {
            return Err(GapReason::new(
                Category::Closure,
                "an `async`/`const`/`static` closure has no Mycelium `lambda` equivalent \
                 (`lambda_expr` is plain and synchronous; RFC-0037 D5)",
            ));
        }

        // Params: each must be a simple, EXPLICITLY-typed identifier (`x: T`) — Mycelium's
        // `lambda_expr`'s `params` production is exactly `Ident ':' type_ref` (mirroring
        // `fn_item`'s own `param`), and this transpiler has no type-inference pass to recover an
        // omitted Rust closure-param type (most Rust closures infer their param types from usage —
        // VR-5: absence, never a guess).
        let mut params: Vec<(String, String)> = Vec::with_capacity(c.inputs.len());
        for pat in &c.inputs {
            let Pat::Type(pt) = pat else {
                return Err(GapReason::new(
                    Category::Closure,
                    format!(
                        "closure parameter `{}` has no explicit type annotation — Mycelium's \
                         `lambda` parameters are always `name: Type` (grammar `lambda_expr` / \
                         `param`) and this transpiler has no type-inference pass to recover an \
                         omitted Rust closure-param type",
                        tokens_to_string(pat)
                    ),
                ));
            };
            let name = match &*pt.pat {
                Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => {
                    pi.ident.to_string()
                }
                _ => {
                    return Err(GapReason::new(
                        Category::Closure,
                        "non-identifier closure-parameter pattern (destructuring) has no \
                         `param ::= Ident ':' type_ref` equivalent",
                    ))
                }
            };
            let name = resolve_surface_ident(&name, "closure parameter")?;
            let ty = map_type(&pt.ty, self.self_ty)?;
            params.push((name, ty));
        }
        if params.is_empty() {
            return Err(GapReason::new(
                Category::Closure,
                "a zero-parameter closure has no v0 `lambda` form (grammar note on \
                 `lambda_expr` — a never-silent refusal, G2)",
            ));
        }
        // DN-111/M-822 multi-arg convention — VERIFY-FIRST FINDING (mitigation #14), Phase 0:
        // `lambda(x: T, y: U) => …` PARSES (the grammar's `params?` production allows any arity),
        // but empirically (against the real `target/debug/myc-check` oracle) the L1 checker treats
        // the resulting value as fully CURRIED (RFC-0024 §4A.8/§5, M-822): each application takes
        // exactly one argument ("`f` has function type and takes exactly 1 argument in stage-1").
        // An ordinary Rust multi-arg call site `f(a, b)` — this transpiler's existing, UNCHANGED
        // `Expr::Call` emission (`visit_call` above; out of this leaf's scope) — therefore fails
        // `myc check` against a directly-multi-param `lambda` declaration. A faithful multi-param
        // closure needs BOTH a curried declaration (`lambda(x: T) => lambda(y: U) => …`) AND a
        // chained call-site rewrite (`f(a)(b)`) — and `visit_call` cannot even emit a chained call
        // today (its call-target match only accepts a bare/qualified `Expr::Path`, not a nested
        // `Expr::Call`). That is a distinct, larger unit of work (a call-site-aware pass, not a
        // closure-EMIT one), so — rather than emit a plausible-but-`myc check`-failing form — a
        // multi-parameter closure is an explicit gap here (G2/VR-5); only the single-parameter
        // form is Mechanical/auto-emitted in Phase 1.
        if params.len() > 1 {
            return Err(GapReason::new(
                Category::Closure,
                format!(
                    "a {}-parameter closure has no auto-emittable Mechanical form in DN-118 \
                     Phase 1 — VERIFIED (not guessed, mitigation #14): `lambda(x: T, y: U) => …` \
                     parses, but the L1 checker treats the value as fully curried (RFC-0024 \
                     §4A.8/§5, M-822), so an ordinary multi-arg call site `f(a, b)` (this \
                     transpiler's unchanged `Expr::Call` emission) fails `myc check` \
                     (\"has function type and takes exactly 1 argument in stage-1\"). A faithful \
                     curried declaration plus a chained call-site rewrite (`f(a)(b)`) is a \
                     separate, larger unit of work — deferred rather than emitted as a \
                     plausible-but-failing form (G2/VR-5)",
                    params.len()
                ),
            ));
        }

        // DN-109 D5/D7 safety gate (DN-118 Phase 1's load-bearing step): classify whether every
        // capture this closure reaches is provably value-safe (read-only / moved / Copy) BEFORE
        // ever emitting a `lambda`. `syn` carries no borrowck facts, so this is deliberately
        // conservative — any *syntactically detectable* sign that the closure mutates a binding it
        // did not itself bind (a direct/compound assignment, an explicit `&mut`, or using it as a
        // method-call receiver at all, since a receiver's `&self` vs `&mut self` split is
        // unknowable from syntax alone) is treated as "cannot prove value-safe" and FLAGGED, never
        // auto-emitted (never-silent, G2/VR-5). This is the boundary DN-109 D7 names: mono's
        // defunctionalization captures a closure's environment as a **value snapshot at
        // construction** (a tag-sum struct field, set once), so an `FnMut`-style closure that
        // mutates a capture *across calls* would, if silently auto-emitted, produce a Mycelium
        // program that reads a DIFFERENT (stale) value every call — a silent semantic divergence,
        // not merely a check-time rejection.
        let mut bound: HashSet<String> = HashSet::new();
        for (name, _) in &params {
            bound.insert(name.clone());
        }
        let mutation = match &*c.body {
            Expr::Block(b) => scan_block_for_capture_mutation(&b.block.stmts, &bound),
            other => scan_expr_for_capture_mutation(other, &bound),
        };
        if let Some(captured) = mutation {
            return Err(GapReason::new(
                Category::Closure,
                format!(
                    "closure captures `{captured}` and appears to mutate it in place \
                     (`FnMut`/`&mut`-style: a direct/compound assignment, an explicit `&mut`, or a \
                     method-call receiver whose mutability `syn` cannot decide without borrowck \
                     facts) — DN-109 D7: this cannot be proven value-safe, so it is never \
                     auto-emitted (VR-5/G2). Suggested idiom: rewrite the closure to thread \
                     `{captured}` as an explicit fold/accumulator parameter (return the updated \
                     value instead of mutating in place), or as a functional update returning a \
                     new value — see DN-118 Phase 1, the FnMut/&mut safety boundary."
                ),
            ));
        }

        // Every remaining capture is provably value-safe (no mutation signal detected): mono's
        // whole-program defunctionalization (RFC-0024 §4A, M-704) resolves the capture set itself
        // at `finish()` — this emitter does NOT synthesize an env record; captured names are left
        // as ordinary in-scope references in the emitted body (module docs above).
        let mut body_env = self.env.clone();
        for (name, ty) in &params {
            body_env.insert(name.clone(), ty.clone());
        }
        let body_text = match &*c.body {
            Expr::Block(b) => emit_block_as_expr(&b.block, self.self_ty, &body_env)?,
            other => emit_expr(other, self.self_ty, &body_env)?,
        };
        let params_text = params
            .iter()
            .map(|(n, t)| format!("{n}: {t}"))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!("lambda({params_text}) => {body_text}"))
    }
}

// ---------------------------------------------------------------------------------------------
// DN-135 (M-1092) — the Result/Option combinator-directed match-inline (Alt A).
//
// The residual: `.map(|()| E)` / `.map_err(|_| C)` / etc. over a closure whose parameter is a
// UNIT pattern `|()|` or a WILDCARD `|_|` — the exact shapes `EmitVisitor::visit_closure`'s DN-118
// Phase-1 gate declines (its `lambda` surface needs an explicitly-typed single IDENTIFIER param).
// The combinator surface itself is NOT the gap: `map`/`map_err`/`and_then`/`or_else`/`fold` already
// exist as native `.myc` free functions whose bodies ARE `match` expressions
// (`lib/std/result.myc:23-46`, `lib/std/option.myc:36-58`), and the generic method-desugar already
// produces the `m(recv, f)` call shape (`visit_method_call`, below). DN-135's native answer:
// INLINE the combinator's own stdlib `match` body (a beta-reduction) with the closure body
// substituted and the closure's param lowered as the arm's BINDER PATTERN — `_` for `|_|`/`|()|`,
// the bare identifier otherwise — which relocates the unmappable construct from the (unsupported)
// `lambda`-parameter position into the (fully-supported) `match`-pattern position. No parameter
// type is ever needed (mode-invariant, DN-126 §4), so this fires identically whether or not the
// closure param happened to carry an explicit type.
//
// Zero kernel growth (KC-3: reuses `match` + the `Ok`/`Err`/`Some`/`None` constructors, already
// active grammar), DRY (inlines the library's own definition — never a parallel/divergent
// semantics), and the receiver gate below is the SAME no-guess discipline `prim_map`'s
// `receiver_gate_matches` uses (an unconfirmed/non-Result/Option receiver — e.g. an iterator's
// `.map` — falls straight through to the unchanged generic desugar, never a guess, VR-5/G2).
//
// **Scope correction against the original DN-135 §3 item 5 text (a real-toolchain finding, house
// rule #4):** a CHAIN (`.map(..).map_err(..)`) does NOT nest safely — `combinator_receiver_kind`
// deliberately does not resolve a `MethodCall` receiver (see that fn's doc for the full empirical
// finding: a nested inlined `match` used as an outer match's scrutinee fails `myc check`'s
// constructor type-parameter inference unless individually ascribed with a type this transpiler
// cannot generally derive). Only a receiver `expr_env_type` resolves directly (a bare identifier,
// or a `(..)`/`&..` wrapper) triggers an inline; each combinator in a chain is judged
// independently on its OWN receiver.
// ---------------------------------------------------------------------------------------------

/// The Result/Option "sum kind" a combinator's receiver resolved to — decides which pair of
/// constructor names (`Ok`/`Err` vs `Some`/`None`) the inlined `match`'s arms use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultOptionKind {
    Result,
    Option,
}

impl ResultOptionKind {
    /// The `(hit, pass)` constructor-name pair for this kind — `hit` is the constructor MOST
    /// combinators transform the payload of (`Ok`/`Some`), `pass` is the one MOST combinators
    /// leave untouched (`Err`/`None`). `map_err` (Result-only) inlines over `pass` instead — it
    /// builds its own arm text directly rather than using this pair.
    fn ctor_names(self) -> (&'static str, &'static str) {
        match self {
            ResultOptionKind::Result => ("Ok", "Err"),
            ResultOptionKind::Option => ("Some", "None"),
        }
    }

    /// The untouched pass-through arm's full `pattern => body` text — `Err(e) => Err(e)` for
    /// Result (the `Err` payload is bound and re-wrapped), `None => None` for Option (`None`
    /// carries no payload to bind).
    fn pass_arm_text(self) -> String {
        match self {
            ResultOptionKind::Result => "Err(e) => Err(e)".to_string(),
            ResultOptionKind::Option => "None => None".to_string(),
        }
    }
}

/// The combinator this pass recognizes by name — the exact `result.myc`/`option.myc` surface DN-135
/// §3 item 1 names. Recognizing a name here does NOT guarantee an inline fires for it (see
/// [`try_inline_result_option_combinator`]'s per-arm dispatch): `unwrap_or` in particular never has
/// a closure-shaped argument to relocate (both the Rust and the stdlib forms take a plain VALUE
/// fallback), so it is named for completeness against the spec's recognized set but always declines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultOptionArm {
    Map,
    MapErr,
    AndThen,
    OrElse,
    Fold,
    UnwrapOr,
}

fn result_option_arm(method: &str) -> Option<ResultOptionArm> {
    match method {
        "map" => Some(ResultOptionArm::Map),
        "map_err" => Some(ResultOptionArm::MapErr),
        "and_then" => Some(ResultOptionArm::AndThen),
        "or_else" => Some(ResultOptionArm::OrElse),
        "fold" => Some(ResultOptionArm::Fold),
        "unwrap_or" => Some(ResultOptionArm::UnwrapOr),
        _ => None,
    }
}

/// `receiver_ty` (an already-[`map_type`]-produced type-ref text, e.g. `"Result[Binary{8}, E]"`)
/// narrowed to `Result`/`Option`, or `None` for anything else (including no known type at all) —
/// the DN-135 receiver gate. Mirrors `prim_map::receiver_gate_matches`'s no-guess discipline
/// (checked against a resolved type, never inferred from usage), keyed off the generic-application
/// HEAD (`map_type`'s `"{name}[{args}]"` production, `crate::map`) since `prim_map`'s own
/// `ReceiverGate::Exact`/`AnyBinaryWidth` gates have no shape for a parameterized head.
fn result_option_kind_of_type(receiver_ty: &str) -> Option<ResultOptionKind> {
    if receiver_ty == "Result" || receiver_ty.starts_with("Result[") {
        Some(ResultOptionKind::Result)
    } else if receiver_ty == "Option" || receiver_ty.starts_with("Option[") {
        Some(ResultOptionKind::Option)
    } else {
        None
    }
}

/// The DN-135 receiver-kind resolution for a method call's receiver expression: `receiver` is a
/// bare identifier (or a `(..)`/`&..` wrapper around one) whose type is present in `env` —
/// [`expr_env_type`], the exact same mechanism `prim_map`'s gate consults (`emit.rs:2120-2123` in
/// `visit_method_call`, `receiver_gate_matches`/`prim_map.rs:228`). Anything else (a `Call`, a
/// field access, a literal, a nested `MethodCall`, …) resolves to `None` — an honest "not known"
/// (VR-5: absence, never a wrong guess) that lets the caller fall through to the unchanged generic
/// desugar (DN-135 §5 stress #2's bounded-faithfulness point: a cross-crate call receiver whose
/// return type this transpiler cannot resolve gaps honestly under bare vet profiling rather than
/// fabricating `Ok`/`Err`).
///
/// **Deliberately does NOT recurse into a `MethodCall` receiver (a CHAIN, `.map(..).map_err(..)`)
/// — a real-toolchain finding, not the original design (VR-5/house rule #4, disconfirms DN-135 §3
/// item 5's "chains nest" claim, which was `Declared`/unverified when written).** A nested inlined
/// `match` used as an OUTER match's scrutinee does **NOT** `myc check`-clean without an explicit
/// type ascription on the inner match's own `Ok`/`Err` constructor arms (confirmed empirically:
/// `match (match r { Ok(_) => Ok(flag), Err(e) => Err(e) }) { .. }` fails checking with `constructor
/// `Ok` does not determine type parameter `E`` — RFC-0007 §11.3 — UNLESS each inner arm is
/// individually ascribed, e.g. `Ok(flag) : Result[Binary{8}, Binary{8}]`; a `let`-bound
/// intermediate does not help either, same error). Supplying a CORRECT ascription type in general
/// would require knowing the inner combinator's OWN output type — for `map`/`and_then` that is the
/// closure's return type, which this transpiler has no inference pass to recover (VR-5: never
/// guess a type to paper over a checker gap). So chain-receiver resolution is left unbuilt here
/// rather than emitting text this leaf cannot prove checks clean; a chained call's OUTER
/// combinator simply declines (falls through to the unchanged generic desugar, same as any other
/// unresolved receiver) while its INNER combinator, if its OWN receiver independently resolves,
/// still inlines correctly on its own. Tracked as a follow-up (a type-ascription-aware chain
/// extension needs its own verify-first pass over the checker's inference rules, not guessed here).
fn combinator_receiver_kind(receiver: &Expr, env: &TypeEnv) -> Option<ResultOptionKind> {
    expr_env_type(receiver, env).and_then(|ty| result_option_kind_of_type(&ty))
}

/// Lower a closure's single-parameter PATTERN to the `match`-arm binder text it relocates to
/// (DN-135's central move): `_` for a wildcard `|_|` or a unit pattern `|()|` (both destructure to
/// nothing at the arm), the bare identifier name for `|x|`/`|x: T|` (the type, if present, is
/// simply unused — a `match` arm binder needs none, which is the mode-invariance argument, DN-126
/// §4). `None` for any other pattern shape (a non-unit tuple destructure, a struct/enum pattern,
/// a `ref`/`@`-subpattern identifier) — never guessed (VR-5); the caller declines to inline and
/// falls through to the unchanged generic desugar, which reaches `visit_closure`'s own identical
/// non-identifier-pattern gap for the same construct.
fn closure_single_param_binder(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Wild(_) => Some("_".to_string()),
        Pat::Tuple(t) if t.elems.is_empty() => Some("_".to_string()),
        Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => Some(pi.ident.to_string()),
        Pat::Type(pt) => closure_single_param_binder(&pt.pat),
        _ => None,
    }
}

/// Extract an inlinable closure-literal ARGUMENT's `(binder, body_text)` pair, or `None` when the
/// argument does not qualify — the DN-135 §3 item 3 split:
///
/// - not a closure literal at all (a function VALUE, e.g. `.map(SomeFn)`) — Alt B's residual role,
///   the existing unchanged `m(recv, f)` free-function call already handles it faithfully;
/// - an `async`/`const`/`static` closure, or one with != 1 parameter, or a non-inlinable parameter
///   pattern ([`closure_single_param_binder`]) — inherits `visit_closure`'s own identical gates
///   unchanged (DN-135 §3 item 3's "multi-param / value-unsafe closure" fallthrough);
/// - a closure that DN-109 D5/D7 cannot prove value-safe (mutates a non-parameter capture in
///   place) — applied BEFORE inlining, identically to `visit_closure`'s own gate (DN-135 §5 stress
///   #4: a single-use INLINED body has no "across calls" snapshot surface at all — there is no
///   reified closure value to go stale — so inlining is strictly SAFER than emitting a `lambda`,
///   never less safe; the gate still applies because duplicating/relocating a body that mutates an
///   outer capture in place would still be unsound regardless of how it is emitted);
/// - the closure's own body fails to emit (a real, independent gap inside the body) — declining
///   here does not swallow that gap: falling through re-derives the SAME emission call inside
///   `visit_closure` and surfaces the identical `GapReason` (never a duplicated/invented message).
fn inline_closure_arg(
    arg: &Expr,
    self_ty: Option<&str>,
    env: &TypeEnv,
) -> Option<(String, String)> {
    let Expr::Closure(c) = arg else {
        return None;
    };
    if c.asyncness.is_some() || c.constness.is_some() || c.movability.is_some() {
        return None;
    }
    if c.inputs.len() != 1 {
        return None;
    }
    let binder = closure_single_param_binder(&c.inputs[0])?;
    let emitted_binder = if binder != "_" {
        resolve_surface_ident(&binder, "closure parameter").ok()?
    } else {
        binder.clone()
    };
    let mut bound: HashSet<String> = HashSet::new();
    bound.insert(emitted_binder);
    let mutation = match &*c.body {
        Expr::Block(b) => scan_block_for_capture_mutation(&b.block.stmts, &bound),
        other => scan_expr_for_capture_mutation(other, &bound),
    };
    if mutation.is_some() {
        return None;
    }
    let body_env = env.clone();
    let body_text = match &*c.body {
        Expr::Block(b) => emit_block_as_expr(&b.block, self_ty, &body_env).ok()?,
        other => emit_expr(other, self_ty, &body_env).ok()?,
    };
    Some((binder, body_text))
}

/// `map`: `{ Ok(<p>) => Ok(<body>), Err(e) => Err(e) }` (Result) / `{ Some(<p>) => Some(<body>),
/// None => None }` (Option) — `lib/std/result.myc:23`/`lib/std/option.myc:33`'s own bodies,
/// verbatim, with `f(x)` substituted by the closure's body and `x` lowered to `<p>`.
fn inline_map(
    recv_text: &str,
    kind: ResultOptionKind,
    arg: &Expr,
    self_ty: Option<&str>,
    env: &TypeEnv,
) -> Option<String> {
    let (hit, _pass) = kind.ctor_names();
    let (binder, body) = inline_closure_arg(arg, self_ty, env)?;
    Some(format!(
        "match {recv_text} {{ {hit}({binder}) => {hit}({body}), {} }}",
        kind.pass_arm_text()
    ))
}

/// `map_err` (Result only — Option has no error side to map): `{ Ok(x) => Ok(x), Err(<p>) =>
/// Err(<body>) }` — `lib/std/result.myc:39`'s own body.
fn inline_map_err(
    recv_text: &str,
    arg: &Expr,
    self_ty: Option<&str>,
    env: &TypeEnv,
) -> Option<String> {
    let (binder, body) = inline_closure_arg(arg, self_ty, env)?;
    Some(format!(
        "match {recv_text} {{ Ok(x) => Ok(x), Err({binder}) => Err({body}) }}"
    ))
}

/// `and_then`: `{ Ok(<p>) => <body>, Err(e) => Err(e) }` (Result) / `{ Some(<p>) => <body>, None =>
/// None }` (Option) — `lib/std/result.myc:29`/`lib/std/option.myc:38`'s own bodies. The closure
/// body is used BARE (not re-wrapped in the hit constructor): `and_then`'s `f` already returns the
/// whole sum type (the monadic bind), unlike `map`'s plain-value-returning `f`.
fn inline_and_then(
    recv_text: &str,
    kind: ResultOptionKind,
    arg: &Expr,
    self_ty: Option<&str>,
    env: &TypeEnv,
) -> Option<String> {
    let (hit, _pass) = kind.ctor_names();
    let (binder, body) = inline_closure_arg(arg, self_ty, env)?;
    Some(format!(
        "match {recv_text} {{ {hit}({binder}) => {body}, {} }}",
        kind.pass_arm_text()
    ))
}

/// `or_else` (Result only — `lib/std/option.myc`'s `or_else` takes a plain Option VALUE `alt`, not
/// a closure, so there is nothing to inline there; it always falls through unchanged):
/// `{ Ok(x) => Ok(x), Err(<p>) => <body> }` — `lib/std/result.myc:45`'s own body.
fn inline_or_else(
    recv_text: &str,
    arg: &Expr,
    self_ty: Option<&str>,
    env: &TypeEnv,
) -> Option<String> {
    let (binder, body) = inline_closure_arg(arg, self_ty, env)?;
    Some(format!(
        "match {recv_text} {{ Ok(x) => Ok(x), Err({binder}) => {body} }}"
    ))
}

/// `fold` on Result: BOTH arguments are closures — `{ Ok(<p1>) => <body1>, Err(<p2>) => <body2> }`
/// (`lib/std/result.myc:33`). Declines (whole call, both arms) unless BOTH arguments inline —
/// never a half-inlined `match` with one arm still holding a raw Rust closure token stream.
fn inline_fold_result(
    recv_text: &str,
    args: &[&Expr],
    self_ty: Option<&str>,
    env: &TypeEnv,
) -> Option<String> {
    if args.len() != 2 {
        return None;
    }
    let (on_ok, on_err) = (args[0], args[1]);
    let (p1, b1) = inline_closure_arg(on_ok, self_ty, env)?;
    let (p2, b2) = inline_closure_arg(on_err, self_ty, env)?;
    Some(format!(
        "match {recv_text} {{ Ok({p1}) => {b1}, Err({p2}) => {b2} }}"
    ))
}

/// `fold` on Option: `on_some` is a closure, `on_none` is a plain VALUE (`lib/std/option.myc:44`'s
/// `fold(o, on_some: A => B, on_none: B)`) — `{ Some(<p>) => <body>, None => <on_none_expr> }`. The
/// second argument is emitted directly via [`emit_expr`] (never through [`inline_closure_arg`],
/// which only ever extracts a CLOSURE literal's binder+body).
fn inline_fold_option(
    recv_text: &str,
    args: &[&Expr],
    self_ty: Option<&str>,
    env: &TypeEnv,
) -> Option<String> {
    if args.len() != 2 {
        return None;
    }
    let (on_some, on_none) = (args[0], args[1]);
    let (p, body) = inline_closure_arg(on_some, self_ty, env)?;
    let on_none_text = emit_expr(on_none, self_ty, env).ok()?;
    Some(format!(
        "match {recv_text} {{ Some({p}) => {body}, None => {on_none_text} }}"
    ))
}

/// The DN-135/M-1092 entry point, consulted first in `visit_method_call`. Returns:
/// - `None` — not applicable (an unrecognized method name, an unconfirmed/non-Result-Option
///   receiver, a non-closure argument, or a closure DN-118's own gates would decline) — the caller
///   falls straight through to the UNCHANGED code below (the `prim_map` forward-map, then the
///   generic desugar), exactly as if this pass did not exist. Never a guess (VR-5/G2).
/// - `Some(Ok(text))` — the inlined `.myc` `match` expression.
/// - `Some(Err(reason))` — the receiver IS a confirmed Result/Option and the method name IS a
///   recognized combinator with an otherwise-inlinable closure argument, but emitting the
///   already-confirmed receiver expression itself failed. Propagated rather than silently
///   swallowed into a `None` "not applicable" (G2) — an internal-consistency edge case the gate
///   above is not expected to let through, but never assumed away.
///
/// **DN-136/P1-a scope note.** This axis is the pre-existing `prim_map::TABLE`-adjacent template
/// the other three axes (patterns/derives/calls) generalize — DN-136 §3 item 4 rules it
/// "already additive" and its migration action is documentation-only: **this function's
/// `(kind, arm)` dispatch is deliberately left unrestructured** (no behavior change; the
/// byte-identical differential in `src/tests/emit.rs` covers it unchanged, same as every other
/// axis). Restructuring it into a literal `&[Row]` table was considered and declined here — the
/// per-`(kind, arm)` cross-product has differing arities/closure-count requirements per
/// combinator (`fold` takes 2 closures, `map`/`and_then`/`map_err`/`or_else` take 1, `unwrap_or`
/// never inlines) that a uniform row shape would either force through extra indirection or
/// under-model; DN-136 itself only asks this axis to "document ... as the template", not migrate
/// it, so restructuring it would be scope creep past the note's own DoD (§8).
fn try_inline_result_option_combinator(
    m: &syn::ExprMethodCall,
    self_ty: Option<&str>,
    env: &TypeEnv,
) -> Option<Result<String, GapReason>> {
    let method = m.method.to_string();
    let arm = result_option_arm(&method)?;
    let kind = combinator_receiver_kind(&m.receiver, env)?;

    // `unwrap_or` never has a closure-shaped argument (Rust's `.unwrap_or(v)` / the stdlib
    // `unwrap_or(r, fallback: A)` both take a plain VALUE) — nothing to relocate a param out of,
    // so this pass never fires for it; named in the recognized set (DN-135 §3 item 1) purely for
    // completeness against the spec, not because it ever inlines.
    if matches!(arm, ResultOptionArm::UnwrapOr) {
        return None;
    }

    // Parenthesized unconditionally (matches DN-135 §1's own worked example) — harmless for a
    // plain identifier receiver too (`(r)` parses identically to `r`, the same `Expr::Paren`
    // erasure `visit_paren` already performs elsewhere in this module). NOTE: this does NOT make
    // a chain safe on its own — `combinator_receiver_kind` never resolves a `MethodCall` receiver
    // in the first place (see that fn's doc), so `m.receiver` here is never itself an inlined
    // nested `match`; this parenthesization only ever wraps an ordinary resolved expression.
    let recv_text = match emit_expr(&m.receiver, self_ty, env) {
        Ok(t) => format!("({t})"),
        Err(e) => return Some(Err(e)),
    };
    let args: Vec<&Expr> = m.args.iter().collect();

    let inlined = match (kind, arm) {
        (_, ResultOptionArm::Map) => {
            let arg = *args.first()?;
            inline_map(&recv_text, kind, arg, self_ty, env)
        }
        (ResultOptionKind::Result, ResultOptionArm::MapErr) => {
            let arg = *args.first()?;
            inline_map_err(&recv_text, arg, self_ty, env)
        }
        (_, ResultOptionArm::AndThen) => {
            let arg = *args.first()?;
            inline_and_then(&recv_text, kind, arg, self_ty, env)
        }
        (ResultOptionKind::Result, ResultOptionArm::OrElse) => {
            let arg = *args.first()?;
            inline_or_else(&recv_text, arg, self_ty, env)
        }
        (ResultOptionKind::Result, ResultOptionArm::Fold) => {
            inline_fold_result(&recv_text, &args, self_ty, env)
        }
        (ResultOptionKind::Option, ResultOptionArm::Fold) => {
            inline_fold_option(&recv_text, &args, self_ty, env)
        }
        // Option has no `map_err` (not a method on `Option[A]` — no error side to map) and its
        // `or_else`'s argument is a plain Option VALUE, not a closure (`or_else(o, alt:
        // Option[A])`, `lib/std/option.myc:49`) — nothing to inline; falls through unchanged.
        (ResultOptionKind::Option, ResultOptionArm::MapErr | ResultOptionArm::OrElse) => None,
        (_, ResultOptionArm::UnwrapOr) => unreachable!("handled above"),
    };

    inlined.map(Ok)
}

/// Extract the "root" identifier a place-expression (an assignment LHS, a `&mut` target, or a
/// method-call receiver) ultimately projects from — unwrapping field access, indexing,
/// parenthesization, and dereference so `cap.field = x`, `cap[0] = x`, and `(*cap).field = x` all
/// resolve to `cap`. `None` when the root is not a bare identifier (nothing to flag against — e.g.
/// a temporary, a literal, a nested call result).
fn place_root_ident(e: &Expr) -> Option<String> {
    match e {
        Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
            Some(p.path.segments.last()?.ident.to_string())
        }
        Expr::Field(f) => place_root_ident(&f.base),
        Expr::Index(i) => place_root_ident(&i.expr),
        Expr::Paren(p) => place_root_ident(&p.expr),
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_)) => place_root_ident(&u.expr),
        _ => None,
    }
}

/// Whether a `syn::BinOp` is one of the ten compound-assignment operators (`+=`, `-=`, …) — syn 2
/// folds compound assignment into `Expr::Binary` (there is no separate `ExprAssignOp`), so this is
/// the gate `scan_expr_for_capture_mutation`'s `Expr::Binary` arm uses to recognize an in-place
/// mutation shape distinct from an ordinary arithmetic/logical binary op.
fn is_compound_assign_op(op: &syn::BinOp) -> bool {
    use syn::BinOp;
    matches!(
        op,
        BinOp::AddAssign(_)
            | BinOp::SubAssign(_)
            | BinOp::MulAssign(_)
            | BinOp::DivAssign(_)
            | BinOp::RemAssign(_)
            | BinOp::BitXorAssign(_)
            | BinOp::BitAndAssign(_)
            | BinOp::BitOrAssign(_)
            | BinOp::ShlAssign(_)
            | BinOp::ShrAssign(_)
    )
}

/// Collect the identifier(s) a closure PARAMETER pattern binds, into `out` — used to seed
/// [`scan_block_for_capture_mutation`]/[`scan_expr_for_capture_mutation`]'s `local` set so a
/// closure's own parameters (and a nested closure's own parameters, `Expr::Closure`'s arm below)
/// are never mistaken for an outer capture. Deliberately narrow (only `Pat::Ident`, plain or
/// type-ascribed) — a pattern shape this collects nothing for still can't cause a false "safe"
/// classification, because `EmitVisitor::visit_closure` itself already gaps any
/// non-`Pat::Ident` PARAMETER before this scan ever runs; this helper exists only so the *nested*-
/// closure recursion inside the scanner has a matching narrow collector to call.
fn collect_closure_param_names(pat: &Pat, out: &mut HashSet<String>) {
    match pat {
        Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => {
            out.insert(pi.ident.to_string());
        }
        Pat::Type(pt) => collect_closure_param_names(&pt.pat, out),
        _ => {}
    }
}

/// The DN-109 D7 capture-mutation scan over a closure BODY BLOCK's statements (see
/// `EmitVisitor::visit_closure`'s doc for the safety rationale). Tracks `let`-bound names
/// (cloned into a fresh, block-scoped `local` set) so a purely internal accumulator — fully
/// `let`-bound and mutated only inside the closure's own body, never escaping — is never confused
/// with a genuine outer capture. Returns the first captured name found syntactically mutated, if
/// any.
fn scan_block_for_capture_mutation(stmts: &[Stmt], local: &HashSet<String>) -> Option<String> {
    let mut local = local.clone();
    for s in stmts {
        match s {
            Stmt::Local(l) => {
                if let Some(init) = &l.init {
                    if let Some(found) = scan_expr_for_capture_mutation(&init.expr, &local) {
                        return Some(found);
                    }
                    if let Some(diverge) = &init.diverge {
                        if let Some(found) = scan_expr_for_capture_mutation(&diverge.1, &local) {
                            return Some(found);
                        }
                    }
                }
                collect_closure_param_names(&l.pat, &mut local);
            }
            Stmt::Expr(e, _) => {
                if let Some(found) = scan_expr_for_capture_mutation(e, &local) {
                    return Some(found);
                }
            }
            // Nested items/macros carry no expression to scan (macro args are opaque tokens, not
            // parsed `Expr`s here — the same PoC-scope boundary `emit_block_as_expr_inner` already
            // draws for `Stmt::Item`/`Stmt::Macro`).
            Stmt::Item(_) | Stmt::Macro(_) => {}
        }
    }
    None
}

/// The DN-109 D7 capture-mutation scan over a single expression (see
/// `EmitVisitor::visit_closure`'s doc). `local` is the set of names bound *within* the closure
/// itself (its own parameters, plus every `let`-bound name in scope so far) — a name outside
/// `local` that this scan finds as the root of an assignment target, an explicit `&mut` target, or
/// a method-call receiver is a capture whose mutability could not be proven safe. Deliberately
/// conservative: this recurses into the shapes common enough to matter (blocks, control flow,
/// calls, field/index/paren/cast, nested closures) and returns `None` (no signal) for any shape it
/// does not specifically recognize — an unrecognized shape containing a real mutation would in any
/// case already fail the ordinary body emission generically (`emit_block_as_expr_inner`/
/// `emit_expr_inner` have no `Expr::Assign`/compound-assign arm at all), so this scan's only job is
/// to catch the mutation *before* emission with a curated, DN-109-cited message — never to be the
/// sole safety boundary.
fn scan_expr_for_capture_mutation(e: &Expr, local: &HashSet<String>) -> Option<String> {
    match e {
        Expr::Assign(a) => {
            if let Some(name) = place_root_ident(&a.left) {
                if !local.contains(&name) {
                    return Some(name);
                }
            }
            scan_expr_for_capture_mutation(&a.left, local)
                .or_else(|| scan_expr_for_capture_mutation(&a.right, local))
        }
        Expr::Binary(b) if is_compound_assign_op(&b.op) => {
            if let Some(name) = place_root_ident(&b.left) {
                if !local.contains(&name) {
                    return Some(name);
                }
            }
            scan_expr_for_capture_mutation(&b.left, local)
                .or_else(|| scan_expr_for_capture_mutation(&b.right, local))
        }
        Expr::Binary(b) => scan_expr_for_capture_mutation(&b.left, local)
            .or_else(|| scan_expr_for_capture_mutation(&b.right, local)),
        Expr::Reference(r) if r.mutability.is_some() => {
            if let Some(name) = place_root_ident(&r.expr) {
                if !local.contains(&name) {
                    return Some(name);
                }
            }
            scan_expr_for_capture_mutation(&r.expr, local)
        }
        Expr::Reference(r) => scan_expr_for_capture_mutation(&r.expr, local),
        Expr::MethodCall(m) => {
            if let Some(name) = place_root_ident(&m.receiver) {
                if !local.contains(&name) {
                    return Some(name);
                }
            }
            scan_expr_for_capture_mutation(&m.receiver, local).or_else(|| {
                m.args
                    .iter()
                    .find_map(|a| scan_expr_for_capture_mutation(a, local))
            })
        }
        Expr::Unary(u) => scan_expr_for_capture_mutation(&u.expr, local),
        Expr::Paren(p) => scan_expr_for_capture_mutation(&p.expr, local),
        Expr::Field(f) => scan_expr_for_capture_mutation(&f.base, local),
        Expr::Index(i) => scan_expr_for_capture_mutation(&i.expr, local)
            .or_else(|| scan_expr_for_capture_mutation(&i.index, local)),
        Expr::Call(c) => scan_expr_for_capture_mutation(&c.func, local).or_else(|| {
            c.args
                .iter()
                .find_map(|a| scan_expr_for_capture_mutation(a, local))
        }),
        Expr::If(i) => scan_expr_for_capture_mutation(&i.cond, local)
            .or_else(|| scan_block_for_capture_mutation(&i.then_branch.stmts, local))
            .or_else(|| {
                i.else_branch
                    .as_ref()
                    .and_then(|(_, e)| scan_expr_for_capture_mutation(e, local))
            }),
        Expr::Block(b) => scan_block_for_capture_mutation(&b.block.stmts, local),
        Expr::Match(m) => scan_expr_for_capture_mutation(&m.expr, local).or_else(|| {
            m.arms
                .iter()
                .find_map(|arm| scan_expr_for_capture_mutation(&arm.body, local))
        }),
        Expr::Tuple(t) => t
            .elems
            .iter()
            .find_map(|e| scan_expr_for_capture_mutation(e, local)),
        Expr::Array(a) => a
            .elems
            .iter()
            .find_map(|e| scan_expr_for_capture_mutation(e, local)),
        Expr::Struct(s) => s
            .fields
            .iter()
            .find_map(|f| scan_expr_for_capture_mutation(&f.expr, local)),
        Expr::Cast(c) => scan_expr_for_capture_mutation(&c.expr, local),
        Expr::Closure(c) => {
            // A nested closure over the same outer capture is exactly the same hazard — recurse
            // with its own params added as further locals, never popped back out (this scan never
            // needs precise lexical scoping, only "is this name bound somewhere enclosing the use"
            // — the conservative direction is to under-report a false capture, not over-report one
            // that's actually a shadowed local, and adding names monotonically never does that).
            let mut inner = local.clone();
            for p in &c.inputs {
                collect_closure_param_names(p, &mut inner);
            }
            scan_expr_for_capture_mutation(&c.body, &inner)
        }
        _ => None,
    }
}

/// A short human label for a `syn::Member` (`self.mode` / `self.0`), for gap-reason messages.
fn member_text(m: &syn::Member) -> String {
    match m {
        syn::Member::Named(id) => id.to_string(),
        syn::Member::Unnamed(idx) => idx.index.to_string(),
    }
}

/// Collect every identifier a match-arm pattern **binds** into `out` — the `Expr::Match` operand-
/// type-env fix (see that arm's docs): a pattern-bound name (e.g. an enum payload field, `Wrap::A(x)`
/// binding `x`) can carry a completely different type than any outer local of the same name it
/// shadows, so every such name must be invalidated in a per-arm `env` copy before the arm body is
/// emitted — otherwise `Expr::Binary`'s operand-type gate could mis-fire on the *outer* type of a
/// name the pattern just rebound. Deliberately conservative and purely structural (no attempt to
/// determine *what* a bound name's type is, only *that* it is bound — VR-5: never guess, and here
/// over-invalidating is the safe direction; a name incorrectly stripped just falls back to the
/// prior, unchanged default emission, never a wrong `Binary{N}`-gated one). Only called on patterns
/// `map_pattern` has already accepted (so recursion depth is already budget-bounded by that call —
/// see `crate::gap::guarded`), but every shape below is still handled defensively, including
/// `Pat::Struct` (not itself accepted by `map_pattern` today, but future-proofed here so a later
/// pattern-shape addition can never silently reintroduce this gap).
fn collect_pattern_bound_names(pat: &Pat, out: &mut HashSet<String>) {
    match pat {
        Pat::Ident(pi) => {
            out.insert(pi.ident.to_string());
            if let Some((_, sub)) = &pi.subpat {
                collect_pattern_bound_names(sub, out);
            }
        }
        Pat::TupleStruct(pts) => {
            for e in &pts.elems {
                collect_pattern_bound_names(e, out);
            }
        }
        Pat::Tuple(pt) => {
            for e in &pt.elems {
                collect_pattern_bound_names(e, out);
            }
        }
        Pat::Struct(ps) => {
            for f in &ps.fields {
                collect_pattern_bound_names(&f.pat, out);
            }
        }
        Pat::Or(po) => {
            for c in &po.cases {
                collect_pattern_bound_names(c, out);
            }
        }
        Pat::Paren(pp) => collect_pattern_bound_names(&pp.pat, out),
        Pat::Reference(pr) => collect_pattern_bound_names(&pr.pat, out),
        // `Pat::Wild`/`Pat::Path`/`Pat::Lit`/everything else binds no name.
        _ => {}
    }
}

/// Whether a match-arm pattern is (or, through `|`/parens/refs, contains) a **string-literal**
/// pattern — the M-1035/ENB-12 marker that the scrutinee is `Bytes`. Drives the `Expr::Match`
/// open-domain exhaustiveness guard (a `Bytes` match needs a wildcard/default arm). Mirrors the
/// same transparent `Pat::Or`/`Pat::Paren`/`Pat::Reference` descent as [`map_pattern_inner`].
fn pattern_contains_str_lit(pat: &Pat) -> bool {
    match pat {
        Pat::Lit(pl) => matches!(&pl.lit, Lit::Str(_)),
        Pat::Or(po) => po.cases.iter().any(pattern_contains_str_lit),
        Pat::Paren(pp) => pattern_contains_str_lit(&pp.pat),
        Pat::Reference(pr) => pattern_contains_str_lit(&pr.pat),
        _ => false,
    }
}

/// Whether a match-arm pattern is an **irrefutable default** — a wildcard `_` or a bare identifier
/// binding (no `ref`, no subpattern) — i.e. the catch-all arm that satisfies M-1035's open-`Bytes`
/// W7 coverage requirement. A guarded arm is never a default (its guard makes it conditional); the
/// caller pairs this with an `a.guard.is_none()` check.
fn is_irrefutable_match_default(pat: &Pat) -> bool {
    match pat {
        Pat::Wild(_) => true,
        Pat::Ident(pi) => pi.by_ref.is_none() && pi.subpat.is_none(),
        Pat::Paren(pp) => is_irrefutable_match_default(&pp.pat),
        _ => false,
    }
}

/// Translate one Rust pattern. Exhaustive `match` over `syn::Pat`; fallback arm errors.
///
/// `self_ty` is `Some(name)` inside an `impl <name>` body (the same threading `emit_expr`/
/// `map_type` already use) — DN-132 P1's [`map_struct_pattern`] is the only arm that consults it
/// today (resolving a bare `Self { .. }` struct pattern to the enclosing type's own ctor name, the
/// pattern-side counterpart of [`known_struct_literal_ty`]'s expression-side resolution); every
/// other arm ignores it unchanged, so a `None` caller (e.g. a free-fn body, or a direct unit-test
/// call) behaves exactly as before this parameter was added.
///
/// **RFC-0041 §4.7 (W1):** guarded by the crate-wide recursion budget (`crate::gap::guarded`) —
/// self-recurses over unbounded/attacker-controlled pattern nesting (e.g. `Pat::Paren`/`Pat::Or`/
/// `Pat::TupleStruct`), so each call consumes one budget frame and refuses with a
/// `Category::RecursionBudget` gap rather than risking a host-stack overflow.
pub fn map_pattern(pat: &Pat, self_ty: Option<&str>) -> Result<String, GapReason> {
    guarded(|| map_pattern_inner(pat, self_ty))
}

/// The recursion-guarded body of [`map_pattern`]. Recursive calls use the public `map_pattern`
/// name so each nested call re-enters the guard.
///
/// **DN-136/P1-a (Alt B).** [`patterns::lookup`] is consulted FIRST — a static, per-axis
/// handler table (generalizing the landed `prim_map::TABLE` pattern, `prim_map.rs:140`) covering
/// the three "gap-closing leaf" pattern kinds that used to serialize on this `match` (M-823
/// or-pattern, M-826 tuple-pattern, M-1089/DN-132 struct-variant pattern — DN-136 §1's own
/// framing of exactly these three). A future pattern leaf adds one file + one append-only
/// `TABLE` row there, never touching this function. The base-kernel pattern forms below
/// (`Wild`/`Ident`/`Path`/`TupleStruct`/`Lit`/`Paren`/`Reference`) are foundational grammar
/// primitives, not additive leaf targets, so they stay here unchanged; a table miss falls
/// through to them, then to the final explicit gap — identical fallback shape to the
/// pre-refactor `match`'s own `_` arm (G2: never a silent drop).
fn map_pattern_inner(pat: &Pat, self_ty: Option<&str>) -> Result<String, GapReason> {
    if let Some(handler) = patterns::lookup(pat) {
        return (handler.emit)(pat, self_ty);
    }
    match pat {
        Pat::Wild(_) => Ok("_".to_string()),
        Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => {
            let name = pi.ident.to_string();
            resolve_surface_ident(&name, "match pattern binding/constructor")
        }
        Pat::Path(pp) if pp.qself.is_none() => {
            let seg = pp
                .path
                .segments
                .last()
                .ok_or_else(|| GapReason::new(Category::Other, "empty path pattern"))?;
            let name = seg.ident.to_string();
            resolve_surface_ident(&name, "match pattern constructor")
        }
        Pat::TupleStruct(pts) if pts.qself.is_none() => {
            let seg = pts.path.segments.last().ok_or_else(|| {
                GapReason::new(Category::Other, "empty tuple-struct pattern path")
            })?;
            let ctor = resolve_surface_ident(&seg.ident.to_string(), "match pattern constructor")?;
            let mut elems = Vec::with_capacity(pts.elems.len());
            for e in &pts.elems {
                elems.push(map_pattern(e, self_ty)?);
            }
            Ok(format!("{}({})", ctor, elems.join(", ")))
        }
        Pat::Lit(pl) => match &pl.lit {
            Lit::Bool(b) => Ok(if b.value { "True" } else { "False" }.to_string()),
            Lit::Int(i) => Ok(i.base10_digits().to_string()),
            // A **string-literal** pattern (`"foo" => …`) is grammatically a valid Mycelium pattern
            // (`pattern ::= literal ::= StrLit`, grammar line 305/414). It was previously gapped
            // because the L1 checker categorically rejected a `match` whose scrutinee is `Bytes`
            // (`match scrutinee must be a data, Binary, or Ternary type, got Bytes`). **M-1035 /
            // ENB-12 landed that enabler** (`check_match` now admits `Ty::Bytes` with byte-string
            // literal arms — DN-99 #72 reclassified `tr-only` → language-enabler, then unblocked),
            // so a string-literal arm now emits and `myc check`-cleans — verified against the real
            // oracle (`fn c(s: Bytes) => Bool = match s { "yes" => True, _ => False };` → `ok`).
            // The **open-`Bytes` exhaustiveness** requirement (M-1035's W7 coverage: a `Bytes` match
            // needs a wildcard/default arm, else `non-exhaustive match on Bytes: missing _`) is
            // enforced at the `Expr::Match` level (see `pattern_contains_str_lit` /
            // `is_irrefutable_match_default`), so a string-literal match is emitted only when it
            // carries the default M-1035 requires — never a check-failing non-exhaustive one
            // (VR-5/G2). See DN-34 §8.21 and `string_literal_pattern_emits_with_l1_enabler`.
            Lit::Str(s) => myc_string_literal(&s.value()),
            _ => Err(GapReason::new(
                Category::Other,
                "unsupported literal pattern kind (only bool/int/string literal patterns map; \
                 a float/byte/char literal pattern has no faithful Mycelium surface — VR-5/G2)",
            )),
        },
        Pat::Paren(pp) => map_pattern(&pp.pat, self_ty),
        Pat::Reference(pr) => map_pattern(&pr.pat, self_ty),
        _ => Err(GapReason::new(
            Category::Other,
            format!("unsupported match pattern form `{}`", tokens_to_string(pat)),
        )),
    }
}

// ---------------------------------------------------------------------------------------------
// Top-level item emitters.
// ---------------------------------------------------------------------------------------------

/// Map a **named-field record** (`{ a: T, b: U }`, a `struct`'s or an enum variant's fields) to the
/// grammar's **positional** constructor form: the field *types* become positional arguments and the
/// field *names* are dropped. Returns `(mapped_field_types, dropped_field_names)`.
///
/// Mycelium's `constructor ::= Ident ('(' type_ref (',' type_ref)* ')')?`
/// (`docs/spec/grammar/mycelium.ebnf` §`constructor`) is **positional-only** — there is no
/// named-field/record surface — so a named-field record emits exactly like a tuple one (`Fields::
/// Unnamed`): its product *structure* is preserved, faithfully, and the field names (surface sugar)
/// are dropped. This is precisely how the `lib/std/*.myc` hand-ports already render a Rust record
/// (`type GuaranteeRow = Row(Bytes, Guarantee, Bytes, Bytes, Bool);`). The caller records the dropped
/// names as a never-silent [`Category::NamedFieldDrop`] sub-gap (G2) — they are *recorded*, not lost.
///
/// A field whose *type* has no confirmed mapping still **refuses the whole record** (via `on_type_gap`,
/// propagating that field's precise reason), never a partial emission (VR-5/G2) — exactly as the
/// positional path already does (so e.g. a `String`/slice field keeps the record a hard gap).
fn map_named_fields_positional(
    fields: &FieldsNamed,
    on_type_gap: impl Fn(&str) -> GapReason,
) -> Result<(Vec<String>, Vec<String>), GapReason> {
    let mut tys = Vec::with_capacity(fields.named.len());
    let mut names = Vec::with_capacity(fields.named.len());
    for f in &fields.named {
        let mapped = map_type(&f.ty, None).map_err(|inner| on_type_gap(&inner.reason))?;
        tys.push(mapped);
        names.push(
            f.ident
                .as_ref()
                .map_or_else(|| "_".to_string(), ToString::to_string),
        );
    }
    Ok((tys, names))
}

// ---------------------------------------------------------------------------------------------
// DN-128 (M-1086) — the std-derive lowering library, struct scope.
//
// `#[derive(...)]` on a `struct` was, until this leaf, unconditionally dropped as one bulk
// `Category::DeriveAttr` sub-gap (the pre-existing `non_doc_attrs`/`sub_gaps.push` pair every
// `emit_*` item function still uses for `enum`/`fn`/impl-method sites — unchanged there, see
// `docs/notes/DN-128-Standard-Derive-Lowering-Library.md` §4/§7 "structs first"). This section
// lowers the four derives DN-128 §2 scopes to this leaf — `Clone`/`Copy` (a satisfied no-op under
// value semantics, ADR-003, DN-128 §6.1) and `Debug`/`Default` (composed `impl Show[T] for T` /
// `impl Init[T] for T` bodies over the DN-127/DN-129 landed prelude traits,
// `crates/mycelium-l1/src/show.rs` / `init.rs`) — to explicit, `.myc`-text `impl` blocks appended
// after the struct's own `type` declaration. `Eq`/`Ord`/`Hash`/`PartialEq`/`PartialOrd` (DN-128 §2's
// other rows) are **out of this leaf's scope** — an unrecognized-name gap, same as any other
// unhandled derive (recorded, never silently dropped, G2).
//
// OQ-1 (DN-128 §3, "does a `lower` RHS have field reflection?") is resolved for THIS emission path
// as **moot**: the field-walk happens here, in the Rust transpiler, over `syn`'s already-typed field
// list — never inside a `.myc` `lower` RHS at all. This is the Alt-C "compiler-internal field-walk"
// DN-128 recommends, one layer further out (the transpiler's own field enumeration, not even
// `mycelium-l1`'s elaborator) — it survives either OQ-1 answer because it never needs one.

/// DN-128 (M-1086) — classify + lower a struct's `#[derive(...)]` list against the standard-derive
/// set this leaf builds (`Debug`->`Show`, `Default`->`Init`, `Clone`/`Copy`->satisfied no-op).
/// Returns the composed `.myc` impl-block text for every derive that lowered successfully (appended
/// after the struct's own `type` declaration in [`emit_struct`]) plus every sub-gap this pass
/// records: an unrecognized derive name (still `Category::DeriveAttr`, same bucket the pre-existing
/// bulk-drop uses), a recognized-but-uncomposable one (a field-eligibility refusal from a derive
/// row's own rule), or a `Clone`/`Copy` satisfied-no-op note (`Category::DeriveSatisfied` — never
/// `DeriveAttr`, it is not a gap). Never partially silent: every derive name that does not end up
/// in the composed-impls list has a corresponding sub-gap explaining why (G2).
///
/// **DN-136/P1-a (Alt B).** [`derives::lookup`] is consulted for each derive-path name — a
/// static, per-axis handler table (generalizing the landed `prim_map::TABLE` pattern) covering
/// the DN-128 standard-derive set (`Debug`/`Default`/`Clone`/`Copy`). **This driver still owns
/// the two-level guarantee's set-orchestration half** (DN-136 §3 item 2 / §7 — a build-blocking
/// invariant this function must never lose): the attribute/derive-list walk, routing each row's
/// [`derives::DeriveOutcome`] to `impls`/`sub_gaps`, and the `unrecognized` bucket + its final
/// summary gap for any derive name no row claims (`Eq`/`Ord`/`Hash`/`PartialEq`/`PartialOrd`,
/// unchanged — still falls through, byte-identical to the pre-refactor catch-all `other =>` arm).
/// A row owns only its OWN per-impl field-atomicity (the other guarantee-level, unchanged inside
/// each row's own rule) — a row can never move this orchestration into itself.
fn lower_struct_derives(
    ty_name: &str,
    attrs: &[Attribute],
    field_types: &[String],
    is_generic: bool,
) -> (Vec<String>, Vec<GapReason>) {
    let mut impls = Vec::new();
    let mut sub_gaps = Vec::new();
    let mut unrecognized = Vec::new();

    for attr in attrs {
        if !attr.path().is_ident("derive") {
            continue;
        }
        let Ok(list) = attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
        ) else {
            sub_gaps.push(GapReason::new(
                Category::DeriveAttr,
                format!(
                    "dropped derive attribute on struct `{ty_name}` (argument list did not parse \
                     as a plain trait-path list): {}",
                    tokens_to_string(attr)
                ),
            ));
            continue;
        };
        for path in list {
            let name = tokens_to_string(&path);
            match derives::lookup(&name) {
                Some(handler) => {
                    let ctx = derives::DeriveCtx {
                        ty_name,
                        field_types,
                        is_generic,
                        name: &name,
                    };
                    match (handler.emit)(&ctx) {
                        derives::DeriveOutcome::Composed(myc) => {
                            // ONESHOT C3: PartialEq composes `fn eq_<T>` — record so expression-
                            // level `==`/`!=` on this user type can call it (kernel `eq` is
                            // Binary/Ternary-only).
                            if name == "PartialEq" {
                                record_local_eq_type(ty_name);
                            }
                            impls.push(myc);
                        }
                        derives::DeriveOutcome::Satisfied(note) => sub_gaps.push(note),
                        derives::DeriveOutcome::Gap(g) => sub_gaps.push(g),
                    }
                }
                None => unrecognized.push(name),
            }
        }
    }
    if !unrecognized.is_empty() {
        sub_gaps.push(GapReason::new(
            Category::DeriveAttr,
            format!(
                "struct `{ty_name}` derive(...) names {} not in the DN-128 standard-derive set this \
                 leaf builds (Debug/Default/Clone/Copy/PartialEq/PartialOrd/Hash all recognized; \
                 bare Eq/Ord are deliberately NOT recognized — see emit/derives/mod.rs::TABLE's \
                 doc for why) — dropped, no confirmed Mycelium surface",
                unrecognized.join(", ")
            ),
        ));
    }
    (impls, sub_gaps)
}

/// `enum` -> `type_item` (`type Name = C1 | C2(T1, T2) | ...;`).
///
/// **ONESHOT C2 (DN-128 §2 enum half):** after the type declaration, lower recognized
/// `#[derive(...)]` names the same way product structs do — `PartialEq` → `fn eq_<T>`
/// ([`derives::eq::compose_enum`]), `Debug` → `impl Show[T]` ([`derives::show::compose_enum`]),
/// `Clone`/`Copy` → satisfied no-op notes. This closes the residual where a parent struct's
/// derived `eq_MatrixRow` called `eq_Fallibility` / `eq_FileKind` / `eq_GuaranteeTag` that never
/// existed because enum derives were bulk-dropped as `DeriveAttr` sub-gaps.
pub fn emit_enum(item: &ItemEnum) -> Result<Emitted, GapReason> {
    let enum_vi = valid_ident(&item.ident.to_string());
    register_ident_emission(&enum_vi, "enum type name")?;
    let enum_name = enum_vi.text.clone();
    let mut doc = Vec::new();
    push_rewrite_doc(&enum_vi, &mut doc);
    let type_params = plain_type_params(&item.generics)?;
    let mut sub_gaps = Vec::new();
    // Tracks whether any variant is a **named-field** record — the M-1006 resolvability gate applies
    // to such an enum *after* mapping (below), so an unmappable field still surfaces its own precise
    // reason first (an honest gap profile: "String field" is a repr gap, not a resolution gap).
    let mut has_named_variant = false;
    // Non-derive non-doc attrs still bulk-drop (e.g. `#[repr(...)]`); derive attrs are handled
    // by [`lower_enum_derives`] after the type text is composed — never silently. Uses the same
    // filter [`emit_struct`] already uses (DN-128) so derive lists are not double-reported.
    let non_derive_non_doc = non_doc_non_derive_attrs(&item.attrs);
    if !non_derive_non_doc.is_empty() {
        sub_gaps.push(GapReason::new(
            Category::DeriveAttr,
            format!(
                "dropped non-doc non-derive attribute(s) on enum `{}`: {}",
                item.ident,
                non_derive_non_doc.join(" ")
            ),
        ));
    }
    let mut ctors = Vec::with_capacity(item.variants.len());
    // Parallel to `ctors`: rewritten variant name + mapped payload field types (for derive
    // composition). Unit variants carry an empty field list.
    let mut variant_shapes: Vec<(String, Vec<String>)> = Vec::with_capacity(item.variants.len());
    for v in &item.variants {
        let variant_vi = valid_ident(&v.ident.to_string());
        register_ident_emission(&variant_vi, "enum variant/constructor")?;
        push_rewrite_doc(&variant_vi, &mut doc);
        let variant_name = variant_vi.text.clone();
        if v.discriminant.is_some() {
            return Err(GapReason::new(
                Category::Other,
                format!(
                    "enum `{}` variant `{}` has an explicit discriminant — sum types are \
                     structural, not numeric",
                    item.ident, v.ident
                ),
            ));
        }
        match &v.fields {
            Fields::Unit => {
                variant_shapes.push((variant_name.clone(), Vec::new()));
                ctors.push(variant_name);
            }
            Fields::Unnamed(fields) => {
                let mut tys = Vec::with_capacity(fields.unnamed.len());
                for f in &fields.unnamed {
                    let mapped = map_type(&f.ty, None).map_err(|inner| {
                        GapReason::new(
                            Category::PayloadVariant,
                            format!(
                                "enum `{}` variant `{}` has a field type with no confirmed \
                                 mapping ({})",
                                item.ident, v.ident, inner.reason
                            ),
                        )
                    })?;
                    tys.push(mapped);
                }
                ctors.push(format!("{variant_name}({})", tys.join(", ")));
                variant_shapes.push((variant_name, tys));
            }
            Fields::Named(fields) => {
                // Named-field variant `Ctor { a: T, b: U }` -> positional `Ctor(T, U)` (grammar
                // §`constructor` is positional-only). Field types kept, names dropped + recorded
                // never-silently (G2); a field whose type gaps still refuses the whole variant
                // (mapped here so that precise reason wins over the resolvability gate below).
                has_named_variant = true;
                let (tys, names) = map_named_fields_positional(fields, |inner| {
                    GapReason::new(
                        Category::PayloadVariant,
                        format!(
                            "enum `{}` variant `{}` has a field type with no confirmed mapping ({})",
                            item.ident, v.ident, inner
                        ),
                    )
                })?;
                sub_gaps.push(GapReason::new(
                    Category::NamedFieldDrop,
                    format!(
                        "enum `{}` variant `{}` named field(s) `{}` emitted positionally as \
                         `{}({})` — Mycelium's `constructor` is positional-only (no record \
                         surface); product structure preserved, field names dropped",
                        item.ident,
                        v.ident,
                        names.join(", "),
                        v.ident,
                        tys.join(", ")
                    ),
                ));
                ctors.push(format!("{variant_name}({})", tys.join(", ")));
                variant_shapes.push((variant_name, tys));
            }
        }
    }
    // M-1006 resolvability gate (applied *after* mapping so an unmappable field's precise reason
    // wins): an enum with a named-field variant only emits when it resolves in-file — otherwise
    // emitting that variant positionally would introduce an out-of-file reference that poisons the
    // file's `myc check`, costing its clean items. An enum with no named-field variant is unaffected.
    // Gate on the **Rust source ident** (not DN-140 `enum_name` rewrite) — see
    // [`named_field_emit_allowed`].
    if has_named_variant && !named_field_emit_allowed(&item.ident.to_string()) {
        return Err(GapReason::new(
            Category::PayloadVariant,
            format!(
                "enum `{}` has a named-field variant referencing a type not resolvable in-file — \
                 emitting it positionally would introduce an unresolved reference that poisons the \
                 file's `myc check`; left gapped under the M-1006 resolvability gate (VR-5/G2)",
                item.ident
            ),
        ));
    }
    let params_text = if type_params.is_empty() {
        String::new()
    } else {
        format!("[{}]", type_params.join(", "))
    };
    let mut myc = String::new();
    for d in doc_lines(&item.attrs) {
        myc.push_str(&d);
        myc.push('\n');
    }
    for d in &doc {
        myc.push_str(d);
        myc.push('\n');
    }
    myc.push_str(&format!(
        "{}type {}{} = {};",
        pub_prefix(&enum_name),
        enum_name,
        params_text,
        ctors.join(" | ")
    ));
    let (derive_impls, derive_gaps) = lower_enum_derives(
        &enum_name,
        &item.attrs,
        &variant_shapes,
        !type_params.is_empty(),
    );
    for imp in derive_impls {
        myc.push_str("\n\n");
        myc.push_str(&imp);
    }
    sub_gaps.extend(derive_gaps);
    Ok(Emitted {
        name: enum_name,
        myc,
        sub_gaps,
    })
}

/// ONESHOT C2 — classify + lower an enum's `#[derive(...)]` list against the sum-type half of
/// the DN-128 standard-derive set (`PartialEq` → [`derives::eq::compose_enum`], `Debug` →
/// [`derives::show::compose_enum`], `Clone`/`Copy` → satisfied no-op). Mirrors
/// [`lower_struct_derives`]'s orchestration for products: every derive name is either composed,
/// noted as satisfied, or recorded as a sub-gap (G2 — never silently dropped). Bare `Eq` is
/// deliberately NOT recognized (same collision reason as the product row — co-occurs with
/// `PartialEq`). `Hash`/`PartialOrd`/`Ord`/`Default` stay unrecognized on enums for this leaf
/// (product-only rows for those; FLAG residual).
fn lower_enum_derives(
    ty_name: &str,
    attrs: &[Attribute],
    variant_shapes: &[(String, Vec<String>)],
    is_generic: bool,
) -> (Vec<String>, Vec<GapReason>) {
    let mut impls = Vec::new();
    let mut sub_gaps = Vec::new();
    let mut unrecognized = Vec::new();

    // Rebuild EnumVariantSpec views once (lifetime over variant_shapes).
    let specs: Vec<derives::EnumVariantSpec<'_>> = variant_shapes
        .iter()
        .map(|(name, fields)| derives::EnumVariantSpec {
            name: name.as_str(),
            field_types: fields.as_slice(),
        })
        .collect();

    for attr in attrs {
        if !attr.path().is_ident("derive") {
            continue;
        }
        let Ok(list) = attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
        ) else {
            sub_gaps.push(GapReason::new(
                Category::DeriveAttr,
                format!(
                    "dropped derive attribute on enum `{ty_name}` (argument list did not parse \
                     as a plain trait-path list): {}",
                    tokens_to_string(attr)
                ),
            ));
            continue;
        };
        for path in list {
            let name = tokens_to_string(&path);
            match name.as_str() {
                "PartialEq" => {
                    if is_generic {
                        sub_gaps.push(GapReason::new(
                            Category::DeriveAttr,
                            format!(
                                "enum `{ty_name}` derive(PartialEq): generic enum — a derived \
                                 equality fn for a generic type needs DN-130's generic-instance \
                                 mechanism, out of this leaf's scope (ONESHOT C2 / DN-128 enum half)"
                            ),
                        ));
                    } else {
                        match derives::eq::compose_enum(ty_name, &specs) {
                            Ok(myc) => {
                                // ONESHOT C3: same bookkeeping as product PartialEq.
                                record_local_eq_type(ty_name);
                                impls.push(myc);
                            }
                            Err(g) => sub_gaps.push(g),
                        }
                    }
                }
                "Debug" => {
                    if is_generic {
                        sub_gaps.push(GapReason::new(
                            Category::DeriveAttr,
                            format!(
                                "enum `{ty_name}` derive(Debug): generic enum — a derived Show \
                                 impl for a generic type needs DN-130's generic-trait-instance \
                                 mechanism, out of this leaf's scope (ONESHOT C2 / DN-128 enum half)"
                            ),
                        ));
                    } else {
                        match derives::show::compose_enum(ty_name, &specs) {
                            Ok(myc) => impls.push(myc),
                            Err(g) => sub_gaps.push(g),
                        }
                    }
                }
                "Clone" | "Copy" => {
                    sub_gaps.push(GapReason::new(
                        Category::DeriveSatisfied,
                        format!(
                            "enum `{ty_name}` derive({name}) is a satisfied no-op under \
                             Mycelium's value semantics (ADR-003 — every value already copies \
                             structurally; DN-128 §6.1) — not emitted as an impl, not a gap"
                        ),
                    ));
                }
                // Bare Eq co-occurs with PartialEq; recognizing it would double-emit eq_* (same
                // collision the product row documents). Silently skip is wrong (G2) — record as
                // satisfied-via-PartialEq note rather than an unrecognized gap that looks like
                // "we forgot Eq".
                "Eq" => {
                    sub_gaps.push(GapReason::new(
                        Category::DeriveSatisfied,
                        format!(
                            "enum `{ty_name}` derive(Eq): covered by co-occurring PartialEq → \
                             `fn eq_{ty_name}` (product-row collision policy; DN-128 / ONESHOT C2) \
                             — not a second emission"
                        ),
                    ));
                }
                _ => unrecognized.push(name),
            }
        }
    }
    if !unrecognized.is_empty() {
        sub_gaps.push(GapReason::new(
            Category::DeriveAttr,
            format!(
                "enum `{ty_name}` derive(...) names {} not in the DN-128 enum-derive set this \
                 leaf builds (Debug/PartialEq/Clone/Copy recognized; bare Eq is a satisfied \
                 co-occurrence note; Hash/PartialOrd/Ord/Default stay product-only for now) — \
                 dropped, no confirmed Mycelium surface",
                unrecognized.join(", ")
            ),
        ));
    }
    (impls, sub_gaps)
}

/// `struct` -> a single-constructor `type_item`. Unit, all-positional (`Fields::Unnamed`), and
/// **named-field** (`Fields::Named`, M-1006) structs all map to the positional `constructor` surface
/// (named fields emit positionally with names dropped + recorded — see
/// [`map_named_fields_positional`]). A field whose *type* has no mapping still refuses the struct.
pub fn emit_struct(item: &ItemStruct) -> Result<Emitted, GapReason> {
    let struct_vi = valid_ident(&item.ident.to_string());
    register_ident_emission(&struct_vi, "struct type/constructor name")?;
    let struct_name = struct_vi.text.clone();
    let mut ident_doc = Vec::new();
    push_rewrite_doc(&struct_vi, &mut ident_doc);
    let type_params = plain_type_params(&item.generics)?;
    let mut sub_gaps = Vec::new();
    let non_derive = non_doc_non_derive_attrs(&item.attrs);
    if !non_derive.is_empty() {
        sub_gaps.push(GapReason::new(
            Category::DeriveAttr,
            format!(
                "dropped non-doc attribute(s) on struct `{}`: {}",
                item.ident,
                non_derive.join(" ")
            ),
        ));
    }
    let mut field_types: Vec<String> = Vec::new();
    let ctor = match &item.fields {
        Fields::Unit => struct_name.clone(),
        Fields::Unnamed(fields) => {
            let mut tys = Vec::with_capacity(fields.unnamed.len());
            for f in &fields.unnamed {
                let mapped = map_type(&f.ty, None).map_err(|inner| {
                    GapReason::new(
                        Category::Struct,
                        format!(
                            "struct `{}` has a field type with no confirmed mapping ({})",
                            item.ident, inner.reason
                        ),
                    )
                })?;
                tys.push(mapped);
            }
            field_types = tys.clone();
            format!("{struct_name}({})", tys.join(", "))
        }
        Fields::Named(fields) => {
            // Named-field struct `Foo { a: T, b: U }` -> positional `Foo(T, U)` (grammar
            // §`constructor` is positional-only; matches the `lib/std/*.myc` hand-ports, e.g.
            // `type GuaranteeRow = Row(...)`). Field types kept, names dropped + recorded
            // never-silently (G2). Map FIRST so a field whose type has no mapping surfaces its own
            // precise reason (a `String` repr gap, say — an honest gap profile), rather than being
            // masked by the resolvability gate below.
            let (tys, names) = map_named_fields_positional(fields, |inner| {
                GapReason::new(
                    Category::Struct,
                    format!(
                        "struct `{}` has a field type with no confirmed mapping ({})",
                        item.ident, inner
                    ),
                )
            })?;
            // M-1006 resolvability gate: even when every field maps, only emit when this struct
            // resolves in-file — otherwise the emission would introduce an out-of-file reference
            // (e.g. a sibling-crate/kernel type) that poisons the file's `myc check`, costing its
            // clean items. When gated out, keep the honest named-field refusal.
            // Gate on the **Rust source ident** (not DN-140 `struct_name` rewrite) — see
            // [`named_field_emit_allowed`]. Reserved-word structs like std-io `Substrate` rewrite
            // to `Substrate_kw` on emission but stay keyed as `Substrate` in the resolvable set.
            if !named_field_emit_allowed(&item.ident.to_string()) {
                return Err(GapReason::new(
                    Category::Struct,
                    format!(
                        "struct `{}` uses named fields and references a type not resolvable in-file \
                         — emitting it positionally would introduce an unresolved reference that \
                         poisons the file's `myc check`; left gapped under the M-1006 resolvability \
                         gate (VR-5/G2)",
                        item.ident
                    ),
                ));
            }
            sub_gaps.push(GapReason::new(
                Category::NamedFieldDrop,
                format!(
                    "struct `{}` named field(s) `{}` emitted positionally as `{}({})` — Mycelium's \
                     `constructor` is positional-only (no record surface); product structure \
                     preserved, field names dropped (matches `lib/std/*.myc` hand-ports)",
                    item.ident,
                    names.join(", "),
                    item.ident,
                    tys.join(", ")
                ),
            ));
            field_types = tys.clone();
            format!("{struct_name}({})", tys.join(", "))
        }
    };
    let params_text = if type_params.is_empty() {
        String::new()
    } else {
        format!("[{}]", type_params.join(", "))
    };
    let mut myc = String::new();
    for d in doc_lines(&item.attrs) {
        myc.push_str(&d);
        myc.push('\n');
    }
    for d in &ident_doc {
        myc.push_str(d);
        myc.push('\n');
    }
    myc.push_str(&format!(
        "{}type {}{} = {};",
        pub_prefix(&struct_name),
        struct_name,
        params_text,
        ctor
    ));
    // DN-128 (M-1086): lower `#[derive(...)]` against the standard-derive set this leaf builds,
    // appending each successfully-composed impl after the struct's own `type` declaration (joined
    // exactly like `transpile.rs`'s own item-to-item `"\n\n"` join, so a single-item and a
    // multi-item `Emitted.myc` are textually indistinguishable — see `lower_struct_derives` docs).
    let (derive_impls, derive_gaps) = lower_struct_derives(
        &struct_name,
        &item.attrs,
        &field_types,
        !type_params.is_empty(),
    );
    for imp in derive_impls {
        myc.push_str("\n\n");
        myc.push_str(&imp);
    }
    sub_gaps.extend(derive_gaps);
    Ok(Emitted {
        name: struct_name,
        myc,
        sub_gaps,
    })
}

/// Top-level `fn` -> `fn_item`. No `self` (no enclosing impl/trait).
pub fn emit_fn(item: &ItemFn) -> Result<Emitted, GapReason> {
    let fn_vi = valid_ident(&item.sig.ident.to_string());
    register_ident_emission(&fn_vi, "function name")?;
    let fn_name = fn_vi.text.clone();
    let mut ident_doc = Vec::new();
    push_rewrite_doc(&fn_vi, &mut ident_doc);
    check_fn_modifiers(&item.sig)?;
    // ORACLE-R1 A2: before mapping the signature, queue co-emits for any missing guarantee-
    // lattice types the params/return mention (so strength_of etc. never file-poison with
    // `unknown type Strength`). Must run even when map_signature later fails for other reasons —
    // co-emit is driven by the Rust surface, not the mapped text.
    note_lattice_deps_from_sig(&item.sig);
    let sig = map_signature(&item.sig.generics, &item.sig.inputs, &item.sig.output, None)?;
    // DN-125 (M-1081): a free fn's `&mut T` parameter(s) route through the value-threading body
    // emitter instead of the ordinary one (a free fn has no receiver, so only S2 applies here).
    let body = if sig.threaded.is_empty() {
        emit_block_as_expr(&item.block, None, &sig_type_env(&sig))?
    } else {
        emit_mutating_block_as_expr(
            &item.block,
            None,
            &sig_type_env(&sig),
            &sig.threaded,
            sig.threaded_extra_ret.is_some(),
        )?
    };
    let mut sub_gaps = Vec::new();
    let non_doc = non_doc_attrs(&item.attrs);
    if !non_doc.is_empty() {
        sub_gaps.push(GapReason::new(
            Category::DeriveAttr,
            format!(
                "dropped non-doc attribute(s) on fn `{}`: {}",
                item.sig.ident,
                non_doc.join(" ")
            ),
        ));
    }
    let mut doc = doc_lines(&item.attrs);
    doc.extend(ident_doc);
    let myc = render_fn(&fn_name, &sig, &body, &doc, pub_prefix(&fn_name));
    Ok(Emitted {
        name: fn_name,
        myc,
        sub_gaps,
    })
}

/// `trait` -> `trait_item` (`trait Name { fn sig1; fn sig2; ... };`). Every method must have no
/// default body (`trait_item`'s `fn_sig` carries no body) and the trait must have no supertrait
/// bound (no supertrait syntax in the grammar). A method whose signature needs `Self`/`self`
/// still requires a concrete substitution the grammar has no slot for at trait-definition time,
/// so such methods fail their signature mapping (an honest, not a fabricated, "Self" binding).
pub fn emit_trait(item: &ItemTrait) -> Result<Emitted, GapReason> {
    let trait_vi = valid_ident(&item.ident.to_string());
    register_ident_emission(&trait_vi, "trait name")?;
    let trait_name = trait_vi.text.clone();
    let mut ident_doc = Vec::new();
    push_rewrite_doc(&trait_vi, &mut ident_doc);
    if !item.supertraits.is_empty() {
        return Err(GapReason::new(
            Category::Trait,
            format!(
                "trait `{}` has supertrait bound(s) — trait_item grammar has no supertrait \
                 syntax (`'trait' Ident type_params? '{{' ...`)",
                item.ident
            ),
        ));
    }
    let type_params = plain_type_params(&item.generics)?;
    let mut sigs = Vec::with_capacity(item.items.len());
    for ti in &item.items {
        match ti {
            TraitItem::Fn(f) => {
                let method_name =
                    resolve_surface_ident(&f.sig.ident.to_string(), "trait method name")?;
                if f.default.is_some() {
                    return Err(GapReason::new(
                        Category::Trait,
                        format!(
                            "trait `{}` method `{}` has a default body — fn_sig carries no \
                             default implementation",
                            item.ident, f.sig.ident
                        ),
                    ));
                }
                check_fn_modifiers(&f.sig)?;
                let sig = map_signature(&f.sig.generics, &f.sig.inputs, &f.sig.output, None)
                    .map_err(|inner| {
                        GapReason::new(
                            Category::Trait,
                            format!(
                                "trait `{}` method `{}` signature has no confirmed mapping \
                                 (a trait-body `Self`/`self` has no concrete referent in this \
                                 grammar; {})",
                                item.ident, f.sig.ident, inner.reason
                            ),
                        )
                    })?;
                sigs.push(render_fn_sig(&method_name, &sig));
            }
            TraitItem::Const(c) => {
                return Err(GapReason::new(
                    Category::AssocConst,
                    format!(
                        "trait `{}` has an associated const `{}` — trait_item body only allows \
                         fn_sig",
                        item.ident, c.ident
                    ),
                ))
            }
            TraitItem::Type(t) => {
                return Err(GapReason::new(
                    Category::Other,
                    format!(
                        "trait `{}` has an associated type `{}` — no equivalent in trait_item \
                         grammar",
                        item.ident, t.ident
                    ),
                ))
            }
            TraitItem::Macro(_) => {
                return Err(GapReason::new(
                    Category::MacroInvocation,
                    format!("trait `{}` body contains a macro invocation", item.ident),
                ))
            }
            _ => {
                return Err(GapReason::new(
                    Category::Other,
                    format!(
                        "trait `{}` contains an unrecognized trait-item form",
                        item.ident
                    ),
                ))
            }
        }
    }
    let params_text = if type_params.is_empty() {
        String::new()
    } else {
        format!("[{}]", type_params.join(", "))
    };
    let mut myc = String::new();
    for d in doc_lines(&item.attrs) {
        myc.push_str(&d);
        myc.push('\n');
    }
    for d in &ident_doc {
        myc.push_str(d);
        myc.push('\n');
    }
    // Each signature on its own indented line (readability, and consistency with the diff
    // harness's line-prefix `fn `/`type ` extraction — see `src/tests/diff.rs`).
    let sig_lines = sigs
        .iter()
        .map(|s| format!("  {s};"))
        .collect::<Vec<_>>()
        .join("\n");
    myc.push_str(&format!(
        "{}trait {}{} {{\n{}\n}};",
        pub_prefix(&trait_name),
        trait_name,
        params_text,
        sig_lines
    ));
    Ok(Emitted {
        name: trait_name,
        myc,
        sub_gaps: Vec::new(),
    })
}

/// **DN-34 §8.13/8.14 "D4" — inherent-impl associated-function name mangling.**
///
/// `crates/mycelium-l1/src/checkty.rs` (`check_registrations`, M-664) desugars every **inherent**
/// `impl T { fn … }` block's methods to **flat top-level `Item::Fn`s, lifted verbatim** — "the
/// `for_ty` is organizational metadata in v0 (**no qualified `T::m` call syntax yet** …); a name
/// collision with another top-level fn is caught by the duplicate-fn check". So two different
/// types' inherent methods sharing a short name (`Duration::from_nanos` / `MonoInstant::from_nanos`,
/// `Task::new` / `TaskCtx::new` / `Deadlock::new`) are a **real** flat-namespace collision under
/// Mycelium's own desugaring, not a transpiler artifact — DN-34 §8.14 deferred closing this ("D4")
/// while the corpus had zero instances; the Phase-0 re-measure (gap-close-2) found 3.
///
/// The fix is a **length-prefixed mangled name** (DN-140 §7, [`crate::reserved::mangled_inherent_fn_name`])
/// after [`crate::reserved::valid_ident`] on each part — deterministic, EXPLAIN-traceable, and
/// boundary-injective by construction. This
/// intentionally does **not** reuse the hand-authored `lib/compiler/README.md` FLAG-ast-5
/// single-letter-per-type constructor-prefix convention (`Nil`/`MNil`/`SNil` in
/// `lib/std/collections.myc`) — that is a curated human choice per type, not mechanically
/// reproducible by an automated emitter without guessing a mnemonic (VR-5).
///
/// **Scope — no-`self`-receiver methods only (a deliberate, documented safety boundary).**
/// Mangling is applied **only** to inherent-impl methods with **no `self` receiver** (Rust
/// associated functions — typically constructors: `fn new(...) -> Self`). Rust has exactly one
/// calling convention for those — the qualified path call `Type::method(...)` — and
/// `emit.rs`'s `visit_call` **already unconditionally gaps every qualified/associated-function
/// call** (`Category::Other`, "no established Mycelium surface form…"), so **no currently-emitted
/// call site anywhere in this crate ever references a no-`self` method by its bare name** —
/// mangling the declaration cannot desync it from a call site that does not exist. A `self`-
/// receiving method (`fn as_nanos(&self) -> …`), by contrast, **is** reachable from an emitted
/// call site (`visit_method_call`'s generic desugar rewrites `recv.method(args)` to a **bare**
/// `method(recv, args...)`, un-qualified) — mangling *those* declarations would require also
/// re-deriving the identical mangled name at every such call site from the receiver's statically
/// inferred type, which is not always resolvable and is a materially larger, separately-riskier
/// change than this fix's scope. So `self`-receiving methods are left un-mangled here (still
/// subject to the ordinary flat-namespace collision risk the DN-34 §8.14 "D4" residual already
/// named) — a documented, narrower fix, not a silently partial one (G2/VR-5).
///
/// Whether `sig` has a `self`/`&self`/`&mut self` receiver (an ordinary Rust *method*) as opposed
/// to a receiver-less *associated function* (typically a constructor). Only the receiver-less case
/// is eligible for [`crate::reserved::mangled_inherent_fn_name`] — see [`crate::reserved`] for the
/// DN-140 encoding (generic self types like `Foo[T]` are escaped before length-prefix join).
fn has_self_receiver(sig: &Signature) -> bool {
    sig.inputs.iter().any(|a| matches!(a, FnArg::Receiver(_)))
}

// ---- DN-122 §13 (M-1080; WU-A) — the MVP foreign-trait-impl rule-swap ----------------------------
//
// **Verify-first (mitigation #14): there is no "synthetic-trait-def" code path in this crate to
// retire.** DN-34 §8.8 records that a per-file *fabricated* `trait Widen { … }` was tried and
// FAILED (`unknown Self` / arg mismatch / identity fork) — but that attempt was never committed
// here; `emit_impl` has always emitted a trait-impl's methods without ever emitting (or attempting
// to emit) a companion trait declaration for a foreign trait. So there is nothing to delete; this
// increment only ADDS the MVP-recognition path below (a smallest-auditable-step reading of "retire
// the failed synthetic-trait-def path for this class" — VR-5, stated rather than silently assumed).
//
// **What this actually changes.** Per DN-122's ratified OQ-6 (§13.2 WU-B): the MVP's target traits
// are **prelude-seeded** (`crates/mycelium-l1/src/ord3.rs`, mirroring `Fuse`/M-965) — ambiently
// available in every checked phylum, so an eligible impl needs **no `use` at all** (exactly how
// `impl Fuse[T] for T` already needs none; DN-122 §13.1: "the transpiler emits the impl against the
// ambient prelude trait — zero new checker work"). `emit_impl`'s per-method emission loop is
// unchanged either way (it already resolves `Self`/the impl's own type correctly, and already
// naturally supports the receiverless, param-typed methods this MVP class uses); this recognizer's
// only two jobs are: (1) tell an MVP-eligible impl apart from every other trait-impl shape, so (2)
// the emitted `impl` line carries the trait's Mycelium type argument (`[<SelfTy>]`) that Rust's own
// zero-explicit-arg `impl Ord3 for T` source never spells out (Mycelium's stage-1 trait model has no
// implicit `Self` slot — RFC-0019 §4.1 — the `T`-for-`T` idiom `Fuse` already established). A shape
// that does NOT match a registered prelude trait is left **entirely unchanged** — still emitted
// exactly as before WU-A landed (an honest, never-fabricated `myc check`-time residual tracked by
// M-876/M-1076, e.g. every `Widen`/`Narrow`/`MycEq`/`MycOrd`/`MycPartialOrd` impl in the corpus,
// all of which are `Self`-receiver-based and so are correctly excluded below).

/// One prelude trait's checked shape, mirroring its `crates/mycelium-l1/src/<name>.rs` hand-built
/// [`TraitInfo`](../../mycelium_l1/checkty/struct.TraitInfo.html) **exactly** — this is the emitter's
/// half of the T-A3 "emit iff check would accept" agreement (`tests/vet.rs`'s live-oracle probes the
/// other half). Every field here must match the seeded trait 1:1; a mismatch would either wrongly
/// refuse an eligible impl (safe — falls to the honest, unchanged path) or, far worse, wrongly emit
/// a `use`-free `impl` the checker then refuses (never allowed to happen — the shared-case-table unit
/// test in `src/tests/emit.rs` pins agreement against the real registry, not a re-typed copy).
struct PreludeTraitShape {
    /// The trait's name — identical on both the Rust source side and the Mycelium prelude side (the
    /// MVP recognizes a foreign trait **by name**; it never renames/reinterprets a differently-named
    /// Rust trait as a prelude one — that would be exactly the kind of guess VR-5 forbids).
    name: &'static str,
    /// Every method the trait requires, in the prelude `TraitInfo`'s own declared order (the impl's
    /// method SET must match exactly — no fewer, no more, per RFC-0019 §4.5's impl-method-set check;
    /// order itself is not significant here, only names/arity/shape are).
    methods: &'static [PreludeMethodShape],
}

/// One method's MVP-recognized shape: receiverless, every value parameter typed either `Self` or the
/// impl's own concrete `for`-type (the single-param, `T`-for-`T` idiom every prelude trait in this
/// registry uses — mirrors `Fuse::join(a: T, b: T) => T`), and a return type that maps to exactly
/// `ret` (a primitive repr text, e.g. `"Binary{8}"` for `Ord3::cmp` — never `Self`, in this v0
/// registry; a prelude trait whose method RETURNS `Self` is not yet a registered shape, YAGNI until
/// one is needed).
struct PreludeMethodShape {
    name: &'static str,
    /// Value-parameter count; every parameter must be `Self`/the impl's own type (never a second,
    /// unrelated concrete type — that would be exactly the M-1076 residual, not this MVP).
    arity: usize,
    /// The exact [`map_type`]-produced return-type text a matching method must have.
    ret: &'static str,
}

/// The MVP's registered prelude traits (DN-122 §13.2 WU-B) — kept intentionally tiny (KISS/YAGNI):
/// exactly the `Ord3` witness DN-122 §13.1's shape (with the `Binary{8}` width deviation `crates/mycelium-l1/src/ord3.rs` documents; `Ord3[A] { fn cmp(a: A, b: A) => Binary{8};
/// }`). Growing this registry (a new prelude trait) is always a **paired** change with
/// `crates/mycelium-l1/src/<name>.rs` — never one side alone (that would silently desync emit from
/// check, exactly what T-A3 exists to catch).
const MVP_PRELUDE_TRAITS: &[PreludeTraitShape] = &[PreludeTraitShape {
    name: "Ord3",
    methods: &[PreludeMethodShape {
        name: "cmp",
        arity: 2,
        ret: "Binary{8}",
    }],
}];

/// Does `ty` (an original, unmapped `syn::Type`) spell `Self`, or literally the same tokens as
/// `self_ty` (the impl's own `syn::Type`)? The two Rust idioms a receiverless method in an `impl
/// Trait for ConcreteType` block can use for "the type this impl is for" — never a guess at a THIRD,
/// unrelated type (VR-5).
fn type_is_self_or_impl_ty(ty: &syn::Type, self_ty: &syn::Type) -> bool {
    if let syn::Type::Path(tp) = ty {
        if tp.qself.is_none() && tp.path.is_ident("Self") {
            return true;
        }
    }
    tokens_to_string(ty) == tokens_to_string(self_ty)
}

/// Is `item` an **MVP-eligible foreign-trait impl** (DN-122 §13.1: single-parameter, param-only-sig)
/// matching a [`MVP_PRELUDE_TRAITS`] entry by name? `Some(shape)` iff: (i) the impl has no explicit
/// trait type-argument (`trait_targs.is_empty()` — the Rust-side idiom for a trait whose sole
/// Mycelium parameter is the impl's own `Self`, mirroring `Fuse`'s `impl Fuse[T] for T`); (ii) the
/// impl's method SET matches the registered shape exactly (same names, same count — RFC-0019 §4.5);
/// (iii) every method is **receiverless** (`has_self_receiver` false — the exact test that correctly
/// EXCLUDES `Widen`/`Narrow`/`MycEq`/`MycOrd`/`MycPartialOrd`, every one of which takes a `self`/
/// `&self` receiver, per DN-122 §13.1's adversarial narrowing, §13.3 finding 3); (iv) every value
/// parameter is `Self`/the impl's own type ([`type_is_self_or_impl_ty`]); (v) the return type maps
/// (via [`map_type`]) to exactly the registered primitive text. Any mismatch returns `None` — the
/// impl then falls through to the ordinary, unchanged emission path (never a partial/guessed match).
fn mvp_prelude_trait_shape<'a>(
    trait_name: &str,
    trait_targs: &[String],
    self_ty: &syn::Type,
    self_ty_text: &str,
    items: &[ImplItem],
) -> Option<&'a PreludeTraitShape> {
    if !trait_targs.is_empty() {
        return None;
    }
    let shape = MVP_PRELUDE_TRAITS.iter().find(|s| s.name == trait_name)?;
    let methods: Vec<&syn::ImplItemFn> = items
        .iter()
        .filter_map(|ii| match ii {
            ImplItem::Fn(f) => Some(f),
            _ => None,
        })
        .collect();
    if methods.len() != shape.methods.len() {
        return None;
    }
    for expected in shape.methods {
        let f = methods.iter().find(|f| f.sig.ident == expected.name)?;
        if has_self_receiver(&f.sig) {
            return None;
        }
        if !f.sig.generics.params.is_empty() {
            return None;
        }
        let value_params: Vec<&syn::PatType> = f
            .sig
            .inputs
            .iter()
            .map(|a| match a {
                FnArg::Typed(pt) => Some(pt),
                FnArg::Receiver(_) => None,
            })
            .collect::<Option<Vec<_>>>()?;
        if value_params.len() != expected.arity {
            return None;
        }
        if !value_params
            .iter()
            .all(|pt| type_is_self_or_impl_ty(&pt.ty, self_ty))
        {
            return None;
        }
        let ReturnType::Type(_, ret_ty) = &f.sig.output else {
            return None;
        };
        let mapped_ret = map_type(ret_ty, Some(self_ty_text)).ok()?;
        if mapped_ret != expected.ret {
            return None;
        }
    }
    Some(shape)
}

/// `impl` -> `impl_item` (trait-instance or inherent form). Unlike enum/struct/trait (which bail
/// the whole item on the first unmappable feature), an impl block is emitted **partially**: each
/// method is attempted independently, a failing method becomes a sub-gap rather than voiding its
/// siblings, and the impl counts as "emitted" as long as at least one method landed. This is a
/// deliberate, documented asymmetry (Declared design choice) — impl methods are far more
/// independent of each other than, say, a trait's default-body/supertrait obligations are of its
/// sibling methods.
pub fn emit_impl(item: &ItemImpl) -> Result<Emitted, GapReason> {
    // DN-131 (Accepted; M-1088/M-1101 build) — the Mycelium `impl_item` grammar's INHERENT tail
    // now HAS a generic-parameter declaration slot: `impl[T] Foo[T]` (DN-103, unbounded) and
    // `impl[T: Bound] Foo[T]` (DN-131, bounded), both landed at the kernel/L1 level
    // (`parse_type_params_bounded` reused verbatim for the impl slot; DN-103's Phase-0 desugar
    // carries the bound onto each lifted method, discharged by the already-landed
    // `check_bounds` + dictionary-free monomorphizer — zero new discharge code, DN-131 §4).
    // This function previously refused ANY impl-level generic parameter unconditionally — a
    // comment/gate that predated DN-103/DN-131's kernel-side landing; it now emits the
    // inherent-impl slot's type-parameter list, carrying each parameter's bound (if any)
    // through verbatim.
    //
    // Scope boundary (never-silent, G2) — DN-131 authorizes ONLY the inherent-impl slot:
    //   - a **trait-instance** impl (`impl<..> Trait for ..`) with a non-empty generics list is
    //     a *different* grammar production + coherence question (DN-130's scope, not yet
    //     built) — still gapped explicitly, unchanged from before this leaf;
    //   - a **lifetime** or **const-generic** impl-level parameter has no confirmed grammar
    //     surface (mirrors `plain_type_params`'s refusal for the same shapes) — gapped;
    //   - a bound that is not a *plain trait name* (carries type arguments, a `?`-relaxed
    //     modifier, a higher-ranked `for<'a>` binder, or is parenthesized) has no confirmed v1
    //     mapping (DN-131 v1 emits plain trait-name bounds only) — gapped, never guessed;
    //   - an impl `where` clause still has no Mycelium equivalent (DN-131 §3: inline bounds
    //     only, no `where` in v1) — gapped, unchanged from before.
    let impl_type_params = if item.trait_.is_some() {
        if !item.generics.params.is_empty() {
            return Err(GapReason::new(
                Category::GenericBound,
                "impl-level generic parameter(s) on a *trait-instance* impl (`impl<..> Trait \
                 for ..`) have no confirmed mapping yet — DN-130 (parametric trait-instance \
                 heads + coherence) is the unbuilt scope for that case; DN-131 authorizes only \
                 the inherent-impl slot",
            ));
        }
        Vec::new()
    } else {
        bounded_impl_type_params(&item.generics)?
    };
    if item.generics.where_clause.is_some() {
        return Err(GapReason::new(
            Category::WhereClause,
            "impl `where` clause has no Mycelium equivalent",
        ));
    }
    let self_ty_text = map_type(&item.self_ty, None).map_err(|inner| {
        GapReason::new(
            Category::Impl,
            format!(
                "impl target type `{}` has no confirmed mapping ({})",
                tokens_to_string(&*item.self_ty),
                inner.reason
            ),
        )
    })?;

    let (trait_name, trait_targs) = if let Some((_, trait_path, _)) = &item.trait_ {
        let seg = trait_path
            .segments
            .last()
            .ok_or_else(|| GapReason::new(Category::Impl, "impl trait path is empty"))?;
        let _trait_head = resolve_surface_ident(&seg.ident.to_string(), "impl trait name")?;
        let targs =
            match &seg.arguments {
                PathArguments::None => Vec::new(),
                PathArguments::AngleBracketed(ab) => {
                    let mut v = Vec::with_capacity(ab.args.len());
                    for ga in &ab.args {
                        match ga {
                            GenericArgument::Type(t) => v.push(map_type(t, Some(&self_ty_text))?),
                            _ => return Err(GapReason::new(
                                Category::GenericBound,
                                "trait type argument is not a plain type (lifetime/const arg) — \
                                 no confirmed mapping",
                            )),
                        }
                    }
                    v
                }
                PathArguments::Parenthesized(_) => return Err(GapReason::new(
                    Category::GenericBound,
                    "parenthesized trait arguments (`Fn`-trait sugar) have no confirmed mapping",
                )),
            };
        (Some(seg.ident.to_string()), targs)
    } else {
        (None, Vec::new())
    };

    // DN-122 §13 (M-1080; WU-A) — the MVP-prelude-trait recognizer (see the module doc block just
    // above `emit_impl`). `None` for every non-eligible shape (including every impl with no trait
    // at all, or any impl whose trait name isn't registered) — the rest of this function's emission
    // logic is completely unchanged by that case, exactly the "leave it as an honest, unfabricated
    // residual" DN-122 §13.2 calls for.
    let mvp_shape = trait_name.as_deref().and_then(|name| {
        mvp_prelude_trait_shape(
            name,
            &trait_targs,
            &item.self_ty,
            &self_ty_text,
            &item.items,
        )
    });

    // **File-gated myc-check poison (2026-07-16 remeasure / express gap-close).** Emitting
    // `impl Trait` for a non-prelude foreign trait produces `unknown trait` CheckErrors that
    // zero an entire file's `checked_fraction`. DN-122 MVP only seeds Ord3 (+ derive Show/Init);
    // synthetic trait defs failed (DN-34 §8.8). **Default → Init** remaps (DN-129). **Widen**
    // with a resolvable width_cast body emits as a free `fn` (no trait wrapper) — keeps the
    // DN-41 width_cast path without ambient `trait Widen`. Other non-prelude trait-impls gap
    // wholesale (G2).
    let (trait_name, trait_targs, mvp_shape, default_to_init, widen_free_fn) =
        match (trait_name, trait_targs, mvp_shape) {
            (Some(n), targs, None) if n == "Default" && targs.is_empty() => {
                (Some("Init".to_string()), targs, None, true, false)
            }
            (Some(n), targs, None) if n == "Widen" => {
                // Free-fn path: still emit width_cast bodies without `impl Widen … for …`.
                (Some(n), targs, None, false, true)
            }
            // Non-prelude foreign traits only (Ord3/Show/Init/Fuse/Fault keep prior emit
            // paths even when MVP shape doesn't match — e.g. self-receiver Ord3 residual).
            (Some(n), _targs, None)
                if !matches!(
                    n.as_str(),
                    "Ord3" | "Show" | "Init" | "Fuse" | "Fault" | "Default"
                ) =>
            {
                return Err(GapReason::new(
                    Category::Impl,
                    format!(
                        "trait-impl of non-prelude trait `{n}` — no ambient trait definition \
                         and synthetic trait emission is refused (DN-34 §8.8 / DN-122 MVP); \
                         gapped the whole impl so the residual cannot file-poison myc-check \
                         (G2; express gap-close 2026-07-16)"
                    ),
                ));
            }
            (n, targs, shape) => (n, targs, shape, false, false),
        };

    let mut sub_gaps = Vec::new();
    let mut method_bodies = Vec::new();
    for ii in &item.items {
        match ii {
            ImplItem::Fn(f) => {
                // DN-41 §2: `Narrow::narrow` is fallible (`Result<To, NarrowError>`) — no
                // `= expr fn_item` body can express a Result-returning refuse in this grammar
                // fragment, regardless of whether `Self`/the target type otherwise map. Intercept
                // before signature mapping so the recorded reason cites the real cause (DN-41)
                // rather than the incidental `Result<..>` generic-type-path gap that would
                // otherwise fire first and obscure it.
                if trait_name.as_deref() == Some("Narrow") && f.sig.ident == "narrow" {
                    sub_gaps.push(GapReason::new(
                        Category::Conversion,
                        "impl method `narrow`: DN-41 (docs/notes/DN-41-Width-Cast-Prim.md §2) \
                         specifies narrowing as fallible — `Result<To, NarrowError>`, refusing \
                         on an out-of-range/non-representable value — but this grammar \
                         fragment's `fn_item` body is a single `= expr` with no \
                         Result-returning surface to express that refuse; left an explicit gap \
                         rather than forced (VR-5)",
                    ));
                    continue;
                }
                if let Err(e) = check_fn_modifiers(&f.sig) {
                    sub_gaps.push(GapReason::new(
                        e.category,
                        format!("impl method `{}`: {}", f.sig.ident, e.reason),
                    ));
                    continue;
                }
                let width_cast_body = try_width_cast_widen_body(
                    trait_name.as_deref(),
                    &f.sig.ident.to_string(),
                    &self_ty_text,
                    &trait_targs,
                );
                match map_signature(
                    &f.sig.generics,
                    &f.sig.inputs,
                    &f.sig.output,
                    Some(&self_ty_text),
                ) {
                    Ok(sig) => {
                        // DN-125 (M-1081): a `&mut self`/`&mut T`-value-threaded method's body
                        // routes through `emit_mutating_block_as_expr` instead of the ordinary
                        // let-chain emitter. MEDIUM fix (strict review of PR #1527):
                        // `sig.threaded.is_empty()` is checked FIRST — a threaded signature's
                        // return type is the mutated-value tuple/type, which `width_cast_body`
                        // (a `Widen`-shaped, non-threaded `Self`-return convention) never
                        // accounts for, so a threaded signature must never let `width_cast_body`
                        // win even if `try_width_cast_widen_body` happened to also fire for the
                        // same method name/trait shape.
                        let body_result = if sig.threaded.is_empty() {
                            match &width_cast_body {
                                Some(body) => Ok(body.clone()),
                                None => emit_block_as_expr(
                                    &f.block,
                                    Some(&self_ty_text),
                                    &sig_type_env(&sig),
                                ),
                            }
                        } else {
                            debug_assert!(
                                width_cast_body.is_none(),
                                "a width_cast Widen-shaped body should never coincide with a \
                                 DN-125 threaded &mut signature — the two body conventions are \
                                 mutually exclusive by construction (see this match's doc)"
                            );
                            emit_mutating_block_as_expr(
                                &f.block,
                                Some(&self_ty_text),
                                &sig_type_env(&sig),
                                &sig.threaded,
                                sig.threaded_extra_ret.is_some(),
                            )
                        };
                        match body_result {
                            Ok(body) => {
                                let non_doc = non_doc_attrs(&f.attrs);
                                if !non_doc.is_empty() {
                                    sub_gaps.push(GapReason::new(
                                        Category::DeriveAttr,
                                        format!(
                                            "dropped non-doc attribute(s) on method `{}`: {}",
                                            f.sig.ident,
                                            non_doc.join(" ")
                                        ),
                                    ));
                                }
                                let mut doc = doc_lines(&f.attrs);
                                if width_cast_body.is_some() {
                                    doc.push(
                                        "// Declared: body emitted via width_cast (DN-41 real \
                                         prim, docs/notes/DN-41-Width-Cast-Prim.md §2) — the \
                                         Binary{M} width witness is a synthesized all-zero BinLit \
                                         (RFC-0020 §Representation-tagged literals); unvalidated \
                                         by a Mycelium checker (crate-level Declared guarantee, \
                                         see src/lib.rs)."
                                            .to_string(),
                                    );
                                }
                                // DN-34 §8.13/8.14 "D4" + express gap-close (2026-07-16):
                                // Mycelium lifts inherent-impl methods to flat top-level `fn`s
                                // (M-664). Collision policy:
                                //   - no-`self` methods: always mangle (pre-existing D4);
                                //   - self-receivers: mangle when the bare name was already
                                //     emitted (fixes multi-type `as_nanos` without renaming the
                                //     first occurrence).
                                // Trait-impl methods stay under `impl Trait for T` (or free-fn
                                // Widen path below).
                                let raw_method = if default_to_init && f.sig.ident == "default" {
                                    "init".to_string()
                                } else {
                                    f.sig.ident.to_string()
                                };
                                let emitted_fn_name = if widen_free_fn {
                                    // Free function, not nested in impl Trait — mangle with both
                                    // self and target widths. i8 and u8 both map to Binary{8}, so
                                    // de-dupe: second identical free-fn is skipped (sub-gapped).
                                    let targ =
                                        trait_targs.first().map(|s| s.as_str()).unwrap_or("?");
                                    let mangled = mangled_inherent_fn_name(
                                        &format!("{self_ty_text}_to_{targ}"),
                                        "widen",
                                    );
                                    if bare_fn_name_taken(&mangled) {
                                        sub_gaps.push(GapReason::new(
                                            Category::Impl,
                                            format!(
                                                "Widen free-fn `{mangled}` already emitted for \
                                                 this Binary width pair (signed+unsigned collapse \
                                                 to the same Binary{{N}}); de-duplicated, not \
                                                 double-emitted (G2)"
                                            ),
                                        ));
                                        continue;
                                    }
                                    doc.push(format!(
                                        "// Declared: Widen free-fn emit `{mangled}` (no ambient \
                                         trait Widen — DN-34 §8.8; width_cast body DN-41) so \
                                         myc-check is not file-poisoned by `unknown trait Widen` \
                                         (express gap-close 2026-07-16)."
                                    ));
                                    record_local_mangled_assoc_fn(&mangled, &sig.params, &sig.ret);
                                    record_bare_fn_name(&mangled);
                                    mangled
                                } else if trait_name.is_none() {
                                    let bare =
                                        resolve_surface_ident(&raw_method, "impl method name")?;
                                    let must_mangle = !has_self_receiver(&f.sig)
                                        || local_mangled_assoc_fn_known(&bare)
                                        || bare_fn_name_taken(&bare);
                                    if must_mangle {
                                        let mangled =
                                            mangled_inherent_fn_name(&self_ty_text, &raw_method);
                                        doc.push(format!(
                                            "// Declared: renamed `impl {} {{ fn {} }}` -> \
                                             `{mangled}` (D4 inherent-impl flat-fn desugar + \
                                             DN-140 length-prefix mangle, M-664).",
                                            self_ty_text, f.sig.ident,
                                        ));
                                        record_local_mangled_assoc_fn(
                                            &mangled,
                                            &sig.params,
                                            &sig.ret,
                                        );
                                        record_bare_fn_name(&mangled);
                                        mangled
                                    } else {
                                        // Bare un-mangled inherent method — still record param
                                        // widths under the bare name so same-file calls can
                                        // rewrite lit args (ORACLE-R1 A5).
                                        record_local_mangled_assoc_fn(&bare, &sig.params, &sig.ret);
                                        record_bare_fn_name(&bare);
                                        bare
                                    }
                                } else {
                                    resolve_surface_ident(&raw_method, "impl method name")?
                                };
                                // Lifted inherent-impl methods are never a cross-nodule `use`
                                // target in the corpus's own Rust source shape (Rust imports a
                                // free fn by name via `use`, never an inherent method that way —
                                // `Type::method(...)` is a qualified call, not an import), so the
                                // pub-needed gate never applies here — always `""`.
                                method_bodies.push(render_fn(
                                    &emitted_fn_name,
                                    &sig,
                                    &body,
                                    &doc,
                                    "",
                                ));
                            }
                            Err(e) => sub_gaps.push(GapReason::new(
                                e.category,
                                format!("impl method `{}` body: {}", f.sig.ident, e.reason),
                            )),
                        }
                    }
                    Err(e) => sub_gaps.push(GapReason::new(
                        e.category,
                        format!("impl method `{}` signature: {}", f.sig.ident, e.reason),
                    )),
                }
            }
            ImplItem::Const(c) => sub_gaps.push(GapReason::new(
                Category::AssocConst,
                format!("impl associated const `{}`", c.ident),
            )),
            ImplItem::Type(t) => sub_gaps.push(GapReason::new(
                Category::Other,
                format!("impl associated type `{}`", t.ident),
            )),
            ImplItem::Macro(_) => sub_gaps.push(GapReason::new(
                Category::MacroInvocation,
                "impl body contains a macro invocation".to_string(),
            )),
            _ => sub_gaps.push(GapReason::new(
                Category::Other,
                "impl contains an unrecognized impl-item form".to_string(),
            )),
        }
    }

    if method_bodies.is_empty() {
        let reason = if sub_gaps.is_empty() {
            "impl block has no items".to_string()
        } else {
            // Fold every sub-issue's own reason into the top-level gap's reason text. When an
            // impl fails wholesale (this arm), its `sub_gaps` are otherwise discarded — they are
            // only surfaced as separate `Gap` records via `emit::Emitted::sub_gaps` on the
            // *success* path (see `Outcome::Emitted` in `transpile.rs`). Folding them here keeps
            // this failure path never-silent too (G2): the specific reason (e.g. "no established
            // Mycelium surface form for `from(...)`") is never lost behind a generic count.
            let details = sub_gaps
                .iter()
                .map(|g| g.reason.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "no member of this impl block could be transpiled ({} sub-issue(s)): {details}",
                sub_gaps.len()
            )
        };
        return Err(GapReason::new(Category::Impl, reason));
    }

    // Each method (and, when present, its own doc-comment lines) indented inside an `impl`
    // block — same readability/extraction rationale as `emit_trait`'s `sig_lines`. Free-fn
    // Widen path skips the indent (top-level `fn`s). `render_fn` may span multiple lines.
    let body_text = method_bodies
        .iter()
        .map(|m| {
            if widen_free_fn {
                m.clone()
            } else {
                m.lines()
                    .map(|l| format!("  {l}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut myc = String::new();
    for d in doc_lines(&item.attrs) {
        myc.push_str(&d);
        myc.push('\n');
    }
    if mvp_shape.is_some() || default_to_init {
        // DN-122 §13 (M-1080; WU-A) / DN-129 Default→Init — EXPLAIN-traceable provenance (G2).
        myc.push_str(
            "// Declared: prelude-trait remapping — foreign-trait impl of a prelude-seeded \
             single-param trait (DN-122 Ord3 MVP and/or DN-129 Default→Init); the `[<SelfTy>]` \
             argument below is synthesized (Rust's zero-explicit-arg `impl Trait for T` never \
             spells it — Mycelium stage-1 has no implicit Self slot, RFC-0019 §4.1).\n",
        );
    }
    let name = if widen_free_fn {
        // Free functions only — body_text lines are already full `fn …;` rows (no impl wrap).
        myc.push_str(&body_text);
        format!(
            "widen_free {} -> {}",
            self_ty_text,
            trait_targs.first().map(|s| s.as_str()).unwrap_or("?")
        )
    } else if let Some(trait_name) = trait_name {
        let targs_text = if mvp_shape.is_some() || default_to_init {
            // The MVP `T`-for-`T` idiom (mirrors `Fuse`/`Init`): sole Mycelium parameter IS Self.
            format!("[{self_ty_text}]")
        } else if trait_targs.is_empty() {
            String::new()
        } else {
            format!("[{}]", trait_targs.join(", "))
        };
        myc.push_str(&format!(
            "impl {trait_name}{targs_text} for {self_ty_text} {{\n{body_text}\n}};"
        ));
        // Include type-args in the name so e.g. `impl Widen<u32> for bool` and
        // `impl Widen<u64> for bool` don't collide in `emitted_items`.
        format!("impl {trait_name}{targs_text} for {self_ty_text}")
    } else {
        // DN-131: the inherent-impl slot's own type-param list (`""` when the impl carries no
        // generic parameters at all — byte-identical to the pre-DN-131 text in that case, the
        // regression guard for the overwhelmingly common non-generic impl).
        let impl_type_params_text = if impl_type_params.is_empty() {
            String::new()
        } else {
            format!("[{}]", impl_type_params.join(", "))
        };
        myc.push_str(&format!(
            "impl{impl_type_params_text} {self_ty_text} {{\n{body_text}\n}};"
        ));
        format!("impl{impl_type_params_text} {self_ty_text}")
    };
    Ok(Emitted {
        name,
        myc,
        sub_gaps,
    })
}
