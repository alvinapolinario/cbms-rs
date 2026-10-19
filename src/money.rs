//! Money handling that bit-matches PHP's float/DECIMAL(16,2) behavior rather than using exact
//! decimal arithmetic. See docs/WORKFLOW.md section 10 item 1 in the sibling `sabonglara` repo:
//! the live betting/odds/payout path in Laravel does everything in raw PHP floats
//! (`round()`/`floor()`), and every DECIMAL column is read back as a JSON *string* because none
//! of the Eloquent models cast money fields to `float`/`decimal:N`. A `rust_decimal`-based exact
//! implementation would silently diverge from production numbers, so `Money` wraps an `f64` and
//! is only ever formatted to a fixed 2-decimal string at the DB/JSON boundary.
//!
//! NOTE: `php_round` below approximates PHP's internal `round()` (which applies its own
//! floating-point precision correction, see PHP's `math.c`). It should be correct for the
//! overwhelming majority of real peso amounts, but has not been verified bit-for-bit against a
//! running copy of the Laravel app yet -- see docs/WORKFLOW.md's verification approach. Treat any
//! off-by-one-centavo discrepancy found during side-by-side testing as a bug in this function,
//! not in the caller.

use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::ops::{Add, Sub};
use std::str::FromStr;

/// Round-half-away-from-zero to `decimals` places, matching PHP's default `round()` mode
/// (`PHP_ROUND_HALF_UP`), with a small epsilon correction for values that sit almost exactly on
/// a rounding boundary due to binary floating-point representation error (e.g. `1.005` is
/// actually stored as slightly less than `1.005`; a naive `(x * 100.0).round() / 100.0` would
/// round that down to `1.00` instead of PHP's `1.01`).
pub fn php_round(value: f64, decimals: i32) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let factor = 10f64.powi(decimals);
    let scaled = value * factor;
    let epsilon = 1e-9 * scaled.abs().max(1.0);
    let rounded = if scaled >= 0.0 {
        (scaled + 0.5 + epsilon).floor()
    } else {
        (scaled - 0.5 - epsilon).ceil()
    };
    rounded / factor
}

#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct Money(pub f64);

impl Money {
    pub const ZERO: Money = Money(0.0);

    pub fn new(value: f64) -> Self {
        Money(value)
    }

    /// Matches PHP `round($value, 2)`.
    pub fn round2(self) -> Self {
        Money(php_round(self.0, 2))
    }

    /// Matches PHP `floor($value)`.
    pub fn floor(self) -> Self {
        Money(self.0.floor())
    }

    pub fn value(self) -> f64 {
        self.0
    }

    pub fn is_positive(self) -> bool {
        self.0 > 0.0
    }

    /// The exact textual form stored in a `DECIMAL(16,2)` column and returned in Laravel's JSON
    /// responses (since no model casts these columns to float/decimal -- see module docs).
    pub fn to_db_string(self) -> String {
        format!("{:.2}", self.0)
    }
}

impl Add for Money {
    type Output = Money;
    fn add(self, rhs: Money) -> Money {
        Money(self.0 + rhs.0)
    }
}

impl Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Money) -> Money {
        Money(self.0 - rhs.0)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_db_string())
    }
}

impl FromStr for Money {
    type Err = std::num::ParseFloatError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Money(s.trim().parse::<f64>()?))
    }
}

/// Matches Eloquent's default DECIMAL-as-string JSON serialization.
impl Serialize for Money {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_db_string())
    }
}

/// Accepts either a JSON string (`"123.45"`, matching what the DB/Laravel would send) or a JSON
/// number (`123.45`), since inbound client requests like `create-bet`'s `amount` field are not
/// guaranteed to be one or the other and PHP's `(float) $request->amount` accepts both.
impl<'de> Deserialize<'de> for Money {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum NumOrStr {
            Num(f64),
            Str(String),
        }
        match NumOrStr::deserialize(deserializer)? {
            NumOrStr::Num(n) => Ok(Money(n)),
            NumOrStr::Str(s) => s.trim().parse::<f64>().map(Money).map_err(DeError::custom),
        }
    }
}

// --- sqlx <-> MySQL DECIMAL(16,2) ------------------------------------------------------------
// Delegates to `rust_decimal::Decimal`'s own sqlx support (sqlx's MySQL driver requires the
// `rust_decimal` or `bigdecimal` feature to decode DECIMAL columns at all -- plain `String` is
// not considered a compatible decode target for them). `Decimal` gives us exact decimal text on
// the way in/out; we immediately convert to/from `f64` at the boundary since all arithmetic on
// `Money` intentionally happens in float space (see module docs).

impl sqlx::Type<sqlx::MySql> for Money {
    fn type_info() -> sqlx::mysql::MySqlTypeInfo {
        <rust_decimal::Decimal as sqlx::Type<sqlx::MySql>>::type_info()
    }

    fn compatible(ty: &sqlx::mysql::MySqlTypeInfo) -> bool {
        <rust_decimal::Decimal as sqlx::Type<sqlx::MySql>>::compatible(ty)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::MySql> for Money {
    fn decode(
        value: sqlx::mysql::MySqlValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let decimal = <rust_decimal::Decimal as sqlx::Decode<sqlx::MySql>>::decode(value)?;
        let parsed: f64 = decimal.to_string().parse()?;
        Ok(Money(parsed))
    }
}

impl<'q> sqlx::Encode<'q, sqlx::MySql> for Money {
    fn encode_by_ref(
        &self,
        buf: &mut Vec<u8>,
    ) -> sqlx::encode::IsNull {
        let decimal: rust_decimal::Decimal = self
            .to_db_string()
            .parse()
            .expect("Money::to_db_string always produces a valid decimal literal");
        <rust_decimal::Decimal as sqlx::Encode<sqlx::MySql>>::encode(decimal, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round2_half_up() {
        assert_eq!(php_round(1.005, 2), 1.01);
        assert_eq!(php_round(2.675, 2), 2.68);
        assert_eq!(php_round(1.004, 2), 1.00);
        assert_eq!(php_round(-1.005, 2), -1.01);
    }

    #[test]
    fn db_string_always_two_places() {
        assert_eq!(Money(5.0).to_db_string(), "5.00");
        assert_eq!(Money(5.1).to_db_string(), "5.10");
        assert_eq!(Money(1234.567).round2().to_db_string(), "1234.57");
    }

    #[test]
    fn floor_matches_php_floor() {
        assert_eq!(Money(199.99).floor().value(), 199.0);
        assert_eq!(Money(200.0).floor().value(), 200.0);
    }

    #[test]
    fn winning_payout_is_floor_of_amount_times_odds() {
        // MatchController@updateMatchStatus: floor($bet->bet_amount * $odds)
        assert_eq!((100.0_f64 * 1.395).floor(), 139.0);
        assert_eq!((10.0_f64 * 1.99).floor(), 19.0);
    }
}
