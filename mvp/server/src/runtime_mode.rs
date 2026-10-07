//! Cheap runtime configuration inspection. This is neither queue readiness nor
//! permission to run work; dependency readiness and engine identity remain checks.
use crate::{ApiResult, App, Database, accounts, active_binding, conflict, internal};
use axum::{Json, extract::State};
use serde_json::{Value, json};
use sqlx::Row;

async fn identity(db: &Database) -> ApiResult<Value> {
    let value = match db {
        Database::Sqlite(pool) => {
            let payload: String = sqlx::query_scalar(
                "SELECT json_object('account',json_extract(payload,'$.account'),'connectorBinding',json_extract(payload,'$.connectorBinding')) FROM workspace WHERE id=1"
            ).fetch_one(pool).await?;
            serde_json::from_str(&payload).map_err(|_| internal("Invalid runtime identity"))?
        }
        Database::Postgres { reader, .. } => {
            let row = sqlx::query(
                "SELECT execution_enabled, (jsonb_typeof(metadata->'account')='string' AND account=metadata->>'account') IS TRUE AS identity_valid, jsonb_build_object('account',metadata->'account','connectorBinding',metadata->'connectorBinding')::text AS projection FROM communityhero.workspaces WHERE id=$1"
            ).bind("local-pilot").fetch_one(reader).await?;
            if row.try_get::<bool,_>("execution_enabled")? || !row.try_get::<bool,_>("identity_valid")? {
                return Err(internal("Invalid runtime identity"));
            }
            serde_json::from_str(row.try_get::<&str,_>("projection")?).map_err(|_| internal("Invalid runtime identity"))?
        }
    };
    Ok(value)
}

fn project(data: &Value, selected: accounts::Profile, external_writes: bool, env: impl Fn(&str) -> Option<String>) -> ApiResult<Value> {
    let profile=accounts::Profile::from_workspace(data)?;
    if profile != selected { return Err(conflict("Runtime account differs from database")); }
    let binding=active_binding(data)?;
    let enabled=|key|env(key).as_deref()==Some("1");
    let background=!enabled("COMMUNITYHERO_BACKGROUND_DISABLED");
    let generation=background && !enabled("COMMUNITYHERO_BACKGROUND_GENERATION_DISABLED");
    let media=crate::media_queue::background_enabled_with(&env);
    let cutoff=crate::media_queue::cutoff_unix_with(env("COMMUNITYHERO_MEDIA_COMMENT_CUTOFF_UTC").as_deref())?;
    Ok(json!({"version":1,"kind":"runtime-configuration","account":profile.key(),
        "displayAccount":profile.display(),"connectorBinding":binding.to_json(),
        "externalWrites":external_writes,"backgroundEnabled":background,
        "workerEnabled":generation && !enabled("COMMUNITYHERO_COMMENT_PREPARATION_DISABLED"),
        "mediaWorkerEnabled":media,"mediaPreparationEnabled":media,"mediaCommentCutoffUnix":cutoff,
        "mediaOpenCommentsOnly":enabled("COMMUNITYHERO_MEDIA_OPEN_COMMENTS_ONLY") || cutoff.is_some(),
        "transcriptRequiredForVideo":true,"visualContextRequiredForVideo":false}))
}

