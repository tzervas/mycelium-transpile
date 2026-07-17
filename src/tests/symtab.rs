//! Unit tests for `src/symtab.rs` (gap-close-2, DN-34 §8.19/§8.20 Import gap-class lever; extended
//! by M-1084 with `self::`/`super::` relative resolution + cross-phylum resolution) —
//! `use_candidates`' tree-flattening + head classification, `SymbolTable`'s resolve/has_module
//! contract, and the M-1084 `qualify_key`/`candidate_lookup_keys` precedence policy. End-to-end
//! (batch-driven) coverage — cross-file resolution, `pub`-propagation, the no-bare-name-collapse
//! property, cross-phylum multi-crate batches — lives in `src/tests/batch.rs`, alongside the rest of
//! the batch-mode test corpus.

use crate::symtab::{use_candidates, CandidateKind, HeadKind, SymbolTable, UseCandidate};

fn candidates_of(src: &str, current_module: &[String]) -> Option<Vec<UseCandidate>> {
    let item: syn::ItemUse = syn::parse_str(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    use_candidates(&item.tree, current_module)
}

fn segs(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|s| s.to_string()).collect()
}

/// One `use_candidates` case: a `use` item's source text (evaluated as if written in a file whose
/// own crate-root-relative module path is `current_module`) and the expected flattened leaves —
/// data-driven per CLAUDE.md "complex test logic lives in fixtures, not test bodies".
struct Case {
    name: &'static str,
    src: &'static str,
    current_module: &'static [&'static str],
    expected: Option<&'static [(&'static [&'static str], Expect, HeadKind)]>,
}

