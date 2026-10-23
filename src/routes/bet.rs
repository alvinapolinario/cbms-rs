//! `/create-bet`, `/void-bet` -- mirrors `BetController@store`/`@voidBet` (docs/WORKFLOW.md
//! section 7.4), written directly against the PHP source.

use crate::auth::{AuthedAdmin, AuthedTeller};
use crate::extract::JsonOrForm;
use crate::models::{Bet, BetReceipt};
use crate::money::Money;
use crate::services::{ledger, odds, transactions::create_log};
use crate::types::DbDateTime;
use crate::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::Arc;

/// Matches the `$fail` closure in `BetController@store`: rolls back and returns 200 with
/// `{success:false,...}` (not an HTTP error status).
fn soft_fail(message: &str) -> Response {
    Json(json!({ "success": false, "message": message })).into_response()
}

#[derive(Deserialize)]
pub struct CreateBetRequest {
    pub side: String,
    pub amount: Money,
    pub match_id: i32,
}

pub async fn create_bet(
    State(state): State<Arc<AppState>>,
    teller: AuthedTeller,
    crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<CreateBetRequest>,
) -> Response {
    if body.side != "Meron" && body.side != "Wala" {
        return soft_fail("Invalid bet side");
    }
    let amount = body.amount.round2();

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return soft_fail(crate::error::log_fault(&e)),
    };

    let event_row = sqlx::query("SELECT event_id, event_name FROM events WHERE event_status = 'Active' LIMIT 1")
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    let Some(event_row) = event_row else {
        return soft_fail("No active event available!");
    };
    let event_id: i32 = event_row.get("event_id");
    let event_name: String = event_row.get("event_name");
    let event_percentage: f64 = sqlx::query_scalar("SELECT event_percentage FROM events WHERE event_id = ?")
        .bind(event_id)
        .fetch_one(&mut *tx)
        .await
        .map(|r: crate::types::Rate| r.value())
        .unwrap_or(0.0);

    // Lock the one open+ongoing fight for this event; bets on it are serialized by this lock.
    let match_row = sqlx::query(
        "SELECT * FROM matches WHERE event_id = ? AND match_bet_status = 'Open' AND match_status = 'Ongoing'
         ORDER BY match_number DESC LIMIT 1 FOR UPDATE",
    )
    .bind(event_id)
    .fetch_optional(&mut *tx)
    .await
    .ok()
    .flatten();
    let Some(match_row) = match_row else {
        return soft_fail("No ongoing match found or bet status is not open.");
    };
    let fight_match_id: i32 = match_row.get("match_id");
    let fight_match_number: i32 = match_row.get("match_number");
    let fight_meron_total: Money = match_row.get("meron_total_bet");
    let fight_wala_total: Money = match_row.get("wala_total_bet");
    let fight_meron_bet_status: i8 = match_row.get("meron_bet_status");
    let fight_wala_bet_status: i8 = match_row.get("wala_bet_status");

    let et_row = sqlx::query("SELECT event_teller_id, teller_id FROM event_tellers WHERE event_teller_id = ?")
        .bind(teller.event_teller_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    if et_row.is_none() {
        return soft_fail("Teller not found! Please relogin your account!");
    }

    let tel_row = sqlx::query("SELECT account_id, teller_name, phone_uid FROM tellers WHERE teller_id = ?")
        .bind(teller.teller_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    let Some(tel_row) = tel_row else {
        return soft_fail("Teller not found! Please relogin your account!");
    };
    let tel_account_id: i32 = tel_row.get("account_id");
    let tel_teller_name: String = tel_row.get("teller_name");
    let tel_phone_uid: String = tel_row.get("phone_uid");
    if tel_phone_uid.is_empty() {
        return soft_fail("Your account is not activated! Please contact administrator!");
    }

    // Sabong2026 compares this field to match_number, even though the JSON key is match_id.
    if fight_match_number != body.match_id {
        return soft_fail("Fight is already lapsed or not existing");
    }

    // Side lock 1 disables that side. A teller named "special" is exempt (BetController@store).
    let side_locked = (body.side == "Meron" && fight_meron_bet_status == 1)
        || (body.side == "Wala" && fight_wala_bet_status == 1);
    if side_locked && tel_teller_name != "special" {
        return soft_fail(&format!("{} bet is currently disabled", body.side));
    }

    // Cryptographically random receipt digits, unique across all bets.
    let digits = state.config.receipt_random_digits as usize;
    let receipt_code = loop {
        // `ThreadRng` is `!Send`, so it must not be held across the `.await` below -- scoped to
        // end before it.
        let candidate = {
            let mut rng = rand::thread_rng();
            let random_digits: String = (0..digits).map(|_| rng.gen_range(0..10).to_string()).collect();
            format!("{}{fight_match_number}{random_digits}", teller.event_teller_id)
        };
        let exists: bool = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM bets WHERE bet_receipt_code = ?")
            .bind(&candidate)
            .fetch_one(&mut *tx)
            .await
            .unwrap_or(0)
            > 0;
        if !exists {
            break candidate;
        }
    };

    let now = DbDateTime::now();
    let insert = sqlx::query(
        "INSERT INTO bets
            (event_teller_id, match_id, bet_receipt_code, bet_side, bet_amount, bet_win_amount, bet_payout_amount,
             bet_payout_datetime, bet_status, bet_datetime, bet_is_winner, bet_void_datetime, bet_payout_by)
         VALUES (?, ?, ?, ?, ?, 0, 0, NULL, 1, ?, 0, NULL, NULL)",
    )
    .bind(teller.event_teller_id)
    .bind(fight_match_id)
    .bind(&receipt_code)
    .bind(&body.side)
    .bind(amount.to_db_string())
    .bind(now.0)
    .execute(&mut *tx)
    .await;
    let bet_id = match insert {
        Ok(r) => r.last_insert_id() as i32,
        Err(e) => return soft_fail(crate::error::log_fault(&e)),
    };

    create_log(&mut *tx, tel_account_id, "Teller App", &format!("Bet Created. receipt={receipt_code}")).await;

    let (new_meron_total, new_wala_total) = match body.side.as_str() {
        "Meron" => (fight_meron_total + amount, fight_wala_total),
        _ => (fight_meron_total, fight_wala_total + amount),
    };
    let (meron_odds, wala_odds) = odds::recompute_odds(new_meron_total, new_wala_total, event_percentage).unwrap_or((1.0, 1.0));

    if let Err(e) = sqlx::query(
        "UPDATE matches SET meron_total_bet = ?, wala_total_bet = ?, meron_odds = ?, wala_odds = ? WHERE match_id = ?",
    )
    .bind(new_meron_total.to_db_string())
    .bind(new_wala_total.to_db_string())
    .bind(meron_odds as f32)
    .bind(wala_odds as f32)
    .bind(fight_match_id)
    .execute(&mut *tx)
    .await
    {
        return soft_fail(crate::error::log_fault(&e));
    }

    if let Err(e) = sqlx::query(
        "UPDATE event_tellers SET teller_balance = teller_balance + ?, teller_match_balance = teller_match_balance + ? WHERE event_teller_id = ?",
    )
    .bind(amount.to_db_string())
    .bind(amount.to_db_string())
    .bind(teller.event_teller_id)
    .execute(&mut *tx)
    .await
    {
        return soft_fail(crate::error::log_fault(&e));
    }

    if let Err(e) = ledger::record(
        &mut tx,
        teller.event_teller_id,
        ledger::TYPE_BET,
        amount,
        Money::ZERO,
        Some(tel_account_id),
        ledger::LedgerMeta {
            fight_no: Some(fight_match_number),
            match_id: Some(fight_match_id),
            reference: Some(&receipt_code),
            note: Some(&body.side),
            source_table: Some("bets"),
            source_id: Some(bet_id),
            ..Default::default()
        },
    )
    .await
    {
        return soft_fail(crate::error::log_fault(&e));
    }

    if let Err(e) = tx.commit().await {
        return soft_fail(crate::error::log_fault(&e));
    }

    let bet = match sqlx::query_as::<_, Bet>("SELECT * FROM bets WHERE bet_id = ?")
        .bind(bet_id)
        .fetch_one(&state.db)
        .await
    {
        Ok(b) => b,
        Err(e) => return soft_fail(crate::error::log_fault(&e)),
    };

    crate::services::realtime::match_updated(&state, fight_match_id).await;
    crate::services::realtime::teller_balances(&state, event_id).await;

    let receipt = BetReceipt {
        bet,
        match_number: fight_match_number,
        event_name,
        teller_name: tel_teller_name,
    };

    Json(json!({ "success": true, "message": "Bet created successfully!", "bet": receipt })).into_response()
}

#[derive(Deserialize)]
pub struct VoidBetRequest {
    pub receipt_code: String,
}

pub async fn void_bet(
    State(state): State<Arc<AppState>>,
    teller: AuthedTeller,
    crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<VoidBetRequest>,
) -> Response {
    let receipt_code = body.receipt_code.trim();
    if receipt_code.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "success": false, "message": "Receipt code is required." })),
        )
            .into_response();
    }

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return soft_fail(crate::error::log_fault(&e)),
    };

    let bet_row = sqlx::query("SELECT * FROM bets WHERE bet_receipt_code = ? FOR UPDATE")
        .bind(receipt_code)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    let Some(bet_row) = bet_row else {
        return soft_fail("Bet does not exist!");
    };
    let bet_id: i32 = bet_row.get("bet_id");
    let bet_event_teller_id: i32 = bet_row.get("event_teller_id");
    let bet_match_id: i32 = bet_row.get("match_id");
    let bet_side: String = bet_row.get("bet_side");
    let bet_amount: Money = bet_row.get("bet_amount");
    let bet_status: i8 = bet_row.get("bet_status");

    if bet_event_teller_id != teller.event_teller_id {
        return soft_fail("You cannot void this bet. Please tell the bettor to go the teller who made this bet!");
    }
    if bet_status == 2 {
        return soft_fail("Bet has already been voided!");
    }
    let claimed: bool = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM claims WHERE bet_id = ?")
        .bind(bet_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(0)
        > 0;
    if claimed {
        return soft_fail("Cannot void the receipt because its winnings have already been claimed!");
    }

    let match_row = sqlx::query("SELECT * FROM matches WHERE match_id = ? AND match_status = 'Ongoing' FOR UPDATE")
        .bind(bet_match_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    let Some(match_row) = match_row else {
        return soft_fail("Cannot void the receipt because the fight has already finished!");
    };
    let match_bet_status: String = match_row.get("match_bet_status");
    if match_bet_status == "Closed" {
        return soft_fail("Cannot void the receipt because the fight is already closed.");
    }
    let match_number: i32 = match_row.get("match_number");
    let meron_total: Money = match_row.get("meron_total_bet");
    let wala_total: Money = match_row.get("wala_total_bet");

    let now = DbDateTime::now();
    if let Err(e) = sqlx::query(
        "UPDATE bets SET bet_status = 2, bet_void_datetime = ?, bet_payout_amount = bet_amount,
            bet_payout_datetime = ?, bet_payout_by = ? WHERE bet_id = ?",
    )
    .bind(now.0)
    .bind(now.0)
    .bind(teller.event_teller_id)
    .bind(bet_id)
    .execute(&mut *tx)
    .await
    {
        return soft_fail(crate::error::log_fault(&e));
    }

    if let Err(e) = sqlx::query(
        "UPDATE event_tellers SET teller_balance = teller_balance - ?, teller_match_balance = teller_match_balance - ? WHERE event_teller_id = ?",
    )
    .bind(bet_amount.to_db_string())
    .bind(bet_amount.to_db_string())
    .bind(teller.event_teller_id)
    .execute(&mut *tx)
    .await
    {
        return soft_fail(crate::error::log_fault(&e));
    }

    if let Err(e) = ledger::record(
        &mut tx,
        teller.event_teller_id,
        ledger::TYPE_VOID,
        Money::ZERO,
        bet_amount,
        Some(teller.account_id),
        ledger::LedgerMeta {
            fight_no: Some(match_number),
            match_id: Some(bet_match_id),
            reference: Some(receipt_code),
            note: Some(&format!("Voided {bet_side} bet; money returned to the bettor")),
            source_table: Some("bets"),
            source_id: Some(bet_id),
            ..Default::default()
        },
    )
    .await
    {
        return soft_fail(crate::error::log_fault(&e));
    }

    let (new_meron_total, new_wala_total) = match bet_side.as_str() {
        "Meron" => (meron_total - bet_amount, wala_total),
        "Wala" => (meron_total, wala_total - bet_amount),
        _ => (meron_total, wala_total),
    };

    let event_percentage: f64 = sqlx::query_scalar(
        "SELECT ev.event_percentage FROM events ev JOIN event_tellers et ON et.event_id = ev.event_id WHERE et.event_teller_id = ?",
    )
    .bind(teller.event_teller_id)
    .fetch_one(&mut *tx)
    .await
    .map(|r: crate::types::Rate| r.value())
    .unwrap_or(0.0);
    let (meron_odds, wala_odds) = odds::recompute_odds(new_meron_total, new_wala_total, event_percentage).unwrap_or((1.0, 1.0));

    if let Err(e) = sqlx::query(
        "UPDATE matches SET meron_total_bet = ?, wala_total_bet = ?, meron_odds = ?, wala_odds = ? WHERE match_id = ?",
    )
    .bind(new_meron_total.to_db_string())
    .bind(new_wala_total.to_db_string())
    .bind(meron_odds as f32)
    .bind(wala_odds as f32)
    .bind(bet_match_id)
    .execute(&mut *tx)
    .await
    {
        return soft_fail(crate::error::log_fault(&e));
    }

    create_log(&mut *tx, teller.account_id, "Teller App", &format!("Bet voided. receipt={receipt_code}")).await;

    if let Err(e) = tx.commit().await {
        return soft_fail(crate::error::log_fault(&e));
    }

    crate::services::realtime::teller_balances(&state, teller.event_id).await;
    crate::services::realtime::match_updated(&state, bet_match_id).await;

    let bet_view = sqlx::query("SELECT * FROM bets_view WHERE bet_id = ?")
        .bind(bet_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let bet_json = bet_view.as_ref().map(bet_view_row_to_json);

    Json(json!({ "bet": bet_json, "success": true, "message": "Receipt voided successfully!" })).into_response()
}

/// Full `bets_view` row -> JSON, covering every column the view actually joins (`bets` +
/// `event_tellers` + `tellers` + `matches` + `events`). Several endpoints return a raw
/// `bets_view` row verbatim (`voidBet`, the reprint family, the history/unclaimed/voided
/// lists), so this is shared rather than re-derived per call site.
pub(crate) fn bet_view_row_to_json(row: &sqlx::mysql::MySqlRow) -> serde_json::Value {
    json!({
        "bet_id": row.try_get::<i32, _>("bet_id").unwrap_or_default(),
        "event_teller_id": row.try_get::<i32, _>("event_teller_id").unwrap_or_default(),
        "match_id": row.try_get::<i32, _>("match_id").unwrap_or_default(),
        "bet_receipt_code": row.try_get::<String, _>("bet_receipt_code").unwrap_or_default(),
        "bet_side": row.try_get::<String, _>("bet_side").unwrap_or_default(),
        "bet_amount": row.try_get::<Money, _>("bet_amount").ok(),
        "bet_win_amount": row.try_get::<Money, _>("bet_win_amount").ok(),
        "bet_payout_amount": row.try_get::<Money, _>("bet_payout_amount").ok(),
        "bet_payout_datetime": row.try_get::<Option<DbDateTime>, _>("bet_payout_datetime").ok().flatten(),
        "bet_status": row.try_get::<i8, _>("bet_status").unwrap_or_default(),
        "bet_datetime": row.try_get::<DbDateTime, _>("bet_datetime").ok(),
        "bet_is_winner": row.try_get::<i8, _>("bet_is_winner").unwrap_or_default(),
        "bet_void_datetime": row.try_get::<Option<DbDateTime>, _>("bet_void_datetime").ok().flatten(),
        "bet_payout_by": row.try_get::<Option<i32>, _>("bet_payout_by").ok().flatten(),
        "teller_id": row.try_get::<i32, _>("teller_id").unwrap_or_default(),
        "teller_balance": row.try_get::<Money, _>("teller_balance").ok(),
        "teller_match_balance": row.try_get::<Money, _>("teller_match_balance").ok(),
        "teller_name": row.try_get::<String, _>("teller_name").unwrap_or_default(),
        "contact_number": row.try_get::<String, _>("contact_number").unwrap_or_default(),
        "phone_uid": row.try_get::<String, _>("phone_uid").unwrap_or_default(),
        "match_number": row.try_get::<i32, _>("match_number").unwrap_or_default(),
        "match_winner": row.try_get::<String, _>("match_winner").unwrap_or_default(),
        "meron_total_bet": row.try_get::<Money, _>("meron_total_bet").ok(),
        "wala_total_bet": row.try_get::<Money, _>("wala_total_bet").ok(),
        "meron_odds": row.try_get::<f32, _>("meron_odds").unwrap_or_default(),
        "wala_odds": row.try_get::<f32, _>("wala_odds").unwrap_or_default(),
        "match_status": row.try_get::<String, _>("match_status").unwrap_or_default(),
        "match_bet_status": row.try_get::<String, _>("match_bet_status").unwrap_or_default(),
        "meron_bet_status": row.try_get::<i8, _>("meron_bet_status").unwrap_or_default(),
        "wala_bet_status": row.try_get::<i8, _>("wala_bet_status").unwrap_or_default(),
        "match_created_datetime": row.try_get::<DbDateTime, _>("match_created_datetime").ok(),
        "event_id": row.try_get::<i32, _>("event_id").unwrap_or_default(),
        "event_name": row.try_get::<String, _>("event_name").unwrap_or_default(),
        "event_description": row.try_get::<String, _>("event_description").unwrap_or_default(),
        "event_date": row.try_get::<chrono::NaiveDate, _>("event_date").ok(),
        "event_status": row.try_get::<String, _>("event_status").unwrap_or_default(),
    })
}

async fn active_event_exists(state: &Arc<AppState>) -> bool {
    sqlx::query("SELECT 1 FROM events WHERE event_status = 'Active' LIMIT 1")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// `POST /reprint-last-receipt` -- mirrors `BetController@reprint`: the teller's own most
/// recent bet, any fight.
pub async fn reprint(State(state): State<Arc<AppState>>, teller: AuthedTeller) -> Response {
    if !active_event_exists(&state).await {
        return Json(json!({ "success": false, "message": "No active event available!" })).into_response();
    }
    let bet = sqlx::query("SELECT * FROM bets_view WHERE event_teller_id = ? ORDER BY bet_id DESC LIMIT 1")
        .bind(teller.event_teller_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(bet) = bet else {
        return Json(json!({ "success": false, "message": "No previous receipt found!" })).into_response();
    };
    create_log(&state.db, teller.account_id, "Teller App", "Bet receipt reprint.").await;
    Json(json!({ "success": true, "message": "Bet reprinted successfully!", "bet": bet_view_row_to_json(&bet) })).into_response()
}

#[derive(Deserialize)]
pub struct ReprintTellerRequest {
    pub bet_receipt_code: String,
}

/// `POST /reprint-teller` -- mirrors `BetController@reprintTeller`: reprint by receipt code
/// (any teller's receipt, not just the caller's own -- matches the real source, which has no
/// ownership check here).
pub async fn reprint_teller(
    State(state): State<Arc<AppState>>,
    teller: AuthedTeller,
    JsonOrForm(body): JsonOrForm<ReprintTellerRequest>,
) -> Response {
    if !active_event_exists(&state).await {
        return Json(json!({ "success": false, "message": "No active event available!" })).into_response();
    }
    let bet = sqlx::query("SELECT * FROM bets_view WHERE bet_receipt_code = ?")
        .bind(&body.bet_receipt_code)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(bet) = bet else {
        return Json(json!({ "success": false, "message": "No receipt found!" })).into_response();
    };
    create_log(&state.db, teller.account_id, "Teller App", "Bet receipt reprint.").await;
    Json(json!({ "success": true, "message": "Bet reprinted successfully!", "bet": bet_view_row_to_json(&bet) })).into_response()
}

/// `POST /reprint-admin` -- mirrors `BetController@reprintAdmin`.
pub async fn reprint_admin(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<ReprintTellerRequest>,
) -> Response {
    let event_id: Option<i32> = sqlx::query_scalar("SELECT event_id FROM events WHERE event_status = 'Active' LIMIT 1")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(event_id) = event_id else {
        return Json(json!({ "success": false, "message": "This event is no longer active." })).into_response();
    };
    let bet = sqlx::query("SELECT * FROM bets_view WHERE event_id = ? AND bet_receipt_code = ?")
        .bind(event_id)
        .bind(&body.bet_receipt_code)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(bet) = bet else {
        return Json(json!({ "success": false, "message": "Reprint failed: The bet receipt code does not exist." })).into_response();
    };
    create_log(&state.db, admin.account_id, "Admin App", "Reprint receipt by Admin.").await;
    Json(json!({ "success": true, "message": "Reprint completed successfully.", "bet": bet_view_row_to_json(&bet) })).into_response()
}

/// `POST /bet-history` -- mirrors `BetController@betHistory`. Bare array, no envelope (matches
/// the real source).
pub async fn bet_history(State(state): State<Arc<AppState>>, teller: AuthedTeller) -> Response {
    ongoing_bets(&state, teller.event_teller_id).await
}

/// `POST /bet-history-ongoing` -- mirrors `BetController@betHistoryOngoing`.
/// The Laravel method applies the same filters as `betHistory`.
pub async fn bet_history_ongoing(State(state): State<Arc<AppState>>, teller: AuthedTeller) -> Response {
    ongoing_bets(&state, teller.event_teller_id).await
}

async fn ongoing_bets(state: &Arc<AppState>, event_teller_id: i32) -> Response {
    let bets = sqlx::query(
        "SELECT * FROM bets_view WHERE event_teller_id = ? AND event_status = 'Active' AND match_status = 'Ongoing' ORDER BY bet_datetime DESC",
    )
    .bind(event_teller_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    Json(bets.iter().map(bet_view_row_to_json).collect::<Vec<_>>()).into_response()
}

/// `POST /unclaimed-receipts` -- mirrors `BetController@unclaimedReceipts`.
pub async fn unclaimed_receipts(State(state): State<Arc<AppState>>, teller: AuthedTeller) -> Response {
    let bets = sqlx::query(
        "SELECT * FROM bets_view WHERE event_teller_id = ? AND bet_payout_amount > 0 AND event_status = 'Active'
         AND bet_status = 1 AND match_status IN ('Draw', 'Cancelled') AND bet_payout_by IS NULL
         ORDER BY bet_datetime DESC",
    )
    .bind(teller.event_teller_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    Json(bets.iter().map(bet_view_row_to_json).collect::<Vec<_>>()).into_response()
}

/// `POST /voided-bets` -- mirrors `BetController@voidedBets`.
pub async fn voided_bets(State(state): State<Arc<AppState>>, teller: AuthedTeller) -> Response {
    let bets = sqlx::query("SELECT * FROM bets_view WHERE event_teller_id = ? AND bet_status = 2 ORDER BY bet_void_datetime DESC")
        .bind(teller.event_teller_id)
        .fetch_all(&state.db)
        .await;
    match bets {
        Ok(rows) => Json(json!({
            "success": true,
            "message": "Payout history retrieved successfully!",
            "data": rows.iter().map(bet_view_row_to_json).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "message": crate::error::log_fault(&e) })),
        )
            .into_response(),
    }
}
