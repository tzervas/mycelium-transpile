//! Mycelium reserved-word snapshot + the identifier collision guard (M-1001).
//!
//! A Rust identifier that is a Mycelium **reserved word**, emitted verbatim into
//! constructor/variant/pattern/type/fn position, fails to **parse** — the lexer tokenizes it as a
//! keyword, not an `Ident` (observed by the M-1000 vet loop as
//! `parse-error: expected a pattern, found Strength(Exact)` on `mycelium-l1/src/eval.rs`, and
//! `expected an identifier, found Binary` on `checkty.rs`). That is a "plausible but wrong" emission
//! the DN-34 §4/§8 flag-don't-guess principle forbids. The transpiler has **no sanctioned renaming
//! scheme** — the self-hosted port's per-type ctor prefixing (`lib/compiler/README.md`
//! FLAG-ast-5/FLAG-parse-2) is a *human* decision, not a mechanical one — so a collision is
//! **gapped** ([`crate::gap::Category::ReservedWord`]), never silently emitted or auto-renamed
//! (G2/VR-5).
//!
//! **DN-140 (M-1106)** supersedes the gap-only posture for program identifiers with a unified
//! [`valid_ident`] emission contract; [`guard_ident`] is retained as a no-op call-through so legacy
//! call sites keep their `?` shape while emission uses [`valid_ident`] for the real text.
//!
//! # Guarantee: `Declared`
//!
//! [`RESERVED`] is a verbatim **snapshot** of `mycelium-l1`'s lexer keyword table
//! (`crates/mycelium-l1/src/token.rs` `fn keyword`) as of **2026-07-12** (refreshed from the
//! 2026-07-06 snapshot — gap-close-2 Phase-0 re-measure — to add `priv`/`wrapping`, landed on the
//! real lexer since the prior snapshot date but missed here; see `src/tests/reserved.rs`'s
//! `snapshot_words_are_all_still_reserved_in_the_lexer` for the drift guard this refresh keeps
//! green), copied row-for-row. It is `Declared`, not authoritative — the lexer is ground truth. A
//! drift test (`src/tests/reserved.rs`, a dev-dependency on `mycelium-l1`) asserts every word here
//! is still rejected by the real `mycelium_l1::token::keyword`, so a snapshot that drifts to a
//! *non*-reserved word is caught (the **over-gap** direction — the one that would regress a valid
//! emission). The **under-gap** direction — a *new* keyword `l1` adds that this list misses — is a
//! residual the vet loop catches as a parse error, never a silent bad emission (this crate has no
//! `mycelium-l1` runtime dependency to introspect its keyword table programmatically — only a
//! dev-dependency for the drift test — so an exhaustive cross-check isn't wired here; when this
//! snapshot is touched again, diff it by eye against `crates/mycelium-l1/src/token.rs`'s `fn
//! keyword`, the cited source of truth).

use crate::gap::{Category, GapReason};

/// The Mycelium reserved-word set — a verbatim snapshot of `mycelium-l1`'s `token::keyword` table
/// (2026-07-12). Grouped as in the source: active keywords, reserved-not-active runtime/surface
/// terms, the repr-type keywords, the scalar-float keywords, and the guarantee-strength keywords.
pub const RESERVED: &[&str] = &[
    // Active + reserved-not-active structural/surface keywords.
    "nodule",
    "phylum",
    "colony",
    "hypha",
    "fuse",
    "mesh",
    "graft",
    "cyst",
    "xloc",
    "forage",
    "backbone",
    "tier",
    "reclaim",
    "consume",
    "grow",
    "lambda",
    "object",
    "via",
    "lower",
    "derive",
    "use",
    "pub",
    // `priv` — the M-1027/DN-104 per-constructor seal marker (missed by the 2026-07-06 snapshot;
    // added in this 2026-07-12 refresh — mycelium-l1/src/token.rs `fn keyword`).
    "priv",
    "type",
    "trait",
    "impl",
    "fn",
    "matured",
    "thaw",
    "let",
    "in",
    "if",
    "then",
    "else",
    "match",
    "for",
    "swap",
    "default",
    "paradigm",
    "with",
    "wild",
    "spore",
    // `wrapping` — RFC-0034 §10/§10.1 (CU-5) named modular-arithmetic opt-out (missed by the
    // 2026-07-06 snapshot; added in this 2026-07-12 refresh).
    "wrapping",
    "to",
    "policy",
    // Repr-type keywords + their RFC-0037 short aliases.
    "Binary",
    "Ternary",
    "Dense",
    "VSA",
    "bin",
    "tern",
    "emb",
    "hvec",
    "Seq",
    "Bytes",
    "Float",
    "Substrate",
    "Sparse",
    // Scalar-float keywords.
    "F16",
    "BF16",
    "F32",
    "F64",
    // Guarantee-strength keywords.
    "Exact",
    "Proven",
    "Empirical",
    "Declared",
];

