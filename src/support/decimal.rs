//! Exact base-10 fixed-point decimal arithmetic for money and business math.
//!
//! Motivation: binary floating point (`f64`) cannot represent values like
//! `0.10` exactly, which is unacceptable for financial systems. This type
//! follows the COBOL / `DECIMAL` tradition: an integer mantissa scaled by a
//! power of ten, with explicit, predictable rounding.
//!
//!   value = mantissa * 10^(-scale)
//!
//! - `mantissa` is an `i128` (≈ 38 significant digits), so it covers any
//!   realistic monetary amount with room to spare.
//! - Every operation is **checked**; overflow returns `Err` rather than
//!   wrapping silently. The runtime turns that into error `E1003`.
//! - Rounding is never implicit on `+`, `-`, `*` (those are exact). Only
//!   division and explicit `round`/`rescale` apply a rounding mode.

use std::cmp::Ordering;
use std::fmt;

/// Rounding strategy applied when reducing scale (division, round, rescale).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rounding {
    /// Round half away from zero (COBOL `ROUNDED`, common for invoices). 2.5 -> 3
    HalfUp,
    /// Round half to even / banker's rounding (reduces cumulative bias). 2.5 -> 2
    HalfEven,
    /// Truncate toward zero. 2.9 -> 2
    Down,
    /// Round away from zero. 2.1 -> 3
    Up,
    /// Round toward negative infinity. -2.1 -> -3
    Floor,
    /// Round toward positive infinity. 2.1 -> 3
    Ceiling,
}

impl Rounding {
    /// Parse a rounding-mode name (case-insensitive). Defaults to `HalfUp`.
    pub fn from_name(s: &str) -> Rounding {
        match s.to_lowercase().replace(['_', '-'], "").as_str() {
            "halfeven" | "bankers" | "even" => Rounding::HalfEven,
            "down" | "truncate" | "trunc" => Rounding::Down,
            "up" => Rounding::Up,
            "floor" => Rounding::Floor,
            "ceiling" | "ceil" => Rounding::Ceiling,
            _ => Rounding::HalfUp,
        }
    }
}

/// A fixed-point decimal number.
#[derive(Debug, Clone, Copy)]
pub struct Decimal {
    mantissa: i128,
    scale: u32,
}

impl Decimal {
    pub fn new(mantissa: i128, scale: u32) -> Self {
        Decimal { mantissa, scale }
    }

    pub fn zero() -> Self {
        Decimal { mantissa: 0, scale: 0 }
    }

    pub fn from_int(n: i64) -> Self {
        Decimal { mantissa: n as i128, scale: 0 }
    }

    pub fn scale(&self) -> u32 {
        self.scale
    }

    pub fn is_zero(&self) -> bool {
        self.mantissa == 0
    }

