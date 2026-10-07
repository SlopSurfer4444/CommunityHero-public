//! Narrow read-only persisted applicability pins for restart proof warming.
//! Original CAS/output verification happens outside the workspace writer.
use super::*;
use serde_json::json;

const MANUAL_PURPOSE:&str="manual_video_frames";
const MANUAL_MAX_ROWS:usize=1024;
const MANUAL_MAX_BYTES:usize=64*1024*1024;
fn manual_identity(metadata:Value)->ApiResult<Value>{
    if !metadata.is_object()||!metadata["account"].is_string(){return Err(internal("Manual frame workspace identity invalid"));}
    let mut view=json!({"account":metadata["account"],"posts":[],"jobs":[]});
    for key in ["connectorBinding","runtimeLifecycle"] {if let Some(value)=metadata.get(key){view[key]=value.clone();}}
    Ok(view)
}
fn manual_roots(jobs:&[Value])->ApiResult<(Vec<Value>,Vec<String>)>{
    if jobs.len()>MANUAL_MAX_ROWS{return Err(internal("Manual frame reader row budget exceeded"));}
    let mut roots=Vec::new();let mut posts=HashSet::new();let mut seen=HashSet::new();let mut bytes=0;
    for job in jobs{
        let id=text(job,"id")?;
        if !seen.insert(id)||job["kind"]!="media"||job["purpose"]!=MANUAL_PURPOSE{return Err(internal("Manual frame reader job identity invalid"));}
        let post=text(&job["manualFrameRequest"]["member"],"postId")?;
        if post.is_empty(){return Err(internal("Manual frame reader source missing"));}posts.insert(post.to_owned());
        roots.push(job.clone());roots.push(json!({"prepareRunId":id}));bytes+=job.to_string().len();
    }
    if bytes>MANUAL_MAX_BYTES{return Err(internal("Manual frame reader byte budget exceeded"));}
    let mut posts=posts.into_iter().collect::<Vec<_>>();posts.sort();Ok((roots,posts))
}
fn manual_budget(view:&Value)->ApiResult<()>{
    for table in ["jobs","posts"] {let mut seen=HashSet::new();for row in crate::list(view,table){if !seen.insert(text(row,"id")?){return Err(internal("Duplicate manual frame dependency identity"));}}}
    if crate::list(view,"jobs").len()+crate::list(view,"posts").len()>MANUAL_MAX_ROWS||view.to_string().len()>MANUAL_MAX_BYTES{return Err(internal("Manual frame dependency context budget exceeded"));}
    Ok(())
}

