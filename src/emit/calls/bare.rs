//! The bare, single-segment call-target path shape (`foo(...)`) — DN-136/P1-a row. Moved verbatim
//! (no behavior change) from `visit_call`'s 1-segment `Expr::Path` arm.

use super::CallHandler;
use crate::gap::{Category, GapReason};
use syn::{Expr, ExprCall};

fn recognizes(c: &ExprCall) -> bool {
    matches!(&*c.func, Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 1)
}

fn resolve(c: &ExprCall, _self_ty: Option<&str>) -> Result<String, GapReason> {
    let Expr::Path(p) = &*c.func else {
        // Unreachable given `recognizes` gates every call through `lookup` — a defensive,
        // never-silent gap rather than a panic (G2).
        return Err(GapReason::new(
            Category::Other,
            "bare call: recognizer/resolve mismatch (internal invariant violation)",
        ));
    };
    p.path
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .ok_or_else(|| GapReason::new(Category::Other, "empty call-target path"))
}

pub const ROW: CallHandler = CallHandler {
    recognizes,
    resolve,
    slug: "bare-call",
    citation: "the bare, single-segment call-target path shape; DN-136 P1-a migration (moved \
               verbatim from visit_call's 1-segment Expr::Path arm)",
};
