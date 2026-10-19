//! `/admin-data` -- mirrors `AdminController@getData`.

use crate::auth::AuthedAdmin;
use crate::money::Money;
use crate::AppState;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::Arc;

pub async fn get_data(State(state): State<Arc<AppState>>, admin: AuthedAdmin) -> Response {
    let event = sqlx::query("SELECT * FROM events WHERE event_status = 'Active' LIMIT 1")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(event) = event else {
        return Json(json!({ "cash_out": 0, "cash_in": 0, "token": "", "admin": Value::Null, "event": Value::Null, "match": Value::Null })).into_response();
    };
    let event_id: i32 = event.get("event_id");

    let account = sqlx::query("SELECT * FROM admins_view WHERE admin_id = ? AND is_active = 1")
        .bind(admin.admin_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    let latest_match = sqlx::query("SELECT match_id, match_number, match_bet_status FROM matches WHERE event_id = ? ORDER BY match_number DESC LIMIT 1")
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
        }),
        None => Value::Null,
    };

    let cash_out_sum: Option<Money> = sqlx::query_scalar("SELECT SUM(cash_out_amount) FROM cash_outs_view WHERE event_id = ? AND cash_out_status = 'Approved'")
        .bind(event_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(None);
    let cash_in_sum: Option<Money> = sqlx::query_scalar("SELECT SUM(cash_in_amount) FROM cash_ins_view WHERE event_id = ? AND cash_in_status = 'Approved'")
        .bind(event_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(None);

    let admin_json = account.map(|row| {
        json!({
            "admin_id": row.get::<i32, _>("admin_id"),
            "account_id": row.get::<i32, _>("account_id"),
            "admin_name": row.get::<String, _>("admin_name"),
            "username": row.get::<String, _>("username"),
            "password": row.get::<String, _>("password"),
            "account_type": row.get::<String, _>("account_type"),
            "is_active": row.get::<i8, _>("is_active"),
            "account_date_created": row.get::<crate::types::DbDateTime, _>("account_date_created"),
        })
    });
    let event_json = json!({
        "event_id": event_id,
        "event_name": event.get::<String, _>("event_name"),
        "event_description": event.get::<String, _>("event_description"),
        "event_date": event.get::<chrono::NaiveDate, _>("event_date"),
        "event_status": event.get::<String, _>("event_status"),
        "admin_cash_on_hand": event.get::<Money, _>("admin_cash_on_hand"),
        "teller_initial_cash_on_hand": event.get::<Money, _>("teller_initial_cash_on_hand"),
        "prizes": event.get::<Money, _>("prizes"),
        "rd": event.get::<Money, _>("rd"),
        "event_percentage": event.get::<crate::types::Rate, _>("event_percentage"),
        "matching_event_id": event.try_get::<Option<i32>, _>("matching_event_id").unwrap_or(None),
    });

    Json(json!({
        "cash_out": cash_out_sum.map(|m| json!(m)).unwrap_or(json!(0)),
        "cash_in": cash_in_sum.map(|m| json!(m)).unwrap_or(json!(0)),
        "token": "",
        "admin": admin_json,
        "event": event_json,
        "match": match_json,
    }))
    .into_response()
}
