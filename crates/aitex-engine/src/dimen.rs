//! Scanning dimensions: `1in`, `16383.99999pt`, `-2.5cm`.
//!
//! Two things make this more than a parse.
//!
//! **The decimal fraction is exact.** `0.1pt` is not 0.1 × 65536 rounded; TeX
//! builds the fraction digit by digit from the right, so the result is the same
//! on every machine. `tex.web` §102 calls this `round_decimals`, and it is why
//! two TeX runs agree to the scaled point.
//!
//! **The unit conversions are exact rationals**, not floating-point constants.
//! One inch is 7227/100 points *by definition*, and TeX multiplies by that
//! fraction with [`crate::scaled::xn_over_d`] rather than by 72.27. Using a
//! float here would put a fraction of a scaled point into every page dimension
//! in the document.

use crate::scaled::{MAX_DIMEN, Scaled, xn_over_d};

/// A physical unit and its ratio to a point, as an exact fraction.
///
/// From `tex.web` §458. These are definitions, not measurements: an inch *is*
/// 7227/100 points in TeX, whatever a ruler says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit {
    pub name: &'static str,
    pub num: i32,
    pub den: i32,
}

pub const UNITS: &[Unit] = &[
    Unit {
        name: "pt",
        num: 1,
        den: 1,
    },
    Unit {
        name: "in",
        num: 7227,
        den: 100,
    },
    Unit {
        name: "pc",
        num: 12,
        den: 1,
    },
    Unit {
        name: "cm",
        num: 7227,
        den: 254,
    },
    Unit {
        name: "mm",
        num: 7227,
        den: 2540,
    },
    Unit {
        name: "bp",
        num: 7227,
        den: 7200,
    },
    Unit {
        name: "dd",
        num: 1238,
        den: 1157,
    },
    Unit {
        name: "cc",
        num: 14856,
        den: 1157,
    },
    // `sp` is the base unit; handled specially since it is not scaled at all.
    Unit {
        name: "sp",
        num: 1,
        den: 1,
    },
];

pub fn unit_by_name(name: &str) -> Option<Unit> {
    UNITS.iter().copied().find(|u| u.name == name)
}

