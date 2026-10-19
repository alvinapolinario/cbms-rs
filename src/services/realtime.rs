//! Pusher HTTP trigger for the seven Sabong2026 channels.
//!
//! beyondcode/laravel-websockets speaks the Pusher REST protocol. Event names match what
//! Laravel actually puts on the wire (`BroadcastEvent` uses `broadcastAs()` when it exists,
//! otherwise `get_class()`), which is also what the Blade `Echo` listeners format to:
//!
//! | Channel         | Wire name                      | Payload key     |
//! |-----------------|--------------------------------|-----------------|
//! | match-updates   | MatchUpdated                   | match           |
//! | fight-history   | App\Events\MatchesUpdated      | match (the list)|
//! | tv-display      | App\Events\TVDisplayUpdated    | match           |
//! | event-tellers   | TellerBalanceUpdated           | event_tellers   |
//! | cash-ins        | CashInUpdated                  | cash_ins        |
//! | cash-outs       | CashOutUpdated                 | cash_outs       |
//! | event-updates   | .EventUpdated                  | event           |
//!
//! `EventUpdated::broadcastAs` returns the string `.EventUpdated`, including the dot.
//! Sabong2026 defines that class and never calls `broadcast(new EventUpdated(...))`. This
//! crate publishes it when an event row is stored or updated, using that same name.
//!
//! The database snapshot is read before the HTTP response returns. The POST to the websocket
//! server runs in the background so a stopped websocket process does not stall a bet.

use std::time::Duration;

use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::Row;

use crate::config::Config;
use crate::db::DbPool;
use crate::models::Fight;
use crate::money::Money;
use crate::rows::row_to_json;
use crate::AppState;

const MATCH_UPDATED: &str = "MatchUpdated";
const MATCHES_UPDATED: &str = "App\\Events\\MatchesUpdated";
const TV_UPDATED: &str = "App\\Events\\TVDisplayUpdated";
const TELLER_UPDATED: &str = "TellerBalanceUpdated";
const CASH_IN_UPDATED: &str = "CashInUpdated";
const CASH_OUT_UPDATED: &str = "CashOutUpdated";
const EVENT_UPDATED: &str = ".EventUpdated";

