//! DN-127 §2/§6/§8 (M-1090 WU-3): `write!` / `format!` → pure `Bytes` via `bytes_concat` of
//! literal `Bytes` fragments and `render(arg)` (`Show` dispatch). No `&mut Formatter` sink.
//!
//! **Guarantee: `Declared`** until live-oracle-witnessed per DN-127 DoD; parsing/lowering is
//! structural over `syn` macro tokens, not rustc-expanded.

use crate::emit::derives::{
    field_derive_kind, is_seeded_scalar_width, scalar_binary_width, zero_bin_literal,
    FieldDeriveKind,
};
use crate::emit::{emit_expr, expr_env_type, myc_string_literal, TypeEnv};
use crate::gap::{Category, GapReason};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Expr, Lit, LitStr, Macro, Token};

/// Lower `format!(…)` / `write!(…)` when recognized; otherwise `Err` (caller may fall back to a
/// generic macro gap).
pub(crate) fn try_lower_expr_macro(
    mac: &Macro,
    self_ty: Option<&str>,
    env: &TypeEnv,
) -> Result<String, GapReason> {
    let name = mac
        .path
        .get_ident()
        .map(|i| i.to_string())
        .unwrap_or_default();
    if name != "format" && name != "write" {
        return Err(macro_gap(
            &name,
            "only `write!`/`format!` are lowered in this leaf (DN-127)",
        ));
    }

    let (fmt, args) = parse_format_macro_tokens(&name, &mac.tokens)?;
    let pieces = parse_format_string(&fmt.value())?;
    if pieces.iter().any(|p| matches!(p, FormatPiece::Unsupported)) {
        return Err(macro_gap(
            &name,
            "format string uses an unsupported specifier (only plain `{}` / `{{` / `}}` are \
             lowered in this MVP — named/width/precision/Debug-only forms are honest gaps, G2)",
        ));
    }

    let mut parts: Vec<String> = Vec::new();
    let mut arg_idx = 0usize;
    for piece in pieces {
        match piece {
            FormatPiece::Literal(text) => {
                parts.push(myc_string_literal(&text)?);
            }
            FormatPiece::Arg => {
                let Some(arg) = args.get(arg_idx) else {
                    return Err(macro_gap(
                        &name,
                        &format!(
                            "format string has more '{{}}' placeholders than macro arguments \
                             (placeholder index {arg_idx})",
                        ),
                    ));
                };
                arg_idx += 1;
                let emitted = emit_expr(arg, self_ty, env)?;
                parts.push(show_render_fragment(arg, &emitted, env)?);
            }
            FormatPiece::Unsupported => unreachable!("filtered above"),
        }
    }
    if arg_idx != args.len() {
        return Err(macro_gap(
            &name,
            &format!(
                "macro has {} unused argument(s) after the format string (only {arg_idx} \
                 placeholder(s))",
                args.len().saturating_sub(arg_idx)
            ),
        ));
    }

    Ok(bytes_concat_chain(&parts))
}

fn macro_gap(macro_name: &str, detail: &str) -> GapReason {
    GapReason::new(
        Category::MacroInvocation,
        format!("`{macro_name}!` — {detail} (DN-127/M-1090 WU-3)"),
    )
}

fn parse_format_macro_tokens(
    macro_name: &str,
    tokens: &proc_macro2::TokenStream,
) -> Result<(LitStr, Vec<Expr>), GapReason> {
    let items: Punctuated<Expr, Token![,]> = Punctuated::parse_terminated
        .parse2(tokens.clone())
        .map_err(|e| {
            macro_gap(
                macro_name,
                &format!("could not parse macro token list as comma-separated expressions: {e}"),
            )
        })?;
    let mut iter = items.into_iter();
    if macro_name == "format" {
        let fmt = next_fmt_lit(macro_name, &mut iter)?;
        Ok((fmt, iter.collect()))
    } else {
        let _sink = iter.next().ok_or_else(|| {
            macro_gap(
                macro_name,
                "`write!` requires at least a sink and a format literal",
            )
        })?;
        let fmt = next_fmt_lit(macro_name, &mut iter)?;
        Ok((fmt, iter.collect()))
    }
}

