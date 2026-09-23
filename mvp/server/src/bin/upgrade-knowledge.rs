//! Explicit offline upgrade; never starts the server or dispatches operations.
use sqlx::{Connection, PgConnection};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if std::env::args().nth(1).as_deref() != Some("--apply") {
        return Err("Usage: upgrade-knowledge --apply (DATABASE_URL environment required)".into());
    }
    let mut conn = PgConnection::connect(&std::env::var("DATABASE_URL")?).await?;
    let mut tx = conn.begin().await?;
    for lease in [438772115_i64, 438772116_i64] {
        let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(lease)
            .fetch_one(&mut *tx)
            .await?;
        if !held {
            return Err("Database is in use; stop the pilot before explicit upgrade".into());
        }
    }
    let enabled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM communityhero.workspaces WHERE execution_enabled)",
    )
    .fetch_one(&mut *tx)
    .await?;
    if enabled {
        return Err("Execution hold must remain enabled".into());
    }
    sqlx::query("CREATE TABLE IF NOT EXISTS communityhero.schema_migrations(version integer PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now())").execute(&mut *tx).await?;
    let applied: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM communityhero.schema_migrations WHERE version=2)",
    )
    .fetch_one(&mut *tx)
    .await?;
    if !applied {
        sqlx::raw_sql(include_str!("../../migrations/0002_knowledge.sql"))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    println!("Knowledge schema v2 ready; execution remains disabled");
    Ok(())
}
