use std::fmt;
use std::str::FromStr;

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{Signed, Zero};
use serde::{Deserialize, Serialize};

use crate::error::{BqError, BqResult};

/// Fixed scale for `BigQuery` `NUMERIC`: 9 fractional digits.
pub const NUMERIC_SCALE: u32 = 9;
const NUMERIC_SCALE_FACTOR: i128 = 1_000_000_000;
/// `10^38 - 1`: exact `BigQuery` `NUMERIC` scaled magnitude limit.
const NUMERIC_MAX_SCALED: i128 = 99_999_999_999_999_999_999_999_999_999_999_999_999;

/// Fixed scale for `BigQuery` `BIGNUMERIC`: 38 fractional digits.
pub const BIGNUMERIC_SCALE: u32 = 38;

/// Exact `BigQuery` `NUMERIC(38, 9)` value backed by scaled `i128`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BqNumeric(i128);

impl BqNumeric {
    pub const ZERO: Self = Self(0);
    pub const MIN: Self = Self(-NUMERIC_MAX_SCALED);
    pub const MAX: Self = Self(NUMERIC_MAX_SCALED);

    pub fn from_scaled(raw: i128) -> BqResult<Self> {
        if raw.unsigned_abs() > NUMERIC_MAX_SCALED.unsigned_abs() {
            return Err(BqError::semantic("NUMERIC value out of range"));
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub const fn raw_scaled(self) -> i128 {
        self.0
    }

    pub const fn from_i64(v: i64) -> Self {
        Self((v as i128) * NUMERIC_SCALE_FACTOR)
    }

    pub fn from_f64(v: f64) -> BqResult<Self> {
        if !v.is_finite() {
            return Err(BqError::semantic(format!(
                "Cannot convert non-finite FLOAT64 ({v}) to NUMERIC"
            )));
        }
        let s = format!("{v:.9}");
        s.parse()
    }

    #[must_use]
    pub fn to_f64(self) -> f64 {
        self.to_string().parse::<f64>().unwrap_or(f64::NAN)
    }

    pub fn checked_add(self, rhs: Self) -> BqResult<Self> {
        let sum = self
            .0
            .checked_add(rhs.0)
            .ok_or_else(|| BqError::semantic("NUMERIC overflow in addition"))?;
        Self::from_scaled(sum)
    }

    pub fn checked_sub(self, rhs: Self) -> BqResult<Self> {
        let diff = self
            .0
            .checked_sub(rhs.0)
            .ok_or_else(|| BqError::semantic("NUMERIC overflow in subtraction"))?;
        Self::from_scaled(diff)
    }

    pub fn checked_neg(self) -> BqResult<Self> {
        Self::from_scaled(-self.0)
    }

    pub fn checked_mul(self, rhs: Self) -> BqResult<Self> {
        let prod = BigInt::from(self.0) * BigInt::from(rhs.0);
        let scaled = div_round_half_away(&prod, &BigInt::from(NUMERIC_SCALE_FACTOR));
        let raw = i128::try_from(scaled)
            .map_err(|_| BqError::semantic("NUMERIC overflow in multiplication"))?;
        Self::from_scaled(raw)
    }

    pub fn checked_div(self, rhs: Self) -> BqResult<Self> {
        if rhs.0 == 0 {
            return Err(BqError::semantic("division by zero in NUMERIC"));
        }
        let num = BigInt::from(self.0) * BigInt::from(NUMERIC_SCALE_FACTOR);
        let scaled = div_round_half_away(&num, &BigInt::from(rhs.0));
        let raw = i128::try_from(scaled)
            .map_err(|_| BqError::semantic("NUMERIC overflow in division"))?;
        Self::from_scaled(raw)
    }

    pub fn round_or_trunc(self, digits: i32, trunc: bool) -> BqResult<Self> {
        let scale = i32::try_from(NUMERIC_SCALE).unwrap_or(9);
        if digits >= scale {
            return Ok(self);
        }
        let shift = usize::try_from(scale.saturating_sub(digits)).unwrap_or(0);
        if shift > 38 {
            return Ok(Self::ZERO);
        }
        let unit = pow10(shift);
        let scaled = round_or_trunc_bigint(&BigInt::from(self.0), &unit, trunc);
        let raw = i128::try_from(scaled)
            .map_err(|_| BqError::semantic("NUMERIC overflow in ROUND/TRUNC"))?;
        Self::from_scaled(raw)
    }
}

impl FromStr for BqNumeric {
    type Err = Box<BqError>;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let scaled = parse_decimal_scaled(s, NUMERIC_SCALE as usize, "NUMERIC")?;
        let raw = i128::try_from(scaled)
            .map_err(|_| BqError::semantic(format!("NUMERIC out of range: '{s}'")))?;
        Self::from_scaled(raw)
    }
}

impl fmt::Display for BqNumeric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_scaled_decimal(f, &BigInt::from(self.0), NUMERIC_SCALE as usize)
    }
}

