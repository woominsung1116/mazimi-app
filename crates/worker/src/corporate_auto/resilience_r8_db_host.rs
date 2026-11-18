//! Hosts the reviewer's round-8 DB module (verbatim) for `r8_db_gate`: the
//! module's `use super::*` reads this module, whose `db()` connects only in
//! the gate's child process.

pub(super) use super::*;

pub(super) async fn db() -> Option<PgPool> {
    if std::env::var("R8_DB_GATE").is_err() {
        return None;
    }
    super::db().await
}

#[rustfmt::skip]
#[path = "resilience_adversarial_r8_db_tests.rs"]
mod adversarial_r8_db;
