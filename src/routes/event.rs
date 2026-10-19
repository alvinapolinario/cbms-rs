//! `/events-admin/*` -- mirrors `EventController@store/@update/@list/@find`. Two quirks worth
//! flagging because they're easy to lose in translation: (1) `store`/`update` return the **bare
//! scalar** `1`/`0` as a plain-text body (not JSON) on success -- Laravel's router wraps a raw
//! non-Response return value in a plain `Response`, not a `JsonResponse`, so there is no
//! `Content-Type: application/json` on that specific path even though every *error* case from
//! the same actions does return JSON. (2) `update`'s request fields are prefixed with an
//! underscore (`_event_name`, `_event_status`, ...) and the row id arrives as `uid` -- `store`
//! uses the plain names. This isn't a typo on my part; it's what the real source does.

use crate::auth::AuthedAdmin;
use crate::error::AppError;
use crate::models::Event;
use crate::money::Money;
use crate::types::Rate;
use crate::AppState;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

fn plain_text(body: &str) -> Response {
    let mut res = (StatusCode::OK, body.to_string()).into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=UTF-8"));
    res
}

fn fail(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "message": message }))).into_response()
}

fn validate_percentage(p: Rate) -> Result<(), Response> {
    let v = p.value();
    if !(0.0..1.0).contains(&v) {
        return Err(fail(StatusCode::UNPROCESSABLE_ENTITY, "Commission must be a decimal between 0 and 1 (e.g. 0.07 for 7%)."));
    }
    Ok(())
}

fn validate_status(status: &str) -> Result<(), Response> {
    if status != "Active" && status != "Completed" {
        return Err(fail(StatusCode::UNPROCESSABLE_ENTITY, "Event status must be Active or Completed."));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct FindRequest {
    pub event_id: i32,
}

pub async fn find(State(state): State<Arc<AppState>>, _admin: AuthedAdmin, crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<FindRequest>) -> Response {
    let event = sqlx::query_as::<_, Event>("SELECT * FROM events WHERE event_id = ?")
        .bind(body.event_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    Json(event).into_response()
}

pub async fn list(State(state): State<Arc<AppState>>, _admin: AuthedAdmin) -> Response {
    let events = sqlx::query_as::<_, Event>("SELECT * FROM events ORDER BY event_id ASC")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    Json(events).into_response()
}

#[derive(Deserialize)]
pub struct StoreRequest {
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

pub async fn store(State(state): State<Arc<AppState>>, admin: AuthedAdmin, crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<StoreRequest>) -> Response {
    if let Err(r) = validate_percentage(body.event_percentage) {
        return r;
    }
    if let Err(r) = validate_status(&body.event_status) {
        return r;
    }
    if body.event_status == "Active" {
        let exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM events WHERE event_status = 'Active'")
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);
        if exists > 0 {
            return fail(StatusCode::UNPROCESSABLE_ENTITY, "Another event is already Active. Complete or close it first.");
        }
    }

    let result = sqlx::query(
        "INSERT INTO events (event_name, event_description, event_percentage, event_date, event_status,
            admin_cash_on_hand, teller_initial_cash_on_hand, prizes, rd)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&body.event_name)
    .bind(&body.event_description)
    .bind(body.event_percentage.to_db_string())
    .bind(body.event_date)
    .bind(&body.event_status)
    .bind(body.admin_cash_on_hand.to_db_string())
    .bind(body.teller_initial_cash_on_hand.to_db_string())
    .bind(body.prizes.to_db_string())
    .bind(body.rd.to_db_string())
    .execute(&state.db)
    .await;

    match result {
        Ok(res) => {
            crate::services::transactions::create_log(
                &state.db,
                admin.account_id,
                "Web App",
                &format!("Event - Event created. event_id={}", res.last_insert_id()),
            )
            .await;
            plain_text("1")
        }
        Err(e) => {
            tracing::error!(error = %e, "event store failed");
            plain_text("0")
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateRequest {
    pub uid: i32,
    #[serde(rename = "_event_name")]
    pub event_name: String,
    #[serde(rename = "_event_description")]
    pub event_description: String,
    #[serde(rename = "_event_percentage")]
    pub event_percentage: Rate,
    #[serde(rename = "_event_date")]
    pub event_date: NaiveDate,
    #[serde(rename = "_event_status")]
    pub event_status: String,
    #[serde(rename = "_admin_cash_on_hand")]
    pub admin_cash_on_hand: Money,
    #[serde(rename = "_teller_initial_cash_on_hand")]
    pub teller_initial_cash_on_hand: Money,
    #[serde(rename = "_prizes")]
    pub prizes: Money,
    #[serde(rename = "_rd")]
    pub rd: Money,
}

pub async fn update(State(state): State<Arc<AppState>>, admin: AuthedAdmin, crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<UpdateRequest>) -> Response {
    if let Err(r) = validate_percentage(body.event_percentage) {
        return r;
    }
    if let Err(r) = validate_status(&body.event_status) {
        return r;
    }
    if body.event_status == "Active" {
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM events WHERE event_status = 'Active' AND event_id <> ?",
        )
        .bind(body.uid)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
        if exists > 0 {
            return fail(StatusCode::UNPROCESSABLE_ENTITY, "Another event is already Active. Complete or close it first.");
        }
    }

    let result = sqlx::query(
        "UPDATE events SET event_name = ?, event_description = ?, event_percentage = ?, event_date = ?,
            event_status = ?, admin_cash_on_hand = ?, teller_initial_cash_on_hand = ?, prizes = ?, rd = ?
         WHERE event_id = ?",
    )
    .bind(&body.event_name)
    .bind(&body.event_description)
    .bind(body.event_percentage.to_db_string())
    .bind(body.event_date)
    .bind(&body.event_status)
    .bind(body.admin_cash_on_hand.to_db_string())
    .bind(body.teller_initial_cash_on_hand.to_db_string())
    .bind(body.prizes.to_db_string())
    .bind(body.rd.to_db_string())
    .bind(body.uid)
    .execute(&state.db)
    .await;

    match result {
        Ok(_) => {
            crate::services::transactions::create_log(
                &state.db,
                admin.account_id,
                "Web App",
                &format!("Event - Event updated. event_id={}", body.uid),
            )
            .await;
            plain_text("1")
        }
        Err(e) => {
            tracing::error!(error = %e, "event update failed");
            plain_text("0")
        }
    }
}

pub async fn delete(State(state): State<Arc<AppState>>, _admin: AuthedAdmin, crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<FindRequest>) -> Result<Response, AppError> {
    let result = sqlx::query("DELETE FROM events WHERE event_id = ?")
        .bind(body.event_id)
        .execute(&state.db)
        .await?;
    Ok(plain_text(if result.rows_affected() > 0 { "1" } else { "0" }))
}
