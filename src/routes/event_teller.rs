//! `/events-admin/tellers`, `/events-admin/update-tellers` -- mirrors
//! `EventTellerController@listByEventApp/@updateTellersApp` (which itself just delegates to
//! `updateTellers($r, $r->event_id)` -- same logic as the web route). See docs/WORKFLOW.md
//! section 7.1 step 4 for the narrative; this implementation was written against the actual PHP
//! source, not just the doc, since the removal/addition bookkeeping here is intricate.
//!
//! One behavioral note: in Laravel, `$adminId = $request->admin_id ?? session('admin')->admin_id
//! ?? null`, with a `400 Missing admin id` guard. For a true API caller this is moot --
//! `CheckAccountToken` middleware already overwrites `admin_id` from the token before the
//! controller runs, so it's always present. We just use the authenticated admin's id directly;
//! the "missing admin id" branch can't happen through this auth path.

use crate::auth::{AuthedAdmin, AuthedTeller};
use crate::error::AppError;
use crate::extract::JsonOrForm;
use crate::money::Money;
use crate::services::ledger::{self, LedgerMeta};
use crate::services::transactions::create_log;
use crate::types::DbDateTime;
use crate::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct ListByEventRequest {
    pub event_id: i32,
}

pub async fn list_by_event_app(
    State(state): State<Arc<AppState>>,
    _admin: AuthedAdmin,
    crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<ListByEventRequest>,
) -> Result<Response, AppError> {
    let tellers = sqlx::query(
        "SELECT tellers.teller_id, tellers.teller_name, accounts.is_active
         FROM tellers LEFT JOIN accounts ON accounts.account_id = tellers.account_id
         ORDER BY tellers.teller_name",
    )
    .fetch_all(&state.db)
    .await?;
    let tellers_json: Vec<_> = tellers
        .iter()
        .map(|r| {
            json!({
                "teller_id": r.get::<i32, _>("teller_id"),
                "teller_name": r.get::<String, _>("teller_name"),
                "is_active": r.try_get::<Option<i8>, _>("is_active").unwrap_or(None),
            })
        })
        .collect();

    let assigned: Vec<i32> = sqlx::query_scalar("SELECT teller_id FROM event_tellers WHERE event_id = ?")
        .bind(body.event_id)
        .fetch_all(&state.db)
        .await?;

    Ok(Json(json!({ "tellers": tellers_json, "assigned_tellers": assigned })).into_response())
}

#[derive(Deserialize)]
pub struct UpdateTellersRequest {
    pub event_id: i32,
    #[serde(default)]
    pub tellers: Vec<i32>,
}

/// Builds `col NOT IN (?, ?, ...)` / `col IN (?, ?, ...)` safely for a possibly-empty list
/// (`IN ()`/`NOT IN ()` is a MySQL syntax error) by falling back to an always-false/-true
/// tautology.
fn in_clause(col: &str, negate: bool, count: usize) -> String {
    if count == 0 {
        return if negate { "1=1".to_string() } else { "1=0".to_string() };
    }
    let placeholders = vec!["?"; count].join(", ");
    format!("{col} {}IN ({placeholders})", if negate { "NOT " } else { "" })
}