/// Whether `word` is a Mycelium reserved word (would not lex as an `Ident`).
pub fn is_reserved(word: &str) -> bool {
    RESERVED.contains(&word)
}

/// Suffix appended to escape a reserved-word identifier or nodule-path segment (DN-139/DN-140).
pub const RESERVED_SEGMENT_SUFFIX: &str = "_kw";

/// DN-140 §4 — why a non-identity [`valid_ident`] rewrite happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteKind {
    Reserved,
    IllegalChars,
    Both,
}

/// DN-140 §4 — metadata for a non-identity rewrite (G2 / EXPLAIN).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    pub original: String,
    pub kind: RewriteKind,
}

/// DN-140 §4 — a guaranteed-legal Mycelium identifier plus optional rewrite metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidIdent {
    pub text: String,
    pub rewrite: Option<Rewrite>,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether `s` matches `^[A-Za-z_][A-Za-z0-9_]*$` (Mycelium identifier shape, reserved or not).
pub fn has_valid_ident_shape(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        None => false,
        Some(c) if !is_ident_start(c) => false,
        Some(_) => s.chars().all(is_ident_continue),
    }
}

/// Whether `text` is a legal, non-reserved Mycelium identifier (DN-140 §2).
pub fn is_legal_non_reserved_ident(text: &str) -> bool {
    has_valid_ident_shape(text) && !is_reserved(text)
}

fn escape_illegal_chars(raw: &str) -> String {
    if raw.is_empty() {
        return "_".to_string();
    }
    let mut work = raw.to_string();
    if work.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        work.insert(0, '_');
    }
    let mut out = String::new();
    for c in work.chars() {
        if is_ident_continue(c) {
            if out.is_empty() && !is_ident_start(c) {
                out.push('_');
            }
            out.push(c);
        } else {
            out.push_str(&format!("_u{:X}_", c as u32));
        }
    }
    if out.is_empty() {
        out.push('_');
    } else if !out.chars().next().is_some_and(is_ident_start) {
        out.insert(0, '_');
    }
    out
}

fn apply_reserved_suffix(text: &str, kind: RewriteKind, original: &str) -> ValidIdent {
    let escaped = format!("{text}{RESERVED_SEGMENT_SUFFIX}");
    ValidIdent {
        text: escaped,
        rewrite: Some(Rewrite {
            original: original.to_string(),
            kind,
        }),
    }
}