fn next_fmt_lit(
    macro_name: &str,
    iter: &mut impl Iterator<Item = Expr>,
) -> Result<LitStr, GapReason> {
    let expr = iter.next().ok_or_else(|| {
        macro_gap(
            macro_name,
            "missing format string literal after macro opener",
        )
    })?;
    match expr {
        Expr::Lit(syn::ExprLit {
            lit: Lit::Str(s), ..
        }) => Ok(s),
        other => Err(macro_gap(
            macro_name,
            &format!(
                "expected a string-literal format argument, got `{}`",
                crate::map::tokens_to_string(&other)
            ),
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FormatPiece {
    Literal(String),
    Arg,
    Unsupported,
}

/// Parse a Rust `format_args!`-style format string (MVP: `{}`, `{{`, `}}` only).
fn parse_format_string(fmt: &str) -> Result<Vec<FormatPiece>, GapReason> {
    let mut out = Vec::new();
    let mut literal = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '}' {
            if chars.peek() == Some(&'}') {
                chars.next();
                literal.push('}');
                continue;
            }
            return Err(macro_gap(
                "format",
                "format string has a lone `}` — only `}}` escapes are supported in this MVP",
            ));
        }
        if c != '{' {
            literal.push(c);
            continue;
        }
        if chars.peek() == Some(&'{') {
            chars.next();
            literal.push('{');
            continue;
        }
        if !literal.is_empty() {
            out.push(FormatPiece::Literal(std::mem::take(&mut literal)));
        }
        let mut spec = String::new();
        let mut closed = false;
        while let Some(&next) = chars.peek() {
            if next == '}' {
                chars.next();
                closed = true;
                if spec.is_empty() {
                    out.push(FormatPiece::Arg);
                } else {
                    out.push(FormatPiece::Unsupported);
                }
                break;
            }
            spec.push(chars.next().unwrap());
        }
        if !closed {
            out.push(FormatPiece::Unsupported);
        }
    }
    if !literal.is_empty() {
        out.push(FormatPiece::Literal(literal));
    }
    Ok(out)
}

/// `Show` dispatch for one interpolation (DN-127 Alt B). Refuses never-silently when no honest
/// render route exists (float OQ-1, unknown type, wide scalar, etc.).
fn show_render_fragment(arg: &Expr, emitted: &str, env: &TypeEnv) -> Result<String, GapReason> {
    if let Expr::Lit(syn::ExprLit {
        lit: Lit::Str(s), ..
    }) = arg
    {
        return myc_string_literal(&s.value());
    }

    let mapped = expr_env_type(arg, env);
    if let Some(ty) = mapped.as_deref() {
        return render_for_mapped_type(emitted, ty);
    }

    if let Expr::Lit(syn::ExprLit {
        lit: Lit::Int(_), ..
    }) = arg
    {
        return Ok(format!(
            "render(width_cast({emitted}, {}))",
            zero_bin_literal(64)
        ));
    }
    if let Expr::Lit(syn::ExprLit {
        lit: Lit::Bool(_), ..
    }) = arg
    {
        return Ok(format!("render({emitted})"));
    }

    Err(macro_gap(
        "format",
        &format!(
            "interpolation of `{}` has no known `Show` route in this scope (missing `TypeEnv` \
             binding and not a literal `Bytes`/`Bool`/integer — honest gap, never fabricated \
             text, G2/VR-5)",
            crate::map::tokens_to_string(arg)
        ),
    ))
}

fn render_for_mapped_type(emitted: &str, ty: &str) -> Result<String, GapReason> {
    match field_derive_kind(ty) {
        FieldDeriveKind::UserNamed | FieldDeriveKind::BytesLike | FieldDeriveKind::BoolLike => {
            Ok(format!("render({emitted})"))
        }
        FieldDeriveKind::ScalarBinary if is_seeded_scalar_width(ty) => {
            Ok(format!("render({emitted})"))
        }
        FieldDeriveKind::ScalarBinary => {
            let w = scalar_binary_width(ty).ok_or_else(|| {
                macro_gap(
                    "format",
                    &format!("scalar type `{ty}` has no parseable width"),
                )
            })?;
            if w > 64 {
                return Err(macro_gap(
                    "format",
                    &format!(
                        "scalar `{ty}` is wider than the seeded `Show` instance (`Binary{{64}}`) — \
                         a narrowing `width_cast` can overflow at runtime (DN-127 OQ-1 / DN-138 \
                         scope); refused never-silently (G2)"
                    ),
                ));
            }
            Ok(format!(
                "render(width_cast({emitted}, {}))",
                zero_bin_literal(64)
            ))
        }
        FieldDeriveKind::Float => Err(macro_gap(
            "format",
            "float interpolation has no native `Show` render (DN-127 OQ-1 / ADR-040) — refused \
             never-silently, not fabricated (G2)",
        )),
        FieldDeriveKind::VecOf | FieldDeriveKind::Deferred => Err(macro_gap(
            "format",
            &format!(
                "type `{ty}` has no ambient `Show` instance in this transpiled nodule (bracketed \
                 or primitive repr without a seeded `impl Show` — honest gap, G2)"
            ),
        )),
    }
}

fn bytes_concat_chain(parts: &[String]) -> String {
    let mut iter = parts.iter();
    let mut acc = iter.next().cloned().unwrap_or_else(|| "\"\"".to_string());
    for p in iter {
        acc = format!("bytes_concat({acc}, {p})");
    }
    acc
}