/// Exact `BigQuery` `BIGNUMERIC` value (38 fractional digits, signed 256-bit range `[-2^255, 2^255 - 1]`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BqBigNumeric(BigInt);

impl BqBigNumeric {
    #[must_use]
    pub fn zero() -> Self {
        Self(BigInt::zero())
    }

    #[must_use]
    pub const fn raw_scaled(&self) -> &BigInt {
        &self.0
    }

    pub fn from_scaled(raw: BigInt) -> BqResult<Self> {
        let min = -(BigInt::from(1_u8) << 255_u32);
        let max = (BigInt::from(1_u8) << 255_u32) - BigInt::from(1_u8);
        if raw < min || raw > max {
            return Err(BqError::semantic("BIGNUMERIC value out of range"));
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn from_i64(v: i64) -> Self {
        Self(BigInt::from(v) * pow10(BIGNUMERIC_SCALE as usize))
    }

    #[must_use]
    pub fn from_numeric(v: BqNumeric) -> Self {
        let diff = (BIGNUMERIC_SCALE - NUMERIC_SCALE) as usize;
        Self(BigInt::from(v.raw_scaled()) * pow10(diff))
    }

    pub fn from_f64(v: f64) -> BqResult<Self> {
        if !v.is_finite() {
            return Err(BqError::semantic(format!(
                "Cannot convert non-finite FLOAT64 ({v}) to BIGNUMERIC"
            )));
        }
        format!("{v:.38}").parse()
    }

    #[must_use]
    pub fn to_f64(&self) -> f64 {
        self.to_string().parse::<f64>().unwrap_or(f64::NAN)
    }

    pub fn checked_add(&self, rhs: &Self) -> BqResult<Self> {
        Self::from_scaled(&self.0 + &rhs.0)
    }

    pub fn checked_sub(&self, rhs: &Self) -> BqResult<Self> {
        Self::from_scaled(&self.0 - &rhs.0)
    }

    pub fn checked_neg(&self) -> BqResult<Self> {
        Self::from_scaled(-&self.0)
    }

    pub fn checked_mul(&self, rhs: &Self) -> BqResult<Self> {
        let prod = &self.0 * &rhs.0;
        let scaled = div_round_half_away(&prod, &pow10(BIGNUMERIC_SCALE as usize));
        Self::from_scaled(scaled)
    }

    pub fn checked_div(&self, rhs: &Self) -> BqResult<Self> {
        if rhs.0.is_zero() {
            return Err(BqError::semantic("division by zero in BIGNUMERIC"));
        }
        let num = &self.0 * pow10(BIGNUMERIC_SCALE as usize);
        let scaled = div_round_half_away(&num, &rhs.0);
        Self::from_scaled(scaled)
    }

    pub fn round_or_trunc(&self, digits: i32, trunc: bool) -> BqResult<Self> {
        let scale = i32::try_from(BIGNUMERIC_SCALE).unwrap_or(38);
        if digits >= scale {
            return Ok(self.clone());
        }
        let shift = usize::try_from(scale.saturating_sub(digits)).unwrap_or(0);
        if shift > 77 {
            return Ok(Self::zero());
        }
        let unit = pow10(shift);
        let scaled = round_or_trunc_bigint(&self.0, &unit, trunc);
        Self::from_scaled(scaled)
    }
}

impl FromStr for BqBigNumeric {
    type Err = Box<BqError>;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let scaled = parse_decimal_scaled(s, BIGNUMERIC_SCALE as usize, "BIGNUMERIC")?;
        Self::from_scaled(scaled)
    }
}

impl fmt::Display for BqBigNumeric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_scaled_decimal(f, &self.0, BIGNUMERIC_SCALE as usize)
    }
}

fn pow10(exp: usize) -> BigInt {
    BigInt::from(10_u8).pow(u32::try_from(exp).unwrap_or(u32::MAX))
}

fn round_or_trunc_bigint(val: &BigInt, unit: &BigInt, trunc: bool) -> BigInt {
    if trunc {
        (val / unit) * unit
    } else {
        div_round_half_away(val, unit) * unit
    }
}

fn div_round_half_away(num: &BigInt, den: &BigInt) -> BigInt {
    let sign = if num.is_negative() ^ den.is_negative() {
        -1_i8
    } else {
        1_i8
    };
    let abs_num = num.abs();
    let abs_den = den.abs();
    let (mut q, r) = abs_num.div_rem(&abs_den);
    if &r * 2_u8 >= abs_den {
        q += 1_u8;
    }
    if sign < 0 {
        -q
    } else {
        q
    }
}

