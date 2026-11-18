//! The candidate query, read-only. The SQL lives once, in
//! `scripts/forms/auto/candidates.sql` (also runnable through psql); this module
//! strips its psql wrapper and runs the statement inside a READ ONLY transaction.
use anyhow::{ensure, Result};
use serde_json::Value;
use sqlx::PgPool;

const SQL: &str = include_str!("../../../../scripts/forms/auto/candidates.sql");

/// The single SELECT of candidates.sql without psql meta commands, comments,
/// the begin/rollback wrapper and the final semicolon.
pub fn statement(sql: &str) -> String {
    let body: Vec<&str> = sql
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with('\\')
                || t.starts_with("--")
                || t.eq_ignore_ascii_case("begin read only;")
                || t.eq_ignore_ascii_case("rollback;"))
        })
        .collect();
    body.join("\n")
        .trim()
        .trim_end_matches(';')
        .trim()
        .to_string()
}

/// The JSON array candidates.sql prints (one row per collector `template` resource).
pub async fn fetch(pool: &PgPool) -> Result<Value> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    let rows: Value = sqlx::query_scalar(&statement(SQL))
        .fetch_one(&mut *tx)
        .await?;
    tx.rollback().await?;
    ensure!(
        rows.is_array(),
        "candidate query did not return a JSON array"
    );
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_sql_is_one_read_only_select() {
        let s = statement(SQL);
        assert!(s.starts_with("with approved as"), "{}", &s[..40]);
        assert!(s.ends_with(") x"));
        assert!(!s.contains('\\') && !s.contains(';'));
        let lower = s.to_ascii_lowercase();
        for word in ["insert ", "update ", "delete ", "drop ", "alter "] {
            assert!(
                !lower.contains(word),
                "candidate query must only read ({word})"
            );
        }
    }
}
