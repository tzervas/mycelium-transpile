//! `std.cmp` / `convert` — ordering, equality, and non-representation value conversions (M-532).
//!
//! # Summary
//!
//! Provides the ordinary ordering, equality, and value-conversion surface every program needs:
//! `eq`/`ord` traits, derived helpers (`min`, `max`, `clamp`, sort keys), and value conversions
//! between scalar/value types — **same representation paradigm, value re-typed within it**.
//!
//! The honesty crux is structural (RFC-0016 §4.4):
//! - **Lossy / narrowing conversion is an explicit fallible `Result`, never a silent narrowing or
//!   truncation** (C1/G2) — `i32 → i8` that does not fit is `Err`, not a wrapped or clamped byte.
//! - Lossless widening (`i8 → i32`, `BF16 → F32`) is **total** — the domain is a subset of the
//!   codomain by construction, so no error arm exists.
//! - `clamp` with inverted bounds is `Err(ClampError::InvertedBounds)`, never a silent swap of
//!   `lo`/`hi`.
//!
//! # Module boundary
//!
//! A **representation change** (binary↔ternary, `F32→BF16`, Dense↔VSA) is `std.swap` (M-516) —
//! certificate-carrying and visible (RFC-0002), **not** a `convert`. This module does **not** cross
//! `Repr` paradigms and emits **no** swap certificate. The one resolved boundary placement
//! (README §5, "Ratification dispositions"): the lossless reverse `BF16 → F32` widening lives
//! **here** — no certificate needed, no paradigm crossing.
//!
//! # Guarantee matrix (RFC-0016 §4.5)
//!
//! Encoded as data in [`GUARANTEE_MATRIX`] and asserted in the test suite — never prose-only.
//!
//! # Contract conformance (RFC-0016 §4.1 C1–C6)
//!
//! - **C1 never-silent:** every fallible op returns `Result`; no sentinel, no clamp.
//! - **C2 honest tag:** every op is `Exact` (no accuracy semantics; C2 → "an op with no accuracy
//!   semantics is simply Exact"); the honesty load is in the fallibility column, not a
//!   probabilistic tag (VR-5).
//! - **C3 no black boxes / EXPLAIN:** narrowing rows are EXPLAIN-able — `NarrowError` is a
//!   reified, inspectable diagnostic carrying the rejected value and the target bounds.
//! - **C4 value-semantic (ADR-003):** all ops are pure functions of their inputs; conversions
//!   return new values, leaving inputs untouched.
//! - **C5 above the kernel (KC-3):** Ring 2 consumer; introduces no trusted code, no `unsafe`,
//!   no FFI, produces no certificate.
//! - **C6 declared, bounded effects:** every op is effect-free (`none`).
//!
//! Design spec: `docs/spec/stdlib/cmp.md`.
//!
//! ## Ambient Representation (RFC-0012 §8-Q3)
//!
//! This crate's public API participates in the RFC-0012 ambient-representation contract:
//! the representation choice (binary/ternary/dense/VSA) is implicit at the call site but
//! always reified, queryable, and EXPLAIN-able — never a black box (C3/SC-3).
//! [Declared per RFC-0012; direction accepted in DN-07 §8-Q3; per-ring pass scheduled as M-540.]
//!
//! **For this crate (Ring 2, Tier B):** Comparison ops are representation-aware where applicable:
//! a cross-representation comparison (`Binary` vs `Ternary` value) requires an explicit swap
//! (via `std.swap`) before comparison — this module does not cross `Repr` paradigms. The lossless
//! `BF16 → F32` widening here emits no certificate because it is not a paradigm crossing; all
//! other narrowing conversions are explicit fallible ops.
//!
//! # Stability (DN-66 freeze, 2026-07-01)
//!
//! This crate's public API, as documented in `docs/spec/stdlib/cmp.md` (spec status:
//! Accepted (2026-06-20)) and asserted by its guarantee-matrix table, is the **frozen baseline** per
//! [DN-66](../../../docs/notes/DN-66-Stdlib-Stable-API-Freeze-And-Rust-Crate-Retirement-Status.md).
//! A future breaking change here needs a spec amendment + changelog entry, not a silent edit (G2).
//!
//! **Unfrozen for the gap-closure window (ADR-045, Accepted 2026-07-10):** this DN-66 freeze is
//! lifted per [ADR-045](../../../docs/adr/ADR-045-Kernel-And-Lexicon-Unfreeze-For-Early-Gap-Closure.md)
//! for a bounded window; a breaking change here still needs a spec amendment + changelog entry
//! (G2), not a silent edit — the window closes per the ADR-045 §2.4 re-freeze conditions, bound by
//! the DN-99 residual worklist.
//! It remains the RFC-0031 D6 differential-oracle reference; the same-named `lib/std/cmp.myc` prototype is a narrower, structurally distinct surface (DN-66 S3.1) — the D6 retirement trigger has not fired, so no item here is `#[deprecated]`.
#![forbid(unsafe_code)]

use mycelium_core::GuaranteeStrength;

// ──────────────────────────────────────────────────────────────────────────────
// § 1. Ordering type
// ──────────────────────────────────────────────────────────────────────────────

/// The result of a comparison — Less, Equal, or Greater.
///
/// A value-semantic equivalent of `std::cmp::Ordering`, defined here so the cmp module is
/// self-contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ordering {
    /// The left operand is less than the right.
    Less,
    /// The operands are equal.
    Equal,
    /// The left operand is greater than the right.
    Greater,
}

impl Ordering {
    /// Reverse the ordering: `Less ↔ Greater`, `Equal ↔ Equal`.
    #[must_use]
    pub fn reverse(self) -> Self {
        match self {
            Ordering::Less => Ordering::Greater,
            Ordering::Equal => Ordering::Equal,
            Ordering::Greater => Ordering::Less,
        }
    }
}

impl From<std::cmp::Ordering> for Ordering {
    fn from(o: std::cmp::Ordering) -> Self {
        match o {
            std::cmp::Ordering::Less => Ordering::Less,
            std::cmp::Ordering::Equal => Ordering::Equal,
            std::cmp::Ordering::Greater => Ordering::Greater,
        }
    }
}

