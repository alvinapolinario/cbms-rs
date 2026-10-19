//! `GET /api/print-service/*` -- `PrintApiController` plus the `X-Print-Key` check.
//!
//! The FastAPI print service calls these paths when `LARAVEL_PRINT_KEY` is set. Sums that PHP
//! casts with `(float)` are JSON numbers. Event rows use the Laravel `events` columns
//! (decimals stay strings).

use axum::async_trait;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::NaiveDate;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::Arc;

use crate::money::Money;
use crate::rows::row_to_json;
use crate::types::Rate;
use crate::AppState;

pub struct PrintAuth;

#[async_trait]
impl FromRequestParts<Arc<AppState>> for PrintAuth {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let expected = state.config.print_service_key.as_str();
        if expected.is_empty() {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "message": "Print service key not configured" })),
            )
                .into_response());
        }
        let provided = parts
            .headers
            .get("x-print-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !keys_equal(expected, provided) {
            return Err((StatusCode::UNAUTHORIZED, Json(json!({ "message": "Unauthorized" }))).into_response());
        }
        Ok(PrintAuth)
    }
}

fn keys_equal(expected: &str, provided: &str) -> bool {
    let a = expected.as_bytes();
    let b = provided.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(sqlx::FromRow, Serialize)]
struct PrintEvent {
    event_id: i32,
    event_name: String,
    event_description: String,
    event_percentage: Rate,
    event_date: NaiveDate,
    event_status: String,
    admin_cash_on_hand: Money,
    teller_initial_cash_on_hand: Money,
    prizes: Money,
    rd: Money,
}

const EVENT_SQL: &str = "SELECT event_id, event_name, event_description, event_percentage, event_date,
    event_status, admin_cash_on_hand, teller_initial_cash_on_hand, prizes, rd FROM events";

const TELLER_COLS: &str = "event_teller_id, event_id, teller_id, teller_name, teller_balance,
    teller_match_balance, event_name, event_status, event_date";

const TELLER_COLS_NO_MATCH: &str = "event_teller_id, event_id, teller_id, teller_name, teller_balance,
    event_name, event_status, event_date";

#[derive(Deserialize)]
pub struct EventQuery {
    pub event_id: Option<i32>,
}

pub async fn events(State(state): State<Arc<AppState>>, _auth: PrintAuth) -> Response {
    match sqlx::query_as::<_, PrintEvent>(&format!("{EVENT_SQL} ORDER BY event_id"))
        .fetch_all(&state.db)
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => server_error(e),
    }
}

pub async fn tellers(
    State(state): State<Arc<AppState>>,
    _auth: PrintAuth,
    Query(q): Query<EventQuery>,
) -> Response {
    let Some(event) = load_event(&state, q.event_id).await else {
        let message = if q.event_id.is_none() { "No active event" } else { "Event not found" };
        return (StatusCode::NOT_FOUND, Json(json!({ "message": message }))).into_response();
    };
    match teller_rows(&state, event.event_id, true).await {
        Ok(tellers) => Json(json!({ "event": event, "tellers": tellers })).into_response(),
        Err(e) => server_error(e),
    }
}

