//! cfg(test)-only complete existing isolated BAW snapshot. This is a reader,
//! not an artifact/owner admission or a pristine bootstrap helper.
use super::*;
use sqlx::Connection;
use std::str::FromStr;

pub(crate) async fn read_native_fixture_workspace_snapshot(url:&str,profile:crate::accounts::Profile)->ApiResult<Value> {
    if profile!=crate::accounts::Profile::BawRussia{return Err(crate::conflict("Isolated BAW native fixture snapshot required"));}
    let options=sqlx::postgres::PgConnectOptions::from_str(url).map_err(|_|crate::conflict("Isolated fixture endpoint required"))?;
    if options.get_host()!="127.0.0.1"||options.get_port()<=1024
        ||options.get_database().is_none_or(|name|!name.starts_with("communityhero_wave2_floor_")||!name.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'_')) {
        return Err(crate::conflict("Isolated fixture endpoint required"));
    }
    let mut connection=PgConnection::connect_with(&options).await.map_err(|_|crate::internal("Native fixture snapshot connection failed"))?;
    let result:ApiResult<Value>=async {
        sqlx::query("SET default_transaction_read_only=on").execute(&mut connection).await?;
        sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut connection).await?;
        let workspace=super::read_postgres(&mut connection).await?;
        if crate::accounts::Profile::from_workspace(&workspace)?!=profile{return Err(crate::conflict("Native fixture company mismatch"));}
        Ok(workspace)
    }.await;
    let rolled_back=sqlx::query("ROLLBACK").execute(&mut connection).await;
    let closed=connection.close().await;
    if rolled_back.is_err()||closed.is_err(){return Err(crate::internal("Native fixture read-only snapshot closure failed"));}result
}

/// One independent read-only connection observes the exact native writer lease
/// and every other backend in this isolated database. No lease acquisition,
/// writer pool, advisory lock, migration or metadata write is performed.
pub(crate) async fn read_native_fixture_backend_cessation(url:&str)->ApiResult<Value> {
    let options=sqlx::postgres::PgConnectOptions::from_str(url).map_err(|_|crate::bad("Isolated database required"))?;
    if options.get_host()!="127.0.0.1"||options.get_database().is_none_or(|name|!name.starts_with("communityhero_wave2_floor_")) {
        return Err(crate::bad("Isolated database required"));
    }
    let mut connection=PgConnection::connect_with(&options).await?;
    let result:ApiResult<Value>=async {
        sqlx::query("SET default_transaction_read_only=on").execute(&mut connection).await?;
        let raw:Vec<String>=sqlx::query_scalar("SELECT jsonb_build_object('pid',pid,'backendStart',backend_start::text,'state',state,'backendType',backend_type)::text FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() ORDER BY pid")
            .fetch_all(&mut connection).await?;
        let backends=raw.iter().map(|row|serde_json::from_str::<Value>(row).map_err(|_|crate::internal("Native backend observation invalid"))).collect::<ApiResult<Vec<_>>>()?;
        let leases:i64=sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE locktype='advisory' AND granted AND classid=0 AND objid=438772116 AND objsubid=1 AND database=(SELECT oid FROM pg_database WHERE datname=current_database())")
            .fetch_one(&mut connection).await?;
        Ok(serde_json::json!({"kind":"native-isolated-postgres-backend-cessation-observation","database":options.get_database(),"backends":backends,"backendCount":backends.len(),"advisoryLeaseCount":leases}))
    }.await;
    let closed=connection.close().await;if closed.is_err(){return Err(crate::internal("Native cessation read connection closure failed"));}result
}
