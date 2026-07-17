//! DN-140 (M-1106) — `valid_ident` + length-prefix mangle property tests.

use crate::reserved::{
    has_valid_ident_shape, is_legal_non_reserved_ident, is_reserved, length_prefix_mangle,
    mangled_inherent_fn_name, valid_ident, RewriteKind, RESERVED_SEGMENT_SUFFIX,
};
use std::collections::HashSet;

#[test]
fn valid_ident_identity_on_ordinary_names() {
    for ok in ["Ordering", "my_fn", "Foo", "DeclaredTime"] {
        let vi = valid_ident(ok);
        assert_eq!(vi.text, ok);
        assert!(vi.rewrite.is_none());
        assert!(is_legal_non_reserved_ident(&vi.text));
    }
}

#[test]
fn valid_ident_reserved_suffix() {
    let vi = valid_ident("Exact");
    assert_eq!(vi.text, format!("Exact{RESERVED_SEGMENT_SUFFIX}"));
    assert_eq!(vi.rewrite.as_ref().unwrap().kind, RewriteKind::Reserved);
    assert!(!is_reserved(&vi.text));
}

#[test]
fn valid_ident_illegal_char_bracket_class() {
    let vi = valid_ident("DeclaredTime[T]");
    assert_eq!(vi.text, "DeclaredTime_u5B_T_u5D_");
    assert_eq!(vi.rewrite.as_ref().unwrap().kind, RewriteKind::IllegalChars);
    assert!(is_legal_non_reserved_ident(&vi.text));
    let other = valid_ident("DeclaredTime[U]");
    assert_ne!(
        vi.text, other.text,
        "distinct instantiations must stay distinct"
    );
}

#[test]
fn valid_ident_idempotent() {
    for raw in ["Exact", "DeclaredTime[T]", "Foo", "Δ"] {
        let once = valid_ident(raw);
        let twice = valid_ident(&once.text);
        assert_eq!(once.text, twice.text, "idempotent for `{raw}`");
    }
}

#[test]
fn valid_ident_non_ascii_scalar_escape() {
    let vi = valid_ident("Δ");
    assert_eq!(vi.text, "_u394_");
    assert!(is_legal_non_reserved_ident(&vi.text));
}

#[test]
fn reserved_and_illegal_branch_outputs_disjoint() {
    let reserved_escape = valid_ident("Exact").text;
    let illegal_escape = valid_ident("Foo[").text;
    assert!(reserved_escape.contains(RESERVED_SEGMENT_SUFFIX));
    assert!(!reserved_escape.contains("_u"));
    assert!(illegal_escape.contains("_u"));
    assert!(!illegal_escape.ends_with(RESERVED_SEGMENT_SUFFIX));
}

#[test]
fn length_prefix_mangle_injective_pairs() {
    let a = length_prefix_mangle("Foo", "bar__baz");
    let b = length_prefix_mangle("Foo__bar", "baz");
    assert_ne!(a, b, "DN-140 §8① boundary collision must not recur");
    assert_eq!(a, "_3Foo8bar__baz");
    assert_eq!(b, "_8Foo__bar3baz");
}

#[test]
fn mangled_inherent_fn_name_composes_valid_ident_and_prefix() {
    let m = mangled_inherent_fn_name("DeclaredTime[T]", "new");
    let vt = valid_ident("DeclaredTime[T]").text;
    let vm = valid_ident("new").text;
    assert_eq!(m, length_prefix_mangle(&vt, &vm));
    assert!(m.contains("DeclaredTime_u5B_T_u5D_"));
    assert!(!m.contains('[') && !m.contains(']'));
}

#[test]
fn length_prefix_encodings_are_pairwise_distinct() {
    let pairs = [
        ("Foo", "new"),
        ("Foo", "bar__baz"),
        ("Foo__bar", "baz"),
        ("DeclaredTime_u5B_T_u5D_", "new"),
    ];
    let mut seen = HashSet::new();
    for (t, m) in pairs {
        let enc = length_prefix_mangle(t, m);
        assert!(
            seen.insert(enc.clone()),
            "duplicate encoding for ({t}, {m})"
        );
        assert!(has_valid_ident_shape(&enc[1..]) || enc.starts_with('_'));
    }
}