    /// Parse a decimal from a string like "-1234.56", "0.001", "42".
    /// Underscores are allowed as digit separators and ignored.
    pub fn parse(s: &str) -> Result<Decimal, String> {
        let t = s.trim();
        if t.is_empty() {
            return Err("empty decimal string".to_string());
        }
        let (neg, body) = match t.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, t.strip_prefix('+').unwrap_or(t)),
        };

        let mut digits = String::new();
        let mut scale: u32 = 0;
        let mut seen_dot = false;
        for ch in body.chars() {
            match ch {
                '0'..='9' => {
                    digits.push(ch);
                    if seen_dot {
                        scale += 1;
                    }
                }
                '.' => {
                    if seen_dot {
                        return Err(format!("invalid decimal `{}`: multiple dots", s));
                    }
                    seen_dot = true;
                }
                '_' => {} // separator, ignore
                _ => return Err(format!("invalid decimal `{}`: unexpected `{}`", s, ch)),
            }
        }
        if digits.is_empty() {
            return Err(format!("invalid decimal `{}`: no digits", s));
        }
        let mut mantissa: i128 = digits
            .parse()
            .map_err(|_| format!("decimal `{}` is too large", s))?;
        if neg {
            mantissa = -mantissa;
        }
        Ok(Decimal { mantissa, scale })
    }

    /// 10^n as i128, checked.
    fn pow10(n: u32) -> Result<i128, String> {
        let mut acc: i128 = 1;
        for _ in 0..n {
            acc = acc
                .checked_mul(10)
                .ok_or_else(|| "decimal overflow (scale too large)".to_string())?;
        }
        Ok(acc)
    }

    /// Rescale the mantissa to `target` scale, rounding if reducing precision.
    pub fn rescale(&self, target: u32, mode: Rounding) -> Result<Decimal, String> {
        if target == self.scale {
            return Ok(*self);
        }
        if target > self.scale {
            let factor = Self::pow10(target - self.scale)?;
            let mantissa = self
                .mantissa
                .checked_mul(factor)
                .ok_or_else(|| "decimal overflow".to_string())?;
            return Ok(Decimal { mantissa, scale: target });
        }
        // Reducing scale: divide with rounding.
        let factor = Self::pow10(self.scale - target)?;
        let q = self.mantissa / factor;
        let r = self.mantissa % factor;
        let rounded = apply_rounding(q, r, factor, mode);
        Ok(Decimal { mantissa: rounded, scale: target })
    }

    /// Align two decimals to a common (max) scale.
    fn align(a: &Decimal, b: &Decimal) -> Result<(i128, i128, u32), String> {
        let scale = a.scale.max(b.scale);
        let am = a.rescale(scale, Rounding::Down)?.mantissa;
        let bm = b.rescale(scale, Rounding::Down)?.mantissa;
        Ok((am, bm, scale))
    }

    pub fn add(&self, other: &Decimal) -> Result<Decimal, String> {
        let (am, bm, scale) = Self::align(self, other)?;
        let mantissa = am.checked_add(bm).ok_or_else(|| "decimal overflow in addition".to_string())?;
        Ok(Decimal { mantissa, scale })
    }

    pub fn sub(&self, other: &Decimal) -> Result<Decimal, String> {
        let (am, bm, scale) = Self::align(self, other)?;
        let mantissa = am.checked_sub(bm).ok_or_else(|| "decimal overflow in subtraction".to_string())?;
        Ok(Decimal { mantissa, scale })
    }

    pub fn mul(&self, other: &Decimal) -> Result<Decimal, String> {
        let mantissa = self
            .mantissa
            .checked_mul(other.mantissa)
            .ok_or_else(|| "decimal overflow in multiplication".to_string())?;
        Ok(Decimal { mantissa, scale: self.scale + other.scale })
    }

    pub fn neg(&self) -> Decimal {
        Decimal { mantissa: -self.mantissa, scale: self.scale }
    }

    pub fn abs(&self) -> Decimal {
        Decimal { mantissa: self.mantissa.abs(), scale: self.scale }
    }

    /// Divide, producing a result with `result_scale` digits, rounded by `mode`.
    /// Returns Err on division by zero or overflow.
    pub fn div(&self, other: &Decimal, result_scale: u32, mode: Rounding) -> Result<Decimal, String> {
        if other.mantissa == 0 {
            return Err("decimal division by zero".to_string());
        }
        // result_m = round( self * 10^result_scale / other )
        // value = (am/10^as) / (bm/10^bs) * 10^result_scale
        //       = am * 10^(result_scale + bs - as) / bm
        let e: i64 = result_scale as i64 + other.scale as i64 - self.scale as i64;
        let (num, den) = if e >= 0 {
            let factor = Self::pow10(e as u32)?;
            (
                self.mantissa
                    .checked_mul(factor)
                    .ok_or_else(|| "decimal overflow in division".to_string())?,
                other.mantissa,
            )
        } else {
            let factor = Self::pow10((-e) as u32)?;
            (
                self.mantissa,
                other
                    .mantissa
                    .checked_mul(factor)
                    .ok_or_else(|| "decimal overflow in division".to_string())?,
            )
        };
        // Sign-aware quotient and remainder on magnitudes.
        let sign: i128 = num.signum() * den.signum();
        let num_abs = num.unsigned_abs();
        let den_abs = den.unsigned_abs();
        let q = (num_abs / den_abs) as i128;
        let r = (num_abs % den_abs) as i128;
        let den_i = den_abs as i128;
        let rounded_mag = round_magnitude(q, r, den_i, mode, sign < 0);
        Ok(Decimal { mantissa: sign * rounded_mag, scale: result_scale })
    }

    pub fn cmp(&self, other: &Decimal) -> Ordering {
        match Self::align(self, other) {
            Ok((am, bm, _)) => am.cmp(&bm),
            Err(_) => self.to_f64().partial_cmp(&other.to_f64()).unwrap_or(Ordering::Equal),
        }
    }

    pub fn to_f64(&self) -> f64 {
        let divisor = 10f64.powi(self.scale as i32);
        self.mantissa as f64 / divisor
    }

    pub fn to_i64_trunc(&self) -> i64 {
        let t = self.rescale(0, Rounding::Down).unwrap_or(*self);
        t.mantissa as i64
    }
}