impl From<Ordering> for std::cmp::Ordering {
    fn from(o: Ordering) -> Self {
        match o {
            Ordering::Less => std::cmp::Ordering::Less,
            Ordering::Equal => std::cmp::Ordering::Equal,
            Ordering::Greater => std::cmp::Ordering::Greater,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// § 2. Comparison traits
// ──────────────────────────────────────────────────────────────────────────────

/// Total equality — respects content-addressed identity where it applies (ADR-003).
///
/// **Guarantee: `Exact` / total.** Two values with equal content are equal; metadata is **not**
/// identity.
pub trait MycEq {
    /// Returns `true` if `self` and `other` are equal (content-addressed; ADR-003).
    fn myc_eq(&self, other: &Self) -> bool;

    /// Returns `true` if `self` and `other` are not equal.
    #[must_use]
    fn myc_ne(&self, other: &Self) -> bool {
        !self.myc_eq(other)
    }
}

/// Total ordering — for types with a well-defined total order.
///
/// **Guarantee: `Exact` / total.**
pub trait MycOrd: MycEq {
    /// Compare `self` with `other`, returning an [`Ordering`].
    fn myc_cmp(&self, other: &Self) -> Ordering;

    /// Returns `true` if `self < other`.
    #[must_use]
    fn myc_lt(&self, other: &Self) -> bool {
        matches!(self.myc_cmp(other), Ordering::Less)
    }

    /// Returns `true` if `self <= other`.
    #[must_use]
    fn myc_le(&self, other: &Self) -> bool {
        matches!(self.myc_cmp(other), Ordering::Less | Ordering::Equal)
    }

    /// Returns `true` if `self > other`.
    #[must_use]
    fn myc_gt(&self, other: &Self) -> bool {
        matches!(self.myc_cmp(other), Ordering::Greater)
    }

    /// Returns `true` if `self >= other`.
    #[must_use]
    fn myc_ge(&self, other: &Self) -> bool {
        matches!(self.myc_cmp(other), Ordering::Greater | Ordering::Equal)
    }
}

/// Partial ordering — for types where some pairs may be incomparable (e.g. floats with `NaN`).
///
/// **Guarantee: `Exact` / total** — `None` is the *defined* incomparable result (`NaN`), not a
/// failure (C1: returning `None` is the honest, never-silent report of incomparability).
///
/// FLAG Q1: float total order (e.g. `total_cmp`) deferred pending M-511 / RFC-0016 §8-Q3.
pub trait MycPartialOrd: MycEq {
    /// Compare `self` with `other`. Returns `None` when the pair is incomparable (e.g. `NaN`).
    fn myc_partial_cmp(&self, other: &Self) -> Option<Ordering>;
}

// ──────────────────────────────────────────────────────────────────────────────
// § 3. Blanket impls for Rust primitives
// ──────────────────────────────────────────────────────────────────────────────

/// Macro to implement `MycEq` + `MycOrd` for a type that already has `PartialEq + Ord`.
macro_rules! impl_myc_ord {
    ($($t:ty),+) => {
        $(
            impl MycEq for $t {
                fn myc_eq(&self, other: &Self) -> bool { self == other }
            }
            impl MycOrd for $t {
                fn myc_cmp(&self, other: &Self) -> Ordering {
                    std::cmp::Ord::cmp(self, other).into()
                }
            }
            impl MycPartialOrd for $t {
                fn myc_partial_cmp(&self, other: &Self) -> Option<Ordering> {
                    Some(self.myc_cmp(other))
                }
            }
        )+
    }
}

impl_myc_ord!(bool, i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize, char);

// Float types: `PartialOrd` only (NaN is incomparable — honest total order is FLAGGED).
macro_rules! impl_myc_partial_ord_float {
    ($($t:ty),+) => {
        $(
            impl MycEq for $t {
                fn myc_eq(&self, other: &Self) -> bool { self == other }
            }
            impl MycPartialOrd for $t {
                fn myc_partial_cmp(&self, other: &Self) -> Option<Ordering> {
                    self.partial_cmp(other).map(Ordering::from)
                }
            }
        )+
    }
}

impl_myc_partial_ord_float!(f32, f64);

// ──────────────────────────────────────────────────────────────────────────────
// § 4. Derived helpers: min, max, clamp
// ──────────────────────────────────────────────────────────────────────────────

/// Return the minimum of two values under total order.
///
/// **Guarantee: `Exact` / total.** No accuracy semantics; a pure selection.
#[must_use]
pub fn myc_min<T: MycOrd + Clone>(a: T, b: T) -> T {
    if a.myc_le(&b) {
        a
    } else {
        b
    }
}

/// Return the maximum of two values under total order.
///
/// **Guarantee: `Exact` / total.** No accuracy semantics; a pure selection.
#[must_use]
pub fn myc_max<T: MycOrd + Clone>(a: T, b: T) -> T {
    if a.myc_ge(&b) {
        a
    } else {
        b
    }
}

/// The explicit error set for `clamp` (spec §3).
///
/// C1 (never-silent): `lo > hi` is **always** a `ClampError::InvertedBounds`, never a silent
/// swap of `lo` and `hi`. The rejected bounds are carried for EXPLAIN (C3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClampError<T> {
    /// The caller supplied inverted bounds (`lo > hi`).
    InvertedBounds {
        /// The lo bound supplied (which was greater than `hi`).
        lo: T,
        /// The hi bound supplied (which was less than `lo`).
        hi: T,
    },
}

impl<T: std::fmt::Debug> std::fmt::Display for ClampError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClampError::InvertedBounds { lo, hi } => {
                write!(f, "inverted clamp bounds: lo={lo:?} > hi={hi:?}")
            }
        }
    }
}

mycelium_std_core::impl_std_error!(
    ClampError<T>,
    generics = [T: std::fmt::Debug],
    where = [T: std::fmt::Debug]
);

/// Clamp `x` to `[lo, hi]` under total order.
///
/// **Guarantee: `Exact`.** Returns `Err(ClampError::InvertedBounds)` when `lo > hi` — never
/// a silent reorder (C1). The error carries the rejected bounds for EXPLAIN (C3).
///
/// # Errors
///
/// Returns [`ClampError::InvertedBounds`] when `lo > hi`.
pub fn myc_clamp<T: MycOrd + Clone>(x: T, lo: T, hi: T) -> Result<T, ClampError<T>> {
    if lo.myc_gt(&hi) {
        return Err(ClampError::InvertedBounds { lo, hi });
    }
    if x.myc_lt(&lo) {
        Ok(lo)
    } else if x.myc_gt(&hi) {
        Ok(hi)
    } else {
        Ok(x)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// § 5. Widen / Narrow traits and error types
// ──────────────────────────────────────────────────────────────────────────────

/// Lossless widening conversion — the domain is a subset of the codomain by construction.
///
/// **Guarantee: `Exact` / total** — widening can never fail; the structural split between
/// `Widen` and `Narrow` is the type-level witness to the "never-silent" guarantee (C1/RFC-0016
/// §4.4). A caller who imports this trait knows the conversion is lossless.
pub trait Widen<To> {
    /// Convert `self` to the wider type. Total — never fails.
    fn widen(self) -> To;
}

/// The explicit error set for a narrowing conversion (spec §3 / §4).
///
/// **EXPLAIN-able (C3):** every variant carries the reified, inspectable diagnostic — the rejected
/// value and (for `OutOfRange`) the target's representable bounds. A narrowing rejection is never
/// an opaque error code; the caller has everything needed to understand *why*.
#[derive(Debug, Clone, PartialEq)]
pub enum NarrowError {
    /// The value is outside the target type's representable range.
    OutOfRange {
        /// String representation of the rejected value.
        value: String,
        /// String representation of the target type's minimum.
        target_min: String,
        /// String representation of the target type's maximum.
        target_max: String,
    },
    /// The value cannot be represented in the target type for a structural reason
    /// (e.g. `NaN`, `±∞`, `f64::MAX` → `i32`).
    NotRepresentable {
        /// Human-readable description of why the value is not representable.
        reason: String,
    },
}

impl std::fmt::Display for NarrowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NarrowError::OutOfRange {
                value,
                target_min,
                target_max,
            } => write!(
                f,
                "value {value} is out of range [{target_min}, {target_max}]"
            ),
            NarrowError::NotRepresentable { reason } => {
                write!(f, "value not representable: {reason}")
            }
        }
    }
}

mycelium_std_core::impl_std_error!(NarrowError);

/// Explicitly-fallible narrowing conversion — the value may not fit in the target type.
///
/// **Guarantee: `Exact` (when `Ok`) / `Err(NarrowError)` when the value does not fit.**
///
/// C1 (never-silent): a narrowing conversion that cannot represent its input returns
/// `Err(NarrowError)` carrying the rejected value and the target bounds — **never** a wrap,
/// clamp, sign-flip, or truncation. The error is a reified, inspectable EXPLAIN artifact (C3).
pub trait Narrow<To> {
    /// Convert `self` to the narrower type. Returns `Err` when the value does not fit.
    ///
    /// # Errors
    ///
    /// Returns [`NarrowError::OutOfRange`] when the value is outside the target range, or
    /// [`NarrowError::NotRepresentable`] when the value cannot be expressed in the target type
    /// at all (e.g. `NaN` → integer).
    fn narrow(self) -> Result<To, NarrowError>;
}

