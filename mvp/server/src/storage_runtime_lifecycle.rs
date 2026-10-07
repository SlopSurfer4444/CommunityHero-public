//! Small native lifecycle metadata writes using the SAME workspace lock as
//! scoped source/media/preparation claims; completion writes are not vetoed.
use super::*;

// App is the owner of the process-local gate. Database stays gate-neutral for
// explicit startup, which already holds its single Standard permit.
impl crate::App {
    pub(crate) async fn change_runtime_lifecycle<T>(&self,
        f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T> {
        self.change_runtime_lifecycle_gated(false,f).await
    }
    pub(crate) async fn change_runtime_lifecycle_with_ledger<T>(&self,
        f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T> {
        self.change_runtime_lifecycle_gated(true,f).await
    }
    async fn change_runtime_lifecycle_gated<T>(&self,complete_ledger:bool,
        f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T> {
        let _total=crate::performance::Span::new(if complete_ledger {
            "runtime.lifecycle.ledger.total"
        }else{"runtime.lifecycle.metadata.total"});
        let waiting=crate::performance::Span::new("runtime.lifecycle.writer.wait");
        // Existing Interactive FIFO/burst/debt rules apply. Waiting here does
        // not acquire the writer pool, preempt its holder or mint authority.
        let _permit=self.gate.acquire(crate::writer_gate::Class::Interactive).await;
        drop(waiting);
        let _held=crate::performance::Span::new("runtime.lifecycle.writer.held");
        let observed=|data:&mut Value| {
            crate::runtime_lifecycle::current_owner(data,&self.lifecycle_owner)?;
            let before=data["runtimeLifecycle"].clone();
            let result=f(data)?;
            crate::runtime_lifecycle::current_owner(data,&self.lifecycle_owner)?;
            Ok((result,before!=data["runtimeLifecycle"]))
        };
        let (result,changed)=if complete_ledger {
            self.db.change_runtime_lifecycle_with_ledger(observed).await?
        }else{self.db.change_runtime_lifecycle(observed).await?};
        if changed {self.bootstrap_cache.invalidate();let _=self.events.send(());}
        Ok(result)
    }
}
impl Database {
    pub(crate) async fn read_runtime_lifecycle(&self)->ApiResult<Value> {
        let metadata=self.read_metadata().await?;
        crate::runtime_lifecycle::status(&metadata)
    }
    /// For begin-drain/status transition only; cannot inspect ledger arrays or
    /// change account, settings, existing jobs or provider credentials.
    pub(crate) async fn change_runtime_lifecycle<T>(&self,
        f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T> {
        match self {
            Self::Sqlite(_)=>self.change(|workspace| {
                let mut metadata=metadata(workspace);
                let before=metadata.clone();let result=f(&mut metadata)?;
                validate_metadata_delta(&before,&metadata)?;
                workspace["runtimeLifecycle"]=metadata["runtimeLifecycle"].clone();Ok(result)
            }).await,
            Self::Postgres{writer,..}=>{
                let mut connection=writer.acquire().await?;
                let mut tx=sqlx::Connection::begin(&mut *connection).await?;
                let mut value=Value::Null;
                let mut before=Value::Null;
                let mut retained_record=None;
                let outcome:ApiResult<_>=async {
                retained_record=Some(sqlx::query("SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
                    .bind(WORKSPACE).fetch_one(&mut *tx).await?);
                let record=retained_record.as_ref().expect("metadata row retained");
                if record.try_get::<bool,_>("execution_enabled")? {return Err(internal("PostgreSQL pilot execution must remain disabled"));}
                value=parse(record.try_get::<&str,_>("metadata")?)?;
                if record.try_get::<Option<String>,_>("account")?.as_deref()!=value["account"].as_str() {
                    return Err(internal("Workspace identity mismatch"));
                }
                before=value.clone();let result=f(&mut value)?;
                validate_metadata_delta(&before,&value)?;
                if before!=value {
                    let changed=sqlx::query("UPDATE communityhero.workspaces SET metadata=jsonb_set(metadata,'{runtimeLifecycle}',$2::jsonb,true) WHERE id=$1")
                        .bind(WORKSPACE).bind(value["runtimeLifecycle"].to_string()).execute(&mut *tx).await?;
                    if changed.rows_affected()!=1 {return Err(internal("Lifecycle workspace disappeared"));}
                }
                Ok(result)
                }.await;
                let (outcome,completion)=super::pg_writer::settle(tx,outcome).await;
                super::pg_writer::release(&mut connection,writer,completion).await;
                drop(retained_record);
                outcome
            }
        }
    }
    /// Bootstrap, mark-drained and stop/successor acceptance need the COMPLETE
    /// current durable ledger inside the ordinary leased writer transaction.
    /// A cheap metadata-only projection must never produce a transfer digest.
    pub(crate) async fn change_runtime_lifecycle_with_ledger<T>(&self,
        f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T> {
        self.change(|workspace|{
            let before=workspace.clone();let result=f(workspace)?;
            validate_metadata_delta(&before,workspace)?;Ok(result)
        }).await
    }
}
fn validate_metadata_delta(before:&Value,after:&Value)->ApiResult<()> {
    let mut left=before.clone();let mut right=after.clone();
    let left=left.as_object_mut().ok_or_else(||internal("Invalid lifecycle workspace"))?;
    let right=right.as_object_mut().ok_or_else(||internal("Invalid lifecycle workspace"))?;
    left.remove("runtimeLifecycle");right.remove("runtimeLifecycle");
    if left!=right {return Err(internal("Lifecycle transition changed protected workspace"));}
    crate::runtime_lifecycle::status(after)?;Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_lifecycle_writer_fences_stale_claim_and_preserves_sqlite_history() {
        let pool=SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE workspace(id INTEGER PRIMARY KEY,payload TEXT NOT NULL)").execute(&pool).await.unwrap();
        // Seed the entire isolated database before the complete-ledger writer;
        // metadata-only callbacks must not invent absent history collections.
        let mut d=crate::empty();normalize(&mut d);
        crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
        d["operations"]=serde_json::json!([{"id":"uncertain","status":"unknown"}]);
        sqlx::query("INSERT INTO workspace(id,payload) VALUES(1,?)").bind(d.to_string()).execute(&pool).await.unwrap();
        let db=Database::Sqlite(pool);let owner=crate::runtime_lifecycle::OwnerToken{account:"BAW Russia".into(),runtime_id:"offline-owner".into(),release_sha256:"a".repeat(64),epoch:1};
        db.change_runtime_lifecycle_with_ledger(|d|{let digest=crate::runtime_lifecycle::ledger_digest(d)?;crate::runtime_lifecycle::initialize(d,owner.clone(),&"b".repeat(64),&digest)}).await.unwrap();
        let old=db.read().await.unwrap()["operations"].clone();
        db.change_runtime_lifecycle_with_ledger(|d|crate::runtime_lifecycle::begin_drain(d,&owner,&"c".repeat(64),"release-one",false)).await.unwrap();
        assert!(db.change(|d|{crate::runtime_lifecycle::require_admission(d,&owner,crate::runtime_lifecycle::AdmissionClass::Media)?;d["jobs"]=serde_json::json!([{"id":"new","status":"running"}]);Ok(())}).await.is_err());
        let current=db.read().await.unwrap();assert_eq!(current["operations"],old);assert!(current["jobs"].as_array().unwrap().is_empty());
        assert_eq!(db.read_runtime_lifecycle().await.unwrap()["phase"],"draining");
    }
    #[test] fn lifecycle_only_write_rejects_unrelated_delta() {
        let before=serde_json::json!({"account":"BAW Russia","jobs":[]});let mut after=before.clone();after["jobs"]=serde_json::json!([{"id":"forged"}]);assert!(validate_metadata_delta(&before,&after).is_err());
    }
    #[tokio::test]
    #[ignore="requires pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
    async fn postgres_runtime_lifecycle_locked_epoch_and_complete_transfer() {
        let db=crate::storage::writer_v51_fixture_db().await;
        db.change(|d|crate::accounts::initialize(d,crate::accounts::Profile::LikeAvto)).await.unwrap();
        let initial=db.read().await.unwrap();
        let owner=crate::runtime_lifecycle::OwnerToken{account:initial["account"].as_str().unwrap().into(),runtime_id:"pg-native-owner".into(),release_sha256:"a".repeat(64),epoch:1};
        db.change_runtime_lifecycle_with_ledger(|d|{let digest=crate::runtime_lifecycle::ledger_digest(d)?;crate::runtime_lifecycle::initialize(d,owner.clone(),&"b".repeat(64),&digest)}).await.unwrap();
        let before=db.read().await.unwrap();
        let drain=db.change_runtime_lifecycle_with_ledger(|d|crate::runtime_lifecycle::begin_drain(d,&owner,&"c".repeat(64),"pg-release",false)).await.unwrap();
        assert!(db.change(|d|{crate::runtime_lifecycle::require_admission(d,&owner,crate::runtime_lifecycle::AdmissionClass::Preparation)?;d["settings"]["lateAdmission"]=serde_json::json!(true);Ok(())}).await.is_err());
        let native=crate::runtime_lifecycle::SettledNative{owner:drain.clone(),application_tasks:0,provider_queued:0,provider_dispatched:0,provider_contained:true,credential_writers:0,unresolved_effects:0};
        let transfer=db.change_runtime_lifecycle_with_ledger(|d|crate::runtime_lifecycle::mark_drained(d,&drain,&native)).await.unwrap();
        db.change_runtime_lifecycle_with_ledger(|d|crate::runtime_lifecycle::commit_stop_checkpoint(d,&drain,&transfer)).await.unwrap();
        let next=db.change_runtime_lifecycle_with_ledger(|d|crate::runtime_lifecycle::accept_successor(d,&drain,&transfer,"pg-successor",&"c".repeat(64),&"e".repeat(64),false)).await.unwrap();
        let after=db.read().await.unwrap();
        assert_eq!(crate::runtime_lifecycle::ledger_digest(&before).unwrap(),crate::runtime_lifecycle::ledger_digest(&after).unwrap());
        assert_eq!(crate::runtime_lifecycle::admission_token(&after,crate::runtime_lifecycle::AdmissionClass::Media).unwrap(),next);
        let retired=crate::runtime_lifecycle::RuntimeIdentity{account:owner.account,runtime_id:owner.runtime_id,release_sha256:owner.release_sha256};
        assert!(db.change(|d|{crate::runtime_lifecycle::current_owner(d,&retired)?;d["settings"]["retiredMutation"]=serde_json::json!(true);Ok(())}).await.is_err());
        assert_eq!(db.read().await.unwrap(),after);db.close().await;
    }
}

#[cfg(test)]
#[path = "storage_runtime_lifecycle_gate_tests.rs"]
mod gate_tests;
