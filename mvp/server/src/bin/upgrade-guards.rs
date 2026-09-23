//! Explicit offline v3 upgrade. No HTTP server, provider or model linkage.
use sqlx::{Connection, PgConnection};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if std::env::args().nth(1).as_deref() != Some("--apply") {
        return Err("Usage: upgrade-guards --apply (DATABASE_URL environment required)".into());
    }
    let mut connection = PgConnection::connect(&std::env::var("DATABASE_URL")?).await?;
    let mut tx = connection.begin().await?;
    for lease in [438772115_i64, 438772116_i64] {
        let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(lease)
            .fetch_one(&mut *tx)
            .await?;
        if !held {
            return Err("Database is in use; stop the pilot before explicit upgrade".into());
        }
    }
    // Bound unexpected DDL contention; migration failure leaves v2 intact.
    sqlx::query("SET LOCAL lock_timeout='5s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout='60s'")
        .execute(&mut *tx)
        .await?;
    let enabled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM communityhero.workspaces WHERE execution_enabled)",
    )
    .fetch_one(&mut *tx)
    .await?;
    if enabled {
        return Err("Execution hold must remain enabled".into());
    }
    sqlx::raw_sql(include_str!("../../migrations/0003_history_guards.sql"))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    println!("History guards schema v3 ready; execution remains disabled");
    Ok(())
}
