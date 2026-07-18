//! DN-127/M-1090 WU-3 property tests: T-1 pure-literal, T-2 Show interpolation, T-3 honest gap.

use crate::emit::{emit_expr, TypeEnv};
use crate::gap::Category;
use crate::transpile::transpile_source;
use syn::parse_str;

fn parse_expr(s: &str) -> syn::Expr {
    parse_str(s).unwrap_or_else(|e| panic!("parse `{s}`: {e}"))
}

/// T-1: pure-literal `format!` lowers to a single `Bytes` literal (Alt C).
#[test]
fn t1_pure_literal_format_emits_bytes_literal() {
    let expr = parse_expr(r#"format!("hello")"#);
    let got = emit_expr(&expr, None, &TypeEnv::new()).expect("T-1 emit");
    assert_eq!(got, r#""hello""#);
}

/// T-1 twin: `write!` ignores the sink and returns the same pure `Bytes` render.
#[test]
fn t1_pure_literal_write_emits_bytes_literal() {
    let expr = parse_expr(r#"write!(f, "hi")"#);
    let got = emit_expr(&expr, None, &TypeEnv::new()).expect("write T-1 emit");
    assert_eq!(got, r#""hi""#);
}

/// T-2: a single Show-able interpolation (`Binary{64}` in `TypeEnv`) uses `render`.
#[test]
fn t2_show_interpolation_bytes_concat_and_render() {
    let expr = parse_expr(r#"format!("n={}", n)"#);
    let mut env = TypeEnv::new();
    env.insert("n".to_string(), "Binary{64}".to_string());
    let got = emit_expr(&expr, None, &env).expect("T-2 emit");
    assert!(
        got.contains("bytes_concat") && got.contains(r#""n=""#) && got.contains("render(n)"),
        "expected bytes_concat literal + render(n), got: {got}"
    );
}

/// T-2: integer literal without env still routes through `render(width_cast(...))`.
#[test]
fn t2_integer_literal_uses_width_cast_render() {
    let expr = parse_expr(r#"format!("{}", 42)"#);
    let got = emit_expr(&expr, None, &TypeEnv::new()).expect("int literal emit");
    assert!(
        got.contains("render(width_cast(42"),
        "expected width_cast render path, got: {got}"
    );
}

/// T-3: unknown identifier → explicit `MacroInvocation` gap (never silent success).
#[test]
fn t3_missing_show_route_is_explicit_gap() {
    let expr = parse_expr(r#"format!("{}", mystery)"#);
    let err = emit_expr(&expr, None, &TypeEnv::new()).expect_err("T-3 must gap");
    assert_eq!(err.category, Category::MacroInvocation);
    assert!(
        err.reason.contains("Show"),
        "gap reason should cite missing Show route: {}",
        err.reason
    );
}

/// Float residual (DN-127 OQ-1): refused never-silently.
#[test]
fn float_interpolation_is_honest_gap() {
    let expr = parse_expr(r#"format!("{}", f)"#);
    let mut env = TypeEnv::new();
    env.insert("f".to_string(), "Float".to_string());
    let err = emit_expr(&expr, None, &env).expect_err("float must gap");
    assert_eq!(err.category, Category::MacroInvocation);
    assert!(err.reason.contains("float"), "{}", err.reason);
}

/// Unsupported format specifiers (`{:?}`) gap rather than fabricate.
#[test]
fn debug_format_spec_is_unsupported_gap() {
    let expr = parse_expr(r#"format!("{:?}", x)"#);
    let mut env = TypeEnv::new();
    env.insert("x".to_string(), "Binary{64}".to_string());
    let err = emit_expr(&expr, None, &env).expect_err(":? spec must gap");
    assert_eq!(err.category, Category::MacroInvocation);
}

/// End-to-end: a `format!` body in a transpiled `fn` emits `bytes_concat` (not a macro gap).
#[test]
fn format_in_fn_body_lowers_to_bytes_concat() {
    let rust = "fn render_msg(n: u64) -> String { format!(\"v={}\", n) }";
    let (myc, report) = transpile_source(rust, "fixture.rs", "wf").expect("transpile");
    assert!(
        myc.contains("bytes_concat") && myc.contains("render"),
        "expected format! lowering in fn body, myc:\n{myc}"
    );
    assert!(
        !report
            .gaps
            .iter()
            .any(|g| g.category == Category::MacroInvocation && g.reason.contains("format!")),
        "format! should not hard-gap: {:?}",
        report.gaps
    );
}

/// Format-string parser: escaped braces compose literally.
#[test]
fn format_parser_escapes_braces() {
    let expr = parse_expr(r#"format!("{{}}")"#);
    let got = emit_expr(&expr, None, &TypeEnv::new()).expect("brace escape");
    assert_eq!(got, r#""{}""#);
}
