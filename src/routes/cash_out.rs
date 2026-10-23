//! `/cash-out*`, `/cash-out-admin*` -- mirrors `CashOutController@storeByTeller/
//! @storeByAdminViaApp/@cashOutHistory/@cashOutHistoryAdmin/@approvalViaApp`. Only the API
//! actions are implemented (web-session `approveCashOut`/`declineCashOut` are out of scope).

use crate::auth::{AuthedAdmin, AuthedTeller};
use crate::extract::JsonOrForm;
use crate::money::Money;
use crate::services::transactions::create_log;
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

#[derive(Deserialize)]
pub struct CashOutTellerRequest {
    pub amount: Money,
}

/// `POST /cash-out` -- mirrors `CashOutController@storeByTeller`.
pub async fn store_by_teller(
    State(state): State<Arc<AppState>>,
    teller: AuthedTeller,
    JsonOrForm(body): JsonOrForm<CashOutTellerRequest>,
) -> Response {
    let row = sqlx::query(
        "SELECT ev.event_status, et.teller_balance FROM events ev JOIN event_tellers et ON et.event_id = ev.event_id WHERE et.event_teller_id = ?",
    )
    .bind(teller.event_teller_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let Some(row) = row else {
        return Json(json!({ "success": false, "message": "Cash-out request failed, event is no longer Active." })).into_response();
    };
    let event_status: String = row.get("event_status");
    if event_status != "Active" {
        return Json(json!({ "success": false, "message": "Cash-out request failed, event is no longer Active." })).into_response();
    }
    let teller_balance: Money = row.get("teller_balance");
    if body.amount.value() > teller_balance.value() {
        return Json(json!({ "success": false, "message": "Not enough cash on hand." })).into_response();
    }

    let now = DbDateTime::now();
    let result = sqlx::query(
        "INSERT INTO cash_outs (admin_id, event_teller_id, cash_out_amount, cash_out_datetime, cash_out_status)
         VALUES (NULL, ?, ?, ?, 'Pending')",
    )
    .bind(teller.event_teller_id)
    .bind(body.amount.to_db_string())
    .bind(now.0)
    .execute(&state.db)
    .await;
    if let Err(e) = result {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "message": "Something went wrong! Please try again!", "error": crate::error::log_fault(&e) })),
        )
            .into_response();
    }

    create_log(&state.db, teller.account_id, "Teller App", "Cash-out request by Teller.").await;

    crate::services::realtime::teller_balances(&state, teller.event_id).await;
    crate::services::realtime::cash_outs(&state, teller.event_id).await;

    Json(json!({ "success": true, "message": "Cash-out request submitted successfully." })).into_response()
}

#[derive(Deserialize)]
pub struct CashOutAdminRequest {
    pub event_teller_id: i32,
    pub amount: Money,
}