/// Round the non-negative magnitude `q` (with remainder `r` over `den`) using `mode`.
/// `negative` indicates the overall sign for Floor/Ceiling decisions.
fn round_magnitude(q: i128, r: i128, den: i128, mode: Rounding, negative: bool) -> i128 {
    if r == 0 {
        return q;
    }
    let twice = r.checked_mul(2).unwrap_or(i128::MAX);
    match mode {
        Rounding::Down => q,
        Rounding::Up => q + 1,
        Rounding::HalfUp => if twice >= den { q + 1 } else { q },
        Rounding::HalfEven => {
            if twice > den {
                q + 1
            } else if twice < den {
                q
            } else if q % 2 == 0 {
                q
            } else {
                q + 1
            }
        }
        Rounding::Floor => if negative { q + 1 } else { q },
        Rounding::Ceiling => if negative { q } else { q + 1 },
    }
}

/// Helper used by rescale where mantissa/remainder carry their own sign.
fn apply_rounding(q: i128, r: i128, factor: i128, mode: Rounding) -> i128 {
    if r == 0 {
        return q;
    }
    let negative = r < 0;
    let mag = round_magnitude(q.abs(), r.abs(), factor.abs(), mode, negative);
    if negative {
        -mag
    } else {
        mag
    }
}

/// Group an unsigned digit string into thousands with `sep` (e.g. "1234567"
/// -> "1,234,567"). An empty separator returns the digits unchanged.
fn group_thousands(digits: &str, sep: &str) -> String {
    if sep.is_empty() || digits.len() <= 3 {
        return digits.to_string();
    }
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push_str(sep);
        }
        out.push(c);
    }
    out
}