pub async fn history(
    State(state): State<Arc<AppState>>,
    _auth: PrintAuth,
    Path(event_teller_id): Path<i32>,
) -> Response {
    let fights = match fight_rows(&state, event_teller_id).await {
        Ok(rows) => rows,
        Err(e) => return server_error(e),
    };
    let teller = sqlx::query(&format!(
        "SELECT {TELLER_COLS} FROM event_tellers_view WHERE event_teller_id = ?"
    ))
    .bind(event_teller_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let balance = teller
        .as_ref()
        .and_then(|row| row.try_get::<Money, _>("teller_balance").ok())
        .map(|m| m.value())
        .unwrap_or(0.0);
    let summary = match cash_summary(&state, event_teller_id).await {
        Ok(s) => s,
        Err(e) => return server_error(e),
    };
    Json(json!({
        "teller": teller.as_ref().map(row_to_json),
        "total_bet": summary.total_bet,
        "total_payout": summary.total_payout,
        "cash_on_hand": balance,
        "fights": fights,
    }))
    .into_response()
}

pub async fn event_totals(
    State(state): State<Arc<AppState>>,
    _auth: PrintAuth,
    Query(q): Query<EventQuery>,
) -> Response {
    let Some(event) = load_event(&state, q.event_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "Event not found" }))).into_response();
    };
    let total_collections = sum_money(
        &state,
        "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view
         WHERE event_id = ? AND bet_status = 1 AND match_status = 'Completed'",
        event.event_id,
    )
    .await;
    let total_fights: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM matches WHERE event_id = ?")
        .bind(event.event_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    let cash_in = sum_money(
        &state,
        "SELECT COALESCE(SUM(cash_in_amount), 0) FROM cash_ins_view
         WHERE event_id = ? AND cash_in_status = 'Approved'",
        event.event_id,
    )
    .await;
    let cash_out = sum_money(
        &state,
        "SELECT COALESCE(SUM(cash_out_amount), 0) FROM cash_outs_view
         WHERE event_id = ? AND cash_out_status = 'Approved'",
        event.event_id,
    )
    .await;
    Json(json!({
        "total_collections": total_collections,
        "total_fights": total_fights,
        "cash_on_hand": (event.admin_cash_on_hand.value() - cash_in) + cash_out,
    }))
    .into_response()
}

pub async fn cash_transactions(
    State(state): State<Arc<AppState>>,
    _auth: PrintAuth,
    Query(q): Query<EventQuery>,
) -> Response {
    let Some(event) = load_event(&state, q.event_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "Event not found" }))).into_response();
    };
    match teller_rows(&state, event.event_id, false).await {
        Ok(tellers) => Json(json!({ "event": event, "tellers": tellers })).into_response(),
        Err(e) => server_error(e),
    }
}

pub async fn teller_cash_summary(
    State(state): State<Arc<AppState>>,
    _auth: PrintAuth,
    Path(event_teller_id): Path<i32>,
) -> Response {
    let teller = sqlx::query(&format!(
        "SELECT {TELLER_COLS_NO_MATCH} FROM event_tellers_view WHERE event_teller_id = ?"
    ))
    .bind(event_teller_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let Some(teller) = teller else {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "Teller not found" }))).into_response();
    };
    let event_id: i32 = teller.try_get("event_id").unwrap_or(0);
    let event = sqlx::query_as::<_, PrintEvent>(&format!("{EVENT_SQL} WHERE event_id = ?"))
        .bind(event_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let summary = match cash_summary(&state, event_teller_id).await {
        Ok(s) => s,
        Err(e) => return server_error(e),
    };
    Json(json!({
        "event": event,
        "teller": row_to_json(&teller),
        "total_bet": summary.total_bet,
        "total_payout": summary.total_payout,
        "total_cash_in": summary.total_cash_in,
        "total_cash_out": summary.total_cash_out,
        "cash_on_hand": summary.cash_on_hand,
    }))
    .into_response()
}