/// `POST /cash-out-admin` -- mirrors `CashOutController@storeByAdminViaApp`.
pub async fn store_by_admin_via_app(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<CashOutAdminRequest>,
) -> Response {
    let teller_balance: Option<Money> = sqlx::query_scalar("SELECT teller_balance FROM event_tellers WHERE event_teller_id = ?")
        .bind(body.event_teller_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(teller_balance) = teller_balance else {
        return Json(json!({ "success": false, "message": "Not enough cash on hand." })).into_response();
    };
    if body.amount.value() > teller_balance.value() {
        return Json(json!({ "success": false, "message": "Not enough cash on hand." })).into_response();
    }

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": crate::error::log_fault(&e) }))).into_response(),
    };

    let now = DbDateTime::now();
    let insert = sqlx::query(
        "INSERT INTO cash_outs (admin_id, event_teller_id, cash_out_amount, cash_out_datetime, cash_out_status)
         VALUES (?, ?, ?, ?, 'Approved')",
    )
    .bind(admin.admin_id)
    .bind(body.event_teller_id)
    .bind(body.amount.to_db_string())
    .bind(now.0)
    .execute(&mut *tx)
    .await;
    let cash_out_id = match insert {
        Ok(r) => r.last_insert_id() as i32,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "message": "Something went wrong! Please try again!", "error": crate::error::log_fault(&e) })),
            )
                .into_response()
        }
    };

    // Atomic: only debit if the teller still has enough (mirrors the PHP's conditional UPDATE).
    let ok = sqlx::query("UPDATE event_tellers SET teller_balance = teller_balance - ? WHERE event_teller_id = ? AND teller_balance >= ?")
        .bind(body.amount.to_db_string())
        .bind(body.event_teller_id)
        .bind(body.amount.to_db_string())
        .execute(&mut *tx)
        .await;
    match ok {
        Ok(r) if r.rows_affected() == 1 => {}
        Ok(_) => {
            let _ = tx.rollback().await;
            return Json(json!({ "success": false, "message": "Not enough cash on hand." })).into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": crate::error::log_fault(&e) }))).into_response(),
    }

    create_log(&mut *tx, admin.account_id, "Admin App", "Cash-out by Admin.").await;

    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": crate::error::log_fault(&e) }))).into_response();
    }

    let cash_out_row = sqlx::query("SELECT * FROM cash_outs_view WHERE cash_out_id = ?")
        .bind(cash_out_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    if let Some(event_id) = crate::services::realtime::event_id_of_teller(&state.db, body.event_teller_id).await {
        crate::services::realtime::teller_balances(&state, event_id).await;
        crate::services::realtime::cash_outs(&state, event_id).await;
    }

    Json(json!({
        "success": true,
        "cash_out": cash_out_row.map(|r| cash_out_view_row_to_json(&r)),
        "message": "Cash-out was successful!",
    }))
    .into_response()
}

