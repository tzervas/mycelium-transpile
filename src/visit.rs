//! A shared, single-sited dispatch layer over the `syn::Expr`/`syn::Type` shapes this crate
//! translates — the M-1041 Scope-A DRY force-multiplier.
//!
//! **Verified tax (mitigation #14, before this module landed):** three independently hand-rolled
//! `match`/`if-let` dispatches over `syn::Expr` variants lived in `emit.rs`
//! (`emit_expr_inner`'s ~19-arm translation match, `expr_env_type`'s 3-arm "is this a
//! transparent-wrapper or a known local?" match, and `known_struct_literal_ty`'s 1-arm
//! `Expr::Struct` extractor), and two independently hand-rolled dispatches over `syn::Type`
//! variants lived in `map.rs` (`map_type_inner`'s translation match and
//! `field_type_user_deps`'s *separately maintained mirror* of the same shapes — `map.rs`'s own
//! doc comment on `field_type_user_deps` names the drift risk explicitly: "this deliberately
//! mirrors `map_type`'s mappable shapes; if the two drift…"). Each of those five functions wrote
//! its own raw `match`, so recognizing a new `syn::Expr`/`syn::Type` shape meant finding and
//! updating every one of them by hand, with no compiler-enforced link between them.
//!
//! This module gives every such site **one** canonical dispatcher (`walk_expr`/`walk_type`) over
//! **one** canonical exhaustive match, plus an `ExprVisitor`/`TypeVisitor` trait with one method
//! per recognized variant (each defaulting to `fallback`). A consumer that cares about *every*
//! variant (the emitter, the type mapper) overrides every method, exactly reproducing today's
//! per-arm bodies with no behavior change. A consumer that cares about only a *few* variants
//! (`expr_env_type`'s 3, `known_struct_literal_ty`'s 1, `field_type_user_deps`'s mirror of
//! `map_type`'s 3) overrides only those and inherits `fallback` for the rest — no more
//! hand-written catch-alls to keep in sync. Recognizing a **new** variant now touches the trait
//! definition (one new method + a `walk_*` arm) plus whichever visitor(s) want real behavior for
//! it — never a silent, independently-drifting fourth or sixth match.
//!
//! **Guarantee: `Declared`.** A pure structural refactor over already-`Declared` heuristic
//! classification logic (see `emit.rs`/`map.rs` module docs) — it changes no emitted `.myc` text
//! and no `GapReason` message. Verified **byte-identical**: `cargo test -p mycelium-transpile`
//! passes with the same 65 tests, same results, before and after this module landed (no test
//! assertion needed updating).

use syn::{Expr, Type};

// -------------------------------------------------------------------------------------------
// `syn::Expr` dispatch (emit.rs: `emit_expr_inner`, `expr_env_type`, `known_struct_literal_ty`).
// -------------------------------------------------------------------------------------------

/// A visitor over the `syn::Expr` shapes this crate recognizes. One method per shape, each
/// defaulting to [`ExprVisitor::fallback`] — so a visitor that only cares about a handful of
/// shapes overrides only those. [`walk_expr`] is the single canonical dispatcher; a variant not
/// listed here (or not overridden by a given visitor) always reaches `fallback`, so every
/// consumer stays never-silent by construction (no shape is ever dropped, only routed to the
/// visitor's own honest "I don't handle this" answer).
///
/// Each method receives both the whole `expr: &Expr` (for a fallback/gap message that needs the
/// full original token text, e.g. "unsupported expression form `{expr}`") and the narrowed inner
/// node — so a method's default body (`self.fallback(expr)`) is always well-formed without the
/// visitor needing to reconstruct the outer `Expr`.
pub(crate) trait ExprVisitor {
    /// This visitor's result type — `Result<String, GapReason>` for the emitter,
    /// `Option<String>` for a narrow type/shape probe, `bool` for a resolvability check, etc.
    type Output;

    /// Called for a `syn::Expr` shape this visitor does not override, AND for a shape whose
    /// *guard* an override itself decides not to satisfy (e.g. a qualified `Expr::Path`, an `if
    /// let` condition, a labeled block) — mirroring how the pre-refactor hand-written `match`
    /// arms fell through to their own `_`/failure case. Every default method below delegates
    /// here.
    fn fallback(&mut self, expr: &Expr) -> Self::Output;

    fn visit_path(&mut self, expr: &Expr, _p: &syn::ExprPath) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_lit(&mut self, expr: &Expr, _l: &syn::ExprLit) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_if(&mut self, expr: &Expr, _e: &syn::ExprIf) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_match(&mut self, expr: &Expr, _m: &syn::ExprMatch) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_binary(&mut self, expr: &Expr, _b: &syn::ExprBinary) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_unary(&mut self, expr: &Expr, _u: &syn::ExprUnary) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_call(&mut self, expr: &Expr, _c: &syn::ExprCall) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_method_call(&mut self, expr: &Expr, _m: &syn::ExprMethodCall) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_paren(&mut self, expr: &Expr, _p: &syn::ExprParen) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_reference(&mut self, expr: &Expr, _r: &syn::ExprReference) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_tuple(&mut self, expr: &Expr, _t: &syn::ExprTuple) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_array(&mut self, expr: &Expr, _a: &syn::ExprArray) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_repeat(&mut self, expr: &Expr, _r: &syn::ExprRepeat) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_block(&mut self, expr: &Expr, _b: &syn::ExprBlock) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_field(&mut self, expr: &Expr, _f: &syn::ExprField) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_struct(&mut self, expr: &Expr, _s: &syn::ExprStruct) -> Self::Output {
        self.fallback(expr)
    }
    fn visit_cast(&mut self, expr: &Expr, _c: &syn::ExprCast) -> Self::Output {
        self.fallback(expr)
    }
    /// DN-118 Phase 1 (the closure-EMIT pass): a `syn::ExprClosure` (`|a, b| …`). Added alongside
    /// the pre-existing 17 shapes — a consumer that does not override this still falls through to
    /// its own `fallback`, exactly like every other never-silent-by-construction shape here.
    fn visit_closure(&mut self, expr: &Expr, _c: &syn::ExprClosure) -> Self::Output {
        self.fallback(expr)
    }
}

