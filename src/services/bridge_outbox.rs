//! Writes to `bridge_outbox` -- the betting -> matching half of the bridge (docs/WORKFLOW.md
//! section 8). Only the *queueing* (same-transaction, idempotency-key-bearing insert) lives
//! here; actual HTTP delivery (the `bridge:work` equivalent background worker) is a later phase
//! -- see `bridge_client`/`bridge_worker` in the plan. These functions are still safe to call
//! now: they're a no-op unless the fight is bridge-linked, and an unflushed outbox row is
//! harmless (it just waits for the worker).

use crate::models::Fight;
use crate::types::DbDateTime;
use serde_json::json;
use uuid::Uuid;

/// Queues a `status` message (docs/WORKFLOW.md section 8.6). No-op if the fight isn't
/// bridge-linked (`source_fight_uid` null) -- matching `MatchingBridge::queueStatus()`'s own
/// guard.
pub async fn queue_status(
    conn: &mut sqlx::MySqlConnection,
    fight: &Fight,
    state: &str,
) -> Result<(), sqlx::Error> {
    let Some(fight_uid) = fight.source_fight_uid else {
        return Ok(());
    };
    let now = DbDateTime::now();
    let payload = json!({
        "fight_uid": fight_uid,
        "fight_no": fight.match_number,
        "state": state,
        "hold_reason": if state == "held" { fight.hold_reason.clone() } else { None },
        "meron_total": fight.meron_total_bet.to_db_string(),
        "wala_total": fight.wala_total_bet.to_db_string(),
        "meron_odds": if fight.meron_odds > 0.0 { fight.meron_odds } else { 0.0 },
        "wala_odds": if fight.wala_odds > 0.0 { fight.wala_odds } else { 0.0 },
        "at": now.0.format("%Y-%m-%d %H:%M:%S").to_string(),
    });
    insert_outbox(conn, "status", Some(fight_uid), payload).await
}

/// Queues a `result` message (docs/WORKFLOW.md section 8.7). `result` is one of
/// `meron`/`wala`/`draw`/`cancelled`. `version` counts outbox attempts for this fight_uid's
/// result messages so far, plus one (matching the PHP behavior noted in WORKFLOW.md section
/// 7.5/10: it counts delivery attempts, not distinct human corrections -- replicated faithfully,
/// not "fixed").
pub async fn queue_result(
    conn: &mut sqlx::MySqlConnection,
    fight: &Fight,
    result: &str,
    previous: Option<&str>,
    declared_by: Option<&str>,
) -> Result<(), sqlx::Error> {
    let Some(fight_uid) = fight.source_fight_uid else {
        return Ok(());
    };
    let prior_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM bridge_outbox WHERE type = 'result' AND fight_uid = ?",
    )
    .bind(fight_uid)
    .fetch_one(&mut *conn)
    .await?;
    let version = prior_count + 1;

    let now = DbDateTime::now();
    let payload = json!({
        "fight_uid": fight_uid,
        "fight_no": fight.match_number,
        "result": result,
        "version": version,
        "previous": previous,
        "declared_by": declared_by,
        "at": now.0.format("%Y-%m-%d %H:%M:%S").to_string(),
    });
    insert_outbox(conn, "result", Some(fight_uid), payload).await
}

async fn insert_outbox(
    conn: &mut sqlx::MySqlConnection,
    msg_type: &str,
    fight_uid: Option<i32>,
    mut payload: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let msg_key = format!(
        "betting-{msg_type}-{}-{}",
        fight_uid.map(|v| v.to_string()).unwrap_or_else(|| "x".to_string()),
        Uuid::new_v4()
    );
    if let serde_json::Value::Object(ref mut map) = payload {
        map.insert("msg_key".to_string(), json!(msg_key));
        map.insert("type".to_string(), json!(msg_type));
    }

    sqlx::query(
        "INSERT INTO bridge_outbox (msg_key, type, fight_uid, payload, created_at) VALUES (?, ?, ?, ?, NOW())",
    )
    .bind(&msg_key)
    .bind(msg_type)
    .bind(fight_uid)
    .bind(payload.to_string())
    .execute(&mut *conn)
    .await?;

    Ok(())
}

#[allow(dead_code)]
pub async fn pending_count(conn: &mut sqlx::MySqlConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM bridge_outbox WHERE sent_at IS NULL")
        .fetch_one(conn)
        .await
}