pub async fn update_tellers_app(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<UpdateTellersRequest>,
) -> Response {
    match update_tellers_inner(&state, admin.account_id, admin.admin_id, body).await {
        Ok(()) => Json(json!({ "message": "Tellers updated successfully!" })).into_response(),
        Err(UpdateTellersError::EventNotFound) => {
            (StatusCode::NOT_FOUND, Json(json!({ "error": "Event not found" }))).into_response()
        }
        Err(UpdateTellersError::Internal(e)) => {
            tracing::error!(error = %e, "update tellers failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("Failed to update tellers: {e}") }))).into_response()
        }
    }
}

enum UpdateTellersError {
    EventNotFound,
    Internal(sqlx::Error),
}

impl From<sqlx::Error> for UpdateTellersError {
    fn from(e: sqlx::Error) -> Self {
        UpdateTellersError::Internal(e)
    }
}

async fn update_tellers_inner(
    state: &Arc<AppState>,
    actor_account_id: i32,
    admin_id: i32,
    body: UpdateTellersRequest,
) -> Result<(), UpdateTellersError> {
    let event_id = body.event_id;

    let teller_initial_cash_on_hand: Option<Money> =
        sqlx::query_scalar("SELECT teller_initial_cash_on_hand FROM events WHERE event_id = ?")
            .bind(event_id)
            .fetch_optional(&state.db)
            .await?;
    let Some(teller_initial_cash_on_hand) = teller_initial_cash_on_hand else {
        return Err(UpdateTellersError::EventNotFound);
    };

    let mut tx = state.db.begin().await?;

    // Tellers who already have bets can never be removed, regardless of the new desired list.
    let tellers_with_bets: Vec<i32> =
        sqlx::query_scalar("SELECT DISTINCT teller_id FROM bets_view WHERE event_id = ?")
            .bind(event_id)
            .fetch_all(&mut *tx)
            .await?;

    let desired = &body.tellers;
    let sql = format!(
        "SELECT event_teller_id, teller_id, teller_balance FROM event_tellers
         WHERE event_id = ? AND {} AND {}",
        in_clause("teller_id", true, tellers_with_bets.len()),
        in_clause("teller_id", true, desired.len()),
    );
    let mut q = sqlx::query(&sql).bind(event_id);
    for id in &tellers_with_bets {
        q = q.bind(id);
    }
    for id in desired {
        q = q.bind(id);
    }
    let removed_rows = q.fetch_all(&mut *tx).await?;

    let mut removed_ids: Vec<i32> = Vec::new();
    for row in &removed_rows {
        let event_teller_id: i32 = row.get("event_teller_id");
        let balance: Money = row.get("teller_balance");
        removed_ids.push(event_teller_id);

        sqlx::query("UPDATE event_tellers SET teller_balance = 0 WHERE event_teller_id = ?")
            .bind(event_teller_id)
            .execute(&mut *tx)
            .await?;

        let has_ledger: bool = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM teller_ledger WHERE event_teller_id = ?",
        )
        .bind(event_teller_id)
        .fetch_one(&mut *tx)
        .await?
            > 0;

        if has_ledger || balance.value() != 0.0 {
            ledger::record(
                &mut *tx,
                event_teller_id,
                ledger::TYPE_ADJUSTMENT,
                Money::ZERO,
                balance,
                Some(actor_account_id),
                LedgerMeta {
                    note: Some("Teller removed from the event (no bets); cash on hand returned"),
                    source_table: Some("event_tellers"),
                    source_id: Some(event_teller_id),
                    ..Default::default()
                },
            )
            .await?;
        }
    }

    if !removed_ids.is_empty() {
        let placeholders = vec!["?"; removed_ids.len()].join(", ");
        let sql = format!("DELETE FROM event_tellers WHERE event_teller_id IN ({placeholders})");
        let mut q = sqlx::query(&sql);
        for id in &removed_ids {
            q = q.bind(id);
        }
        q.execute(&mut *tx).await?;
    }

    for teller_id in desired {
        let exists: bool = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM event_tellers WHERE event_id = ? AND teller_id = ?",
        )
        .bind(event_id)
        .bind(teller_id)
        .fetch_one(&mut *tx)
        .await?
            > 0;
        if exists {
            continue;
        }

        let result = sqlx::query(
            "INSERT INTO event_tellers (event_id, teller_id, teller_balance, teller_match_balance) VALUES (?, ?, ?, 0)",
        )
        .bind(event_id)
        .bind(teller_id)
        .bind(teller_initial_cash_on_hand.to_db_string())
        .execute(&mut *tx)
        .await?;
        let event_teller_id = result.last_insert_id() as i32;

        let now = DbDateTime::now();
        // Note: `cash_ins` has no `event_id` column (confirmed against the live schema) --
        // the PHP `CashIn::create([..., 'event_id' => $eventId, ...])` call passes one anyway,
        // but Eloquent's mass-assignment guard silently drops any key not in `$fillable`, so it
        // never actually reaches the table. Same pattern as `EventTeller`'s `is_disabled` a few
        // lines up.
        let cash_in_result = sqlx::query(
            "INSERT INTO cash_ins (admin_id, event_teller_id, cash_in_amount, cash_in_datetime, cash_in_type, cash_in_status)
             VALUES (?, ?, ?, ?, 'AdminCashInInit', 'Approved')",
        )
        .bind(admin_id)
        .bind(event_teller_id)
        .bind(teller_initial_cash_on_hand.to_db_string())
        .bind(now.0)
        .execute(&mut *tx)
        .await?;
        let cash_in_id = cash_in_result.last_insert_id() as i32;

        sqlx::query("UPDATE cash_ins SET approved_at = NOW() WHERE cash_in_id = ? AND approved_at IS NULL")
            .bind(cash_in_id)
            .execute(&mut *tx)
            .await?;

        ledger::record(
            &mut *tx,
            event_teller_id,
            ledger::TYPE_OPENING,
            teller_initial_cash_on_hand,
            Money::ZERO,
            Some(actor_account_id),
            LedgerMeta {
                reference: Some(&format!("CI-{cash_in_id}")),
                approved_by: Some(admin_id),
                source_table: Some("cash_ins"),
                source_id: Some(cash_in_id),
                note: Some(&format!("requested {}", now.0.format("%Y-%m-%d %H:%M:%S"))),
                ..Default::default()
            },
        )
        .await?;
    }

    if !removed_ids.is_empty() {
        let placeholders = vec!["?"; removed_ids.len()].join(", ");
        let sql = format!("DELETE FROM cash_ins WHERE event_teller_id IN ({placeholders})");
        let mut q = sqlx::query(&sql);
        for id in &removed_ids {
            q = q.bind(id);
        }
        q.execute(&mut *tx).await?;
    }

    // TODO(realtime): broadcast CashInUpdated once the Pusher-trigger client exists
    // (docs/WORKFLOW.md section 9) -- out of scope for this pass, doesn't affect this
    // endpoint's own HTTP response.

    create_log(
        &mut *tx,
        actor_account_id,
        "Web App",
        &format!("Event - Event tellers updated. {}", desired.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")),
    )
    .await;

    tx.commit().await?;
    Ok(())
}

