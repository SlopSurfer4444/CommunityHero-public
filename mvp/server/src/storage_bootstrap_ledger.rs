//! Complete existing native PostgreSQL projection in a read-only snapshot.
//! No server writer lease, migrations, seeding, admission, provider or auth setup.
use super::*;
use sqlx::Connection;

pub(crate) async fn read_bootstrap_ledger_snapshot(url:&str,profile:crate::accounts::Profile)->ApiResult<Value> {
    let mut connection=PgConnection::connect(url).await?;
    let result:ApiResult<Value>=async {
        sqlx::query("SET default_transaction_read_only = on").execute(&mut connection).await?;
        sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut connection).await?;
        // Exactly the server's complete projection: account/metadata identity,
        // every TABLES row in ordinal order, relational checks and validation.
        let workspace=super::read_postgres(&mut connection).await?;
        crate::runtime_bootstrap_ledger_cli::snapshot_report(&workspace,profile)
    }.await;
    // No workspace writes occur. Roll back even on read/validation failure.
    let rolled_back=sqlx::query("ROLLBACK").execute(&mut connection).await;
    let closed=connection.close().await;
    if rolled_back.is_err()||closed.is_err(){return Err(internal("Native read-only ledger snapshot closure failed"));}
    result
}
