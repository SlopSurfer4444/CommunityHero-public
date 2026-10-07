//! Dedicated, non-pooling offline session. Never reconnect/reacquire/reapply.
use super::*;
use crate::predecessor_recovery::{self as pr,Budget,VerifiedImport,VerifiedStartup,AuthorizedTransition};
use sqlx::{Connection,postgres::PgConnectOptions};
use std::{fs::{File,OpenOptions},str::FromStr};

fn endpoint(url:&str,scope:&Value)->ApiResult<PgConnectOptions> {
    pr::scope(scope)?;let options=PgConnectOptions::from_str(url).map_err(|_|pr::fail())?;
    let expected=&scope["storage"];let host=match options.get_host(){"[::1]"=>"::1",v=>v};
    if expected["host"]!=host||expected["port"].as_u64()!=Some(options.get_port() as u64)||expected["database"].as_str()!=options.get_database()||expected["user"]!=options.get_username(){return Err(pr::fail());}Ok(options)
}
fn server_lock(scope:&Value)->ApiResult<File> {
    let data=Path::new(pr::text(&scope["storage"]["dataDir"])?);
    if !data.is_absolute()||std::fs::canonicalize(data).map_err(|_|pr::fail())?.to_string_lossy().trim_start_matches("\\\\?\\")!=data.to_string_lossy().trim_start_matches("\\\\?\\"){return Err(pr::fail());}
    for a in data.ancestors(){let m=std::fs::symlink_metadata(a).map_err(|_|pr::fail())?;if m.file_type().is_symlink(){return Err(pr::fail());}#[cfg(windows)]{use std::os::windows::fs::MetadataExt;if m.file_attributes()&0x400!=0{return Err(pr::fail());}}}
    let lock_path=data.join("server.lock");if lock_path.exists(){let m=std::fs::symlink_metadata(&lock_path).map_err(|_|pr::fail())?;if !m.is_file()||m.file_type().is_symlink(){return Err(pr::fail());}#[cfg(windows)]{use std::os::windows::fs::MetadataExt;if m.file_attributes()&0x400!=0{return Err(pr::fail());}}}
    let f=OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path).map_err(|_|pr::fail())?;
    f.try_lock().map_err(|_|pr::fail())?;Ok(f)
}
async fn session(c:&mut PgConnection,scope:&Value)->ApiResult<Value> {
    let r=sqlx::query("SELECT pg_backend_pid() AS pid,current_database() AS database,current_user AS username,(SELECT backend_start::text FROM pg_stat_activity WHERE pid=pg_backend_pid()) AS birth,EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND pid=pg_backend_pid() AND granted AND classid=0 AND objid::bigint=$1 AND objsubid=1) AS held")
        .bind(LEASE).fetch_one(&mut *c).await?;
    if !r.try_get::<bool,_>("held")?||r.try_get::<String,_>("database")?!=pr::text(&scope["storage"]["database"])?||r.try_get::<String,_>("username")?!=pr::text(&scope["storage"]["user"])?{return Err(pr::fail());}
    Ok(serde_json::json!({"pid":r.try_get::<i32,_>("pid")?,"birth":r.try_get::<String,_>("birth")?}))
}
async fn clients(c:&mut PgConnection,allowed:Option<i32>)->ApiResult<()> {
    let n:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND backend_type='client backend' AND pid<>pg_backend_pid() AND ($1::int IS NULL OR pid<>$1)").bind(allowed).fetch_one(&mut *c).await?;
    if n!=0{return Err(pr::fail());}Ok(())
}
async fn schema(c:&mut PgConnection)->ApiResult<()> {
    let applied:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communityhero.schema_migrations WHERE version=$1)").bind(crate::db_guards::REQUIRED_SCHEMA).fetch_one(&mut *c).await?;
    if !applied{return Err(pr::fail());}Ok(())
}
async fn bounded_read(c:&mut PgConnection)->ApiResult<Value> {
    // All native entity tables are locked before byte/count admission. No row
    // payload crosses the driver boundary until the complete budget is checked.
    sqlx::query("SELECT id FROM communityhero.workspaces WHERE id=$1 FOR UPDATE").bind(WORKSPACE).fetch_one(&mut *c).await?;
    let lock=format!("LOCK TABLE {} IN SHARE ROW EXCLUSIVE MODE",TABLES.iter().map(|t|format!("communityhero.{t}")).collect::<Vec<_>>().join(","));
    sqlx::query(sqlx::AssertSqlSafe(lock.as_str())).execute(&mut *c).await?;
    let metadata:i64=sqlx::query_scalar("SELECT octet_length(metadata::text)::bigint FROM communityhero.workspaces WHERE id=$1").bind(WORKSPACE).fetch_one(&mut *c).await?;
    if metadata<0{return Err(pr::fail());}let mut bytes=metadata as u64+4096;
    for table in TABLES {
        let q=format!("SELECT count(*)::bigint AS n,COALESCE(sum(octet_length(payload::text)::bigint+2),0)::bigint AS bytes FROM communityhero.{table} WHERE workspace_id=$1");
        let r=sqlx::query(sqlx::AssertSqlSafe(q.as_str())).bind(WORKSPACE).fetch_one(&mut *c).await?;let n=r.try_get::<i64,_>("n")?;let b=r.try_get::<i64,_>("bytes")?;
        if n<0||n>i32::MAX as i64||b<0{return Err(pr::fail());}bytes=bytes.checked_add(b as u64).ok_or_else(pr::fail)?;if bytes>pr::WORKSPACE_BYTES{return Err(pr::fail());}
    }
    let q="SELECT count(*)::bigint AS n,COALESCE(sum(octet_length(payload::text)::bigint),0)::bigint AS bytes FROM communityhero.operations WHERE workspace_id=$1 AND payload ? 'dispatchPermit' AND (payload#>>'{dispatchPermit,phase}') IS DISTINCT FROM 'transport_settled'";
    let r=sqlx::query(q).bind(WORKSPACE).fetch_one(&mut *c).await?;
    if r.try_get::<i64,_>("n")?>pr::COHORT_ROWS as i64||r.try_get::<i64,_>("bytes")?>pr::COHORT_BYTES as i64{return Err(pr::fail());}
    let d=read_postgres(c).await?;if d.to_string().len() as u64>pr::WORKSPACE_BYTES{return Err(pr::fail());}Ok(d)
}
async fn persist(c:&mut PgConnection,before:&Value,change:&AuthorizedTransition)->ApiResult<()> {
    change.validate(before)?;let after=change.workspace();validate(after)?;immutable_versions(before,after)?;
    if before["account"]!=after["account"]||before["connectorBinding"]!=after["connectorBinding"]{return Err(pr::fail());}
    for table in TABLES {
        let old=rows(before,table)?;let new=rows(after,table)?;
        if new.len()<old.len()||old.iter().zip(new).any(|(a,b)|a["id"]!=b["id"]){return Err(pr::fail());}
        if table!="operations"&&table!="audit" {if old!=new{return Err(pr::fail());}continue;}
        let columns=projection(table);let statement=format!("INSERT INTO communityhero.{table}(workspace_id,id,ordinal,payload{}) VALUES($1,$2,$3,$4::jsonb{}) ON CONFLICT(workspace_id,id) DO UPDATE SET payload=EXCLUDED.payload{}",columns.iter().map(|(col,_)|format!(",{col}")).collect::<String>(),(0..columns.len()).map(|n|format!(",${}",n+5)).collect::<String>(),columns.iter().map(|(col,_)|format!(",{col}=EXCLUDED.{col}")).collect::<String>());
        for (n,value) in new.iter().enumerate(){if old.get(n)==Some(value){continue;}let mut q=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(text(value,"id")?).bind(n as i32).bind(value.to_string());for (_,key) in columns{q=q.bind(value[*key].as_str());}q.execute(&mut *c).await?;}
    }
    let meta=metadata(after);if meta!=metadata(before){let result=sqlx::query("UPDATE communityhero.workspaces SET metadata=$1::jsonb WHERE id=$2").bind(meta.to_string()).bind(WORKSPACE).execute(&mut *c).await?;if result.rows_affected()!=1{return Err(pr::fail());}}
    Ok(())
}
async fn offline(url:&str,input:&Value,import:Option<&VerifiedImport>,reconcile:bool,budget:&Budget)->ApiResult<Value> {
    let scope=&input["scope"];let options=endpoint(url,scope)?;let _lock=server_lock(scope)?;
    let mut c=tokio::time::timeout(budget.remaining(5000)?,PgConnection::connect_with(&options)).await.map_err(|_|pr::fail())??;
    let outcome:ApiResult<Value>=tokio::time::timeout(budget.remaining(60000)?,async {
        let held:bool=tokio::time::timeout(budget.remaining(5000)?,sqlx::query_scalar("SELECT pg_try_advisory_lock($1)").bind(LEASE).fetch_one(&mut c)).await.map_err(|_|pr::fail())??;if !held{return Err(pr::fail());}
        let identity=session(&mut c,scope).await?;schema(&mut c).await?;clients(&mut c,None).await?;
        let duration=budget.remaining(60000)?;
        tokio::time::timeout(duration,async {
            let mut tx=c.begin().await?;
            sqlx::query("SET LOCAL statement_timeout='55000ms'").execute(&mut *tx).await?;
            sqlx::query("SET LOCAL lock_timeout='5000ms'").execute(&mut *tx).await?;
            let attempt:ApiResult<Value>=async {
                if session(&mut tx,scope).await?!=identity{return Err(pr::fail());}clients(&mut tx,None).await?;
                let before=bounded_read(&mut tx).await?;
                let output=match import {
                    None=>pr::capture(&before,scope,&input["owner"],&input["package"] )?,
                    Some(verified) if reconcile=>verified.observe_reconcile(&before)?,
                    Some(verified)=>{let transition=verified.transition(&before)?;persist(&mut tx,&before,&transition).await?;transition.output()},
                };
                budget.remaining(1)?;clients(&mut tx,None).await?;if session(&mut tx,scope).await?!=identity{return Err(pr::fail());}Ok(output)
            }.await;
            match attempt {Ok(output)=>{
                tx.commit().await?;
                if let Some(verified)=import.filter(|_|!reconcile){
                    // Actual post-COMMIT native readback. A response loss or
                    // any drift withholds Q; it never reapplies this transition.
                    let mut readback=c.begin().await?;let observed:ApiResult<_>=async {
                        clients(&mut readback,None).await?;if session(&mut readback,scope).await?!=identity{return Err(pr::fail());}
                        let current=bounded_read(&mut readback).await?;let actual=verified.reconcile(&current)?;
                        if actual!=output{return Err(pr::fail());}Ok(actual)
                    }.await;
                    match observed {Ok(actual)=>{readback.commit().await?;Ok(actual)},Err(error)=>{if readback.rollback().await.is_err(){return Err(pr::fail());}Err(error)}}
                }else{Ok(output)}
            },Err(error)=>{if tx.rollback().await.is_err(){return Err(pr::fail());}Err(error)}}
        }).await.map_err(|_|pr::fail())?
    }).await.map_err(|_|pr::fail()).and_then(|v|v);
    // Close the SAME actual session; never use a pool or reacquire on failure.
    // A failed/expired close remains an error, even after acknowledged COMMIT.
    let close=tokio::time::timeout(Duration::from_millis(10000),c.close()).await;
    if !matches!(close,Ok(Ok(()))){return Err(pr::fail());}outcome
}
pub(crate) async fn capture_predecessor(url:&str,input:&Value,budget:&Budget)->ApiResult<Value>{offline(url,input,None,true,budget).await}
pub(crate) async fn import_predecessor(url:&str,import:&VerifiedImport,reconcile:bool,budget:&Budget)->ApiResult<Value>{offline(url,import.input(),Some(import),reconcile,budget).await}