pub async fn analytics_event(
    State(state): State<Arc<AppState>>,
    _auth: PrintAuth,
    Path(event_id): Path<i32>,
) -> Response {
    let event = sqlx::query_as::<_, PrintEvent>(&format!("{EVENT_SQL} WHERE event_id = ?"))
        .bind(event_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(event) = event else {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "Event not found" }))).into_response();
    };
    let gross_meron = sum_money(
        &state,
        "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view
         WHERE event_id = ? AND bet_status = 1 AND bet_side = 'Meron' AND match_status = 'Completed'",
        event.event_id,
    )
    .await;
    let gross_wala = sum_money(
        &state,
        "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view
         WHERE event_id = ? AND bet_status = 1 AND bet_side = 'Wala' AND match_status = 'Completed'",
        event.event_id,
    )
    .await;
    let payout = sum_join(
        &state,
        "SELECT COALESCE(SUM(claims.claim_amount), 0) FROM claims
         JOIN event_tellers ON claims.event_teller_id = event_tellers.event_teller_id
         WHERE event_tellers.event_id = ?",
        event.event_id,
    )
    .await;
    let unclaim = sum_money(
        &state,
        "SELECT COALESCE(SUM(bet_payout_amount), 0) FROM bets_view
         WHERE event_id = ? AND bet_payout_datetime IS NULL AND bet_payout_amount > 0 AND bet_status = 1",
        event.event_id,
    )
    .await;
    let fight_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM matches WHERE event_id = ?")
        .bind(event.event_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    let mut tellers = match teller_rows(&state, event.event_id, false).await {
        Ok(rows) => rows,
        Err(e) => return server_error(e),
    };
    let mut slim = Vec::with_capacity(tellers.len());
    for row in tellers.drain(..) {
        slim.push(json!({
            "event_teller_id": row.get("event_teller_id").cloned().unwrap_or(Value::Null),
            "teller_id": row.get("teller_id").cloned().unwrap_or(Value::Null),
            "teller_name": row.get("teller_name").cloned().unwrap_or(Value::Null),
            "total_bet": row.get("total_bet").cloned().unwrap_or(json!(0.0)),
            "total_payout": row.get("total_payout").cloned().unwrap_or(json!(0.0)),
            "total_cash_in": row.get("total_cash_in").cloned().unwrap_or(json!(0.0)),
            "total_cash_out": row.get("total_cash_out").cloned().unwrap_or(json!(0.0)),
            "cash_on_hand": row.get("cash_on_hand").cloned().unwrap_or(json!(0.0)),
        }));
    }
    slim.sort_by(|a, b| {
        let ba = a.get("total_bet").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let bb = b.get("total_bet").and_then(|v| v.as_f64()).unwrap_or(0.0);
        bb.partial_cmp(&ba).unwrap_or(std::cmp::Ordering::Equal)
    });
    Json(json!({
        "event": {
            "event_id": event.event_id,
            "event_name": event.event_name,
            "event_date": event.event_date.format("%Y-%m-%d").to_string(),
            "event_status": event.event_status,
        },
        "gross": gross_meron + gross_wala,
        "payout": payout,
        "unclaim": unclaim,
        "fight_count": fight_count,
        "tellers": slim,
    }))
    .into_response()
}

struct Summary {
    total_bet: f64,
    total_payout: f64,
    total_cash_in: f64,
    total_cash_out: f64,
    cash_on_hand: f64,
}

async fn cash_summary(state: &AppState, event_teller_id: i32) -> Result<Summary, sqlx::Error> {
    let total_bet = sum_money(
        state,
        "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view
         WHERE event_teller_id = ? AND bet_status = 1
           AND match_status <> 'Draw' AND match_status <> 'Cancelled'",
        event_teller_id,
    )
    .await;
    let total_payout = sum_money(
        state,
        "SELECT COALESCE(SUM(claim_amount), 0) FROM claims WHERE event_teller_id = ?",
        event_teller_id,
    )
    .await;
    let total_cash_in = sum_money(
        state,
        "SELECT COALESCE(SUM(cash_in_amount), 0) FROM cash_ins_view
         WHERE event_teller_id = ? AND cash_in_status = 'Approved'",
        event_teller_id,
    )
    .await;
    let total_cash_out = sum_money(
        state,
        "SELECT COALESCE(SUM(cash_out_amount), 0) FROM cash_outs_view
         WHERE event_teller_id = ? AND cash_out_status = 'Approved'",
        event_teller_id,
    )
    .await;
    let balance: Option<Money> =
        sqlx::query_scalar("SELECT teller_balance FROM event_tellers WHERE event_teller_id = ?")
            .bind(event_teller_id)
            .fetch_optional(&state.db)
            .await?;
    Ok(Summary {
        total_bet,
        total_payout,
        total_cash_in,
        total_cash_out,
        cash_on_hand: balance.map(|m| m.value()).unwrap_or(0.0),
    })
}