/// Map an arbitrary identifier-position string to a legal Mycelium identifier (DN-140 §4).
///
/// Total, deterministic, position-independent, idempotent. Never fails to produce legal `text`.
pub fn valid_ident(raw: &str) -> ValidIdent {
    if raw.is_empty() {
        return ValidIdent {
            text: "_".to_string(),
            rewrite: Some(Rewrite {
                original: raw.to_string(),
                kind: RewriteKind::IllegalChars,
            }),
        };
    }

    if has_valid_ident_shape(raw) && !is_reserved(raw) {
        return ValidIdent {
            text: raw.to_string(),
            rewrite: None,
        };
    }

    if has_valid_ident_shape(raw) && is_reserved(raw) {
        return apply_reserved_suffix(raw, RewriteKind::Reserved, raw);
    }

    let escaped = escape_illegal_chars(raw);
    if is_reserved(&escaped) {
        return apply_reserved_suffix(&escaped, RewriteKind::Both, raw);
    }
    ValidIdent {
        text: escaped,
        rewrite: Some(Rewrite {
            original: raw.to_string(),
            kind: RewriteKind::IllegalChars,
        }),
    }
}

/// G2 reification line for a non-identity [`valid_ident`] rewrite (DN-140 §9).
pub fn declared_rewrite_comment(vi: &ValidIdent) -> Option<String> {
    let r = vi.rewrite.as_ref()?;
    let why = match r.kind {
        RewriteKind::Reserved => "reserved-word collision",
        RewriteKind::IllegalChars => "illegal identifier characters",
        RewriteKind::Both => "reserved-word collision after illegal-character escape",
    };
    Some(format!(
        "// Declared: renamed {} -> {} ({why}, DN-140)",
        r.original, vi.text
    ))
}

/// DN-140 length-prefix inherent-fn mangle: `_` + `dec(|vt|)` + `vt` + `dec(|vm|)` + `vm` (§7).
pub fn length_prefix_mangle(vt: &str, vm: &str) -> String {
    format!("_{}{}{}{}", vt.len(), vt, vm.len(), vm)
}

/// Compose [`valid_ident`] on both parts, then length-prefix mangle (DN-140 §7).
pub fn mangled_inherent_fn_name(self_ty_text: &str, method_name: &str) -> String {
    let vt = valid_ident(self_ty_text).text;
    let vm = valid_ident(method_name).text;
    length_prefix_mangle(&vt, &vm)
}

/// Guard an identifier the emitter is about to place into `.myc` surface text.
///
/// **DN-140 call-through:** emission sites must use [`valid_ident`] for the emitted spelling;
/// this function always returns `Ok(())` so callers keep their control flow while the real
/// legalization happens at the reference/declaration site.
pub fn guard_ident(_name: &str, _context: &str) -> Result<(), GapReason> {
    Ok(())
}

