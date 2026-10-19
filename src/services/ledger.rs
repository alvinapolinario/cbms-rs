//! `teller_ledger` -- an append-only, structured cash journal per teller (one line per
//! movement, always carrying the balance immediately before and after). Mirrors
//! `App\Services\Ledger\TellerLedger` exactly, including its use of **exact integer-centavo**
//! arithmetic (via `App\Services\Closing\Money::cents()`/`fmt()`) rather than the raw-float math
//! used on the live betting path -- this is the one place in the whole app that intentionally
//! uses the other arithmetic system (see docs/WORKFLOW.md section 10 item 1), so money values
//! here are converted to/from exact centavos rather than computed in `f64` directly.
//!
//! `record()` MUST be called inside the same DB transaction as the balance change it describes,
//! and AFTER that change has already been applied to `event_tellers.teller_balance` -- it reads
//! the *already-updated* balance as `balance_after` and derives `balance_before` algebraically
//! (`after - in + out`), exactly like the PHP original.
//!
//! MVP scope only implements the `opening` (docs/WORKFLOW.md section 7.1) and `bet` (section
//! 7.4) line types actually needed by the routes built so far -- not the full cash-in/cash-out
//! approval flow (`cashDecision()` in the PHP), which is a later phase.

use crate::db::DbPool;
use crate::money::Money;
use sqlx::Row;

pub const TYPE_OPENING: &str = "opening";
pub const TYPE_BET: &str = "bet";
#[allow(dead_code)]
pub const TYPE_VOID: &str = "void";
#[allow(dead_code)]
pub const TYPE_PAYOUT_WIN: &str = "payout_win";
#[allow(dead_code)]
pub const TYPE_PAYOUT_REFUND: &str = "payout_refund";
#[allow(dead_code)]
pub const TYPE_CASH_IN: &str = "cash_in";
#[allow(dead_code)]
pub const TYPE_CASH_OUT: &str = "cash_out";
#[allow(dead_code)]
pub const TYPE_CASH_DECLINED: &str = "cash_declined";
pub const TYPE_ADJUSTMENT: &str = "adjustment";

/// Matches `App\Services\Closing\Money::cents()`: parses the exact 2-decimal string (never does
/// float multiplication, which could introduce the very drift this exact-arithmetic path exists
/// to avoid).
pub fn money_to_cents(m: Money) -> i64 {
    let s = m.to_db_string();
    let (sign, digits) = match s.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, s.as_str()),
    };
    let mut parts = digits.splitn(2, '.');
    let whole: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let frac: i64 = parts.next().unwrap_or("00").parse().unwrap_or(0);
    sign * (whole * 100 + frac)
}

/// Matches `Money::fmt()`.
pub fn cents_to_db_string(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

#[derive(Default)]
pub struct LedgerMeta<'a> {
    pub fight_no: Option<i32>,
    pub match_id: Option<i32>,
    pub reference: Option<&'a str>,
    pub related_event_teller_id: Option<i32>,
    pub approved_by: Option<i32>,
    pub note: Option<&'a str>,
    pub source_table: Option<&'a str>,
    pub source_id: Option<i32>,
}

/// Writes one ledger line. Must run on a connection that is inside the same transaction as the
/// balance change, and after that change was applied -- callers pass `&mut *tx` (reborrowed from
/// an open `sqlx::Transaction`).
pub async fn record(
    conn: &mut sqlx::MySqlConnection,
    event_teller_id: i32,
    entry_type: &str,
    amount_in: Money,
    amount_out: Money,
    actor_account_id: Option<i32>,
    meta: LedgerMeta<'_>,
) -> Result<(), sqlx::Error> {
    let row = sqlx::query("SELECT event_id, teller_balance FROM event_tellers WHERE event_teller_id = ? FOR UPDATE")
        .bind(event_teller_id)
        .fetch_one(&mut *conn)
        .await?;
    let event_id: i32 = row.get("event_id");
    let current_balance: Money = row.get("teller_balance");

    let in_c = money_to_cents(amount_in);
    let out_c = money_to_cents(amount_out);
    let after_c = money_to_cents(current_balance);
    let before_c = after_c - in_c + out_c;

    // line_no is `int(10) unsigned`.
    let next_line_no: Option<u32> = sqlx::query_scalar(
        "SELECT MAX(line_no) FROM teller_ledger WHERE event_teller_id = ?",
    )
    .bind(event_teller_id)
    .fetch_one(&mut *conn)
    .await?;
    let line_no = next_line_no.unwrap_or(0) + 1;

    let reference: Option<String> = meta.reference.map(|r| r.chars().take(60).collect());
    let note: Option<String> = meta.note.map(|n| n.chars().take(255).collect());

    sqlx::query(
        "INSERT INTO teller_ledger
            (event_id, event_teller_id, line_no, entry_at, type, fight_no, match_id, reference,
             amount_in, amount_out, balance_before, balance_after, related_event_teller_id,
             recorded_by_account_id, approved_by_account_id, note, source_table, source_id)
         VALUES (?, ?, ?, NOW(), ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(event_id)
    .bind(event_teller_id)
    .bind(line_no)
    .bind(entry_type)
    .bind(meta.fight_no)
    .bind(meta.match_id)
    .bind(reference)
    .bind(cents_to_db_string(in_c))
    .bind(cents_to_db_string(out_c))
    .bind(cents_to_db_string(before_c))
    .bind(cents_to_db_string(after_c))
    .bind(meta.related_event_teller_id)
    .bind(actor_account_id)
    .bind(meta.approved_by)
    .bind(note)
    .bind(meta.source_table)
    .bind(meta.source_id)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cents_roundtrip() {
        assert_eq!(money_to_cents(Money::new(1234.56)), 123456);
        assert_eq!(money_to_cents(Money::new(-5.0)), -500);
        assert_eq!(cents_to_db_string(123456), "1234.56");
        assert_eq!(cents_to_db_string(-5), "-0.05");
    }
}