impl Database {
    pub(crate) async fn initialize_verified_startup(&self,admission:&crate::runtime_lifecycle_startup::Admission,proof:Option<&VerifiedStartup>,launch:&pr::VerifiedLaunch)->ApiResult<crate::runtime_lifecycle::OwnerToken> {
        let Self::Postgres{writer,reader}=self else{return Err(pr::fail());};
        let scope=launch.scope();if proof.is_some_and(|p|p.scope()!=scope){return Err(pr::fail());}
        let url=std::env::var("COMMUNITYHERO_DATABASE_URL").map_err(|_|pr::fail())?;endpoint(&url,scope)?;
        let budget=Budget::new();
        let mut reader_connection=tokio::time::timeout(budget.remaining(5000)?,reader.acquire()).await.map_err(|_|pr::fail())??;
        let reader_pid:i32=tokio::time::timeout(budget.remaining(5000)?,sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *reader_connection)).await.map_err(|_|pr::fail())??;
        let mut connection=tokio::time::timeout(budget.remaining(5000)?,writer.acquire()).await.map_err(|_|pr::fail())??;
        let identity=tokio::time::timeout(budget.remaining(5000)?,session(&mut connection,scope)).await.map_err(|_|pr::fail())??;
        let outcome=tokio::time::timeout(budget.remaining(60000)?,async {
            let mut tx=connection.begin().await?;sqlx::query("SET LOCAL statement_timeout='55000ms'").execute(&mut *tx).await?;
            sqlx::query("SET LOCAL lock_timeout='5000ms'").execute(&mut *tx).await?;
            let mut expected_after=Value::Null;
            let attempt:ApiResult<_>=async {clients(&mut tx,Some(reader_pid)).await?;let before=bounded_read(&mut tx).await?;
                let transition=launch.transition(&before,admission,proof)?;persist(&mut tx,&before,&transition).await?;
                clients(&mut tx,Some(reader_pid)).await?;if session(&mut tx,scope).await?!=identity{return Err(pr::fail());}
                expected_after=transition.workspace().clone();
                crate::runtime_lifecycle::parse_token(&transition.output())
            }.await;
            let (outcome,completion)=super::pg_writer::settle(tx,attempt).await;
            let outcome=match outcome {Err(error)=>Err(error),Ok(token)=>{
                let mut readback=connection.begin().await?;let check:ApiResult<_>=async {
                    clients(&mut readback,Some(reader_pid)).await?;if session(&mut readback,scope).await?!=identity{return Err(pr::fail());}
                    let current=bounded_read(&mut readback).await?;if current!=expected_after{return Err(pr::fail());}Ok(token)
                }.await;
                match check {Ok(token)=>{readback.commit().await?;Ok(token)},Err(error)=>{if readback.rollback().await.is_err(){return Err(pr::fail());}Err(error)}}
            }};
            Ok::<_,crate::ApiError>((outcome,completion))
        }).await.map_err(|_|pr::fail())?;
        let outcome=outcome?;
        tokio::time::timeout(Duration::from_millis(5000),super::pg_writer::release(&mut connection,writer,outcome.1)).await.map_err(|_|pr::fail())?;
        tokio::time::timeout(Duration::from_millis(5000),reader_connection.return_to_pool()).await.map_err(|_|pr::fail())?;outcome.0
    }
}