impl Database {
    /// Exact current manual-frame warming input. No CAS IO, model, or mutation.
    /// Limits fail the whole read explicitly; incomplete history is not returned.
    pub(crate) async fn read_manual_frame_context(&self)->ApiResult<Value>{
        match self{
            Self::Sqlite(pool)=>{
                let mut tx=pool.begin().await?;
                let raw:String=sqlx::query_scalar("SELECT json_set('{}','$.account',json_extract(payload,'$.account')) FROM workspace WHERE id=1").fetch_one(&mut *tx).await?;
                let mut metadata=parse(&raw)?;
                for key in ["connectorBinding","runtimeLifecycle"]{
                    let path=format!("$.{key}");
                    let record=sqlx::query("SELECT json_type(payload,?1) AS kind, json_quote(json_extract(payload,?1)) AS value FROM workspace WHERE id=1").bind(path).fetch_one(&mut *tx).await?;
                    if record.try_get::<Option<String>,_>("kind")?.is_some(){metadata[key]=parse(record.try_get::<&str,_>("value")?)?;}
                }
                let mut view=manual_identity(metadata)?;
                let records=sqlx::query_scalar::<_,String>("SELECT j.value FROM workspace w,json_each(w.payload,'$.jobs') j WHERE w.id=1 AND json_extract(j.value,'$.purpose')=?1 ORDER BY CAST(j.key AS INTEGER) LIMIT 1025").bind(MANUAL_PURPOSE).fetch_all(&mut *tx).await?;
                let jobs=records.into_iter().map(|raw|parse(&raw)).collect::<ApiResult<Vec<_>>>()?;let(roots,posts)=manual_roots(&jobs)?;
                if !jobs.is_empty(){
                    view["jobs"]=json!(super::reads::dispatch::dependency_jobs_sqlite_bounded(&mut tx,&roots,MANUAL_MAX_ROWS,MANUAL_MAX_BYTES).await?);
                    let records=sqlx::query_scalar::<_,String>("SELECT p.value FROM workspace w,json_each(w.payload,'$.posts') p WHERE w.id=1 AND json_extract(p.value,'$.id') IN (SELECT value FROM json_each(?1)) ORDER BY CAST(p.key AS INTEGER)").bind(json!(posts).to_string()).fetch_all(&mut *tx).await?;
                    view["posts"]=json!(records.into_iter().map(|raw|parse(&raw)).collect::<ApiResult<Vec<_>>>()?);
                }
                manual_budget(&view)?;tx.commit().await?;Ok(view)
            },
            Self::Postgres{reader,..}=>{
                let mut tx=reader.begin().await?;
                sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                let record=sqlx::query("SELECT account,execution_enabled,(SELECT jsonb_object_agg(e.key,e.value) FROM jsonb_each(metadata) e WHERE e.key IN ('account','connectorBinding','runtimeLifecycle'))::text AS metadata FROM communityhero.workspaces WHERE id=$1").bind(WORKSPACE).fetch_one(&mut *tx).await?;
                let metadata=parse(record.try_get::<&str,_>("metadata")?)?;
                if record.try_get::<bool,_>("execution_enabled")?||record.try_get::<Option<String>,_>("account")?.as_deref()!=metadata["account"].as_str(){return Err(internal("Manual frame workspace identity mismatch"));}
                let mut view=manual_identity(metadata)?;
                let ids=sqlx::query_scalar::<_,String>("SELECT id FROM communityhero.jobs WHERE workspace_id=$1 AND payload->>'purpose'=$2 ORDER BY ordinal LIMIT 1025").bind(WORKSPACE).bind(MANUAL_PURPOSE).fetch_all(&mut *tx).await?;
                if ids.len()>MANUAL_MAX_ROWS{return Err(internal("Manual frame reader row budget exceeded"));}
                if !ids.is_empty(){
                    let selectors=ids.iter().map(|id|json!({"prepareRunId":id})).collect::<Vec<_>>();
                    let bodies=super::reads::dispatch::dependency_jobs_pg_bounded(&mut tx,&selectors,MANUAL_MAX_ROWS,MANUAL_MAX_BYTES).await?;
                    let manual=bodies.iter().filter(|job|job["purpose"]==MANUAL_PURPOSE).cloned().collect::<Vec<_>>();let(_,posts)=manual_roots(&manual)?;
                    view["jobs"]=json!(bodies);
                    let records=sqlx::query("SELECT id,ordinal,payload::text FROM communityhero.posts WHERE workspace_id=$1 AND (id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[])) ORDER BY ordinal").bind(WORKSPACE).bind(posts).fetch_all(&mut *tx).await?;
                    let mut data=Vec::new();let mut seen=HashSet::new();let mut order=None;
                    for row in records {let post=parse(row.try_get::<&str,_>("payload")?)?;let ordinal=row.try_get::<i32,_>("ordinal")?;
                        if row.try_get::<&str,_>("id")?!=text(&post,"id")?||!seen.insert(text(&post,"id")?.to_owned())||ordinal<0||order.is_some_and(|old|old>=ordinal){return Err(internal("Manual frame source identity/order mismatch"));}
                        order=Some(ordinal);data.push(post);
                    }view["posts"]=json!(data);
                }
                manual_budget(&view)?;tx.commit().await?;Ok(view)
            }
        }
    }
    pub(crate) async fn read_media_analysis_proofs(&self)->ApiResult<Vec<Value>> {
        let records:Vec<String>=match self {
            Self::Sqlite(pool)=>sqlx::query_scalar("SELECT json_extract(j.value,'$.result.proof') FROM workspace w,json_each(w.payload,'$.jobs') j WHERE w.id=1 AND json_extract(j.value,'$.kind')='media_analysis_applicability' AND json_extract(j.value,'$.status')='completed' AND json_extract(j.value,'$.account')=json_extract(w.payload,'$.account') AND json_extract(j.value,'$.result.proof.companyId')=json_extract(w.payload,'$.account') AND json_extract(j.value,'$.result.proof.account')=json_extract(w.payload,'$.account') AND json_type(j.value,'$.result.proof')='object'")
                .fetch_all(pool).await?,
            Self::Postgres{reader,..}=>sqlx::query_scalar("SELECT (j.payload#>'{result,proof}')::text FROM communityhero.jobs j JOIN communityhero.workspaces w ON w.id=j.workspace_id WHERE j.workspace_id=$1 AND j.kind='media_analysis_applicability' AND j.status='completed' AND j.payload->>'kind'=j.kind AND j.payload->>'status'=j.status AND j.payload->>'account'=w.account AND j.payload#>>'{result,proof,companyId}'=w.account AND j.payload#>>'{result,proof,account}'=w.account AND jsonb_typeof(j.payload#>'{result,proof}')='object'")
                .bind(WORKSPACE).fetch_all(reader).await?,
        };
        records.into_iter().map(|record|parse(&record)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    async fn raw_manual_fixture(d:&Value)->Database{
        let pool=sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE workspace(id INTEGER PRIMARY KEY,payload TEXT NOT NULL)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO workspace(id,payload) VALUES(1,?)").bind(d.to_string()).execute(&pool).await.unwrap();Database::Sqlite(pool)
    }
    #[tokio::test]
    async fn manual_warm_reader_empty_is_narrow_and_identity_preserves_absence(){
        let d=json!({"account":"BAW Russia","jobs":[],"posts":[{"id":"cold","text":"irrelevant".repeat(1024)}],"settings":{"private":"excluded"},"runtimeLifecycle":{"owner":{"epoch":7}}});
        let db=raw_manual_fixture(&d).await;let view=db.read_manual_frame_context().await.unwrap();
        assert_eq!(view,json!({"account":"BAW Russia","jobs":[],"posts":[],"runtimeLifecycle":d["runtimeLifecycle"]}));
        assert_eq!(db.read().await.unwrap()["settings"],d["settings"]);db.close().await;
    }
    #[tokio::test]
    async fn manual_warm_reader_retains_recursive_exact_source_and_prepare_bodies_and_rejects_ambiguity(){
        // This is structural reader coverage, not invented model/frame proof.
        let d=json!({"account":"BAW Russia","connectorBinding":null,"jobs":[
            {"id":"manual","kind":"media","purpose":"manual_video_frames","status":"held","sourceJobId":"source","manualFrameRequest":{"member":{"postId":"selected"},"request":{"prepareJobId":"prepare"}}},
            {"id":"source","kind":"manual_frame_source","status":"running","parentManualFrameRequestId":"manual","sourceAssetPin":{"postId":"selected"}},
            {"id":"prepare","kind":"assistant","status":"running","prepareBundle":{"id":"family","request":{"manualFrameRequestIds":["manual"],"frozen":null}}},
            {"id":"duplicate","kind":"assistant","status":"completed","prepareBundle":{"id":"family","request":{"frozen":null}}},
            {"id":"cold","kind":"assistant","status":"completed","result":{"unrelated":"excluded".repeat(1024)}}],
            "posts":[{"id":"selected","attachments":[],"nullProof":null},{"id":"unrelated","text":"excluded"}],"items":[],"settings":{"private":"excluded"}});
        let db=raw_manual_fixture(&d).await;let view=db.read_manual_frame_context().await.unwrap();
        assert_eq!(view["posts"],json!([d["posts"][0]]));assert_eq!(view["connectorBinding"],Value::Null);
        for id in ["manual","source","prepare","duplicate"]{assert_eq!(crate::row(&view,"jobs",id).unwrap(),crate::row(&d,"jobs",id).unwrap());}
        assert!(crate::row(&view,"jobs","cold").is_err());assert!(view.get("settings").is_none());db.close().await;
        let mut invalid=d.clone();invalid["jobs"][0]["sourceJobId"]=json!(42);let db=raw_manual_fixture(&invalid).await;
        assert!(db.read_manual_frame_context().await.unwrap_err().1.contains("ambiguous"));db.close().await;
        let mut over=d;over["jobs"][0]["manualFrameRequest"]["member"]["postId"]=json!("");let db=raw_manual_fixture(&over).await;
        assert!(db.read_manual_frame_context().await.is_err());db.close().await;
    }
    #[tokio::test]
    async fn sqlite_restart_reader_only_returns_completed_company_pins() {
        let pool=sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE workspace(id INTEGER PRIMARY KEY,payload TEXT NOT NULL)").execute(&pool).await.unwrap();
        let pin=json!({"schemaVersion":1,"account":"LikeAvto","companyId":"LikeAvto","resultSha256":"a".repeat(64)});
        let good=json!({"id":"proof","kind":"media_analysis_applicability","status":"completed","account":"LikeAvto","result":{"proof":pin}});
        let mut foreign=good.clone();foreign["account"]=json!("BAW Russia");
        let mut embedded_foreign=good.clone();embedded_foreign["result"]["proof"]["companyId"]=json!("BAW Russia");
        let mut running=good.clone();running["status"]=json!("running");
        let mut unrelated=good.clone();unrelated["kind"]=json!("context_sync");
        let payload=json!({"account":"LikeAvto","jobs":[good,foreign,embedded_foreign,running,unrelated],"branches":[{"text":"unrelated large evidence"}]}).to_string();
        sqlx::query("INSERT INTO workspace(id,payload) VALUES(1,?)").bind(payload).execute(&pool).await.unwrap();
        let db=Database::Sqlite(pool);
        assert_eq!(db.read_media_analysis_proofs().await.unwrap(),vec![pin]);
    }

    #[tokio::test]
    #[ignore="ROOT-owned fresh isolated PostgreSQL fixture"]
    async fn postgres_restart_proof_reader_company_and_relational_parity() {
        let db=crate::storage::writer_v51_fixture_db().await;
        let Database::Postgres{writer,..}=&db else {panic!("isolated PostgreSQL required")};
        let pin=serde_json::json!({"schemaVersion":1,"account":"LikeAvto","companyId":"LikeAvto","resultSha256":"a".repeat(64)});
        let good=serde_json::json!({"id":"proof-good","kind":"media_analysis_applicability","status":"completed","account":"LikeAvto","result":{"proof":pin}});
        let mut mismatch=good.clone();mismatch["id"]=serde_json::json!("proof-mismatch");mismatch["kind"]=serde_json::json!("context_sync");
        let mut foreign=good.clone();foreign["id"]=serde_json::json!("proof-foreign");foreign["result"]["proof"]["companyId"]=serde_json::json!("BAW Russia");
        for (ordinal,job) in [good,mismatch,foreign].iter().enumerate() {
            sqlx::query("INSERT INTO communityhero.jobs(workspace_id,id,ordinal,payload,kind,status) VALUES($1,$2,$3,$4::jsonb,'media_analysis_applicability','completed')")
                .bind(WORKSPACE).bind(job["id"].as_str().unwrap()).bind(ordinal as i64).bind(job.to_string()).execute(writer).await.unwrap();
        }
        assert_eq!(db.read_media_analysis_proofs().await.unwrap(),vec![pin]);
    }
}