fn parse_decimal_scaled(input: &str, scale: usize, label: &str) -> BqResult<BigInt> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(BqError::semantic(format!(
            "Invalid {label} literal: '{input}'"
        )));
    }
    let (neg, unsigned) = strip_sign(trimmed);
    let (mantissa, exp) = split_exponent(unsigned, input, label)?;
    let (digits, frac_len) = extract_mantissa_digits(mantissa, input, label)?;
    let raw = BigInt::parse_bytes(digits.as_bytes(), 10)
        .ok_or_else(|| BqError::semantic(format!("Invalid {label} literal: '{input}'")))?;
    let total_shift =
        i64::try_from(scale).unwrap_or(0) + exp - i64::try_from(frac_len).unwrap_or(0);
    let scaled = shift_decimal(raw, total_shift);
    Ok(if neg { -scaled } else { scaled })
}

fn strip_sign(s: &str) -> (bool, &str) {
    s.strip_prefix('-').map_or_else(
        || s.strip_prefix('+').map_or((false, s), |rest| (false, rest)),
        |rest| (true, rest),
    )
}

fn split_exponent<'a>(s: &'a str, full: &str, label: &str) -> BqResult<(&'a str, i64)> {
    match s.find(['e', 'E']) {
        Some(pos) => {
            let (mantissa, exp_part) = s.split_at(pos);
            let exp_str = exp_part.get(1..).unwrap_or("");
            let exp = exp_str
                .parse::<i64>()
                .map_err(|_| BqError::semantic(format!("Invalid {label} exponent: '{full}'")))?;
            Ok((mantissa, exp))
        }
        None => Ok((s, 0)),
    }
}

fn extract_mantissa_digits(mantissa: &str, full: &str, label: &str) -> BqResult<(String, usize)> {
    let mut parts = mantissa.split('.');
    let int_part = parts.next().unwrap_or("");
    let frac_part = parts.next();
    if parts.next().is_some() || (int_part.is_empty() && frac_part.unwrap_or("").is_empty()) {
        return Err(BqError::semantic(format!(
            "Invalid {label} literal: '{full}'"
        )));
    }
    let frac = frac_part.unwrap_or("");
    if !int_part.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(BqError::semantic(format!(
            "Invalid {label} literal: '{full}'"
        )));
    }
    let mut digits = String::with_capacity(int_part.len() + frac.len());
    digits.push_str(if int_part.is_empty() { "0" } else { int_part });
    digits.push_str(frac);
    Ok((digits, frac.len()))
}

fn shift_decimal(raw: BigInt, shift: i64) -> BigInt {
    if shift >= 0 {
        raw * pow10(usize::try_from(shift).unwrap_or(0))
    } else {
        div_round_half_away(&raw, &pow10(usize::try_from(-shift).unwrap_or(0)))
    }
}

fn fmt_scaled_decimal(f: &mut fmt::Formatter<'_>, val: &BigInt, scale: usize) -> fmt::Result {
    if val.is_zero() {
        return f.write_str("0");
    }
    if val.is_negative() {
        f.write_str("-")?;
    }
    let abs = val.abs();
    let factor = pow10(scale);
    let (int_part, frac_part) = abs.div_rem(&factor);
    write!(f, "{int_part}")?;
    if !frac_part.is_zero() {
        let padded = format!("{frac_part:0>scale$}");
        let trimmed = padded.trim_end_matches('0');
        write!(f, ".{trimmed}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_parses_formats_and_rounds() {
        let a: BqNumeric = "123.4567890123".parse().unwrap();
        assert_eq!(a.to_string(), "123.456789012");
        let half: BqNumeric = "0.0000000005".parse().unwrap();
        assert_eq!(half.to_string(), "0.000000001");
        let neg_half: BqNumeric = "-0.0000000005".parse().unwrap();
        assert_eq!(neg_half.to_string(), "-0.000000001");
    }

    #[test]
    fn numeric_arithmetic_and_overflow() {
        let a: BqNumeric = "10.5".parse().unwrap();
        let b: BqNumeric = "3".parse().unwrap();
        assert_eq!(a.checked_mul(b).unwrap().to_string(), "31.5");
        assert_eq!(a.checked_div(b).unwrap().to_string(), "3.5");
        assert!(a.checked_div(BqNumeric::ZERO).is_err());
        assert!(BqNumeric::MAX.checked_add(BqNumeric::from_i64(1)).is_err());
    }

    #[test]
    fn bignumeric_handles_38_fractional_digits() {
        let a: BqBigNumeric = "1.00000000000000000000000000000000000001".parse().unwrap();
        let b: BqBigNumeric = "2.00000000000000000000000000000000000002".parse().unwrap();
        assert_eq!(
            a.checked_add(&b).unwrap().to_string(),
            "3.00000000000000000000000000000000000003"
        );
    }
}