impl Decimal {
    /// COBOL PICTURE-style fixed formatting for money / business output.
    ///
    /// Rescales to exactly `decimals` fractional places (rounding **half-up**,
    /// the conventional `ROUNDED` behaviour), groups the integer part in threes
    /// with `thousands`, and joins the fraction with `point`. Examples:
    ///   `format(2, ",", ".")` on `1234567.5`  -> `"1,234,567.50"` (US)
    ///   `format(2, ".", ",")` on `1234567.5`  -> `"1.234.567,50"` (EU)
    ///   `format(0, ",", ".")` on `-1234`      -> `"-1,234"`
    pub fn format(&self, decimals: u32, thousands: &str, point: &str) -> String {
        let scaled = self.rescale(decimals, Rounding::HalfUp).unwrap_or(*self);
        let neg = scaled.mantissa < 0 && scaled.mantissa != 0;
        let digits = scaled.mantissa.unsigned_abs().to_string();
        let scale = decimals as usize;

        let (int_str, frac_str) = if scale == 0 {
            (digits.as_str().to_string(), String::new())
        } else if digits.len() <= scale {
            let zeros = "0".repeat(scale - digits.len());
            ("0".to_string(), format!("{}{}", zeros, digits))
        } else {
            let split = digits.len() - scale;
            (digits[..split].to_string(), digits[split..].to_string())
        };

        let mut out = String::new();
        if neg {
            out.push('-');
        }
        out.push_str(&group_thousands(&int_str, thousands));
        if scale > 0 {
            out.push_str(point);
            out.push_str(&frac_str);
        }
        out
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.scale == 0 {
            return write!(f, "{}", self.mantissa);
        }
        let neg = self.mantissa < 0;
        let digits = self.mantissa.unsigned_abs().to_string();
        let scale = self.scale as usize;
        let s = if digits.len() <= scale {
            // pad leading zeros: 0.00x
            let zeros = "0".repeat(scale - digits.len());
            format!("0.{}{}", zeros, digits)
        } else {
            let split = digits.len() - scale;
            format!("{}.{}", &digits[..split], &digits[split..])
        };
        if neg {
            write!(f, "-{}", s)
        } else {
            write!(f, "{}", s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::parse(s).unwrap()
    }

    #[test]
    fn parse_and_display_roundtrip() {
        assert_eq!(d("123.45").to_string(), "123.45");
        assert_eq!(d("-0.001").to_string(), "-0.001");
        assert_eq!(d("42").to_string(), "42");
        assert_eq!(d("0.10").to_string(), "0.10");
        assert_eq!(d("1_000.50").to_string(), "1000.50");
    }

    #[test]
    fn addition_is_exact() {
        // The classic 0.1 + 0.2 == 0.3 that floats get wrong.
        assert_eq!(d("0.1").add(&d("0.2")).unwrap().to_string(), "0.3");
        assert_eq!(d("19.99").add(&d("0.01")).unwrap().to_string(), "20.00");
    }

    #[test]
    fn subtraction_and_negatives() {
        assert_eq!(d("100.00").sub(&d("0.01")).unwrap().to_string(), "99.99");
        assert_eq!(d("0").sub(&d("5.5")).unwrap().to_string(), "-5.5");
    }

    #[test]
    fn multiplication_scales_add() {
        assert_eq!(d("1.50").mul(&d("3")).unwrap().to_string(), "4.50");
        assert_eq!(d("0.01").mul(&d("0.01")).unwrap().to_string(), "0.0001");
    }

    #[test]
    fn division_with_rounding() {
        // 10 / 3 at scale 2, half-up -> 3.33
        assert_eq!(d("10").div(&d("3"), 2, Rounding::HalfUp).unwrap().to_string(), "3.33");
        // 1 / 8 = 0.125 -> at scale 2 half-up -> 0.13
        assert_eq!(d("1").div(&d("8"), 2, Rounding::HalfUp).unwrap().to_string(), "0.13");
        // banker's rounding: 2.5 -> 2, 3.5 -> 4 at scale 0
        assert_eq!(d("2.5").rescale(0, Rounding::HalfEven).unwrap().to_string(), "2");
        assert_eq!(d("3.5").rescale(0, Rounding::HalfEven).unwrap().to_string(), "4");
        // half-up: 2.5 -> 3
        assert_eq!(d("2.5").rescale(0, Rounding::HalfUp).unwrap().to_string(), "3");
    }

    #[test]
    fn negative_division_rounding() {
        assert_eq!(d("-10").div(&d("3"), 2, Rounding::HalfUp).unwrap().to_string(), "-3.33");
        assert_eq!(d("-1").div(&d("8"), 2, Rounding::Floor).unwrap().to_string(), "-0.13");
        assert_eq!(d("-1").div(&d("8"), 2, Rounding::Ceiling).unwrap().to_string(), "-0.12");
    }

    #[test]
    fn division_by_zero_errs() {
        assert!(d("1").div(&d("0"), 2, Rounding::HalfUp).is_err());
    }

    #[test]
    fn comparison() {
        assert_eq!(d("1.50").cmp(&d("1.5")), Ordering::Equal);
        assert_eq!(d("1.51").cmp(&d("1.5")), Ordering::Greater);
        assert_eq!(d("-0.01").cmp(&d("0")), Ordering::Less);
    }

    #[test]
    fn money_tax_example() {
        // price 19.99 * qty 3, then 11% tax, rounded to cents half-up
        let subtotal = d("19.99").mul(&Decimal::from_int(3)).unwrap();
        assert_eq!(subtotal.to_string(), "59.97");
        let tax = subtotal.mul(&d("0.11")).unwrap().rescale(2, Rounding::HalfUp).unwrap();
        assert_eq!(tax.to_string(), "6.60");
        let total = subtotal.add(&tax).unwrap();
        assert_eq!(total.to_string(), "66.57");
    }
}

// ============================================================================
// Property 8 — Decimal money arithmetic is exact (R7.4).
// ============================================================================
#[cfg(test)]
mod exact_decimal_property {
    // Feature: memory-safe-self-hosting, Property 8: Decimal money arithmetic is exact
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};

    // ---- A generated case: three decimals, each a random integer mantissa
    //      paired with a small scale. We keep the raw (mantissa, scale) parts
    //      so the case is cheap to clone, easy to print on failure, and — most
    //      importantly — is turned into a *string* and re-parsed, exercising the
    //      real `Decimal::parse` path with valid inputs.

    #[derive(Clone, Debug)]
    struct DecCase {
        a_m: i64,
        a_s: u32,
        b_m: i64,
        b_s: u32,
        c_m: i64,
        c_s: u32,
    }

    /// Mantissa magnitude bound. Kept at 1e9 so that every operation exercised
    /// below (align, add, sub, and the nested products in the distributive law)
    /// stays comfortably inside `i128` — the property is about *exactness*, not
    /// overflow, so we generate inside the exact domain on purpose.
    const MANT: i64 = 1_000_000_000;
    /// Maximum fractional scale (0..=6 decimal places, i.e. up to micro-units).
    const MAX_SCALE: u64 = 7;

    /// Render `(mantissa, scale)` as a decimal *string* (e.g. `(-12345, 2)` ->
    /// "-123.45"), then it is parsed back so the generator only ever feeds valid
    /// textual decimals into `Decimal::parse`.
    fn dec_string(mant: i64, scale: u32) -> String {
        let neg = mant < 0;
        let digits = mant.unsigned_abs().to_string();
        let body = if scale == 0 {
            digits
        } else {
            let scale = scale as usize;
            if digits.len() <= scale {
                let zeros = "0".repeat(scale - digits.len());
                format!("0.{}{}", zeros, digits)
            } else {
                let split = digits.len() - scale;
                format!("{}.{}", &digits[..split], &digits[split..])
            }
        };
        if neg {
            format!("-{}", body)
        } else {
            body
        }
    }

    fn parse_dec(mant: i64, scale: u32) -> Decimal {
        Decimal::parse(&dec_string(mant, scale)).expect("generated decimal string must parse")
    }

    impl DecCase {
        fn a(&self) -> Decimal {
            parse_dec(self.a_m, self.a_s)
        }
        fn b(&self) -> Decimal {
            parse_dec(self.b_m, self.b_s)
        }
        fn c(&self) -> Decimal {
            parse_dec(self.c_m, self.c_s)
        }
    }

    /// Exact equality by *value* (scale-independent): two decimals are equal iff
    /// they compare `Equal`. Crucially this never goes through `f64`.
    fn eq(x: &Decimal, y: &Decimal) -> bool {
        x.cmp(y) == Ordering::Equal
    }

    fn case_gen() -> Gen<DecCase> {
        Gen::new(
            |rng: &mut Rng, _size: usize| DecCase {
                a_m: rng.range_i64(-MANT, MANT),
                a_s: rng.below(MAX_SCALE) as u32,
                b_m: rng.range_i64(-MANT, MANT),
                b_s: rng.below(MAX_SCALE) as u32,
                c_m: rng.range_i64(-MANT, MANT),
                c_s: rng.below(MAX_SCALE) as u32,
            },
            // Shrinking adds little here (every field is independent and the
            // counterexample is already minimal-to-read); keep it empty.
            |_| Vec::new(),
        )
    }

    /// Property 8: decimal money arithmetic is exact — no binary floating-point
    /// error ever creeps in. For all generated decimals `a, b, c`:
    ///   1. serialize/parse round-trip is lossless: `parse(d.to_string()) == d`;
    ///   2. addition is commutative and associative at decimal precision;
    ///   3. additive inverse is exact: `(a + b) - b == a` (the canonical place
    ///      where floats drift, e.g. `0.1 + 0.2`);
    ///   4. multiplication is commutative and distributes over addition exactly;
    ///   5. `0` and `1` are exact additive/multiplicative identities.
    ///
    /// Validates: Requirements 7.4
    #[test]
    fn prop_decimal_arithmetic_is_exact() {
        pbt::for_all("P8 decimal money arithmetic is exact", &case_gen(), |c: &DecCase| {
            let a = c.a();
            let b = c.b();
            let cc = c.c();

            // 1. Serialize/parse round-trip is lossless for every decimal.
            for d in [&a, &b, &cc] {
                let s = d.to_string();
                let reparsed = match Decimal::parse(&s) {
                    Ok(v) => v,
                    Err(_) => return false,
                };
                if reparsed.to_string() != s || !eq(&reparsed, d) {
                    return false;
                }
            }

            // 2. Addition: commutative and associative at decimal precision.
            let ab = match a.add(&b) {
                Ok(v) => v,
                Err(_) => return false,
            };
            let ba = match b.add(&a) {
                Ok(v) => v,
                Err(_) => return false,
            };
            if !eq(&ab, &ba) {
                return false;
            }
            let ab_c = match ab.add(&cc) {
                Ok(v) => v,
                Err(_) => return false,
            };
            let bc = match b.add(&cc) {
                Ok(v) => v,
                Err(_) => return false,
            };
            let a_bc = match a.add(&bc) {
                Ok(v) => v,
                Err(_) => return false,
            };
            if !eq(&ab_c, &a_bc) {
                return false;
            }

            // 3. Additive inverse is exact: (a + b) - b == a, with no drift.
            let back = match ab.sub(&b) {
                Ok(v) => v,
                Err(_) => return false,
            };
            if !eq(&back, &a) {
                return false;
            }

            // 4. Multiplication: commutative and distributes over addition.
            let mul_ab = match a.mul(&b) {
                Ok(v) => v,
                Err(_) => return false,
            };
            let mul_ba = match b.mul(&a) {
                Ok(v) => v,
                Err(_) => return false,
            };
            if !eq(&mul_ab, &mul_ba) {
                return false;
            }
            // a * (b + c) == a*b + a*c
            let lhs = match a.mul(&bc) {
                Ok(v) => v,
                Err(_) => return false,
            };
            let mul_ac = match a.mul(&cc) {
                Ok(v) => v,
                Err(_) => return false,
            };
            let rhs = match mul_ab.add(&mul_ac) {
                Ok(v) => v,
                Err(_) => return false,
            };
            if !eq(&lhs, &rhs) {
                return false;
            }

            // 5. 0 and 1 are exact identities.
            let add_id = match a.add(&Decimal::zero()) {
                Ok(v) => v,
                Err(_) => return false,
            };
            let mul_id = match a.mul(&Decimal::from_int(1)) {
                Ok(v) => v,
                Err(_) => return false,
            };
            if !eq(&add_id, &a) || !eq(&mul_id, &a) {
                return false;
            }

            // Fixed anchor: the textbook case floats get wrong stays exact.
            let one_tenth = Decimal::parse("0.1").unwrap();
            let two_tenths = Decimal::parse("0.2").unwrap();
            one_tenth.add(&two_tenths).unwrap().to_string() == "0.3"
        });
    }
}
