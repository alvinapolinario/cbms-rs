use crate::money::Money;
use crate::types::DbDateTime;
use serde::Serialize;

/// `bets` table. See docs/WORKFLOW.md section 7.4 (placement) / 7.5 (settlement).
#[derive(Clone, Debug, sqlx::FromRow, Serialize)]
pub struct Bet {
    pub bet_id: i32,
    pub event_teller_id: i32,
    pub match_id: i32,
    pub bet_receipt_code: String,
    pub bet_side: String,
    pub bet_amount: Money,
    pub bet_win_amount: Money,
    pub bet_payout_amount: Money,
    pub bet_payout_datetime: Option<DbDateTime>,
    pub bet_status: i8,
    pub bet_datetime: DbDateTime,
    pub bet_is_winner: i8,
    pub bet_void_datetime: Option<DbDateTime>,
    pub bet_payout_by: Option<i32>,
}

pub const BET_STATUS_LIVE: i8 = 1;
pub const BET_STATUS_VOIDED: i8 = 2;

/// `BetController@store`'s response bolts three extra, flat (not nested) fields onto the
/// freshly-created `Bet` row before returning it -- see docs/WORKFLOW.md section 7.4 step 14 and
/// the plan's verified response shape. This struct reproduces that exact flat shape.
#[derive(Clone, Debug, Serialize)]
pub struct BetReceipt {
    #[serde(flatten)]
    pub bet: Bet,
    pub match_number: i32,
    pub event_name: String,
    pub teller_name: String,
}
