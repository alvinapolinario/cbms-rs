use crate::money::Money;
use serde::Serialize;

/// `event_tellers` -- a teller's assignment + running cash balance for one event. This id
/// (`event_teller_id`), not `teller_id` alone, is the teller's real business identity
/// throughout the rest of the system (bets, cash movements, claims, the ledger) -- see
/// docs/WORKFLOW.md section 4.
#[derive(Clone, Debug, sqlx::FromRow, Serialize)]
pub struct EventTeller {
    pub event_teller_id: i32,
    pub event_id: i32,
    pub teller_id: i32,
    pub teller_balance: Money,
    pub teller_match_balance: Money,
}