/// **Gap-close-2 Phase-0 regression fix, revised (PR #1517 review HIGH — cross-file nodule-path
/// collision).** Sanitize a derived nodule path (`transpile::derive_nodule_path`, M-1042) against
/// [`RESERVED`]. M-1042 extended nodule-path derivation to include a file's **intra-crate module
/// path** as dotted segments (`crates/mycelium-l1/src/fuse.rs` -> `l1.fuse`,
/// `crates/mycelium-std-runtime/src/colony.rs` -> `std.runtime.colony`) — but a segment that is
/// itself a reserved word (`fuse`, `colony`, …) was never run through [`guard_ident`], so it
/// leaked verbatim into the `.myc` header (`nodule l1.fuse;`), which the real lexer tokenizes as a
/// **keyword**, not a path identifier — a hard `myc check` **parse error** ("expected an
/// identifier, found Fuse"), not a clean gap. That regressed the `checked_fraction`'s G2 "zero
/// hard parse failures" invariant for every file whose file/dir-name happens to be a reserved
/// word.
///
/// The original fix (2026-07-11) **dropped** the colliding segment. That reintroduced a *silent*
/// collision one level up: `crates/mycelium-l1/src/fuse.rs` (`l1.fuse`) and `crates/mycelium-l1/
/// src/nodule.rs` (`l1.nodule`) both drop their sole reserved segment and sanitize to the
/// identical `l1` nodule path — two distinct source files emitting the same `nodule l1;` header.
/// Each file myc-checks "Clean" individually, so the per-file vet loop cannot see the collision
/// (G2: an undisclosed possible-collision is exactly the "no black boxes" rule exists to prevent).
///
/// The fix here instead **escapes** each colliding segment in place — `word` becomes
/// `word{RESERVED_SEGMENT_SUFFIX}` (`fuse` -> `fuse_kw`) — rather than deleting it. This is
/// **collision-free among the reserved words themselves, by construction**: the suffix is a
/// constant appended verbatim, so the map `word -> word + RESERVED_SEGMENT_SUFFIX` is injective
/// (two different reserved words can never produce the same escaped segment), and no entry in
/// [`RESERVED`] ends in `RESERVED_SEGMENT_SUFFIX` (checked by
/// `escaped_reserved_words_are_never_themselves_reserved` in `src/tests/reserved.rs`), so an
/// escaped segment can never re-trigger the very guard it exists to satisfy. Every other segment
/// (the non-colliding crate/module prefix) is passed through unchanged, so `l1.fuse` -> `l1.fuse_kw`
/// and `l1.nodule` -> `l1.nodule_kw` are now distinct.
///
/// **DN-140:** each segment is the reserved-word branch of [`valid_ident`] (per-segment
/// specialization, §7).
///
/// **Documented residual (`Declared`, not `Proven` — VR-5):** because both Rust and Mycelium
/// identifiers are ASCII-only (`is_ident_continue` in `mycelium-l1/src/lexer.rs` — no
/// non-ASCII-marker escape is available to either language), this is not a mathematical proof of
/// global uniqueness against *every* possible source tree — a hypothetical sibling file literally
/// named `fuse_kw.rs` alongside `fuse.rs` in the same directory would still collide post-escape.
/// That shape is not present in this corpus (checked against `crates/mycelium-l1/src/token.rs`'s
/// keyword list at the 2026-07-12 snapshot) and is vanishingly unlikely by convention (`_kw` is not
/// a real Rust module-naming pattern in this codebase); it remains a residual, not a silent one —
/// the gap reason below always names the exact original path and the exact escaped segment(s), so
/// a future occurrence is diagnosable, not invisible.
///
/// A nodule-path segment is transpiler-derived file-layout metadata, not a *program* identifier —
/// unlike [`guard_ident`]'s callers (which gap rather than guess a rename for a real symbol, since
/// there is no sanctioned auto-rename for program surface), escaping file-layout metadata with a
/// fixed, disclosed marker is a deterministic, EXPLAIN-traceable transform, not a guess. Returns
/// `(sanitized_path, Some(reason))` when a segment collided, or `(nodule_path.to_owned(), None)`
/// unchanged when it did not.
pub fn sanitize_nodule_path(nodule_path: &str) -> (String, Option<GapReason>) {
    let segments: Vec<&str> = nodule_path.split('.').collect();
    let mut colliding = Vec::new();
    let escaped: Vec<String> = segments
        .iter()
        .map(|&s| {
            let vi = valid_ident(s);
            if vi.rewrite.is_some() && is_reserved(s) {
                colliding.push(s);
            }
            vi.text
        })
        .collect();
    if colliding.is_empty() {
        return (nodule_path.to_string(), None);
    }
    let sanitized = escaped.join(".");
    let reason = GapReason::new(
        Category::ReservedWord,
        format!(
            "derived nodule path `{nodule_path}` has segment(s) [{}] colliding with a Mycelium \
             reserved word — emitting them verbatim would fail to parse (`nodule {nodule_path};` \
             tokenizes the colliding word as a keyword, not an identifier); each colliding \
             segment is escaped with the `{RESERVED_SEGMENT_SUFFIX}` suffix (now `{sanitized}`) \
             rather than dropped, so distinct source files whose only differing segment is a \
             reserved word (e.g. `l1.fuse` vs `l1.nodule`) stay distinguishable instead of both \
             collapsing onto the same nodule path (G2/VR-5)",
            colliding.join(", ")
        ),
    );
    (sanitized, Some(reason))
}