/// `POST /teller-data` -- mirrors `EventTellerController@getData`: the same shape as the login
/// response, minus a real token (`token: ""`), used by the app to refresh its own state.
pub async fn get_data_teller(State(state): State<Arc<AppState>>, teller: AuthedTeller) -> Response {
    let row = sqlx::query("SELECT * FROM event_tellers_view WHERE event_teller_id = ? AND event_status = 'Active'")
        .bind(teller.event_teller_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(row) = row else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "token": Value::Null, "teller": Value::Null, "event": Value::Null, "match": Value::Null,
                "error": "No active event found for the given teller. Please relogin app."
            })),
        )
            .into_response();
    };

    let event_id: i32 = row.get("event_id");
    let latest_match = sqlx::query("SELECT match_id, match_number, match_bet_status, meron_bet_status, wala_bet_status FROM matches WHERE event_id = ? ORDER BY match_id DESC LIMIT 1")
        .bind(event_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let match_json: Value = match latest_match {
        Some(m) => json!({
            "match_id": m.get::<i32, _>("match_id"),
            "match_number": m.get::<i32, _>("match_number"),
            "match_bet_status": m.get::<String, _>("match_bet_status"),
            "meron_bet_status": m.get::<i8, _>("meron_bet_status"),
            "wala_bet_status": m.get::<i8, _>("wala_bet_status"),
        }),
        None => Value::Null,
    };

    let teller_balance: Money = row.get("teller_balance");
    let teller_match_balance: Money = row.get("teller_match_balance");

    Json(json!({
        "token": "",
        "teller": {
            "phone_uid": row.get::<String, _>("phone_uid"),
            "event_teller_id": row.get::<i32, _>("event_teller_id"),
            "teller_id": row.get::<i32, _>("teller_id"),
            "teller_name": row.get::<String, _>("teller_name"),
            "teller_balance": teller_balance,
            "teller_match_balance": teller_match_balance,
        },
        "event": {
            "event_id": event_id,
            "event_name": row.get::<String, _>("event_name"),
            "event_status": row.get::<String, _>("event_status"),
        },
        "match": match_json,
    }))
    .into_response()
}

