//! DN-127 / M-1090 WU-3 — expression-position `write!` / `format!` lowering (Alt C literals
//! first, then `Show`/`render` dispatch). Other macros stay honest [`Category::MacroInvocation`]
//! gaps (G2).

mod write_format;

pub(crate) use write_format::try_lower_expr_macro;