pub(crate) async fn get(State(app):State<App>) -> ApiResult<Json<Value>> {
    let data=identity(&app.db).await?;
    project(&data,app.account,app.external_writes,|key|std::env::var_os(key).map(|v|v.to_string_lossy().into_owned())).map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data(profile:accounts::Profile)->Value {json!({"account":profile.display(),"connectorBinding":profile.binding()})}
    #[test]
    fn runtime_mode_flags_match_worker_gates_without_readiness_claims() {
        for mask in 0..16 {
            let keys=["COMMUNITYHERO_BACKGROUND_DISABLED","COMMUNITYHERO_BACKGROUND_GENERATION_DISABLED","COMMUNITYHERO_COMMENT_PREPARATION_DISABLED","COMMUNITYHERO_MEDIA_OPEN_COMMENTS_ONLY"];
            let v=project(&data(accounts::Profile::LikeAvto),accounts::Profile::LikeAvto,false,|key|keys.iter().position(|k|*k==key).is_some_and(|i|mask&(1<<i)!=0).then(||"1".into())).unwrap();
            assert_eq!(v["workerEnabled"],mask&7==0);
            assert_eq!(v["mediaWorkerEnabled"],mask&3==0);
            assert_eq!(v["mediaOpenCommentsOnly"],mask&8!=0);
            assert_eq!(v["transcriptRequiredForVideo"],true);
            assert_eq!(v["visualContextRequiredForVideo"],false);
            assert!(v.get("ready").is_none());assert!(v.get("candidateCount").is_none());
        }
    }
    #[test]
    fn media_override_is_independent_and_cutoff_is_reported_exactly() {
        let profile=accounts::Profile::LikeAvto;
        let value=project(&data(profile),profile,false,|key|match key {
            "COMMUNITYHERO_BACKGROUND_GENERATION_DISABLED"|"COMMUNITYHERO_MEDIA_PREPARATION_ENABLED"=>Some("1".into()),
            "COMMUNITYHERO_MEDIA_COMMENT_CUTOFF_UTC"=>Some("2026-09-27T16:00:00Z".into()),
            _=>None,
        }).unwrap();
        assert_eq!(value["workerEnabled"],false);
        assert_eq!(value["mediaWorkerEnabled"],true);
        assert_eq!(value["mediaPreparationEnabled"],true);
        assert_eq!(value["mediaCommentCutoffUnix"],1790524800_i64);
        assert_eq!(value["mediaOpenCommentsOnly"],true);
        assert_eq!(value["externalWrites"],false);
        for disabled in ["COMMUNITYHERO_BACKGROUND_DISABLED","COMMUNITYHERO_MEDIA_PREPARATION_ENABLED"] {
            let result=project(&data(profile),profile,false,|key|if key==disabled {
                Some(if key=="COMMUNITYHERO_BACKGROUND_DISABLED"{"1"}else{"0"}.into())
            }else{None}).unwrap();
            assert_eq!(result["mediaWorkerEnabled"],false);
        }
        assert!(project(&data(profile),profile,false,|key|
            (key=="COMMUNITYHERO_MEDIA_COMMENT_CUTOFF_UTC").then(||"invalid".into())).is_err());
    }
    #[test]
    fn runtime_mode_rejects_foreign_database_and_binding() {
        let mut v=data(accounts::Profile::LikeAvto);
        assert!(project(&v,accounts::Profile::BawRussia,false,|_|None).is_err());
        v["connectorBinding"]=accounts::Profile::BawRussia.binding();
        assert!(project(&v,accounts::Profile::LikeAvto,false,|_|None).is_err());
    }
    #[tokio::test]
    async fn runtime_mode_reads_only_identity_and_does_not_repair_or_change_history() {
        let dir=tempfile::tempdir().unwrap();let pool=crate::open_db(&dir.path().join("workspace.sqlite")).await.unwrap();
        let mut v=data(accounts::Profile::BawRussia);
        // Deliberately invalid domain collections cannot affect mode inspection.
        v["jobs"]=json!({"unrelated":"x".repeat(1_000_000)});v["items"]=json!("unrelated");
        let before=v.to_string();
        sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(&before).execute(&pool).await.unwrap();
        let db=Database::Sqlite(pool.clone());let scoped=identity(&db).await.unwrap();
        assert_eq!(scoped,data(accounts::Profile::BawRussia));
        assert_eq!(project(&scoped,accounts::Profile::BawRussia,false,|_|None).unwrap()["account"],"baw-russia");
        let after:String=sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1").fetch_one(&pool).await.unwrap();
        assert_eq!(before,after);db.close().await;assert!(identity(&db).await.is_err());
    }
}