/// `POST /cash-out/history` -- mirrors `CashOutController@cashOutHistory`. Same reasoning as
/// `cash_in_history`: the real middleware overwrites `event_teller_id` with the token's own
/// value, so this reads it straight from the auth extractor instead of trusting the client.
pub async fn cash_out_history(State(state): State<Arc<AppState>>, teller: AuthedTeller) -> Response {
    let rows = sqlx::query("SELECT * FROM cash_outs_view WHERE event_teller_id = ? ORDER BY cash_out_datetime DESC")
        .bind(teller.event_teller_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    Json(json!({
        "success": true,
        "message": "Cash-out history loaded successfully.",
        "data": rows.iter().map(cash_out_view_row_to_json).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// `POST /cash-out-admin/history` -- mirrors `CashOutController@cashOutHistoryAdmin`.
pub async fn cash_out_history_admin(State(state): State<Arc<AppState>>, _admin: AuthedAdmin) -> Response {
    let event_id: Option<i32> = sqlx::query_scalar("SELECT event_id FROM events WHERE event_status = 'Active' LIMIT 1")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(event_id) = event_id else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "message": "Unable to retrieve cash-out history." })),
        )
            .into_response();
    };
    let rows = sqlx::query("SELECT * FROM cash_outs_view WHERE event_id = ? ORDER BY cash_out_datetime DESC")
        .bind(event_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    Json(json!({
        "success": true,
        "message": "Cash-out history loaded successfully.",
        "data": rows.iter().map(cash_out_view_row_to_json).collect::<Vec<_>>(),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct CashOutApprovalRequest {
    pub cash_out_id: i32,
    pub cash_out_status: String,
}

/// `POST /cash-out-admin/approval` -- mirrors `CashOutController@approvalViaApp`.
pub async fn approval_via_app(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<CashOutApprovalRequest>,
) -> Response {
    let current_status: Option<String> = sqlx::query_scalar("SELECT cash_out_status FROM cash_outs WHERE cash_out_id = ?")
        .bind(body.cash_out_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if current_status.as_deref() != Some("Pending") {
        return Json(json!({ "success": false, "message": "The request is invalid." })).into_response();
    }

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": crate::error::log_fault(&e) }))).into_response(),
    };

    let won = sqlx::query("UPDATE cash_outs SET cash_out_status = ?, admin_id = ? WHERE cash_out_id = ? AND cash_out_status = 'Pending'")
        .bind(&body.cash_out_status)
        .bind(admin.admin_id)
        .bind(body.cash_out_id)
        .execute(&mut *tx)
        .await;
    match won {
        Ok(r) if r.rows_affected() == 1 => {}
        Ok(_) => {
            let _ = tx.rollback().await;
            return Json(json!({ "success": false, "message": "The request is invalid." })).into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": crate::error::log_fault(&e) }))).into_response(),
    }

    let (event_teller_id, amount): (i32, Money) =
        match sqlx::query_as("SELECT event_teller_id, cash_out_amount FROM cash_outs WHERE cash_out_id = ?")
            .bind(body.cash_out_id)
            .fetch_one(&mut *tx)
            .await
        {
            Ok(v) => v,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": crate::error::log_fault(&e) }))).into_response(),
        };

    if body.cash_out_status == "Approved" {
        if let Err(_) = sqlx::query("UPDATE event_tellers SET teller_balance = teller_balance - ? WHERE event_teller_id = ?")
            .bind(amount.to_db_string())
            .bind(event_teller_id)
            .execute(&mut *tx)
            .await
        {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": "Approval failed. Please try again." }))).into_response();
        }
    }

    create_log(&mut *tx, admin.account_id, "Admin App", "Cash-out approval by Admin.").await;

    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": crate::error::log_fault(&e) }))).into_response();
    }

    let cash_out_row = sqlx::query("SELECT * FROM cash_outs_view WHERE cash_out_id = ?")
        .bind(body.cash_out_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    if let Some(event_id) = crate::services::realtime::event_id_of_teller(&state.db, event_teller_id).await {
        crate::services::realtime::teller_balances(&state, event_id).await;
        crate::services::realtime::cash_outs(&state, event_id).await;
    }

    Json(json!({
        "cash_out": cash_out_row.map(|r| cash_out_view_row_to_json(&r)),
        "success": true,
        "message": "Cash-out approved successfully.",
    }))
    .into_response()
}

fn cash_out_view_row_to_json(row: &sqlx::mysql::MySqlRow) -> serde_json::Value {
    json!({
        "cash_out_id": row.try_get::<i32, _>("cash_out_id").unwrap_or_default(),
        "event_teller_id": row.try_get::<i32, _>("event_teller_id").unwrap_or_default(),
        "admin_id": row.try_get::<Option<i32>, _>("admin_id").ok().flatten(),
        "cash_out_amount": row.try_get::<Money, _>("cash_out_amount").ok(),
        "cash_out_status": row.try_get::<String, _>("cash_out_status").unwrap_or_default(),
        "cash_out_datetime": row.try_get::<DbDateTime, _>("cash_out_datetime").ok(),
        "event_id": row.try_get::<i32, _>("event_id").unwrap_or_default(),
        "teller_id": row.try_get::<i32, _>("teller_id").unwrap_or_default(),
        "teller_balance": row.try_get::<Money, _>("teller_balance").ok(),
        "teller_match_balance": row.try_get::<Money, _>("teller_match_balance").ok(),
        "event_name": row.try_get::<String, _>("event_name").unwrap_or_default(),
        "event_description": row.try_get::<String, _>("event_description").unwrap_or_default(),
        "event_date": row.try_get::<chrono::NaiveDate, _>("event_date").ok(),
        "event_status": row.try_get::<String, _>("event_status").unwrap_or_default(),
        "admin_cash_on_hand": row.try_get::<Money, _>("admin_cash_on_hand").ok(),
        "teller_name": row.try_get::<String, _>("teller_name").unwrap_or_default(),
        "contact_number": row.try_get::<String, _>("contact_number").unwrap_or_default(),
        "phone_uid": row.try_get::<String, _>("phone_uid").unwrap_or_default(),
        "admin_name": row.try_get::<Option<String>, _>("admin_name").ok().flatten(),
    })
}
