//! `/cash-in*`, `/cash-in-admin*` -- mirrors `CashInController@storeByTeller/
//! @storeByAdminViaApp/@cashInHistory/@cashInHistoryAdmin/@approvalViaApp`. Only the API
//! (`account.token:*`) actions are implemented -- the web-session `approveCashIn`/`declineCashIn`
//! actions are out of scope (API-only per the project plan).

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
pub struct CashInTellerRequest {
    pub amount: Money,
}

/// `POST /cash-in` -- mirrors `CashInController@storeByTeller`: always created `Pending`.
pub async fn store_by_teller(
    State(state): State<Arc<AppState>>,
    teller: AuthedTeller,
    JsonOrForm(body): JsonOrForm<CashInTellerRequest>,
) -> Response {
    let event_status: Option<String> = sqlx::query_scalar(
        "SELECT ev.event_status FROM events ev JOIN event_tellers et ON et.event_id = ev.event_id WHERE et.event_teller_id = ?",
    )
    .bind(teller.event_teller_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    if event_status.as_deref() != Some("Active") {
        return Json(json!({ "success": false, "message": "Cash-in request failed, event is no longer Active." })).into_response();
    }

    let now = DbDateTime::now();
    let result = sqlx::query(
        "INSERT INTO cash_ins (admin_id, event_teller_id, cash_in_amount, cash_in_datetime, cash_in_type, cash_in_status)
         VALUES (NULL, ?, ?, ?, 'TellerCashIn', 'Pending')",
    )
    .bind(teller.event_teller_id)
    .bind(body.amount.to_db_string())
    .bind(now.0)
    .execute(&state.db)
    .await;
    if let Err(e) = result {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "message": "Something went wrong! Please try again!", "error": format!("{e}") })),
        )
            .into_response();
    }

    create_log(&state.db, teller.account_id, "Teller App", "Cash-in request by Teller.").await;

    crate::services::realtime::teller_balances(&state, teller.event_id).await;
    crate::services::realtime::cash_ins(&state, teller.event_id).await;

    Json(json!({ "success": true, "message": "Cash-in request submitted successfully." })).into_response()
}

#[derive(Deserialize)]
pub struct CashInAdminRequest {
    pub event_teller_id: i32,
    pub amount: Money,
}

/// `POST /cash-in-admin` -- mirrors `CashInController@storeByAdminViaApp`: created already
/// `Approved`, balance updated in the same step.
pub async fn store_by_admin_via_app(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<CashInAdminRequest>,
) -> Response {
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response(),
    };

    let now = DbDateTime::now();
    let insert = sqlx::query(
        "INSERT INTO cash_ins (admin_id, event_teller_id, cash_in_amount, cash_in_datetime, cash_in_type, cash_in_status)
         VALUES (?, ?, ?, ?, 'AdminCashIn', 'Approved')",
    )
    .bind(admin.admin_id)
    .bind(body.event_teller_id)
    .bind(body.amount.to_db_string())
    .bind(now.0)
    .execute(&mut *tx)
    .await;
    let cash_in_id = match insert {
        Ok(r) => r.last_insert_id() as i32,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "message": "Something went wrong! Please try again!", "error": format!("{e}") })),
            )
                .into_response()
        }
    };

    if let Err(e) = sqlx::query("UPDATE event_tellers SET teller_balance = teller_balance + ? WHERE event_teller_id = ?")
        .bind(body.amount.to_db_string())
        .bind(body.event_teller_id)
        .execute(&mut *tx)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response();
    }

    create_log(&mut *tx, admin.account_id, "Admin App", "Cash-in by Admin.").await;

    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response();
    }

    let cash_in_row = sqlx::query("SELECT * FROM cash_ins_view WHERE cash_in_id = ?")
        .bind(cash_in_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    if let Some(event_id) = crate::services::realtime::event_id_of_teller(&state.db, body.event_teller_id).await {
        crate::services::realtime::teller_balances(&state, event_id).await;
        crate::services::realtime::cash_ins(&state, event_id).await;
    }

    Json(json!({
        "success": true,
        "cash_in": cash_in_row.map(|r| cash_in_view_row_to_json(&r)),
        "message": "Cash-in was successful!",
    }))
    .into_response()
}