/// The single canonical `syn::Expr` dispatch (see module docs) — every `Expr::*`-matching
/// consumer in this crate routes through this one match instead of writing its own. `syn::Expr`
/// is `#[non_exhaustive]` and this crate only ever recognizes a fixed subset of its shapes, so
/// every other variant (and any future `syn` addition) falls to `v.fallback(expr)`, exactly as
/// the pre-refactor per-consumer `_ =>` catch-alls did.
pub(crate) fn walk_expr<V: ExprVisitor + ?Sized>(expr: &Expr, v: &mut V) -> V::Output {
    match expr {
        Expr::Path(p) => v.visit_path(expr, p),
        Expr::Lit(l) => v.visit_lit(expr, l),
        Expr::If(e) => v.visit_if(expr, e),
        Expr::Match(m) => v.visit_match(expr, m),
        Expr::Binary(b) => v.visit_binary(expr, b),
        Expr::Unary(u) => v.visit_unary(expr, u),
        Expr::Call(c) => v.visit_call(expr, c),
        Expr::MethodCall(m) => v.visit_method_call(expr, m),
        Expr::Paren(p) => v.visit_paren(expr, p),
        Expr::Reference(r) => v.visit_reference(expr, r),
        Expr::Tuple(t) => v.visit_tuple(expr, t),
        Expr::Array(a) => v.visit_array(expr, a),
        Expr::Repeat(r) => v.visit_repeat(expr, r),
        Expr::Block(b) => v.visit_block(expr, b),
        Expr::Field(f) => v.visit_field(expr, f),
        Expr::Struct(s) => v.visit_struct(expr, s),
        Expr::Cast(c) => v.visit_cast(expr, c),
        Expr::Closure(c) => v.visit_closure(expr, c),
        _ => v.fallback(expr),
    }
}

// -------------------------------------------------------------------------------------------
// `syn::Type` dispatch (map.rs: `map_type_inner`, `field_type_user_deps`).
// -------------------------------------------------------------------------------------------

/// The `syn::Type` twin of [`ExprVisitor`] — same shape, same rationale: `map_type_inner`
/// (the translation) and `field_type_user_deps` (the M-1006 resolvability-fixpoint mirror) each
/// wrote their own `match` over the same three `Type` shapes (`Type::Path`/`Type::Tuple`/
/// `Type::Reference`); `field_type_user_deps`'s own doc comment already named the drift risk.
/// One [`walk_type`] dispatcher now backs both.
pub(crate) trait TypeVisitor {
    type Output;

    /// Called for a `syn::Type` shape not overridden (or not matched by an override's own
    /// guard) — see [`ExprVisitor::fallback`]'s identical rationale.
    fn fallback(&mut self, ty: &Type) -> Self::Output;

    fn visit_path(&mut self, ty: &Type, _p: &syn::TypePath) -> Self::Output {
        self.fallback(ty)
    }
    fn visit_tuple(&mut self, ty: &Type, _t: &syn::TypeTuple) -> Self::Output {
        self.fallback(ty)
    }
    fn visit_reference(&mut self, ty: &Type, _r: &syn::TypeReference) -> Self::Output {
        self.fallback(ty)
    }
    /// L-MAP (DN-99 register rows 15/35): a native Rust slice type `[T]` — reached either bare
    /// (rare; `[T]` is unsized in real Rust and normally appears only behind an indirection) or,
    /// far more commonly, as the referent `visit_reference` recurses into once `&[T]`'s `&` is
    /// erased. Defaults to `fallback` like every other shape here — a visitor that does not
    /// override this still routes a slice type to its own honest "I don't handle this" answer
    /// (never-silent by construction, G2).
    fn visit_slice(&mut self, ty: &Type, _s: &syn::TypeSlice) -> Self::Output {
        self.fallback(ty)
    }
}

/// The single canonical `syn::Type` dispatch (see module docs). `syn::Type` is
/// `#[non_exhaustive]`; every shape this crate does not recognize falls to `v.fallback(ty)`.
pub(crate) fn walk_type<V: TypeVisitor + ?Sized>(ty: &Type, v: &mut V) -> V::Output {
    match ty {
        Type::Path(p) => v.visit_path(ty, p),
        Type::Tuple(t) => v.visit_tuple(ty, t),
        Type::Reference(r) => v.visit_reference(ty, r),
        Type::Slice(s) => v.visit_slice(ty, s),
        _ => v.fallback(ty),
    }
}
