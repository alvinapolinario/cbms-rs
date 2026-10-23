//! Bet settlement for a fight result -- mirrors the `Completed`/`Cancelled`/`Draw` branches of
//! `MatchController@updateMatchStatus` exactly (docs/WORKFLOW.md section 7.5). The winning
//! payout is `floor(bet_amount * odds)` in raw PHP floats, using whatever `meron_odds`/
//! `wala_odds` were just recomputed moments earlier in the same request -- not re-rounded again
//! here. Losing-side bets are left exactly as the defensive reset leaves them (there is no
//! separate "mark loser" step in the original, so there isn't one here either).

use crate::money::Money;
use sqlx::Row;

pub enum Outcome {
    /// `winner` is `"Meron"` or `"Wala"`.
    Completed { winner: &'static str },
    Cancelled,
    Draw,
}

/// `floor(bet_amount * odds)` in f64, matching `MatchController@updateMatchStatus`.
pub fn payout_amount(bet_amount: f64, odds: f64) -> f64 {
    (bet_amount * odds).floor()
}

pub async fn settle_bets(
    conn: &mut sqlx::MySqlConnection,
    match_id: i32,
    outcome: &Outcome,
    meron_odds: f64,
    wala_odds: f64,
) -> Result<(), sqlx::Error> {
    match outcome {
        Outcome::Completed { winner } => {
            sqlx::query(
                "UPDATE bets SET bet_win_amount = 0, bet_payout_amount = 0, bet_is_winner = 0
                 WHERE match_id = ? AND bet_status = 1",
            )
            .bind(match_id)
            .execute(&mut *conn)
            .await?;

            let odds = if *winner == "Meron" { meron_odds } else { wala_odds };
            let bets = sqlx::query("SELECT bet_id, bet_amount FROM bets WHERE match_id = ? AND bet_side = ? AND bet_status = 1")
                .bind(match_id)
                .bind(*winner)
                .fetch_all(&mut *conn)
                .await?;

            for chunk in bets.chunks(100) {
                apply_payout_chunk(&mut *conn, chunk, odds).await?;
            }
        }
        Outcome::Cancelled | Outcome::Draw => {
            sqlx::query(
                "UPDATE bets SET bet_win_amount = 0, bet_payout_amount = bet_amount, bet_is_winner = 0
                 WHERE match_id = ? AND bet_status = 1",
            )
            .bind(match_id)
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}

async fn apply_payout_chunk(
    conn: &mut sqlx::MySqlConnection,
    rows: &[sqlx::mysql::MySqlRow],
    odds: f64,
) -> Result<(), sqlx::Error> {
    if rows.is_empty() {
        return Ok(());
    }
    let payouts: Vec<(i32, String)> = rows
        .iter()
        .map(|row| {
            let bet_id: i32 = row.get("bet_id");
            let bet_amount: Money = row.get("bet_amount");
            let win = Money::new(payout_amount(bet_amount.value(), odds));
            (bet_id, win.to_db_string())
        })
        .collect();

    let mut sql = String::from("UPDATE bets SET bet_win_amount = CASE bet_id ");
    for _ in &payouts {
        sql.push_str("WHEN ? THEN ? ");
    }
    sql.push_str("END, bet_payout_amount = CASE bet_id ");
    for _ in &payouts {
        sql.push_str("WHEN ? THEN ? ");
    }
    sql.push_str("END, bet_is_winner = 1 WHERE bet_id IN (");
    sql.push_str(&vec!["?"; payouts.len()].join(","));
    sql.push(')');

    let mut query = sqlx::query(&sql);
    for (bet_id, win) in &payouts {
        query = query.bind(*bet_id).bind(win);
    }
    for (bet_id, win) in &payouts {
        query = query.bind(*bet_id).bind(win);
    }
    for (bet_id, _) in &payouts {
        query = query.bind(*bet_id);
    }
    query.execute(&mut *conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payout_floors_like_php() {
        assert_eq!(payout_amount(100.0, 1.395), 139.0);
        assert_eq!(payout_amount(10.0, 1.99), 19.0);
        assert_eq!(payout_amount(100.0, 1.6), 160.0);
        assert_eq!(payout_amount(0.0, 2.5), 0.0);
    }
}
