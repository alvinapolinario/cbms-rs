//! `/claim-winning`, `/claim-winning-reprint`, `/claimed-receipts` -- mirrors
//! `ClaimController@claimWinning/@claimWinningReprint/@cashOutHistory`. Unlike most of the bet
//! endpoints, these return the raw `Bet`/`Fight` Eloquent models (not the `_view` denormalized
//! tables) with a couple of fields bolted on -- confirmed against the actual source, not assumed
//! from the pattern used elsewhere.

use crate::auth::AuthedTeller;
use crate::extract::JsonOrForm;
use crate::models::Fight;
use crate::money::Money;
use crate::services::{ledger, transactions::create_log};
use crate::types::DbDateTime;
use crate::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;

fn soft_fail(message: &str) -> Response {
    Json(json!({ "success": false, "message": message })).into_response()
}

#[derive(Deserialize)]
pub struct ReceiptCodeRequest {
    pub receipt_code: String,
}

/// `POST /claim-winning` -- mirrors `ClaimController@claimWinning`.
pub async fn claim_winning(
    State(state): State<Arc<AppState>>,
    teller: AuthedTeller,
    JsonOrForm(body): JsonOrForm<ReceiptCodeRequest>,
) -> Response {
    let receipt_code = body.receipt_code.trim();
    if receipt_code.is_empty() {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "success": false, "message": "Receipt code is required." }))).into_response();
    }

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return soft_fail(&format!("{e}")),
    };

    let bet_row = sqlx::query("SELECT * FROM bets WHERE bet_receipt_code = ? FOR UPDATE")
        .bind(receipt_code)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    let Some(bet_row) = bet_row else {
        return soft_fail("Receipt not found. Please check the receipt code.");
    };
    let bet_id: i32 = bet_row.get("bet_id");
    let bet_status: i8 = bet_row.get("bet_status");
    let bet_payout_amount: Money = bet_row.get("bet_payout_amount");
    let bet_event_teller_id: i32 = bet_row.get("event_teller_id");
    let bet_match_id: i32 = bet_row.get("match_id");
    let bet_is_winner: i8 = bet_row.get("bet_is_winner");
    let bet_side: String = bet_row.get("bet_side");

    if bet_status == 2 {
        return soft_fail("Payout cannot be processed for this receipt because it has been voided.");
    }
    if bet_payout_amount.value() <= 0.0 {
        return soft_fail("This receipt cannot be processed for payout. Please contact the administrator for assistance.");
    }

    let event_status: Option<String> = sqlx::query_scalar(
        "SELECT ev.event_status FROM event_tellers et JOIN events ev ON ev.event_id = et.event_id WHERE et.event_teller_id = ?",
    )
    .bind(bet_event_teller_id)
    .fetch_optional(&mut *tx)
    .await
    .ok()
    .flatten();
    if event_status.as_deref() != Some("Active") {
        return soft_fail("This receipt is not eligible for payout as the event is no longer active.");
    }

    let already_claimed: Option<(String, DbDateTime)> = sqlx::query_as("SELECT teller_name, claim_datetime FROM claims_view WHERE bet_id = ?")
        .bind(bet_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    if let Some((teller_name, claim_datetime)) = already_claimed {
        return soft_fail(&format!(
            "This receipt was already claimed by {teller_name} on {}",
            claim_datetime.0.format("%B %-d, %Y %I:%M %p")
        ));
    }

    // Atomic: deduct only if the paying teller has enough cash on hand.
    let deducted = sqlx::query("UPDATE event_tellers SET teller_balance = teller_balance - ? WHERE event_teller_id = ? AND teller_balance >= ?")
        .bind(bet_payout_amount.to_db_string())
        .bind(teller.event_teller_id)
        .bind(bet_payout_amount.to_db_string())
        .execute(&mut *tx)
        .await;
    match deducted {
        Ok(r) if r.rows_affected() == 1 => {}
        Ok(_) => return soft_fail("Insufficient cash on hand. Please request a cash-in from the administrator."),
        Err(e) => return soft_fail(&format!("{e}")),
    }

    let now = DbDateTime::now();
    let claim_insert = sqlx::query("INSERT INTO claims (bet_id, event_teller_id, claim_amount, claim_datetime) VALUES (?, ?, ?, ?)")
        .bind(bet_id)
        .bind(teller.event_teller_id)
        .bind(bet_payout_amount.to_db_string())
        .bind(now.0)
        .execute(&mut *tx)
        .await;
    let claim_id = match claim_insert {
        Ok(r) => r.last_insert_id() as i32,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "message": "Something went wrong! Please try again!", "error": format!("{e}") })),
            )
                .into_response()
        }
    };

    if let Err(e) = sqlx::query("UPDATE bets SET bet_payout_datetime = ?, bet_payout_by = ? WHERE bet_id = ?")
        .bind(now.0)
        .bind(teller.event_teller_id)
        .bind(bet_id)
        .execute(&mut *tx)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response();
    }

    let fight_no: Option<i32> = sqlx::query_scalar("SELECT match_number FROM matches WHERE match_id = ?")
        .bind(bet_match_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    let match_status: Option<String> = sqlx::query_scalar("SELECT match_status FROM matches WHERE match_id = ?")
        .bind(bet_match_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    let cross_teller = bet_event_teller_id != teller.event_teller_id;
    let note = if bet_is_winner == 1 {
        format!("{bet_side} win")
    } else {
        format!("{} refund", match_status.clone().unwrap_or_else(|| "Refund".to_string()))
    };
    let note = if cross_teller { format!("{note}; receipt issued by another teller") } else { note };

    if let Err(e) = ledger::record(
        &mut tx,
        teller.event_teller_id,
        if bet_is_winner == 1 { ledger::TYPE_PAYOUT_WIN } else { ledger::TYPE_PAYOUT_REFUND },
        Money::ZERO,
        bet_payout_amount,
        Some(teller.account_id),
        ledger::LedgerMeta {
            fight_no,
            match_id: Some(bet_match_id),
            reference: Some(receipt_code),
            related_event_teller_id: Some(bet_event_teller_id),
            note: Some(&note),
            source_table: Some("claims"),
            source_id: Some(claim_id),
            ..Default::default()
        },
    )
    .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response();
    }

    let teller_name: Option<String> = sqlx::query_scalar("SELECT teller_name FROM tellers t JOIN event_tellers et ON et.teller_id = t.teller_id WHERE et.event_teller_id = ?")
        .bind(teller.event_teller_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    create_log(&mut *tx, teller.account_id, "Teller App", "Bet payout claimed.").await;

    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response();
    }

    // TODO(realtime): broadcast TellerBalanceUpdated once the Pusher-trigger client exists.

    let bet = sqlx::query_as::<_, crate::models::Bet>("SELECT * FROM bets WHERE bet_id = ?")
        .bind(bet_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let fight = sqlx::query_as::<_, Fight>("SELECT * FROM matches WHERE match_id = ?")
        .bind(bet_match_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let event_name: Option<String> = sqlx::query_scalar("SELECT ev.event_name FROM matches m JOIN events ev ON ev.event_id = m.event_id WHERE m.match_id = ?")
        .bind(bet_match_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    let match_json = fight.map(|f| {
        let mut v = serde_json::to_value(&f).unwrap_or(json!({}));
        if let Some(obj) = v.as_object_mut() {
            obj.insert("event_name".to_string(), json!(event_name));
        }
        v
    });
    let claim_json = json!({
        "claim_id": claim_id,
        "bet_id": bet_id,
        "event_teller_id": teller.event_teller_id,
        "claim_amount": bet_payout_amount,
        "claim_datetime": now,
        "teller_name": teller_name,
    });

    Json(json!({
        "success": true,
        "bet": bet,
        "match": match_json,
        "claim": claim_json,
        "message": "Payout has been created successfully.",
    }))
    .into_response()
}

/// `POST /claim-winning-reprint` -- mirrors `ClaimController@claimWinningReprint`.
pub async fn claim_winning_reprint(State(state): State<Arc<AppState>>, _teller: AuthedTeller, JsonOrForm(body): JsonOrForm<ReceiptCodeRequest>) -> Response {
    let bet_row = sqlx::query("SELECT * FROM bets_view WHERE bet_receipt_code = ?")
        .bind(&body.receipt_code)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(bet_row) = bet_row else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "message": "Something went wrong! Please try again!" })),
        )
            .into_response();
    };
    let bet_id: i32 = bet_row.get("bet_id");
    let match_id: i32 = bet_row.get("match_id");
    let event_name: String = bet_row.get("event_name");

    let fight = sqlx::query_as::<_, Fight>("SELECT * FROM matches WHERE match_id = ?")
        .bind(match_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let match_json = fight.map(|f| {
        let mut v = serde_json::to_value(&f).unwrap_or(json!({}));
        if let Some(obj) = v.as_object_mut() {
            obj.insert("event_name".to_string(), json!(event_name));
        }
        v
    });

    let claim_row = sqlx::query("SELECT * FROM claims_view WHERE bet_id = ?")
        .bind(bet_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let claim_json = claim_row.map(|r| {
        json!({
            "claim_id": r.try_get::<i32, _>("claim_id").unwrap_or_default(),
            "bet_id": r.try_get::<i32, _>("bet_id").unwrap_or_default(),
            "event_teller_id": r.try_get::<i32, _>("event_teller_id").unwrap_or_default(),
            "claim_amount": r.try_get::<Money, _>("claim_amount").ok(),
            "claim_datetime": r.try_get::<DbDateTime, _>("claim_datetime").ok(),
            "teller_name": r.try_get::<String, _>("teller_name").unwrap_or_default(),
            "match_number": r.try_get::<i32, _>("match_number").unwrap_or_default(),
            "match_winner": r.try_get::<String, _>("match_winner").unwrap_or_default(),
            "meron_odds": r.try_get::<f32, _>("meron_odds").unwrap_or_default(),
            "wala_odds": r.try_get::<f32, _>("wala_odds").unwrap_or_default(),
            "bet_receipt_code": r.try_get::<String, _>("bet_receipt_code").unwrap_or_default(),
            "bet_side": r.try_get::<String, _>("bet_side").unwrap_or_default(),
        })
    });

    Json(json!({
        "success": true,
        "bet": super::bet::bet_view_row_to_json(&bet_row),
        "match": match_json,
        "claim": claim_json,
        "message": "Reprint payout successfully.",
    }))
    .into_response()
}

/// `POST /claimed-receipts` -- mirrors `ClaimController@cashOutHistory` (named for the payout
/// side despite sharing a name with the cash-out controller's method -- that's the real source).
pub async fn claimed_receipts(State(state): State<Arc<AppState>>, teller: AuthedTeller) -> Response {
    let event_status: Option<String> = sqlx::query_scalar(
        "SELECT ev.event_status FROM event_tellers et JOIN events ev ON ev.event_id = et.event_id WHERE et.event_teller_id = ?",
    )
    .bind(teller.event_teller_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    if event_status.as_deref() != Some("Active") {
        return Json(json!({ "success": false, "message": "Event is no longer Active." })).into_response();
    }

    let rows = sqlx::query("SELECT * FROM claims_view WHERE event_teller_id = ? ORDER BY claim_datetime DESC")
        .bind(teller.event_teller_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let data: Vec<_> = rows
        .iter()
        .map(|r| {
            json!({
                "claim_id": r.try_get::<i32, _>("claim_id").unwrap_or_default(),
                "bet_id": r.try_get::<i32, _>("bet_id").unwrap_or_default(),
                "event_teller_id": r.try_get::<i32, _>("event_teller_id").unwrap_or_default(),
                "claim_amount": r.try_get::<Money, _>("claim_amount").ok(),
                "claim_datetime": r.try_get::<DbDateTime, _>("claim_datetime").ok(),
                "teller_name": r.try_get::<String, _>("teller_name").unwrap_or_default(),
                "match_number": r.try_get::<i32, _>("match_number").unwrap_or_default(),
                "match_winner": r.try_get::<String, _>("match_winner").unwrap_or_default(),
                "meron_odds": r.try_get::<f32, _>("meron_odds").unwrap_or_default(),
                "wala_odds": r.try_get::<f32, _>("wala_odds").unwrap_or_default(),
                "bet_receipt_code": r.try_get::<String, _>("bet_receipt_code").unwrap_or_default(),
                "bet_side": r.try_get::<String, _>("bet_side").unwrap_or_default(),
            })
        })
        .collect();

    Json(json!({ "success": true, "message": "Payout history loaded successfully.", "data": data })).into_response()
}
