//! The parimutuel odds formula from `MatchController`, with the result rounded to one decimal
//! place. Deliberately `f64`, not `rust_decimal`: the pool math is still PHP float arithmetic,
//! and the result is stored in a `FLOAT` column.

use crate::money::{php_round, Money};

/// Returns `Some((meron_odds, wala_odds))` when both sides have a positive pool (the only case
/// the real formula recomputes); `None` when one side is empty, in which case every caller in
/// the real app either leaves the previous odds value untouched (`toggleMeron`/`toggleWala`/
/// `updateMatchBetStatus`/`updateMatchStatus`) or sets both to `1.0` (`BetController@store`/
/// `voidBet`) -- the two behaviors differ by call site, so this function doesn't decide that part
/// for you.
/// One decimal place for display and JSON. `FLOAT` storage turns 1.55 into
/// 1.5499999523162842, so the epsilon is wide enough to recover that value and
/// still too small to move a genuine tenth (1.54 stays 1.5).
pub fn display_odds(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let scaled = value * 10.0;
    let epsilon = 1e-6 * scaled.abs().max(1.0);
    let rounded = if scaled >= 0.0 {
        (scaled + 0.5 + epsilon).floor()
    } else {
        (scaled - 0.5 - epsilon).ceil()
    };
    rounded / 10.0
}

pub fn serialize_odds<S: serde::Serializer>(value: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(display_odds(f64::from(*value)))
}

pub fn recompute_odds(meron_total: Money, wala_total: Money, event_percentage: f64) -> Option<(f64, f64)> {
    let m = meron_total.value();
    let w = wala_total.value();
    if m > 0.0 && w > 0.0 {
        let meron_calculated = m - (m * event_percentage);
        let wala_calculated = w - (w * event_percentage);
        let computed_total = meron_calculated + wala_calculated;
        let meron_odds = php_round(computed_total / m, 1);
        let wala_odds = php_round(computed_total / w, 1);
        Some((meron_odds, wala_odds))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parimutuel_split() {
        // 7% commission, meron=1000, wala=500 -> pool_net = 1395, odds = 1395/1000=1.395~1.4, 1395/500=2.79~2.8
        let (m, w) = recompute_odds(Money::new(1000.0), Money::new(500.0), 0.07).unwrap();
        assert_eq!(m, 1.4);
        assert_eq!(w, 2.8);
    }

    #[test]
    fn one_sided_pool_is_none() {
        assert!(recompute_odds(Money::new(1000.0), Money::new(0.0), 0.07).is_none());
    }

    #[test]
    fn float_noise_displays_as_one_decimal() {
        assert_eq!(display_odds(1.5499999523162842), 1.6);
        assert_eq!(display_odds(2.319999933242798), 2.3);
        assert_eq!(display_odds(1.659999966621399), 1.7);
        assert_eq!(serde_json::to_string(&display_odds(1.5499999523162842)).unwrap(), "1.6");
    }
}