pub async fn match_updated(state: &AppState, match_id: i32) {
    let fight = sqlx::query_as::<_, Fight>("SELECT * FROM matches WHERE match_id = ?")
        .bind(match_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if let Some(fight) = fight {
        spawn_trigger(&state.config, "match-updates", MATCH_UPDATED, json!({ "match": fight }));
    }
}

pub async fn latest_match_updated(state: &AppState, event_id: i32) {
    let fight = sqlx::query_as::<_, Fight>(
        "SELECT * FROM matches WHERE event_id = ? ORDER BY match_number DESC LIMIT 1",
    )
    .bind(event_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    if let Some(fight) = fight {
        spawn_trigger(&state.config, "match-updates", MATCH_UPDATED, json!({ "match": fight }));
    }
}

pub async fn fight_history(state: &AppState, event_id: i32) {
    let matches = sqlx::query_as::<_, Fight>(
        "SELECT * FROM matches WHERE event_id = ? ORDER BY match_number DESC",
    )
    .bind(event_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    spawn_trigger(
        &state.config,
        "fight-history",
        MATCHES_UPDATED,
        json!({ "match": matches }),
    );
}

pub async fn tv_display(state: &AppState, match_id: i32) {
    let fight = sqlx::query_as::<_, Fight>("SELECT * FROM matches WHERE match_id = ?")
        .bind(match_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if let Some(fight) = fight {
        spawn_trigger(&state.config, "tv-display", TV_UPDATED, json!({ "match": fight }));
    }
}

pub async fn teller_balances(state: &AppState, event_id: i32) {
    let rows = sqlx::query("SELECT * FROM event_tellers_view WHERE event_id = ? ORDER BY teller_name ASC")
        .bind(event_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let mut tellers = Vec::with_capacity(rows.len());
    for row in rows {
        let event_teller_id: i32 = row.try_get("event_teller_id").unwrap_or(0);
        let mut obj = match row_to_json(&row) {
            Value::Object(map) => map,
            other => {
                tellers.push(other);
                continue;
            }
        };
        let (total_bet, total_payout) = teller_totals(&state.db, event_teller_id).await;
        obj.insert("total_bet".into(), json!(total_bet));
        obj.insert("total_payout".into(), json!(total_payout));
        tellers.push(Value::Object(obj));
    }
    spawn_trigger(
        &state.config,
        "event-tellers",
        TELLER_UPDATED,
        json!({ "event_tellers": tellers }),
    );
}

pub async fn cash_ins(state: &AppState, event_id: i32) {
    let rows = sqlx::query("SELECT * FROM cash_ins_view WHERE event_id = ? ORDER BY cash_in_id DESC")
        .bind(event_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let cash_ins: Vec<Value> = rows.iter().map(row_to_json).collect();
    spawn_trigger(&state.config, "cash-ins", CASH_IN_UPDATED, json!({ "cash_ins": cash_ins }));
}

pub async fn cash_outs(state: &AppState, event_id: i32) {
    let rows = sqlx::query("SELECT * FROM cash_outs_view WHERE event_id = ? ORDER BY cash_out_id DESC")
        .bind(event_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let cash_outs: Vec<Value> = rows.iter().map(row_to_json).collect();
    spawn_trigger(
        &state.config,
        "cash-outs",
        CASH_OUT_UPDATED,
        json!({ "cash_outs": cash_outs }),
    );
}

pub async fn event_updated(state: &AppState, event_id: i32) {
    let row = sqlx::query(
        "SELECT event_id, event_name, event_description, event_percentage, event_date, event_status,
                admin_cash_on_hand, teller_initial_cash_on_hand, prizes, rd
         FROM events WHERE event_id = ?",
    )
    .bind(event_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    if let Some(row) = row {
        spawn_trigger(
            &state.config,
            "event-updates",
            EVENT_UPDATED,
            json!({ "event": row_to_json(&row) }),
        );
    }
}

pub async fn event_id_of_teller(db: &DbPool, event_teller_id: i32) -> Option<i32> {
    sqlx::query_scalar("SELECT event_id FROM event_tellers WHERE event_teller_id = ?")
        .bind(event_teller_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

async fn teller_totals(db: &DbPool, event_teller_id: i32) -> (f64, f64) {
    let total_bet: Option<Money> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view
         WHERE event_teller_id = ? AND bet_status = 1
           AND match_status <> 'Draw' AND match_status <> 'Cancelled'",
    )
    .bind(event_teller_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let total_payout: Option<Money> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(claim_amount), 0) FROM claims WHERE event_teller_id = ?",
    )
    .bind(event_teller_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    (
        total_bet.map(|m| m.value()).unwrap_or(0.0),
        total_payout.map(|m| m.value()).unwrap_or(0.0),
    )
}

fn spawn_trigger(config: &Config, channel: &str, name: &str, data: Value) {
    let config = config.clone();
    let channel = channel.to_string();
    let name = name.to_string();
    tokio::spawn(async move {
        if let Err(err) = trigger(&config, &channel, &name, &data).await {
            tracing::warn!(%err, channel = %channel, event = %name, "websocket trigger failed");
        }
    });
}

async fn trigger(config: &Config, channel: &str, name: &str, data: &Value) -> Result<(), String> {
    let body = serde_json::json!({
        "name": name,
        "channels": [channel],
        "data": serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string()),
    });
    let body_bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    let body_md5 = format!("{:x}", Md5::digest(&body_bytes));
    let timestamp = chrono::Utc::now().timestamp();
    let path = format!("/apps/{}/events", config.pusher_app_id);
    let query = format!(
        "auth_key={}&auth_timestamp={}&auth_version=1.0&body_md5={}",
        config.pusher_app_key, timestamp, body_md5
    );
    let to_sign = format!("POST\n{path}\n{query}");
    let mut mac = Hmac::<Sha256>::new_from_slice(config.pusher_app_secret.as_bytes())
        .map_err(|e| e.to_string())?;
    mac.update(to_sign.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());
    let url = format!(
        "{}://{}:{}{}?{}&auth_signature={}",
        config.pusher_scheme, config.pusher_host, config.pusher_port, path, query, signature
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("websocket HTTP {}", response.status()));
    }
    Ok(())
}