// ──────────────────────────────────────────────────────────────────────────────
// § 6. Concrete Widen impls — lossless integer widening
// ──────────────────────────────────────────────────────────────────────────────
//
// Each impl is total (no error arm) because the source domain is structurally a subset of the
// target domain. This is the type-level witness to C1 for widening ops.

// Signed widening: every value fits without modification.
impl Widen<i16> for i8 {
    fn widen(self) -> i16 {
        i16::from(self)
    }
}
impl Widen<i32> for i8 {
    fn widen(self) -> i32 {
        i32::from(self)
    }
}
impl Widen<i64> for i8 {
    fn widen(self) -> i64 {
        i64::from(self)
    }
}
impl Widen<i128> for i8 {
    fn widen(self) -> i128 {
        i128::from(self)
    }
}
impl Widen<i32> for i16 {
    fn widen(self) -> i32 {
        i32::from(self)
    }
}
impl Widen<i64> for i16 {
    fn widen(self) -> i64 {
        i64::from(self)
    }
}
impl Widen<i128> for i16 {
    fn widen(self) -> i128 {
        i128::from(self)
    }
}
impl Widen<i64> for i32 {
    fn widen(self) -> i64 {
        i64::from(self)
    }
}
impl Widen<i128> for i32 {
    fn widen(self) -> i128 {
        i128::from(self)
    }
}
impl Widen<i128> for i64 {
    fn widen(self) -> i128 {
        i128::from(self)
    }
}

// Unsigned widening.
impl Widen<u16> for u8 {
    fn widen(self) -> u16 {
        u16::from(self)
    }
}
impl Widen<u32> for u8 {
    fn widen(self) -> u32 {
        u32::from(self)
    }
}
impl Widen<u64> for u8 {
    fn widen(self) -> u64 {
        u64::from(self)
    }
}
impl Widen<u128> for u8 {
    fn widen(self) -> u128 {
        u128::from(self)
    }
}
impl Widen<u32> for u16 {
    fn widen(self) -> u32 {
        u32::from(self)
    }
}
impl Widen<u64> for u16 {
    fn widen(self) -> u64 {
        u64::from(self)
    }
}
impl Widen<u128> for u16 {
    fn widen(self) -> u128 {
        u128::from(self)
    }
}
impl Widen<u64> for u32 {
    fn widen(self) -> u64 {
        u64::from(self)
    }
}
impl Widen<u128> for u32 {
    fn widen(self) -> u128 {
        u128::from(self)
    }
}
impl Widen<u128> for u64 {
    fn widen(self) -> u128 {
        u128::from(self)
    }
}

// bool → integer widening (false = 0, true = 1).
impl Widen<i32> for bool {
    fn widen(self) -> i32 {
        i32::from(self)
    }
}
impl Widen<i64> for bool {
    fn widen(self) -> i64 {
        i64::from(self)
    }
}
impl Widen<u32> for bool {
    fn widen(self) -> u32 {
        u32::from(self)
    }
}
impl Widen<u64> for bool {
    fn widen(self) -> u64 {
        u64::from(self)
    }
}

// Float widening: f32 → f64 is lossless (every f32 value is exactly representable as f64).
impl Widen<f64> for f32 {
    fn widen(self) -> f64 {
        f64::from(self)
    }
}

// BF16 → F32 lossless reverse widening (ratified placement: README §5, "BF16→F32 → cmp/convert").
// BF16 has 8-bit exponent + 7-bit mantissa; F32 has 8-bit exponent + 23-bit mantissa.
// Every BF16 value is exactly representable as F32 (the mantissa is a superset).
// No certificate is needed (no paradigm crossing; this is same-paradigm widening).
// The value type here is the kernel's scalar representation stored as f32 bits.
//
// FLAG: The kernel `Value` type with `ScalarKind::Bf16` payload stores BF16 values as f64 scalars
// in the `Payload::Scalars` vector (see mycelium-core value.rs / repr.rs). The BF16→F32 widening
// at the *kernel-Value level* requires reading `ScalarKind::Bf16` elements and producing
// `ScalarKind::F32` elements. That operation is over the kernel `Value` type (not plain f32/f64
// scalars) and its correct home is unclear until the kernel Value surface lands. This plain-scalar
// form (f32 bit-pattern → f64 widening via the BF16 mantissa fill) is provided as the basis.
// See FLAG-BF16-KERNEL below.

/// A BF16 value stored as its bit pattern in a `u16`.
///
/// BF16 = 1-bit sign + 8-bit exponent + 7-bit mantissa (the upper 16 bits of an f32 bit pattern).
/// Every BF16 value is exactly representable as an f32 (zero-fill the lower 16 mantissa bits).
/// This is a lossless widening — no certificate required, no paradigm change (same floating-point
/// value model, same exponent range; only mantissa precision is extended).
///
/// Ratified placement: `cmp`/`convert` owns the lossless `BF16 → F32` widening
/// (README §5 "Ratification dispositions"); `swap` keeps only the certified/lossy `F32 → BF16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bf16Bits(pub u16);

impl Bf16Bits {
    /// The BF16 bit-pattern for positive zero.
    pub const ZERO: Bf16Bits = Bf16Bits(0x0000);
    /// The BF16 bit-pattern for positive one.
    pub const ONE: Bf16Bits = Bf16Bits(0x3F80);
    /// The BF16 bit-pattern for negative one.
    pub const NEG_ONE: Bf16Bits = Bf16Bits(0xBF80);
    /// The BF16 bit-pattern for NaN (a quiet NaN in f32 bit layout).
    pub const NAN: Bf16Bits = Bf16Bits(0x7FC0);
    /// The BF16 bit-pattern for positive infinity.
    pub const INFINITY: Bf16Bits = Bf16Bits(0x7F80);
    /// The BF16 bit-pattern for negative infinity.
    pub const NEG_INFINITY: Bf16Bits = Bf16Bits(0xFF80);

    /// Widen this BF16 value to an f32 by zero-filling the lower 16 mantissa bits.
    ///
    /// Total — every BF16 value is exactly representable as an f32 (no precision is lost, no
    /// exponent is out of range). This is the lossless `BF16 → F32` widening that lives in
    /// `cmp`/`convert` (ratified; README §5).
    #[must_use]
    pub fn to_f32(self) -> f32 {
        // The BF16 bit pattern occupies the upper 16 bits of an f32 bit pattern.
        // Zero-filling the lower 16 bits gives the exact f32 value.
        f32::from_bits(u32::from(self.0) << 16)
    }

    // NOTE: `from_f32` narrowing (F32 → BF16) is deliberately NOT defined here.
    // That is a lossy representation change and belongs to `std.swap` (M-516), which carries a
    // SwapCertificate (RFC-0002). Offering it here would hide a certified op behind an ordinary
    // convert name — exactly the silent default C1 forbids. (spec §2 / README §5 boundary clause)
}