/// Convert a decimal fraction to the fractional part of a scaled value.
///
/// `tex.web` §102. Digits are folded in from the *right*, each step halving
/// the accumulated value, so `.5` is exactly 32768 and no rounding error can
/// accumulate. TeX keeps at most 17 digits because beyond that they cannot
/// affect the result.
pub fn round_decimals(digits: &[u8]) -> i32 {
    let mut a: i32 = 0;
    for &d in digits.iter().take(17).rev() {
        a = (a + (d as i32) * 0x20000) / 10;
    }
    (a + 1) / 2
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimenError {
    /// A number with no unit after it. TeX says "Illegal unit of measure".
    IllegalUnit,
    /// Beyond ±(2^30 − 1) sp.
    TooLarge,
}

/// Combine an integer part, a decimal fraction and a unit into scaled points.
///
/// Follows `tex.web` §458: the integer part and the fraction are scaled
/// *separately*, and the remainder of the first division is folded into the
/// second. Scaling `whole * 65536 + frac` in one multiplication overflows for
/// perfectly ordinary values — 10000in is only 723 million scaled points, but
/// the intermediate product is 47 billion.
pub fn make_dimen(
    negative: bool,
    whole: i64,
    frac: &[u8],
    unit: Unit,
    unit_name: &str,
) -> Result<Scaled, DimenError> {
    // `sp` is the base unit: the value is already in scaled points, and a
    // fractional scaled point does not exist.
    if unit_name == "sp" {
        if whole.abs() > MAX_DIMEN as i64 {
            return Err(DimenError::TooLarge);
        }
        return Ok(Scaled(if negative {
            -(whole as i32)
        } else {
            whole as i32
        }));
    }

    let mut f = round_decimals(frac) as i64;
    let mut cur = whole;

    if unit.num != unit.den {
        // TeX's own guard: beyond this the scaled result cannot fit.
        if cur >= 16384 {
            return Err(DimenError::TooLarge);
        }
        let (q, r) = xn_over_d(Scaled(cur as i32), unit.num, unit.den);
        cur = q.0 as i64;
        f = (unit.num as i64 * f + 65536 * r.0.unsigned_abs() as i64) / unit.den as i64;
        cur += f / 65536;
        f %= 65536;
    }

    let sp = cur
        .checked_mul(65536)
        .ok_or(DimenError::TooLarge)?
        .checked_add(f)
        .ok_or(DimenError::TooLarge)?;
    if sp.abs() > MAX_DIMEN as i64 {
        return Err(DimenError::TooLarge);
    }
    Ok(Scaled(if negative { -(sp as i32) } else { sp as i32 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dimen(neg: bool, whole: i64, frac: &str, unit: &str) -> Scaled {
        let digits: alloc::vec::Vec<u8> = frac.bytes().map(|b| b - b'0').collect();
        make_dimen(neg, whole, &digits, unit_by_name(unit).expect("unit"), unit)
            .expect("valid dimension")
    }

    #[test]
    fn one_point_is_one_unit() {
        assert_eq!(dimen(false, 1, "", "pt"), Scaled(65536));
        assert_eq!(dimen(true, 1, "", "pt"), Scaled(-65536));
    }

    /// Every value here came from `\the\dimen0` in TeX Live, not from a
    /// calculator:
    ///
    /// ```text
    /// 1in 72.26999pt   1cm 28.45274pt   1mm 2.84526pt   1bp 1.00374pt
    /// 1pc 12.0pt       1dd 1.07pt       1cc 12.8401pt
    /// 2.5cm 71.13188pt   -0.5in -36.135pt   16383.99999pt -> 16383.99998pt
    /// ```
    ///
    /// Note the last one: TeX *stores* 16383.99999pt as 16383.99998pt, because
    /// max_dimen is 2^30-1 sp and the requested value rounds past it.
    #[test]
    fn physical_units_match_the_reference_engine() {
        let pt = |x: &str| -> Scaled {
            // Parse "72.26999" the way the engine prints it.
            let (w, f) = x.split_once('.').unwrap_or((x, ""));
            let neg = w.starts_with('-');
            let whole: i64 = w.trim_start_matches('-').parse().expect("int");
            let digits: alloc::vec::Vec<u8> = f.bytes().map(|b| b - b'0').collect();
            make_dimen(neg, whole, &digits, unit_by_name("pt").expect("pt"), "pt").expect("valid")
        };
        assert_eq!(dimen(false, 1, "", "in"), pt("72.26999"));
        assert_eq!(dimen(false, 1, "", "cm"), pt("28.45274"));
        assert_eq!(dimen(false, 1, "", "mm"), pt("2.84526"));
        assert_eq!(dimen(false, 1, "", "bp"), pt("1.00374"));
        assert_eq!(dimen(false, 1, "", "pc"), pt("12.0"));
        assert_eq!(dimen(false, 1, "", "dd"), pt("1.07"));
        assert_eq!(dimen(false, 1, "", "cc"), pt("12.8401"));
        assert_eq!(dimen(false, 2, "5", "cm"), pt("71.13188"));
        assert_eq!(dimen(true, 0, "5", "in"), pt("-36.135"));
    }

    #[test]
    fn sp_is_the_base_unit_and_is_not_scaled() {
        assert_eq!(dimen(false, 1, "", "sp"), Scaled(1));
        assert_eq!(dimen(false, 65536, "", "sp"), Scaled(65536));
    }

    /// The fraction is built exactly, not by multiplying a float.
    #[test]
    fn decimal_fractions_are_exact() {
        assert_eq!(round_decimals(&[5]), 32768, ".5 is exactly half a point");
        assert_eq!(round_decimals(&[2, 5]), 16384, ".25 is exactly a quarter");
        assert_eq!(round_decimals(&[]), 0);
        assert_eq!(dimen(false, 0, "5", "pt"), Scaled(32768));
        assert_eq!(dimen(false, 2, "5", "pt"), Scaled(2 * 65536 + 32768));
    }

    /// plain.tex line 254 asks for 16383.99999pt, and TeX stores
    /// 16383.99998pt — max_dimen is 2^30-1 sp and the request rounds past it.
    #[test]
    fn maxdimen_saturates_rather_than_overflowing() {
        let d = dimen(false, 16383, "99999", "pt");
        assert!(d.0 <= MAX_DIMEN, "must not exceed max_dimen, got {}", d.0);
        assert_eq!(crate::scaled::print_scaled(d), "16383.99998");
    }

    #[test]
    fn oversized_dimensions_are_rejected_not_wrapped() {
        let digits: alloc::vec::Vec<u8> = alloc::vec![];
        assert_eq!(
            make_dimen(
                false,
                100_000,
                &digits,
                unit_by_name("pt").expect("pt"),
                "pt"
            ),
            Err(DimenError::TooLarge)
        );
        // And through a unit conversion.
        assert_eq!(
            make_dimen(
                false,
                10_000,
                &digits,
                unit_by_name("in").expect("in"),
                "in"
            ),
            Err(DimenError::TooLarge)
        );
    }

    #[test]
    fn unknown_units_are_not_recognized() {
        assert!(unit_by_name("zz").is_none());
        assert!(unit_by_name("").is_none());
        assert!(unit_by_name("pt").is_some());
    }

    #[test]
    fn more_than_seventeen_fraction_digits_are_ignored() {
        // TeX keeps 17; beyond that they cannot change the answer.
        let long: alloc::vec::Vec<u8> = core::iter::repeat_n(9u8, 30).collect();
        let short: alloc::vec::Vec<u8> = core::iter::repeat_n(9u8, 17).collect();
        assert_eq!(round_decimals(&long), round_decimals(&short));
    }
}