#[derive(Debug, PartialEq, Eq)]
enum Expect {
    Name(&'static str),
    SelfModule,
    Rename(&'static str, &'static str),
    Glob,
}

const CASES: &[Case] = &[
    Case {
        name: "crate_headed_single",
        src: "use crate::checkty::Width;",
        current_module: &[],
        expected: Some(&[(&["checkty"], Expect::Name("Width"), HeadKind::SameCrate)]),
    },
    Case {
        name: "crate_headed_grouped",
        src: "use crate::checkty::{Width, CheckError};",
        current_module: &[],
        expected: Some(&[
            (&["checkty"], Expect::Name("Width"), HeadKind::SameCrate),
            (
                &["checkty"],
                Expect::Name("CheckError"),
                HeadKind::SameCrate,
            ),
        ]),
    },
    Case {
        name: "bare_head_crate_root_form",
        src: "use error::FsErr;",
        current_module: &[],
        expected: Some(&[(&["error"], Expect::Name("FsErr"), HeadKind::Bare)]),
    },
    Case {
        name: "bare_head_pub_use",
        src: "pub use metadata::{FileKind, Metadata};",
        current_module: &[],
        expected: Some(&[
            (&["metadata"], Expect::Name("FileKind"), HeadKind::Bare),
            (&["metadata"], Expect::Name("Metadata"), HeadKind::Bare),
        ]),
    },
    Case {
        name: "nested_module_path",
        src: "use crate::foo::bar::Baz;",
        current_module: &[],
        expected: Some(&[(&["foo", "bar"], Expect::Name("Baz"), HeadKind::SameCrate)]),
    },
    Case {
        name: "self_in_group",
        src: "use crate::decision::{self, Head};",
        current_module: &[],
        expected: Some(&[
            (&["decision"], Expect::SelfModule, HeadKind::SameCrate),
            (&["decision"], Expect::Name("Head"), HeadKind::SameCrate),
        ]),
    },
    Case {
        name: "rename",
        src: "use mycelium_interp::EvalError as KernelError;",
        current_module: &[],
        expected: Some(&[(
            &["mycelium_interp"],
            Expect::Rename("EvalError", "KernelError"),
            HeadKind::Bare,
        )]),
    },
    Case {
        name: "glob",
        src: "use crate::checkty::*;",
        current_module: &[],
        expected: Some(&[(&["checkty"], Expect::Glob, HeadKind::SameCrate)]),
    },
    // ── M-1084: `self::`/`super::` relative resolution ──────────────────────────────────────────
    Case {
        name: "self_headed_at_crate_root",
        // `current_module == []` (a crate-root file): `self::foo::Bar` == `foo::Bar`.
        src: "use self::foo::Bar;",
        current_module: &[],
        expected: Some(&[(&["foo"], Expect::Name("Bar"), HeadKind::SameCrate)]),
    },
    Case {
        name: "self_headed_in_nested_module",
        // A file at module `checkty` (e.g. `checkty/mod.rs`): `self::foo::Bar` -> `checkty.foo.Bar`.
        src: "use self::foo::Bar;",
        current_module: &["checkty"],
        expected: Some(&[(
            &["checkty", "foo"],
            Expect::Name("Bar"),
            HeadKind::SameCrate,
        )]),
    },
    Case {
        name: "self_headed_leaf_directly_in_current_module",
        // `use self::Bar;` in a file at module `checkty` names an item declared IN `checkty` itself
        // (no further submodule segment) -- the common `pub use self::foo::Bar;` re-export pattern's
        // simpler sibling.
        src: "use self::Bar;",
        current_module: &["checkty"],
        expected: Some(&[(&["checkty"], Expect::Name("Bar"), HeadKind::SameCrate)]),
    },
    Case {
        name: "super_headed_in_nested_module",
        // A file at module `foo.bar` (`foo/bar.rs`): `super::Baz` -> the PARENT module `foo`.
        src: "use super::Baz;",
        current_module: &["foo", "bar"],
        expected: Some(&[(&["foo"], Expect::Name("Baz"), HeadKind::SameCrate)]),
    },
    Case {
        name: "super_headed_up_to_crate_root",
        // A file at module `foo` (`foo.rs`, one level deep): `super::Baz` -> the crate root `[]`.
        src: "use super::Baz;",
        current_module: &["foo"],
        expected: Some(&[(&[], Expect::Name("Baz"), HeadKind::SameCrate)]),
    },
    Case {
        name: "super_headed_out_of_scope_at_crate_root",
        // No parent to go up to -- a genuine structural miss (real Rust itself rejects this).
        src: "use super::foo::Bar;",
        current_module: &[],
        expected: None,
    },
    Case {
        name: "multi_level_super_headed",
        // A file at module `m0.m1.m2`: `super::super::super::target::Marker` walks up 3 levels,
        // landing exactly at the crate root.
        src: "use super::super::super::target::Marker;",
        current_module: &["m0", "m1", "m2"],
        expected: Some(&[(&["target"], Expect::Name("Marker"), HeadKind::SameCrate)]),
    },
    Case {
        name: "multi_level_super_headed_partial",
        // Two levels up from `m0.m1` -> the crate root, then down into `sibling`.
        src: "use super::super::sibling::Thing;",
        current_module: &["m0", "m1"],
        expected: Some(&[(&["sibling"], Expect::Name("Thing"), HeadKind::SameCrate)]),
    },
    Case {
        name: "multi_level_super_walks_past_crate_root",
        // Two `super::` from a ONE-level-deep module walks past the crate root -- a genuine
        // structural miss, never a guess.
        src: "use super::super::target::Marker;",
        current_module: &["m0"],
        expected: None,
    },
];

#[test]
fn use_candidates_matches_expected_for_every_case() {
    for case in CASES {
        let got = candidates_of(case.src, &segs(case.current_module));
        match (got, case.expected) {
            (None, None) => {}
            (Some(got), Some(expected)) => {
                assert_eq!(
                    got.len(),
                    expected.len(),
                    "{}: leaf count mismatch — got {got:?}",
                    case.name
                );
                for (leaf, (want_segs, exp, want_head)) in got.iter().zip(expected.iter()) {
                    assert_eq!(
                        leaf.module_segs,
                        want_segs.to_vec(),
                        "{}: module_segs mismatch for {leaf:?}",
                        case.name
                    );
                    assert_eq!(
                        leaf.head_kind, *want_head,
                        "{}: head_kind mismatch for {leaf:?}",
                        case.name
                    );
                    let matches = match (&leaf.kind, exp) {
                        (CandidateKind::Name(n), Expect::Name(e)) => n == e,
                        (CandidateKind::SelfModule, Expect::SelfModule) => true,
                        (CandidateKind::Rename { from, to }, Expect::Rename(ef, et)) => {
                            from == ef && to == et
                        }
                        (CandidateKind::Glob, Expect::Glob) => true,
                        _ => false,
                    };
                    assert!(
                        matches,
                        "{}: kind mismatch for {leaf:?} vs {exp:?}",
                        case.name
                    );
                }
            }
            (got, expected) => panic!(
                "{}: expected {expected:?}-shaped result, got {got:?}",
                case.name
            ),
        }
    }
}

/// `SymbolTable::resolve` only ever hits when the module key AND the name are both present (an
/// item that exists in Rust source but never made it into the sibling's `emitted` set is a miss,
/// not a partial match) — and a hit is always the sibling's own derived nodule path, never the
/// bare module key (no-bare-name-collapse, the M-1060 lesson).
#[test]
fn symbol_table_resolve_requires_both_module_and_emitted_name() {
    let mut table = SymbolTable::new();
    table.insert(
        "checkty".to_string(),
        "l1.checkty".to_string(),
        ["Width".to_string(), "CheckError".to_string()]
            .into_iter()
            .collect(),
    );

    assert_eq!(table.resolve("checkty", "Width"), Some("l1.checkty"));
    assert_ne!(
        table.resolve("checkty", "Width"),
        Some("Width"),
        "a resolved hit must never be the bare module key or item name"
    );
    // In the Rust source `checkty.rs` may well declare `Env`/`Ty`, but this batch's baseline pass
    // never emitted them (they gapped) — so they are absent from `emitted` and must miss, not
    // fall back to a guessed resolution.
    assert_eq!(table.resolve("checkty", "Env"), None);
    // An unknown module entirely.
    assert_eq!(table.resolve("elab", "Width"), None);

    assert!(table.has_module("checkty"));
    assert!(!table.has_module("elab"));
}

/// `SymbolTable::module_key` is the exact `.`-join `use_candidates`' `module_segs` are matched
/// against — pinned directly so a future refactor of either side is caught by this contract test.
#[test]
fn symbol_table_module_key_is_dot_joined() {
    assert_eq!(SymbolTable::module_key(&["checkty".to_string()]), "checkty");
    assert_eq!(
        SymbolTable::module_key(&["foo".to_string(), "bar".to_string()]),
        "foo.bar"
    );
    assert_eq!(SymbolTable::module_key(&[]), "");
}

// ── M-1084: `SymbolTable::qualify_key` + `candidate_lookup_keys` ────────────────────────────────

/// `qualify_key` never collapses to a bare, unqualified name — the crate-root case (`module_key`
/// empty) qualifies to the crate identifier alone, never an empty/omitted qualifier.
#[test]
fn qualify_key_never_collapses_to_bare() {
    assert_eq!(
        SymbolTable::qualify_key("mycelium_std_rand", ""),
        "mycelium_std_rand"
    );
    assert_eq!(
        SymbolTable::qualify_key("mycelium_std_rand", "rng"),
        "mycelium_std_rand.rng"
    );
    assert_eq!(
        SymbolTable::qualify_key("mycelium_std_rand", "rng.gen"),
        "mycelium_std_rand.rng.gen"
    );
}

/// A `SameCrate` candidate (`crate::`/`self::`/`super::`-derived) yields exactly ONE lookup key,
/// qualified under the current file's own crate identity when derivable -- regardless of
/// `current_module` (a `SameCrate` head is unaffected by the root-file-only Bare-head gate below).
#[test]
fn same_crate_candidate_yields_one_qualified_key() {
    let candidate = UseCandidate {
        module_segs: segs(&["checkty"]),
        kind: CandidateKind::Name("Width".to_string()),
        head_kind: HeadKind::SameCrate,
    };
    assert_eq!(
        SymbolTable::candidate_lookup_keys(Some("mycelium_l1"), &[], &candidate),
        vec!["mycelium_l1.checkty".to_string()]
    );
    // No real crate context (e.g. a `src`-ancestor-less test fixture) -- degrades to the bare key,
    // byte-identical to pre-M-1084 behavior.
    assert_eq!(
        SymbolTable::candidate_lookup_keys(None, &[], &candidate),
        vec!["checkty".to_string()]
    );
}

/// A `Bare` candidate written in the CRATE-ROOT file (`current_module` empty) yields the
/// same-crate interpretation FIRST (real Rust's own root-file lexical shadowing — a crate-root
/// `mod foo;` is a name in the crate-root file's own scope), then the cross-phylum interpretation
/// (the head read literally as the named phylum's own extern-crate identifier) — never the
/// reverse order, and never just one when a real crate identity is derivable.
#[test]
fn bare_candidate_in_root_file_yields_same_crate_key_before_cross_phylum_key() {
    let candidate = UseCandidate {
        module_segs: segs(&["mycelium_std_rand", "rng"]),
        kind: CandidateKind::Name("Foo".to_string()),
        head_kind: HeadKind::Bare,
    };
    let keys = SymbolTable::candidate_lookup_keys(Some("mycelium_std_sys_host"), &[], &candidate);
    assert_eq!(
        keys,
        vec![
            "mycelium_std_sys_host.mycelium_std_rand.rng".to_string(),
            "mycelium_std_rand.rng".to_string(),
        ],
        "in the crate-root file, the same-crate interpretation must be tried first"
    );

    // No real crate context: the same-crate key degrades to the bare module key, the cross-phylum
    // key is unaffected (it never depends on the CURRENT file's own crate identity).
    let keys_no_ctx = SymbolTable::candidate_lookup_keys(None, &[], &candidate);
    assert_eq!(
        keys_no_ctx,
        vec![
            "mycelium_std_rand.rng".to_string(),
            "mycelium_std_rand.rng".to_string(),
        ],
        "with no crate context the same-crate key IS the cross-phylum key (both bare) -- still \
         never a guess, just a redundant (harmless) duplicate try"
    );
}

/// **CRITICAL fix (strict-review finding on M-1084/PR #1541):** a `Bare` candidate written in a
/// NON-ROOT file (`current_module` non-empty) yields the cross-phylum interpretation ONLY -- the
/// same-crate key is never tried, matching real Rust: a non-root file's local scope does not
/// implicitly contain the crate root's sibling `mod` declarations, so a bare head there can never
/// resolve against a same-crate submodule it never lexically sees. Before this fix,
/// `candidate_lookup_keys` tried the same-crate key first for EVERY file regardless of
/// `current_module`, silently mis-binding a genuine cross-phylum reference whenever the current
/// crate happened to have an unrelated same-named submodule.
#[test]
fn bare_candidate_in_non_root_file_yields_cross_phylum_key_only() {
    let candidate = UseCandidate {
        module_segs: segs(&["crate_b"]),
        kind: CandidateKind::Name("Foo".to_string()),
        head_kind: HeadKind::Bare,
    };
    // Written in `crate-a/src/sub.rs` (current_module = ["sub"], NOT the crate root).
    let keys = SymbolTable::candidate_lookup_keys(Some("crate_a"), &segs(&["sub"]), &candidate);
    assert_eq!(
        keys,
        vec!["crate_b".to_string()],
        "a non-root file's bare head must resolve cross-phylum ONLY -- never the same-crate key \
         `crate_a.crate_b`, even though crate-a also has (elsewhere) a submodule literally named \
         crate_b"
    );
}

/// Precedence in practice, CRATE-ROOT case: when BOTH a same-crate submodule AND a same-named
/// extern phylum exist in the table, and the referencing `use` is written IN THE CRATE-ROOT FILE,
/// the same-crate interpretation wins (matches real Rust's own root-file shadowing rule) — an
/// exhaustive property, not just a single hand-picked pair.
#[test]
fn resolve_prefers_same_crate_over_cross_phylum_on_ambiguity_in_root_file() {
    let mut table = SymbolTable::new();
    // This crate's OWN submodule literally named `sibling`.
    table.insert(
        "mycelium_a.sibling".to_string(),
        "a.sibling".to_string(),
        ["Thing".to_string()].into_iter().collect(),
    );
    // A DIFFERENT phylum, coincidentally also named `sibling` (crate identifier), exporting the
    // SAME item name at its crate root.
    table.insert(
        "sibling".to_string(),
        "b.sibling".to_string(),
        ["Thing".to_string()].into_iter().collect(),
    );

    let candidate = UseCandidate {
        module_segs: segs(&["sibling"]),
        kind: CandidateKind::Name("Thing".to_string()),
        head_kind: HeadKind::Bare,
    };
    // current_module = [] -- the referencing `use` lives in the crate-root file.
    let keys = SymbolTable::candidate_lookup_keys(Some("mycelium_a"), &[], &candidate);
    let hit = keys.iter().find_map(|k| table.resolve(k, "Thing"));
    assert_eq!(
        hit,
        Some("a.sibling"),
        "in the crate-root file, the current crate's own submodule must shadow a same-named \
         extern phylum"
    );
}

/// The NON-ROOT twin of the precedence test above: the SAME ambiguous table, but the referencing
/// `use` is written in a NON-ROOT file -- the same-crate submodule must NOT shadow the sibling
/// phylum there (it is never even tried), so resolution goes straight to the genuine sibling.
#[test]
fn resolve_goes_cross_phylum_only_from_non_root_file_even_with_a_same_crate_submodule() {
    let mut table = SymbolTable::new();
    table.insert(
        "mycelium_a.sibling".to_string(),
        "a.sibling".to_string(),
        ["Thing".to_string()].into_iter().collect(),
    );
    table.insert(
        "sibling".to_string(),
        "b.sibling".to_string(),
        ["Thing".to_string()].into_iter().collect(),
    );

    let candidate = UseCandidate {
        module_segs: segs(&["sibling"]),
        kind: CandidateKind::Name("Thing".to_string()),
        head_kind: HeadKind::Bare,
    };
    // current_module = ["sub"] -- NOT the crate root.
    let keys = SymbolTable::candidate_lookup_keys(Some("mycelium_a"), &segs(&["sub"]), &candidate);
    assert_eq!(
        keys,
        vec!["sibling".to_string()],
        "from a non-root file the same-crate key must never even be tried"
    );
    let hit = keys.iter().find_map(|k| table.resolve(k, "Thing"));
    assert_eq!(
        hit,
        Some("b.sibling"),
        "from a non-root file, the genuine sibling phylum resolves -- the same-crate submodule \
         is never consulted, so it cannot wrongly shadow it"
    );
}