async fn teller_rows(state: &AppState, event_id: i32, with_match_balance: bool) -> Result<Vec<Value>, sqlx::Error> {
    let cols = if with_match_balance { TELLER_COLS } else { TELLER_COLS_NO_MATCH };
    let rows = sqlx::query(&format!(
        "SELECT {cols} FROM event_tellers_view WHERE event_id = ? ORDER BY teller_name ASC"
    ))
    .bind(event_id)
    .fetch_all(&state.db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let event_teller_id: i32 = row.try_get("event_teller_id").unwrap_or(0);
        let summary = cash_summary(state, event_teller_id).await?;
        let mut obj = match row_to_json(&row) {
            Value::Object(map) => map,
            other => {
                out.push(other);
                continue;
            }
        };
        obj.insert("total_bet".into(), json!(summary.total_bet));
        obj.insert("total_payout".into(), json!(summary.total_payout));
        obj.insert("total_cash_in".into(), json!(summary.total_cash_in));
        obj.insert("total_cash_out".into(), json!(summary.total_cash_out));
        obj.insert("cash_on_hand".into(), json!(summary.cash_on_hand));
        out.push(Value::Object(obj));
    }
    Ok(out)
}

async fn fight_rows(state: &AppState, event_teller_id: i32) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT matches.match_status, matches.match_winner, matches.wala_odds, matches.meron_odds,
                matches.match_number,
                SUM(CASE WHEN bets.bet_side = 'Meron' AND bets.bet_status = 1 AND matches.match_status = 'Completed' THEN bets.bet_amount ELSE 0 END) AS total_meron,
                SUM(CASE WHEN bets.bet_side = 'Wala' AND bets.bet_status = 1 AND matches.match_status = 'Completed' THEN bets.bet_amount ELSE 0 END) AS total_wala,
                SUM(CASE WHEN bets.bet_status = 1 AND matches.match_status = 'Completed' THEN bets.bet_amount ELSE 0 END) AS total_bet
         FROM bets
         JOIN matches ON bets.match_id = matches.match_id
         WHERE bets.event_teller_id = ?
         GROUP BY matches.match_id, matches.match_winner, matches.meron_odds, matches.wala_odds,
                  matches.match_status, matches.match_number
         ORDER BY matches.match_number DESC",
    )
    .bind(event_teller_id)
    .fetch_all(&state.db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(json!({
            "match_status": row.try_get::<String, _>("match_status").unwrap_or_default(),
            "match_winner": row.try_get::<String, _>("match_winner").unwrap_or_default(),
            "wala_odds": row.try_get::<f32, _>("wala_odds").unwrap_or(0.0),
            "meron_odds": row.try_get::<f32, _>("meron_odds").unwrap_or(0.0),
            "match_number": row.try_get::<i32, _>("match_number").unwrap_or(0),
            "total_meron": money_f64(&row, "total_meron"),
            "total_wala": money_f64(&row, "total_wala"),
            "total_bet": money_f64(&row, "total_bet"),
        }));
    }
    Ok(out)
}

fn money_f64(row: &sqlx::mysql::MySqlRow, col: &str) -> f64 {
    row.try_get::<Money, _>(col).map(|m| m.value()).unwrap_or(0.0)
}

async fn load_event(state: &AppState, event_id: Option<i32>) -> Option<PrintEvent> {
    match event_id {
        Some(id) => sqlx::query_as::<_, PrintEvent>(&format!("{EVENT_SQL} WHERE event_id = ?"))
            .bind(id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten(),
        None => sqlx::query_as::<_, PrintEvent>(&format!("{EVENT_SQL} WHERE event_status = 'Active' LIMIT 1"))
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten(),
    }
}

async fn sum_money(state: &AppState, sql: &str, id: i32) -> f64 {
    sqlx::query_scalar::<_, Money>(sql)
        .bind(id)
        .fetch_one(&state.db)
        .await
        .map(|m| m.value())
        .unwrap_or(0.0)
}

async fn sum_join(state: &AppState, sql: &str, id: i32) -> f64 {
    sum_money(state, sql, id).await
}

fn server_error(err: sqlx::Error) -> Response {
    tracing::error!(error = %err, "print-service query failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "message": "Unexpected server error." })),
    )
        .into_response()
}
