use crate::money::Money;
use crate::types::Rate;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// `events` -- one row per derby event/day.
#[derive(Clone, Debug, sqlx::FromRow, Serialize, Deserialize)]
pub struct Event {
    pub event_id: i32,
    pub event_name: String,
    pub event_description: String,
    pub event_date: NaiveDate,
    pub event_status: String,
    pub admin_cash_on_hand: Money,
    pub teller_initial_cash_on_hand: Money,
    pub prizes: Money,
    pub rd: Money,
    pub event_percentage: Rate,
    pub matching_event_id: Option<i32>,
}

/// Body of `/events-admin/store` and `/events-admin/update`. Mirrors `EventController@store`'s
/// validated fields exactly (WORKFLOW.md section 7.1 step 1) -- `event_status` is restricted to
/// `Active`/`Completed` there (never `Closed` on direct create/update).
#[derive(Clone, Debug, Deserialize)]
pub struct EventRequest {
    pub event_name: String,
    pub event_description: String,
    pub event_percentage: Rate,
    pub event_date: NaiveDate,
    pub event_status: String,
    pub admin_cash_on_hand: Money,
    pub teller_initial_cash_on_hand: Money,
    pub prizes: Money,
    pub rd: Money,
}