async fn sum_or_zero(pool: &crate::db::DbPool, sql: &str, event_teller_id: i32) -> Value {
    let sum: Option<Money> = sqlx::query_scalar(sql).bind(event_teller_id).fetch_one(pool).await.unwrap_or(None);
    sum.map(|m| json!(m)).unwrap_or(json!(0))
}

/// `POST /event-tellers-admin` -- mirrors `EventTellerController@listTellersAdmin` +
/// `attachTellerAckTotals`.
pub async fn list_tellers_admin(State(state): State<Arc<AppState>>, _admin: AuthedAdmin) -> Response {
    let event_id: Option<i32> = sqlx::query_scalar("SELECT event_id FROM events WHERE event_status = 'Active' LIMIT 1")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(event_id) = event_id else {
        return Json(json!({ "success": false, "tellers": Value::Array(vec![]) })).into_response();
    };

    let rows = sqlx::query("SELECT * FROM event_tellers_view WHERE event_id = ? ORDER BY teller_name ASC")
        .bind(event_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

    let mut tellers = Vec::with_capacity(rows.len());
    for row in rows {
        let event_teller_id: i32 = row.get("event_teller_id");
        let total_bet = sum_or_zero(
            &state.db,
            "SELECT SUM(bet_amount) FROM bets_view WHERE event_teller_id = ? AND bet_status = 1 AND match_status <> 'Draw' AND match_status <> 'Cancelled'",
            event_teller_id,
        )
        .await;
        let total_payout = sum_or_zero(&state.db, "SELECT SUM(claim_amount) FROM claims WHERE event_teller_id = ?", event_teller_id).await;
        let total_cash_in = sum_or_zero(
            &state.db,
            "SELECT SUM(cash_in_amount) FROM cash_ins_view WHERE event_teller_id = ? AND cash_in_status = 'Approved'",
            event_teller_id,
        )
        .await;
        let total_cash_out = sum_or_zero(
            &state.db,
            "SELECT SUM(cash_out_amount) FROM cash_outs_view WHERE event_teller_id = ? AND cash_out_status = 'Approved'",
            event_teller_id,
        )
        .await;

        let teller_balance: Money = row.get("teller_balance");
        let teller_match_balance: Money = row.get("teller_match_balance");
        tellers.push(json!({
            "event_teller_id": event_teller_id,
            "event_id": row.get::<i32, _>("event_id"),
            "teller_id": row.get::<i32, _>("teller_id"),
            "teller_balance": teller_balance,
            "teller_match_balance": teller_match_balance,
            "event_name": row.get::<String, _>("event_name"),
            "event_description": row.get::<String, _>("event_description"),
            "event_date": row.get::<chrono::NaiveDate, _>("event_date"),
            "event_status": row.get::<String, _>("event_status"),
            "account_id": row.get::<i32, _>("account_id"),
            "teller_name": row.get::<String, _>("teller_name"),
            "contact_number": row.get::<String, _>("contact_number"),
            "phone_uid": row.get::<String, _>("phone_uid"),
            "username": row.get::<String, _>("username"),
            "account_type": row.get::<String, _>("account_type"),
            "is_active": row.get::<i8, _>("is_active"),
            "account_date_created": row.get::<DbDateTime, _>("account_date_created"),
            "total_bet": total_bet,
            "total_payout": total_payout,
            "total_cash_in": total_cash_in,
            "total_cash_out": total_cash_out,
        }));
    }

    Json(json!({ "success": true, "tellers": tellers })).into_response()
}