impl Widen<f32> for Bf16Bits {
    /// Lossless `BF16 → F32` widening — total, `Exact`, no certificate (spec §4; README §5).
    fn widen(self) -> f32 {
        self.to_f32()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// § 7. Concrete Narrow impls — explicitly-fallible narrowing conversions
// ──────────────────────────────────────────────────────────────────────────────
//
// Every impl is fallible: the source domain may exceed the target's range. Out-of-range always
// returns Err(NarrowError::OutOfRange{…}) with the rejected value and the target bounds — never
// a silent wrap, clamp, or truncation (C1).

/// Helper macro for signed→signed / signed→unsigned integer narrowing.
macro_rules! impl_narrow_int {
    // Signed narrowing: check that value fits in [To::MIN, To::MAX].
    (signed $From:ty => $To:ty) => {
        impl Narrow<$To> for $From {
            fn narrow(self) -> Result<$To, NarrowError> {
                <$To>::try_from(self).map_err(|_| NarrowError::OutOfRange {
                    value: self.to_string(),
                    target_min: <$To>::MIN.to_string(),
                    target_max: <$To>::MAX.to_string(),
                })
            }
        }
    };
}

// Signed → narrower signed
impl_narrow_int!(signed i16 => i8);
impl_narrow_int!(signed i32 => i8);
impl_narrow_int!(signed i32 => i16);
impl_narrow_int!(signed i64 => i8);
impl_narrow_int!(signed i64 => i16);
impl_narrow_int!(signed i64 => i32);
impl_narrow_int!(signed i128 => i8);
impl_narrow_int!(signed i128 => i16);
impl_narrow_int!(signed i128 => i32);
impl_narrow_int!(signed i128 => i64);

// Unsigned → narrower unsigned
impl_narrow_int!(signed u16 => u8);
impl_narrow_int!(signed u32 => u8);
impl_narrow_int!(signed u32 => u16);
impl_narrow_int!(signed u64 => u8);
impl_narrow_int!(signed u64 => u16);
impl_narrow_int!(signed u64 => u32);
impl_narrow_int!(signed u128 => u8);
impl_narrow_int!(signed u128 => u16);
impl_narrow_int!(signed u128 => u32);
impl_narrow_int!(signed u128 => u64);

// Signed → unsigned (may fail on negative values)
impl_narrow_int!(signed i8 => u8);
impl_narrow_int!(signed i8 => u16);
impl_narrow_int!(signed i8 => u32);
impl_narrow_int!(signed i8 => u64);
impl_narrow_int!(signed i16 => u8);
impl_narrow_int!(signed i16 => u16);
impl_narrow_int!(signed i16 => u32);
impl_narrow_int!(signed i16 => u64);
impl_narrow_int!(signed i32 => u8);
impl_narrow_int!(signed i32 => u16);
impl_narrow_int!(signed i32 => u32);
impl_narrow_int!(signed i32 => u64);
impl_narrow_int!(signed i64 => u8);
impl_narrow_int!(signed i64 => u16);
impl_narrow_int!(signed i64 => u32);
impl_narrow_int!(signed i64 => u64);

// Unsigned → signed (may fail when value exceeds signed max)
impl_narrow_int!(signed u8 => i8);
impl_narrow_int!(signed u16 => i8);
impl_narrow_int!(signed u16 => i16);
impl_narrow_int!(signed u32 => i8);
impl_narrow_int!(signed u32 => i16);
impl_narrow_int!(signed u32 => i32);
impl_narrow_int!(signed u64 => i8);
impl_narrow_int!(signed u64 => i16);
impl_narrow_int!(signed u64 => i32);
impl_narrow_int!(signed u64 => i64);
impl_narrow_int!(signed u128 => i8);
impl_narrow_int!(signed u128 => i16);
impl_narrow_int!(signed u128 => i32);
impl_narrow_int!(signed u128 => i64);
impl_narrow_int!(signed u128 => i128);

// f64 → f32 narrowing (may fail: out-of-range, NaN, ±Inf, subnormal values map to 0 in f32).
// FLAG Q2: The rounding-mode concern (for finite f64 that maps to a normal f32 with precision
// loss) is deferred pending M-525/std.math coordination. Here we only accept exact-fit f64 values
// (those whose f32 round-trip is lossless). Full float narrowing with rounding-mode policy
// belongs to std.math (M-525), not here.
impl Narrow<f32> for f64 {
    /// Narrow an `f64` to `f32`.
    ///
    /// Returns `Ok` only when the value is exactly representable as an `f32` (i.e., the
    /// round-trip `f32 → f64 → f32` is lossless). Returns `Err(NarrowError::NotRepresentable)`
    /// for `NaN` / `±∞` (non-finite) and `Err(NarrowError::OutOfRange)` when the magnitude
    /// exceeds `f32::MAX`. Returns `Err(NarrowError::NotRepresentable)` when the value is finite
    /// but not exactly representable as an `f32` (precision loss would occur).
    ///
    /// FLAG Q2: full float-narrowing with configurable rounding mode belongs to `std.math`
    /// (M-525); this impl only covers the exact-representability case.
    ///
    /// # Errors
    ///
    /// - [`NarrowError::NotRepresentable`] for NaN / ±∞ or precision-lossy finite values.
    /// - [`NarrowError::OutOfRange`] when `|value| > f32::MAX`.
    fn narrow(self) -> Result<f32, NarrowError> {
        if self.is_nan() {
            return Err(NarrowError::NotRepresentable {
                reason: "NaN is not representable as a finite value".to_owned(),
            });
        }
        if self.is_infinite() {
            return Err(NarrowError::NotRepresentable {
                reason: format!("{self} (±∞) is not a finite value"),
            });
        }
        // Check magnitude overflow.
        let abs = self.abs();
        if abs > f64::from(f32::MAX) {
            return Err(NarrowError::OutOfRange {
                value: self.to_string(),
                target_min: f32::MIN.to_string(),
                target_max: f32::MAX.to_string(),
            });
        }
        // Accept only exact representations: cast and round-trip check.
        let as_f32 = self as f32;
        let round_trip = f64::from(as_f32);
        if round_trip != self {
            return Err(NarrowError::NotRepresentable {
                reason: format!(
                    "{self} is not exactly representable as f32 (would round to {as_f32})"
                ),
            });
        }
        Ok(as_f32)
    }
}

// f64 → integer narrowing (NaN/±Inf → NotRepresentable; out-of-range → OutOfRange).
// FLAG Q2: rounding for finite f64 that lies between two integers is an OutOfRange/NotRepresentable
// here (we require the value to be exactly an integer). Rounding-mode policy → std.math (M-525).
macro_rules! impl_narrow_f64_to_int {
    ($($To:ty),+) => {
        $(
            impl Narrow<$To> for f64 {
                fn narrow(self) -> Result<$To, NarrowError> {
                    if self.is_nan() {
                        return Err(NarrowError::NotRepresentable {
                            reason: "NaN is not representable as an integer".to_owned(),
                        });
                    }
                    if self.is_infinite() {
                        return Err(NarrowError::NotRepresentable {
                            reason: format!("{self} (±∞) is not representable as an integer"),
                        });
                    }
                    // Must be an exact integer (no fractional part).
                    if self.fract() != 0.0 {
                        return Err(NarrowError::NotRepresentable {
                            reason: format!(
                                "{self} has a fractional part and is not exactly representable \
                                 as {}; rounding belongs to std.math (M-525)",
                                stringify!($To)
                            ),
                        });
                    }
                    // Range check via i128. The naive `self > (<$To>::MAX as f64)` test is WRONG
                    // for i64/u64: `i64::MAX as f64` rounds UP to 2^63, so `2^63` (one past
                    // i64::MAX) is not `>` it and would slip through to the saturating `as` cast,
                    // silently yielding `Ok(i64::MAX)` for an unrepresentable input (C1 violation).
                    // `self` is finite + integral here, so `self as i128` is its exact value
                    // (saturating only far beyond any 64-bit target, which still gives the correct
                    // out-of-range verdict). All target bounds are exact in i128.
                    let as_int = self as i128;
                    if as_int < (<$To>::MIN as i128) || as_int > (<$To>::MAX as i128) {
                        return Err(NarrowError::OutOfRange {
                            value: self.to_string(),
                            target_min: <$To>::MIN.to_string(),
                            target_max: <$To>::MAX.to_string(),
                        });
                    }
                    Ok(self as $To)
                }
            }
        )+
    }
}

impl_narrow_f64_to_int!(i8, i16, i32, i64, u8, u16, u32, u64);

// f32 → integer narrowing (same discipline as f64 → integer).
macro_rules! impl_narrow_f32_to_int {
    ($($To:ty),+) => {
        $(
            impl Narrow<$To> for f32 {
                fn narrow(self) -> Result<$To, NarrowError> {
                    if self.is_nan() {
                        return Err(NarrowError::NotRepresentable {
                            reason: "NaN is not representable as an integer".to_owned(),
                        });
                    }
                    if self.is_infinite() {
                        return Err(NarrowError::NotRepresentable {
                            reason: format!("{self} (±∞) is not representable as an integer"),
                        });
                    }
                    if self.fract() != 0.0 {
                        return Err(NarrowError::NotRepresentable {
                            reason: format!(
                                "{self} has a fractional part and is not exactly representable \
                                 as {}; rounding belongs to std.math (M-525)",
                                stringify!($To)
                            ),
                        });
                    }
                    // Exact range check via i128 — see the f64 impl: comparing against
                    // `<$To>::MAX as f64` rounds up at i64/u64 and lets one-past-max slip through
                    // to a silent saturating cast (C1 violation). `self` is finite + integral.
                    let as_int = self as i128;
                    if as_int < (<$To>::MIN as i128) || as_int > (<$To>::MAX as i128) {
                        return Err(NarrowError::OutOfRange {
                            value: self.to_string(),
                            target_min: <$To>::MIN.to_string(),
                            target_max: <$To>::MAX.to_string(),
                        });
                    }
                    Ok(self as $To)
                }
            }
        )+
    }
}

impl_narrow_f32_to_int!(i8, i16, i32, i64, u8, u16, u32, u64);

// ──────────────────────────────────────────────────────────────────────────────
// § 8. Guarantee matrix (RFC-0016 §4.5) — encoded as data, asserted in tests
// ──────────────────────────────────────────────────────────────────────────────

/// One row of the `std.cmp`/`convert` guarantee matrix (RFC-0016 §4.5; spec §4).
///
/// Encoded as data so tests can assert structural invariants rather than relying on prose.
/// The spec's nine rows map 1:1 to the entries in [`GUARANTEE_MATRIX`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixRow {
    /// The exported op or trait.
    pub op: &'static str,
    /// The honest guarantee tag (`Exact` for all rows — no accuracy semantics).
    pub guarantee: GuaranteeStrength,
    /// Whether the op is fallible (returns `Result`).
    pub fallible: bool,
    /// Whether the op surfaces an inspectable EXPLAIN artifact (the `NarrowError`).
    pub explainable: bool,
    /// A brief description of the fallibility shape.
    pub fallibility_desc: &'static str,
    /// Declared effects (always `"none"` for this module).
    pub effects: &'static str,
}

/// The `std.cmp`/`convert` guarantee matrix (spec §4).
///
/// Nine rows, all `Exact` and effect-free. The honesty is carried by the `fallibility_desc`
/// column, not a probabilistic tag (VR-5 / C2). Narrowing rows are `explainable = true` because
/// the `NarrowError` is a reified, inspectable rejection diagnostic (C3).
///
/// Asserted in tests via [`assert_matrix_invariants`] — never prose-only (RFC-0016 §4.5).
pub const GUARANTEE_MATRIX: &[MatrixRow] = &[
    MatrixRow {
        op: "eq / ne",
        guarantee: GuaranteeStrength::Exact,
        fallible: false,
        explainable: false,
        fallibility_desc: "total (bool)",
        effects: "none",
    },
    MatrixRow {
        op: "cmp (total order)",
        guarantee: GuaranteeStrength::Exact,
        fallible: false,
        explainable: false,
        fallibility_desc: "total (Ordering)",
        effects: "none",
    },
    MatrixRow {
        op: "partial_cmp",
        guarantee: GuaranteeStrength::Exact,
        fallible: false,
        explainable: false,
        fallibility_desc: "total — None is the defined incomparable result (e.g. NaN), not failure",
        effects: "none",
    },
    MatrixRow {
        op: "lt / le / gt / ge",
        guarantee: GuaranteeStrength::Exact,
        fallible: false,
        explainable: false,
        fallibility_desc: "total (bool)",
        effects: "none",
    },
    MatrixRow {
        op: "min / max",
        guarantee: GuaranteeStrength::Exact,
        fallible: false,
        explainable: false,
        fallibility_desc: "total",
        effects: "none",
    },
    MatrixRow {
        op: "clamp",
        guarantee: GuaranteeStrength::Exact,
        fallible: true,
        explainable: true,
        fallibility_desc: "Err(ClampError::InvertedBounds{lo, hi}) when lo > hi — never silent",
        effects: "none",
    },
    MatrixRow {
        op: "widen (lossless, e.g. i8→i32, BF16→F32)",
        guarantee: GuaranteeStrength::Exact,
        fallible: false,
        explainable: false,
        fallibility_desc: "total — domain ⊆ codomain, no error arm",
        effects: "none",
    },
    MatrixRow {
        op: "narrow (fallible, e.g. i32→i8)",
        guarantee: GuaranteeStrength::Exact,
        fallible: true,
        explainable: true,
        fallibility_desc: "Err(NarrowError::OutOfRange{value, target_min, target_max}) — never silent truncation/wrap",
        effects: "none",
    },
    MatrixRow {
        op: "narrow (not-representable, e.g. f64→i32 on NaN/±∞/overflow)",
        guarantee: GuaranteeStrength::Exact,
        fallible: true,
        explainable: true,
        fallibility_desc: "Err(NarrowError::NotRepresentable{reason}) — reified reason record",
        effects: "none",
    },
];

/// Assert the structural invariants of the guarantee matrix — called from tests.
///
/// Discharges RFC-0016 §4.5: "encoded as data, asserted in tests, never prose-only."
/// Panics with a descriptive message on any violation.
pub fn assert_matrix_invariants() {
    assert_eq!(
        GUARANTEE_MATRIX.len(),
        9,
        "spec §4 lists nine matrix rows; found {}",
        GUARANTEE_MATRIX.len()
    );
    for row in GUARANTEE_MATRIX {
        assert!(!row.op.is_empty(), "matrix row has empty op name");
        assert_eq!(
            row.guarantee,
            GuaranteeStrength::Exact,
            "op {}: all cmp/convert rows must be Exact (no accuracy semantics; VR-5)",
            row.op
        );
        assert_eq!(
            row.effects, "none",
            "op {}: all cmp/convert ops are effect-free (C6)",
            row.op
        );
        // Fallible ops must have a non-empty fallibility description.
        if row.fallible {
            assert!(
                !row.fallibility_desc.is_empty(),
                "op {}: fallible rows must describe the error set",
                row.op
            );
        }
        // EXPLAIN-able rows must be fallible (the EXPLAIN artifact is the error value).
        if row.explainable {
            assert!(
                row.fallible,
                "op {}: explainable implies fallible (the NarrowError/ClampError is the EXPLAIN artifact)",
                row.op
            );
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// § 9. Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Guarantee matrix invariants ──────────────────────────────────────────

    /// The guarantee matrix is internally consistent (RFC-0016 §4.5).
    #[test]
    fn guarantee_matrix_invariants_hold() {
        assert_matrix_invariants();
    }

    /// All nine expected ops appear in the matrix.
    #[test]
    fn matrix_has_nine_rows() {
        assert_eq!(GUARANTEE_MATRIX.len(), 9, "spec §4 lists nine rows");
    }

    /// Every row is Exact (no accuracy semantics; C2/VR-5).
    #[test]
    fn all_rows_are_exact() {
        for row in GUARANTEE_MATRIX {
            assert_eq!(
                row.guarantee,
                GuaranteeStrength::Exact,
                "op '{}' must be Exact",
                row.op
            );
        }
    }

    /// Every row declares no effects (C6).
    #[test]
    fn all_rows_are_effect_free() {
        for row in GUARANTEE_MATRIX {
            assert_eq!(row.effects, "none", "op '{}' must be effect-free", row.op);
        }
    }

    /// Only narrowing / clamp rows are EXPLAIN-able.
    #[test]
    fn only_fallible_rows_are_explainable() {
        for row in GUARANTEE_MATRIX {
            if row.explainable {
                assert!(
                    row.fallible,
                    "op '{}': explainable implies fallible",
                    row.op
                );
            }
        }
    }

    // ── Ordering ─────────────────────────────────────────────────────────────

    /// `Ordering::reverse` is an involution: `reverse(reverse(x)) == x`.
    #[test]
    fn ordering_reverse_is_involution() {
        for o in [Ordering::Less, Ordering::Equal, Ordering::Greater] {
            assert_eq!(o.reverse().reverse(), o);
        }
    }

    /// `Less.reverse() == Greater`, `Greater.reverse() == Less`, `Equal` is self-dual.
    #[test]
    fn ordering_reverse_values() {
        assert_eq!(Ordering::Less.reverse(), Ordering::Greater);
        assert_eq!(Ordering::Greater.reverse(), Ordering::Less);
        assert_eq!(Ordering::Equal.reverse(), Ordering::Equal);
    }

    // ── MycOrd for integers ───────────────────────────────────────────────────

    /// `myc_cmp` on integers agrees with the expected total order.
    #[test]
    fn myc_cmp_integer_agrees_with_standard() {
        assert_eq!(1i32.myc_cmp(&2i32), Ordering::Less);
        assert_eq!(2i32.myc_cmp(&2i32), Ordering::Equal);
        assert_eq!(3i32.myc_cmp(&2i32), Ordering::Greater);
    }

    /// `lt / le / gt / ge` are consistent with `cmp` for integers (property test over a range).
    #[test]
    fn myc_ord_predicates_consistent_with_cmp_over_range() {
        for a in -10i32..=10 {
            for b in -10i32..=10 {
                let ord = a.myc_cmp(&b);
                assert_eq!(a.myc_lt(&b), ord == Ordering::Less);
                assert_eq!(a.myc_le(&b), ord != Ordering::Greater);
                assert_eq!(a.myc_gt(&b), ord == Ordering::Greater);
                assert_eq!(a.myc_ge(&b), ord != Ordering::Less);
                assert_eq!(a.myc_eq(&b), ord == Ordering::Equal);
                assert_eq!(a.myc_ne(&b), ord != Ordering::Equal);
            }
        }
    }

    /// Total order is antisymmetric: if `a <= b` and `b <= a` then `a == b`.
    #[test]
    fn myc_ord_is_antisymmetric() {
        for a in -5i32..=5 {
            for b in -5i32..=5 {
                if a.myc_le(&b) && b.myc_le(&a) {
                    assert_eq!(
                        a, b,
                        "antisymmetry violation: {a} ≤ {b} and {b} ≤ {a} but {a} ≠ {b}"
                    );
                }
            }
        }
    }

    /// Total order is transitive: if `a <= b` and `b <= c` then `a <= c`.
    #[test]
    fn myc_ord_is_transitive() {
        for a in -5i32..=5 {
            for b in -5i32..=5 {
                for c in -5i32..=5 {
                    if a.myc_le(&b) && b.myc_le(&c) {
                        assert!(a.myc_le(&c), "transitivity: {a} ≤ {b} ≤ {c} but {a} > {c}");
                    }
                }
            }
        }
    }

    // ── MycPartialOrd for floats ──────────────────────────────────────────────

    /// `NaN` compared to anything (including itself) yields `None` (never-silent).
    #[test]
    fn partial_cmp_nan_yields_none() {
        let nan = f64::NAN;
        assert_eq!(nan.myc_partial_cmp(&nan), None, "NaN <=> NaN must be None");
        assert_eq!(
            nan.myc_partial_cmp(&1.0f64),
            None,
            "NaN <=> 1.0 must be None"
        );
        assert_eq!(
            1.0f64.myc_partial_cmp(&nan),
            None,
            "1.0 <=> NaN must be None"
        );
    }

    /// Normal float values compare as expected.
    #[test]
    fn partial_cmp_normal_floats() {
        assert_eq!(1.0f64.myc_partial_cmp(&2.0f64), Some(Ordering::Less));
        assert_eq!(2.0f64.myc_partial_cmp(&2.0f64), Some(Ordering::Equal));
        assert_eq!(3.0f64.myc_partial_cmp(&2.0f64), Some(Ordering::Greater));
    }

    // ── min / max ─────────────────────────────────────────────────────────────

    /// `min` returns the smaller value (property test).
    #[test]
    fn myc_min_is_correct() {
        for a in -10i32..=10 {
            for b in -10i32..=10 {
                let m = myc_min(a, b);
                assert_eq!(m, a.min(b), "min({a},{b}) = {m}");
            }
        }
    }

    /// `max` returns the larger value (property test).
    #[test]
    fn myc_max_is_correct() {
        for a in -10i32..=10 {
            for b in -10i32..=10 {
                assert_eq!(myc_max(a, b), a.max(b));
            }
        }
    }

    // ── clamp ─────────────────────────────────────────────────────────────────

    /// Valid clamp: value in range returns unchanged.
    #[test]
    fn clamp_in_range_returns_unchanged() {
        assert_eq!(myc_clamp(5i32, 0, 10), Ok(5));
    }

    /// Valid clamp: value below lo returns lo.
    #[test]
    fn clamp_below_lo_returns_lo() {
        assert_eq!(myc_clamp(-5i32, 0, 10), Ok(0));
    }

    /// Valid clamp: value above hi returns hi.
    #[test]
    fn clamp_above_hi_returns_hi() {
        assert_eq!(myc_clamp(15i32, 0, 10), Ok(10));
    }

    /// `clamp` with `lo > hi` returns `Err(ClampError::InvertedBounds)` — never a silent swap.
    /// Mutation witness: swap lo and hi → Ok.
    #[test]
    fn clamp_inverted_bounds_explicit_error() {
        let result = myc_clamp(5i32, 10, 0);
        assert!(
            matches!(result, Err(ClampError::InvertedBounds { lo: 10, hi: 0 })),
            "expected Err(InvertedBounds{{10, 0}}), got {result:?}"
        );
    }

    /// `lo == hi` is valid and returns `lo`/`hi` (degenerate but not inverted).
    #[test]
    fn clamp_equal_bounds_is_valid() {
        assert_eq!(myc_clamp(5i32, 3, 3), Ok(3));
    }

    /// Clamp property: for any valid bounds, `clamp(x, lo, hi) ∈ [lo, hi]`.
    #[test]
    fn clamp_result_is_in_bounds() {
        for x in -10i32..=10 {
            for lo in -5i32..=5 {
                for hi in lo..=5 {
                    let result = myc_clamp(x, lo, hi).expect("valid bounds");
                    assert!(
                        result >= lo && result <= hi,
                        "clamp({x}, {lo}, {hi}) = {result} is outside [{lo}, {hi}]"
                    );
                }
            }
        }
    }

    // ── Widen (integer) ───────────────────────────────────────────────────────

    /// `i8 → i32` widening is total and lossless (property test over full i8 corpus).
    #[test]
    fn widen_i8_to_i32_full_corpus() {
        for v in i8::MIN..=i8::MAX {
            let wide: i32 = v.widen();
            assert_eq!(wide, i32::from(v), "widen({v}: i8) -> i32 mismatch");
        }
    }

    /// `u8 → u32` widening is total and lossless (property test over full u8 corpus).
    #[test]
    fn widen_u8_to_u32_full_corpus() {
        for v in u8::MIN..=u8::MAX {
            let wide: u32 = v.widen();
            assert_eq!(wide, u32::from(v));
        }
    }

    /// `bool → i32` widening: false=0, true=1.
    #[test]
    fn widen_bool_to_i32() {
        assert_eq!(<bool as Widen<i32>>::widen(false), 0i32);
        assert_eq!(<bool as Widen<i32>>::widen(true), 1i32);
    }

    /// `f32 → f64` widening: round-trip is exact for all test values.
    #[test]
    fn widen_f32_to_f64_round_trip() {
        for v in [0.0f32, 1.0, -1.0, f32::MIN_POSITIVE, f32::MAX] {
            let wide: f64 = v.widen();
            assert_eq!(wide as f32, v, "widen({v}: f32) -> f64 round-trip mismatch");
        }
    }

    // ── Widen (BF16 → F32) ────────────────────────────────────────────────────

    /// BF16 zero widens to f32 zero.
    #[test]
    fn widen_bf16_zero_to_f32() {
        let bf16_zero = Bf16Bits::ZERO;
        assert_eq!(bf16_zero.widen(), 0.0f32);
    }

    /// BF16 one (0x3F80) widens to f32 one (same bit pattern).
    #[test]
    fn widen_bf16_one_to_f32() {
        let bf16_one = Bf16Bits::ONE;
        assert_eq!(bf16_one.widen(), 1.0f32);
    }

    /// BF16 negative one widens to -1.0f32.
    #[test]
    fn widen_bf16_neg_one_to_f32() {
        let bf16_neg_one = Bf16Bits::NEG_ONE;
        assert_eq!(bf16_neg_one.widen(), -1.0f32);
    }

    /// The widened f32 always encodes the BF16's upper 16 bits (zero-fill property).
    ///
    /// Property: for any BF16 bit pattern `b`, `f32::to_bits(bf16.widen())` has its
    /// upper 16 bits equal to `b` and its lower 16 bits equal to 0.
    #[test]
    fn widen_bf16_zero_fills_lower_16_bits() {
        // Test a representative sample of BF16 patterns.
        for upper16 in [0x0000u16, 0x3F80, 0xBF80, 0x4000, 0xC000, 0x4080] {
            let bf16 = Bf16Bits(upper16);
            let f32_bits = bf16.widen().to_bits();
            assert_eq!(
                (f32_bits >> 16) as u16,
                upper16,
                "upper 16 bits of f32 must match BF16 pattern {upper16:#06x}"
            );
            assert_eq!(
                f32_bits & 0xFFFF,
                0,
                "lower 16 bits of f32 must be zero-filled for BF16 {upper16:#06x}"
            );
        }
    }

    /// BF16 special values: NaN and ±Inf widen to the corresponding f32 special values.
    #[test]
    fn widen_bf16_special_values() {
        assert!(Bf16Bits::NAN.widen().is_nan());
        assert!(Bf16Bits::INFINITY.widen().is_infinite() && Bf16Bits::INFINITY.widen() > 0.0);
        assert!(
            Bf16Bits::NEG_INFINITY.widen().is_infinite() && Bf16Bits::NEG_INFINITY.widen() < 0.0
        );
    }

    // ── Narrow (integer) ──────────────────────────────────────────────────────

    /// `i32 → i8` narrowing: values in range return Ok; out-of-range return Err with diagnostics.
    #[test]
    fn narrow_i32_to_i8_in_range() {
        assert_eq!(<i32 as Narrow<i8>>::narrow(0), Ok(0i8));
        assert_eq!(<i32 as Narrow<i8>>::narrow(127), Ok(127i8));
        assert_eq!(<i32 as Narrow<i8>>::narrow(-128), Ok(-128i8));
    }

    /// `i32 → i8` narrowing: overflow returns `Err(OutOfRange)` with the rejected value and
    /// target bounds — never a silent truncation or wrap (C1).
    /// Mutation witness: change 128 to 127 → Ok.
    #[test]
    fn narrow_i32_to_i8_overflow_is_explicit_error() {
        let result = <i32 as Narrow<i8>>::narrow(128);
        match result {
            Err(NarrowError::OutOfRange {
                value,
                target_min,
                target_max,
            }) => {
                assert_eq!(value, "128");
                assert_eq!(target_min, "-128");
                assert_eq!(target_max, "127");
            }
            other => panic!("expected Err(OutOfRange), got {other:?}"),
        }
    }

    /// `i32 → i8` narrowing: underflow is also an explicit error.
    #[test]
    fn narrow_i32_to_i8_underflow_is_explicit_error() {
        let result = <i32 as Narrow<i8>>::narrow(-129);
        assert!(
            matches!(result, Err(NarrowError::OutOfRange { .. })),
            "expected Err(OutOfRange), got {result:?}"
        );
    }

    /// Round-trip property: for every `i8`, `widen: i8→i32` then `narrow: i32→i8` is identity.
    #[test]
    fn narrow_round_trip_i8_corpus() {
        for v in i8::MIN..=i8::MAX {
            let wide: i32 = v.widen();
            let back: i8 = <i32 as Narrow<i8>>::narrow(wide)
                .unwrap_or_else(|e| panic!("narrow(widen({v})) failed: {e}"));
            assert_eq!(back, v, "round-trip failed for {v}");
        }
    }

    /// `u64 → u8` narrowing: values above 255 return Err with correct bounds.
    #[test]
    fn narrow_u64_to_u8_overflow() {
        let result = <u64 as Narrow<u8>>::narrow(256);
        match result {
            Err(NarrowError::OutOfRange {
                value,
                target_min,
                target_max,
            }) => {
                assert_eq!(value, "256");
                assert_eq!(target_min, "0");
                assert_eq!(target_max, "255");
            }
            other => panic!("expected Err(OutOfRange{{256, 0, 255}}), got {other:?}"),
        }
    }

    /// Signed → unsigned narrowing: negative values return `Err(OutOfRange)`.
    #[test]
    fn narrow_i32_to_u32_negative_is_error() {
        let result = <i32 as Narrow<u32>>::narrow(-1);
        assert!(
            matches!(result, Err(NarrowError::OutOfRange { .. })),
            "expected Err(OutOfRange), got {result:?}"
        );
    }

    // ── Narrow (float → int) ──────────────────────────────────────────────────

    /// `f64 → i32`: NaN returns `Err(NotRepresentable)` — never a silent default.
    /// Mutation witness: replace NaN with 1.0 → Ok.
    #[test]
    fn narrow_f64_nan_to_i32_not_representable() {
        let result = <f64 as Narrow<i32>>::narrow(f64::NAN);
        assert!(
            matches!(result, Err(NarrowError::NotRepresentable { .. })),
            "expected Err(NotRepresentable), got {result:?}"
        );
    }

    /// `f64 → i32`: ±Inf returns `Err(NotRepresentable)`.
    #[test]
    fn narrow_f64_inf_to_i32_not_representable() {
        assert!(matches!(
            <f64 as Narrow<i32>>::narrow(f64::INFINITY),
            Err(NarrowError::NotRepresentable { .. })
        ));
        assert!(matches!(
            <f64 as Narrow<i32>>::narrow(f64::NEG_INFINITY),
            Err(NarrowError::NotRepresentable { .. })
        ));
    }

    /// `f64 → i32`: magnitude overflow returns `Err(OutOfRange)`.
    #[test]
    fn narrow_f64_overflow_to_i32_out_of_range() {
        let result = <f64 as Narrow<i32>>::narrow(1e18f64);
        assert!(
            matches!(result, Err(NarrowError::OutOfRange { .. })),
            "expected Err(OutOfRange), got {result:?}"
        );
    }

    /// `f64 → i32`: exact integer values in range succeed.
    #[test]
    fn narrow_f64_exact_integer_to_i32_succeeds() {
        assert_eq!(<f64 as Narrow<i32>>::narrow(42.0), Ok(42i32));
        assert_eq!(<f64 as Narrow<i32>>::narrow(-1.0), Ok(-1i32));
        assert_eq!(<f64 as Narrow<i32>>::narrow(0.0), Ok(0i32));
    }

    /// `f64 → i32`: fractional values return `Err(NotRepresentable)` — rounding belongs to
    /// std.math (M-525), not here (FLAG Q2).
    #[test]
    fn narrow_f64_fractional_to_i32_not_representable() {
        let result = <f64 as Narrow<i32>>::narrow(1.5);
        assert!(
            matches!(result, Err(NarrowError::NotRepresentable { .. })),
            "expected Err(NotRepresentable) for 1.5, got {result:?}"
        );
    }

    // ── Narrow (f64 → f32) ────────────────────────────────────────────────────

    /// `f64 → f32`: exact f32 values round-trip.
    #[test]
    fn narrow_f64_to_f32_exact_values_succeed() {
        assert_eq!(<f64 as Narrow<f32>>::narrow(1.0f64), Ok(1.0f32));
        assert_eq!(<f64 as Narrow<f32>>::narrow(-1.0f64), Ok(-1.0f32));
        assert_eq!(<f64 as Narrow<f32>>::narrow(0.0f64), Ok(0.0f32));
    }

    /// `f64 → f32`: NaN returns `Err(NotRepresentable)`.
    #[test]
    fn narrow_f64_to_f32_nan_is_error() {
        assert!(matches!(
            <f64 as Narrow<f32>>::narrow(f64::NAN),
            Err(NarrowError::NotRepresentable { .. })
        ));
    }

    /// `f64 → f32`: ±Inf returns `Err(NotRepresentable)`.
    #[test]
    fn narrow_f64_to_f32_inf_is_error() {
        assert!(matches!(
            <f64 as Narrow<f32>>::narrow(f64::INFINITY),
            Err(NarrowError::NotRepresentable { .. })
        ));
    }

    /// `f64 → f32`: magnitude overflow returns `Err(OutOfRange)`.
    #[test]
    fn narrow_f64_to_f32_overflow_is_out_of_range() {
        // f64::MAX >> f32::MAX
        assert!(matches!(
            <f64 as Narrow<f32>>::narrow(f64::MAX),
            Err(NarrowError::OutOfRange { .. })
        ));
    }

    // ── NarrowError EXPLAIN / diagnostics (C3) ────────────────────────────────

    /// `NarrowError::OutOfRange` carries the rejected value and target bounds (EXPLAIN; C3).
    #[test]
    fn narrow_error_out_of_range_carries_diagnostics() {
        match <i32 as Narrow<i8>>::narrow(200) {
            Err(NarrowError::OutOfRange {
                value,
                target_min,
                target_max,
            }) => {
                assert!(!value.is_empty(), "value must be non-empty");
                assert!(!target_min.is_empty(), "target_min must be non-empty");
                assert!(!target_max.is_empty(), "target_max must be non-empty");
                // The rejected value is in the description.
                assert!(value.contains("200"), "value field must contain '200'");
            }
            other => panic!("expected Err(OutOfRange), got {other:?}"),
        }
    }

    /// `NarrowError::NotRepresentable` carries a non-empty reason string (EXPLAIN; C3).
    #[test]
    fn narrow_error_not_representable_carries_reason() {
        match <f64 as Narrow<i32>>::narrow(f64::NAN) {
            Err(NarrowError::NotRepresentable { reason }) => {
                assert!(!reason.is_empty(), "reason must be non-empty");
            }
            other => panic!("expected Err(NotRepresentable), got {other:?}"),
        }
    }

    /// `ClampError::InvertedBounds` carries the rejected lo and hi values (EXPLAIN; C3).
    #[test]
    fn clamp_error_carries_diagnostics() {
        match myc_clamp(5i32, 10, 3) {
            Err(ClampError::InvertedBounds { lo, hi }) => {
                assert_eq!(lo, 10);
                assert_eq!(hi, 3);
            }
            other => panic!("expected Err(InvertedBounds), got {other:?}"),
        }
    }

    // ── Never-silent invariants (C1 / G2) ─────────────────────────────────────

    /// No integer narrowing silently truncates or wraps: all out-of-range values are Err.
    ///
    /// This is the property test for C1 / G2 across the integer narrowing surface.
    #[test]
    fn narrow_never_silent_integer_overflow() {
        // i32 → i8: every value outside [-128, 127] must be Err.
        for v in [128i32, 255, 1000, -129, i32::MAX, i32::MIN] {
            let result = <i32 as Narrow<i8>>::narrow(v);
            assert!(
                result.is_err(),
                "narrow({v}: i32 → i8) must be Err for out-of-range values; got {result:?}"
            );
        }
        // i32 → i8: boundary values must succeed.
        assert!(<i32 as Narrow<i8>>::narrow(127).is_ok());
        assert!(<i32 as Narrow<i8>>::narrow(-128).is_ok());
    }

    /// Float→integer narrowing at the i64/u64 boundary must NOT silently saturate.
    ///
    /// `i64::MAX as f64` rounds up to 2^63, so the value 2^63 is one past `i64::MAX`; a naive
    /// `value > (i64::MAX as f64)` range check would pass it and the saturating `as` cast would
    /// return `Ok(i64::MAX)` — a silent wrong value (C1). These probe that exact boundary.
    #[test]
    fn narrow_float_to_int_boundary_is_never_silent() {
        let two_pow_63: f64 = 9_223_372_036_854_775_808.0; // 2^63 == (i64::MAX as f64)
        let two_pow_64: f64 = 18_446_744_073_709_551_616.0; // 2^64 == (u64::MAX as f64)

        // 2^63 is one past i64::MAX → must be Err, not Ok(i64::MAX).
        assert!(matches!(
            <f64 as Narrow<i64>>::narrow(two_pow_63),
            Err(NarrowError::OutOfRange { .. })
        ));
        assert!(matches!(
            <f32 as Narrow<i64>>::narrow(two_pow_63 as f32),
            Err(NarrowError::OutOfRange { .. })
        ));
        // 2^64 is one past u64::MAX → must be Err, not Ok(u64::MAX).
        assert!(matches!(
            <f64 as Narrow<u64>>::narrow(two_pow_64),
            Err(NarrowError::OutOfRange { .. })
        ));
        // i64::MIN (-2^63) IS exactly representable → must succeed.
        assert_eq!(<f64 as Narrow<i64>>::narrow(-two_pow_63).unwrap(), i64::MIN);
        // The largest exactly-representable f64 below 2^63 round-trips.
        let max_repr = 9_223_372_036_854_774_784.0_f64; // 2^63 - 1024
        assert_eq!(
            <f64 as Narrow<i64>>::narrow(max_repr).unwrap(),
            9_223_372_036_854_774_784_i64
        );
    }

    /// No float-to-int narrowing silently produces a wrong integer.
    #[test]
    fn narrow_never_silent_float_to_int() {
        // NaN, ±Inf: must be Err(NotRepresentable).
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                <f64 as Narrow<i32>>::narrow(v).is_err(),
                "narrow({v}: f64 → i32) must be Err"
            );
        }
        // Fractional values: must be Err (no silent rounding).
        for v in [0.5f64, 1.1, -0.1, 1e6 + 0.5] {
            assert!(
                <f64 as Narrow<i32>>::narrow(v).is_err(),
                "narrow({v}: f64 → i32) must be Err (fractional, no rounding)"
            );
        }
    }

    // ── Widen/Narrow symmetry ─────────────────────────────────────────────────

    /// For any `i16` value, `narrow(widen(v))` is the identity (round-trip property).
    #[test]
    fn narrow_widen_round_trip_i16_to_i32() {
        // Full i16 corpus is too large for a simple loop; test a representative range.
        for v in (i16::MIN..=i16::MAX).step_by(100) {
            let wide: i32 = v.widen();
            let back: i16 = <i32 as Narrow<i16>>::narrow(wide)
                .unwrap_or_else(|e| panic!("narrow(widen({v}): i32 → i16) failed: {e}"));
            assert_eq!(back, v);
        }
    }
}
