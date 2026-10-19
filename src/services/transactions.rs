/// Parity with the base `Controller::createLog()` helper every Laravel controller calls on
/// state-changing (and some failed) actions. `account_id = -1` for unauthenticated/failed
/// attempts where there's no real account yet.
///
/// Generic over the executor so a call made inside an open transaction writes its log row as
/// part of that same transaction (and rolls back with it) -- matching Laravel, where `DB::table`
/// calls implicitly use the current connection's open transaction too.
pub async fn create_log<'e, E>(executor: E, account_id: i32, transaction_type: &str, message: &str)
where
    E: sqlx::Executor<'e, Database = sqlx::MySql>,
{
    let _ = sqlx::query(
        "INSERT INTO transactions (account_id, transaction_type, transaction_message, transaction_datetime)
         VALUES (?, ?, ?, NOW())",
    )
    .bind(account_id)
    .bind(transaction_type)
    .bind(message)
    .execute(executor)
    .await;
}
