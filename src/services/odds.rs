//! The parimutuel odds formula, byte-identical to `MatchController`'s inline computation
//! (`toggleMeron`/`toggleWala`/`updateMatchBetStatus`/`updateMatchStatus`/`BetController@store`/
//! `@voidBet` all repeat this exact sequence of float operations -- see docs/WORKFLOW.md section
//! 7.3). Deliberately `f64`, not `rust_decimal`: the live app computes this in PHP floats and
//! stores the result in a `FLOAT` column, so bit-identical behavior matters more here than exact
//! decimal arithmetic would.

use crate::money::{php_round, Money};

/// Returns `Some((meron_odds, wala_odds))` when both sides have a positive pool (the only case
/// the real formula recomputes); `None` when one side is empty, in which case every caller in
/// the real app either leaves the previous odds value untouched (`toggleMeron`/`toggleWala`/
/// `updateMatchBetStatus`/`updateMatchStatus`) or sets both to `1.0` (`BetController@store`/
/// `voidBet`) -- the two behaviors differ by call site, so this function doesn't decide that part
/// for you.
pub fn recompute_odds(meron_total: Money, wala_total: Money, event_percentage: f64) -> Option<(f64, f64)> {
    let m = meron_total.value();
    let w = wala_total.value();
    if m > 0.0 && w > 0.0 {
        let meron_calculated = m - (m * event_percentage);
        let wala_calculated = w - (w * event_percentage);
        let computed_total = meron_calculated + wala_calculated;
        let meron_odds = php_round(computed_total / m, 2);
        let wala_odds = php_round(computed_total / w, 2);
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
        // 7% commission, meron=1000, wala=500 -> pool_net = 1395, odds = 1395/1000=1.395~1.4, 1395/500=2.79
        let (m, w) = recompute_odds(Money::new(1000.0), Money::new(500.0), 0.07).unwrap();
        assert_eq!(m, 1.4);
        assert_eq!(w, 2.79);
    }

    #[test]
    fn one_sided_pool_is_none() {
        assert!(recompute_odds(Money::new(1000.0), Money::new(0.0), 0.07).is_none());
    }
}