/// `POST /cash-in/history` -- mirrors `CashInController@cashInHistory`. Laravel reads
/// `$r->event_teller_id` directly, but `CheckAccountToken` middleware overwrites that field with
/// the token's own value before the controller ever sees it -- so this uses `teller.event_teller_id`
/// straight from the auth extractor rather than trusting whatever the client sent (which also
/// means this doesn't need a body/query param at all, matching `bet_history`/`voided_bets`).
pub async fn cash_in_history(State(state): State<Arc<AppState>>, teller: AuthedTeller) -> Response {
    let rows = sqlx::query("SELECT * FROM cash_ins_view WHERE event_teller_id = ? ORDER BY cash_in_datetime DESC")
        .bind(teller.event_teller_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    Json(json!({
        "success": true,
        "message": "Cash-in history loaded successfully.",
        "data": rows.iter().map(cash_in_view_row_to_json).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// `POST /cash-in-admin/history` -- mirrors `CashInController@cashInHistoryAdmin`.
pub async fn cash_in_history_admin(State(state): State<Arc<AppState>>, _admin: AuthedAdmin) -> Response {
    let event_id: Option<i32> = sqlx::query_scalar("SELECT event_id FROM events WHERE event_status = 'Active' LIMIT 1")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(event_id) = event_id else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "message": "Unable to retrieve cash-in history." })),
        )
            .into_response();
    };
    let rows = sqlx::query("SELECT * FROM cash_ins_view WHERE event_id = ? ORDER BY cash_in_datetime DESC")
        .bind(event_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    Json(json!({
        "success": true,
        "message": "Cash-in history loaded successfully.",
        "data": rows.iter().map(cash_in_view_row_to_json).collect::<Vec<_>>(),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct CashInApprovalRequest {
    pub cash_in_id: i32,
    pub cash_in_status: String,
}

/// `POST /cash-in-admin/approval` -- mirrors `CashInController@approvalViaApp`.
pub async fn approval_via_app(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<CashInApprovalRequest>,
) -> Response {
    let current_status: Option<String> = sqlx::query_scalar("SELECT cash_in_status FROM cash_ins WHERE cash_in_id = ?")
        .bind(body.cash_in_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if current_status.as_deref() != Some("Pending") {
        return Json(json!({ "success": false, "message": "Invalid request" })).into_response();
    }

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response(),
    };

    let won = sqlx::query("UPDATE cash_ins SET cash_in_status = ?, admin_id = ? WHERE cash_in_id = ? AND cash_in_status = 'Pending'")
        .bind(&body.cash_in_status)
        .bind(admin.admin_id)
        .bind(body.cash_in_id)
        .execute(&mut *tx)
        .await;
    match won {
        Ok(r) if r.rows_affected() == 1 => {}
        Ok(_) => {
            let _ = tx.rollback().await;
            return Json(json!({ "success": false, "message": "Invalid request" })).into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response(),
    }

    let event_teller_id: i32 = match sqlx::query_scalar("SELECT event_teller_id FROM cash_ins WHERE cash_in_id = ?")
        .bind(body.cash_in_id)
        .fetch_one(&mut *tx)
        .await
    {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response(),
    };

    if body.cash_in_status == "Approved" {
        let amount: Money = match sqlx::query_scalar("SELECT cash_in_amount FROM cash_ins WHERE cash_in_id = ?")
            .bind(body.cash_in_id)
            .fetch_one(&mut *tx)
            .await
        {
            Ok(v) => v,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response(),
        };
        if let Err(_) = sqlx::query("UPDATE event_tellers SET teller_balance = teller_balance + ? WHERE event_teller_id = ?")
            .bind(amount.to_db_string())
            .bind(event_teller_id)
            .execute(&mut *tx)
            .await
        {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": "Approval failed. Please try again." }))).into_response();
        }
    }

    create_log(&mut *tx, admin.account_id, "Admin App", "Cash-in approval by Admin.").await;

    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response();
    }

    if let Some(event_id) = crate::services::realtime::event_id_of_teller(&state.db, event_teller_id).await {
        crate::services::realtime::teller_balances(&state, event_id).await;
        crate::services::realtime::cash_ins(&state, event_id).await;
    }

    Json(json!({ "success": true, "message": "Cash-in has been approved successfully!" })).into_response()
}

/// `cash_ins_view` -- note it does NOT have an `approved_at` column (that's on the raw
/// `cash_ins` table only); verified against the live `SHOW CREATE VIEW`, not assumed.
fn cash_in_view_row_to_json(row: &sqlx::mysql::MySqlRow) -> serde_json::Value {
    json!({
        "cash_in_id": row.try_get::<i32, _>("cash_in_id").unwrap_or_default(),
        "admin_id": row.try_get::<Option<i32>, _>("admin_id").ok().flatten(),
        "event_teller_id": row.try_get::<i32, _>("event_teller_id").unwrap_or_default(),
        "cash_in_amount": row.try_get::<Money, _>("cash_in_amount").ok(),
        "cash_in_datetime": row.try_get::<DbDateTime, _>("cash_in_datetime").ok(),
        "cash_in_type": row.try_get::<String, _>("cash_in_type").unwrap_or_default(),
        "cash_in_status": row.try_get::<String, _>("cash_in_status").unwrap_or_default(),
        "admin_name": row.try_get::<Option<String>, _>("admin_name").ok().flatten(),
        "teller_id": row.try_get::<i32, _>("teller_id").unwrap_or_default(),
        "teller_balance": row.try_get::<Money, _>("teller_balance").ok(),
        "teller_name": row.try_get::<String, _>("teller_name").unwrap_or_default(),
        "contact_number": row.try_get::<String, _>("contact_number").unwrap_or_default(),
        "phone_uid": row.try_get::<String, _>("phone_uid").unwrap_or_default(),
        "event_name": row.try_get::<String, _>("event_name").unwrap_or_default(),
        "event_description": row.try_get::<String, _>("event_description").unwrap_or_default(),
        "event_date": row.try_get::<chrono::NaiveDate, _>("event_date").ok(),
        "event_status": row.try_get::<String, _>("event_status").unwrap_or_default(),
        "admin_cash_on_hand": row.try_get::<Money, _>("admin_cash_on_hand").ok(),
        "event_id": row.try_get::<i32, _>("event_id").unwrap_or_default(),
    })
}
