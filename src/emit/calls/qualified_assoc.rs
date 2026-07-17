//! DN-133 (M-1094) — the resolution-gated 2-segment qualified/associated-fn call-target shape
//! (`Type::method(...)`, including `Self::method(...)`) — DN-136/P1-a row. Moved verbatim (no
//! behavior change) from `visit_call`'s 2-segment `Expr::Path` arm. Covers BOTH the same-file
//! (`local_mangled`) and cross-nodule (`imported_type_keys`) resolution tiers through the SAME
//! row — DN-136's illustrative `emit/calls/cross_nodule.rs` split does not correspond to a
//! separately-*recognizable* AST shape here (both tiers resolve the identical 2-segment path
//! form); this row documents that deviation rather than fabricating a shape distinction the code
//! does not have (VR-5 — see [`super`]'s module doc for the ordering-invariant argument).

use super::CallHandler;
use crate::gap::{Category, GapReason};
use crate::map::tokens_to_string;
use syn::{Expr, ExprCall};

fn recognizes(c: &ExprCall) -> bool {
    matches!(&*c.func, Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 2)
}

/// Mycelium calls are bare identifiers (`app_expr ::= primary ('(' args? ')')*`, `primary ::= ...
/// | path`, `path ::= Ident ('.' Ident)*` — no `::`/qualifier form), so the bare last segment can
/// never be emitted directly (the D4 fabrication this arm replaced — `i16::from(self)` ->
/// `from(self)`, and `from` is not a Mycelium builtin). The decl side already mangles a
/// receiver-less inherent-impl associated fn to `{Type}__{method}`
/// (`crate::emit::mangled_inherent_fn_name`, applied in `emit_impl`) — SAFE to reference here
/// ONLY when that exact mangled declaration is PROVEN present (never a guess — VR-5/G2):
/// same-file (this file's own single left-to-right pass already recorded every earlier item's
/// real emission — never a forward reference), or a resolved M-1084 cross-nodule sibling.
/// `Self::method(...)` resolves its head via the driver's threaded `self_ty` — absent (not
/// inside an impl body) simply doesn't resolve, like every other unresolved head. A
/// primitive/std associated fn (no emitted decl, e.g. `i128::try_from`), an unresolved type, or a
/// call naming a `self`-receiving method (excluded by construction:
/// `mangled_inherent_fn_name` is only ever applied to a receiver-less method, so
/// `local_mangled`/the cross-nodule set never contains one — it stays reachable only from its own
/// bare call site) all fall through to the honest gap below rather than fabricate a call.
fn resolve(c: &ExprCall, self_ty: Option<&str>) -> Result<String, GapReason> {
    let Expr::Path(p) = &*c.func else {
        // Unreachable given `recognizes` gates every call through `lookup` — a defensive,
        // never-silent gap rather than a panic (G2).
        return Err(GapReason::new(
            Category::Other,
            "qualified_assoc call: recognizer/resolve mismatch (internal invariant violation)",
        ));
    };
    let head = p.path.segments[0].ident.to_string();
    let method = p.path.segments[1].ident.to_string();
    let resolved_head = if head == "Self" {
        self_ty.map(str::to_owned)
    } else {
        Some(head.clone())
    };
    let known_mangled = resolved_head.and_then(|h| {
        let mangled = crate::emit::mangled_inherent_fn_name(&h, &method);
        (crate::emit::local_mangled_assoc_fn_known(&mangled)
            || crate::emit::cross_nodule_resolve_mangled(&head, &mangled))
        .then_some(mangled)
    });
    match known_mangled {
        Some(mangled) => Ok(mangled),
        None => Err(GapReason::new(
            Category::Other,
            format!(
                "qualified/associated-function call `{}` did not resolve to a \
                 known-emitted associated fn — Mycelium calls are bare \
                 identifiers, and the only sound surface form for \
                 `Type::method(...)` is the mangled `Type__method` name the \
                 declaration side already emits for a receiver-less inherent-impl \
                 associated fn (DN-34 §8.13/8.14), referenced only when that exact \
                 declaration is PROVEN present (same-file, or a resolved M-1084 \
                 cross-nodule sibling). A primitive/std associated fn with no \
                 emitted decl (e.g. `i128::try_from`), an unresolved type, or a \
                 `self`-receiving method (excluded by construction — it stays \
                 reachable only from its own bare call site) all gap here rather \
                 than fabricate a call (G2/VR-5, DN-133)",
                tokens_to_string(&*c.func)
            ),
        )),
    }
}

pub const ROW: CallHandler = CallHandler {
    recognizes,
    resolve,
    slug: "DN-133/M-1094 (qualified/associated-fn call)",
    citation: "DN-133/M-1094 (resolution-gated qualified/associated-fn call emission); DN-136 \
               P1-a migration (moved verbatim from visit_call's 2-segment Expr::Path arm)",
};
