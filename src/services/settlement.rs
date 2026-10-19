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

            for row in bets {
                let bet_id: i32 = row.get("bet_id");
                let bet_amount: Money = row.get("bet_amount");
                let win = Money::new((bet_amount.value() * odds).floor());
                sqlx::query("UPDATE bets SET bet_win_amount = ?, bet_payout_amount = ?, bet_is_winner = 1 WHERE bet_id = ?")
                    .bind(win.to_db_string())
                    .bind(win.to_db_string())
                    .bind(bet_id)
                    .execute(&mut *conn)
                    .await?;
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
