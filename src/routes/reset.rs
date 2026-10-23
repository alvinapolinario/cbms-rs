//! Same wipe as Laravel `ResetController@execute`: bets, cash, claims, fights, teller phone
//! locks, and event-teller rows. Teller and admin logins stay. A dump is written first.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::auth::AuthedAdmin;
use crate::AppState;

#[derive(Deserialize)]
pub struct ResetRequest {
    pub confirmation: String,
}

pub async fn execute(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<ResetRequest>,
) -> Response {
    if body.confirmation != "RESET" {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "Type RESET to confirm.");
    }

    let database_url = state.config.database_url.clone();
    let backup = match tokio::task::spawn_blocking(move || write_backup(&database_url)).await {
        Ok(Ok(path)) => path,
        Ok(Err(message)) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &message),
        Err(_) => return fail(StatusCode::INTERNAL_SERVER_ERROR, "Could not start the backup."),
    };

    if let Err(message) = clear_tables(&state).await {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, &message);
    }

    let name = backup
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("backup.sql");
    tracing::info!(admin_id = admin.account_id, backup = name, "operator reset completed");

    Json(json!({
        "message": format!("Reset complete. Teller phones are unlocked. Backup: {name}"),
        "backup": name
    }))
    .into_response()
}

async fn clear_tables(state: &AppState) -> Result<(), String> {
    let mut conn = state.db.acquire().await.map_err(|e| e.to_string())?;
    sqlx::query("SET FOREIGN_KEY_CHECKS=0")
        .execute(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;

    let statements = [
        "TRUNCATE bets",
        "TRUNCATE cash_ins",
        "TRUNCATE cash_outs",
        "TRUNCATE claims",
        "UPDATE event_tellers SET teller_balance = 0, teller_match_balance = 0",
        "TRUNCATE matches",
        "TRUNCATE personal_access_tokens",
        "UPDATE tellers SET phone_uid = ''",
        "TRUNCATE transactions",
        "TRUNCATE event_tellers",
        "TRUNCATE teller_shorts",
        "TRUNCATE salaries",
        "TRUNCATE teller_remittances",
    ];
    for statement in statements {
        if let Err(e) = sqlx::query(statement).execute(&mut *conn).await {
            let _ = sqlx::query("SET FOREIGN_KEY_CHECKS=1").execute(&mut *conn).await;
            return Err(format!("{statement} failed: {e}"));
        }
    }
    sqlx::query("SET FOREIGN_KEY_CHECKS=1")
        .execute(&mut *conn)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn write_backup(database_url: &str) -> Result<PathBuf, String> {
    let (user, password, host, port, database) = parse_mysql_url(database_url)?;
    let dir = std::env::var("BACKUP_DIR").unwrap_or_else(|_| "/var/backups/cbms".to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create the backup folder: {e}"))?;
    let stamp = chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S");
    let path = PathBuf::from(&dir).join(format!("backup_before_reset_{stamp}.sql"));
    let file = std::fs::File::create(&path).map_err(|e| format!("Could not create the backup file: {e}"))?;
    let output = Command::new("mariadb-dump")
        .env("MYSQL_PWD", password)
        .args([
            &format!("-h{host}"),
            &format!("-P{port}"),
            &format!("-u{user}"),
            "--single-transaction",
            "--quick",
            "--routines",
            "--triggers",
            &database,
        ])
        .stdout(file)
        .output()
        .map_err(|_| "mariadb-dump is not installed, so the reset was not run.".to_string())?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&path);
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Backup failed, so nothing was reset. {}", err.trim()));
    }
    let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if len == 0 {
        let _ = std::fs::remove_file(&path);
        return Err("Backup file was empty, so nothing was reset.".to_string());
    }
    Ok(path)
}

fn parse_mysql_url(url: &str) -> Result<(String, String, String, String, String), String> {
    let rest = url
        .strip_prefix("mysql://")
        .ok_or_else(|| "DATABASE_URL is not a mysql URL.".to_string())?;
    let (auth, hostpath) = rest
        .split_once('@')
        .ok_or_else(|| "DATABASE_URL is missing a host.".to_string())?;
    let (user, password) = auth
        .split_once(':')
        .ok_or_else(|| "DATABASE_URL is missing a password.".to_string())?;
    let (hostport, database) = hostpath
        .split_once('/')
        .ok_or_else(|| "DATABASE_URL is missing a database name.".to_string())?;
    let database = database.split('?').next().unwrap_or(database);
    let (host, port) = match hostport.split_once(':') {
        Some((host, port)) => (host, port),
        None => (hostport, "3306"),
    };
    Ok((
        percent_decode(user),
        percent_decode(password),
        host.to_string(),
        port.to_string(),
        percent_decode(database),
    ))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn fail(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "message": message }))).into_response()
}
