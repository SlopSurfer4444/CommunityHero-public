//! Separate ORIGINAL libtest role. Production admission/executable guards are
//! unchanged. No synthetic Admission or workspace initialization is used here.
use super::*;
use serde_json::json;
use sqlx::{Connection, Row};
use std::{io::Write, str::FromStr};

pub(crate) const SELECTOR:&str="wave_execution_fixture_tests::paid_floor_producer::emit_owner_bound_paid_precrash";
pub(crate) const UNKNOWN_SELECTOR:&str="wave_execution_fixture_tests::floor_characterization::emit_native_unknown_floor_seed";
pub(crate) const CORPUS_SELECTOR:&str="wave_execution_fixture_tests::floor_characterization::emit_mixed_native_floor_corpus";
pub(crate) const WRITE_SELECTOR:&str="wave_execution_fixture_tests::floor_characterization::characterize_full_source_write_cache_and_fencing";
pub(crate) struct VerifiedFixtureProducer {
    pub(crate) admission:Admission,
    pub(crate) record:Value,
    pub(crate) admission_pin:Value,
    pub(crate) response:Value,
    pub(crate) data_root:PathBuf,
    pub(crate) cas_root:PathBuf,
    pub(crate) barrier:PathBuf,
    pub(crate) timeout_ms:u64,
}
fn pinned_json(pin:&Value,limit:u64)->ApiResult<Value> {
    if !exact(pin,&["path","sha256"]){return Err(fail());}
    let path=safe_path(text(&pin["path"])?,Path::new(""))?;
    json(&read_pinned(&path,digest(&pin["sha256"])?,limit)?)
}
fn pin_eq(a:&Value,b:&Value)->ApiResult<()> {
    if digest(&a["sha256"])?!=digest(&b["sha256"])?
        ||!same_path(&std::fs::canonicalize(safe_path(text(&a["path"])?,Path::new(""))?).map_err(|_|fail())?,
            &std::fs::canonicalize(safe_path(text(&b["path"])?,Path::new(""))?).map_err(|_|fail())?){return Err(fail());}Ok(())
}
fn directory(value:&Value)->ApiResult<PathBuf> {
    let path=safe_path(text(value)?,Path::new(""))?;
    if !path.is_absolute(){return Err(fail());}
    for ancestor in path.ancestors() {
        let m=std::fs::symlink_metadata(ancestor).map_err(|_|fail())?;
        if !m.is_dir()||m.file_type().is_symlink(){return Err(fail());}
        #[cfg(windows)] {use std::os::windows::fs::MetadataExt;if m.file_attributes()&0x400!=0{return Err(fail());}}
    }
    std::fs::canonicalize(path).map_err(|_|fail())
}
fn inventory(root:&Path)->ApiResult<Vec<String>> {
    fn walk(root:&Path,dir:&Path,files:&mut Vec<String>)->ApiResult<()> {
        for entry in std::fs::read_dir(dir).map_err(|_|fail())? {
            let path=entry.map_err(|_|fail())?.path();let m=std::fs::symlink_metadata(&path).map_err(|_|fail())?;
            if m.file_type().is_symlink(){return Err(fail());}
            #[cfg(windows)] {use std::os::windows::fs::MetadataExt;if m.file_attributes()&0x400!=0{return Err(fail());}}
            if m.is_dir(){walk(root,&path,files)?;}else if m.is_file(){
                files.push(path.strip_prefix(root).map_err(|_|fail())?.to_string_lossy().replace('\\',"/"));
                if files.len()>10000{return Err(fail());}
            }else{return Err(fail());}
        }Ok(())
    }
    let mut files=Vec::new();walk(root,root,&mut files)?;files.sort();Ok(files)
}
pub(crate) fn verify_original_test_role(role:&Value,actual:&Path)->ApiResult<(Value,Value)> {
    if !exact(role,&["core","build","checkpoint","server","testArtifact","manifest"]){return Err(fail());}
    let core=pinned_json(&role["core"],16777216)?;let build=pinned_json(&role["build"],16777216)?;
    let checkpoint=pinned_json(&role["checkpoint"],16777216)?;let manifest=pinned_json(&role["manifest"],16777216)?;
    if core["kind"]!="company-independent-immutable-core"||core["schemaVersion"]!=1
        ||build["status"]!="passed"||build["testExit"]!=0||build["buildExit"]!=0||build["sourceChangedDuringBuild"]!=false
        ||checkpoint["kind"]!="unpromoted-source-checkpoint"
        ||manifest["kind"]!="native-original-build-member-manifest"||manifest["schemaVersion"]!=1||manifest["classification"]!="ORIGINAL-NATIVE-BUILD"
        ||!exact(&manifest,&["schemaVersion","kind","classification","core","build","checkpoint","server","testArtifact","sources","assets"]){return Err(fail());}
    for field in ["core","build","checkpoint","server","testArtifact"]{pin_eq(&manifest[field],&role[field])?;}
    pin_eq(&core["releaseReceipt"],&role["build"])?;pin_eq(&core["sourceCheckpoint"],&role["checkpoint"])?;
    pin_eq(&build["sourceCheckpoint"],&role["checkpoint"])?;pin_eq(&build["artifact"],&role["server"])?;pin_eq(&build["testArtifact"],&role["testArtifact"])?;
    let server=safe_path(text(&core["binary"])?,Path::new(""))?;
    pin_eq(&json!({"path":server,"sha256":core["binarySha256"]}),&role["server"])?;
    let test=safe_path(text(&role["testArtifact"]["path"])?,Path::new(""))?;
    if !same_path(&std::fs::canonicalize(&test).map_err(|_|fail())?,&std::fs::canonicalize(actual).map_err(|_|fail())?)
        ||same_path(&server,&test)||role["server"]["sha256"]==role["testArtifact"]["sha256"]{return Err(fail());}
    read_pinned(&test,digest(&role["testArtifact"]["sha256"])?,1073741824)?;
    read_pinned(&server,digest(&role["server"]["sha256"])?,1073741824)?;
    let root=directory(&checkpoint["root"])?;let files=checkpoint["files"].as_array().filter(|v|!v.is_empty()).ok_or_else(fail)?;
    if checkpoint["fileCount"].as_u64()!=Some(files.len() as u64)||manifest["sources"]!=checkpoint["files"]{return Err(fail());}
    let mut expected=Vec::new();let mut seen=HashSet::new();
    for row in files {
        let name=text(&row["path"])?;
        if !name.starts_with("mvp/")&&!name.starts_with("project/"){return Err(fail());}
        if !seen.insert(name.to_ascii_lowercase()){return Err(fail());}
        let bytes=read_pinned(&safe_path(name,&root)?,digest(&row["sha256"])?,268435456)?;
        if row["bytes"].as_u64()!=Some(bytes.len() as u64){return Err(fail());}expected.push(name.into());
    }
    let checkpoint_path=safe_path(text(&role["checkpoint"]["path"])?,Path::new(""))?;
    if let Ok(relative)=checkpoint_path.strip_prefix(&root){expected.push(relative.to_string_lossy().replace('\\',"/"));}
    expected.sort();if inventory(&root)?!=expected{return Err(fail());}
    let sources=build["sources"].as_array().filter(|v|!v.is_empty()).ok_or_else(fail)?;
    let compiled=checkpoint["compiledSources"].as_array().filter(|v|v.len()==sources.len()).ok_or_else(fail)?;
    if build["sourceFileCount"].as_u64()!=Some(sources.len() as u64){return Err(fail());}
    for (source,name) in sources.iter().zip(compiled) {
        let name=text(name)?;let row=files.iter().find(|row|row["path"]==name).ok_or_else(fail)?;
        let canonical=safe_path(text(&source["path"])?,Path::new(""))?;
        if source["sha256"]!=row["sha256"]||!canonical.to_string_lossy().replace('\\',"/").ends_with(&format!("/{name}")){return Err(fail());}
    }
    for field in ["prebuildPins","postbuildPins"]{if pinned_json(&build[field],16777216)?!=build["sources"]{return Err(fail());}}
    for field in ["rustAggregate","rustRelease"] {let pin=&build[field];read_pinned(&safe_path(text(&pin["path"])?,Path::new(""))?,digest(&pin["sha256"])?,134217728)?;}
    for pin in build["toolPins"].as_array().filter(|v|!v.is_empty()).ok_or_else(fail)? {
        read_pinned(&safe_path(text(&pin["path"])?,Path::new(""))?,digest(&pin["sha256"])?,1073741824)?;
    }
    let runtime=directory(&core["runtimeRoot"])?;let assets=core["assets"].as_array().filter(|v|!v.is_empty()).ok_or_else(fail)?;
    if manifest["assets"]!=core["assets"]{return Err(fail());}
    let mut names=Vec::new();
    for asset in assets {
        let name=text(&asset["path"])?;
        if !["adapters/","cli/","connectors/","web/"].iter().any(|prefix|name.starts_with(prefix)){return Err(fail());}
        let source=if let Some(web)=name.strip_prefix("web/"){format!("mvp/workshop/{web}")}else{format!("mvp/{name}")};
        if files.iter().find(|row|row["path"]==source).map(|row|&row["sha256"])!=Some(&asset["sha256"]){return Err(fail());}
        read_pinned(&safe_path(name,&runtime)?,digest(&asset["sha256"])?,268435456)?;names.push(name.to_owned());
    }
    names.sort();if inventory(&runtime)?!=names||checkpoint["runtimeAssets"]!=json!(names){return Err(fail());}
    Ok((core,checkpoint))
}
fn require_record(v:&Value,now:chrono::DateTime<chrono::Utc>)->ApiResult<()> {
    if !exact(v,&["schemaVersion","kind","runId","attemptId","selector","account","company","originalFloor","fixedOwner","database","casRoot","dataRoot","preLedgerSha256","barrier","previousCessation","fixture","review","createdAt","expiresAt"])
        ||v["schemaVersion"]!=1||v["kind"]!="root-reviewed-native-fixture-paid-producer-admission"
        ||![SELECTOR,CORPUS_SELECTOR,WRITE_SELECTOR,UNKNOWN_SELECTOR].iter().any(|selector|v["selector"]==*selector)
        ||v["account"]!="baw-russia"||v["company"]!="BAW Russia"||!opaque(text(&v["runId"])?)||!opaque(text(&v["attemptId"])?)
        ||!exact(&v["fixedOwner"],&["account","runtimeId","releaseSha256","epoch"])
        ||v["fixedOwner"]["account"]!=v["company"]||!opaque(text(&v["fixedOwner"]["runtimeId"])?)
        ||v["fixedOwner"]["releaseSha256"]!=v["originalFloor"]["core"]["sha256"]
        ||v["fixedOwner"]["epoch"].as_u64().is_none_or(|n|n==0)
        ||!exact(&v["database"],&["clusterSystemId","creationReceipt","host","port","name","role","schemaVersion"])
        ||v["database"]["host"]!="127.0.0.1"||v["database"]["schemaVersion"]!=3
        ||!text(&v["database"]["name"])?.starts_with("communityhero_wave2_floor_")
        ||!exact(&v["barrier"],&["path","timeoutMs"])||v["barrier"]["timeoutMs"].as_u64().is_none_or(|n|!(1000..=120000).contains(&n))
        ||!exact(&v["fixture"],&["itemId","response"]){return Err(fail());}
    digest(&v["preLedgerSha256"])?;digest(&v["fixedOwner"]["releaseSha256"])?;
    let created=chrono::DateTime::parse_from_rfc3339(text(&v["createdAt"])?).map_err(|_|fail())?.with_timezone(&chrono::Utc);
    let expiry=chrono::DateTime::parse_from_rfc3339(text(&v["expiresAt"])?).map_err(|_|fail())?.with_timezone(&chrono::Utc);
    if created>now||expiry<=now||expiry-created>chrono::Duration::minutes(30)||expiry<=created{return Err(fail());}Ok(())
}
pub(crate) fn verified_fixture_producer_from_environment()->ApiResult<VerifiedFixtureProducer> {
    let path=safe_path(&std::env::var("COMMUNITYHERO_FLOOR_PRODUCER_ADMISSION_PATH").map_err(|_|fail())?,Path::new(""))?;
    let sha=std::env::var("COMMUNITYHERO_FLOOR_PRODUCER_ADMISSION_SHA256").map_err(|_|fail())?;
    let record=json(&read_pinned(&path,&sha,131072)?)?;require_record(&record,chrono::Utc::now())?;
    let (core,_checkpoint)=verify_original_test_role(&record["originalFloor"],&std::env::current_exe().map_err(|_|fail())?)?;
    let creation=pinned_json(&record["database"]["creationReceipt"],131072)?;
    let mut database=record["database"].clone();database.as_object_mut().ok_or_else(fail)?.remove("creationReceipt");
    if creation["kind"]!="root-owned-isolated-postgres-database-creation"||creation["account"]!="baw-russia"||creation["database"]!=database
        ||creation["dataRoot"]!=record["dataRoot"]||creation["casRoot"]!=record["casRoot"]{return Err(fail());}
    let data_root=directory(&record["dataRoot"])?;let cas_root=directory(&record["casRoot"])?;
    let effective_data=directory(&Value::String(std::env::var("COMMUNITYHERO_DATA_DIR").map_err(|_|fail())?))?;
    if !same_path(&effective_data,&data_root){return Err(fail());}
    let effective=directory(&Value::String(std::env::var("COMMUNITYHERO_MEDIA_EVIDENCE_DIR").map_err(|_|fail())?))?;
    if !same_path(&effective,&cas_root){return Err(fail());}
    let barrier=safe_path(text(&record["barrier"]["path"])?,Path::new(""))?;
    let barrier_name=format!("{}.barrier.json",text(&record["attemptId"])?);
    if !same_path(barrier.parent().ok_or_else(fail)?,&data_root)||barrier.file_name().and_then(|v|v.to_str())!=Some(barrier_name.as_str())||barrier.exists(){return Err(fail());}
    let ceased=pinned_json(&record["previousCessation"],131072)?;
    if ceased["kind"]!="owned-process-cessation"||ceased["status"]!="ceased"||ceased["cleanup"]!="verified"||ceased["activeDescendants"]!=0||ceased["backendCount"]!=0||ceased["timedOut"]!=false{return Err(fail());}
    pin_eq(&ceased["executable"],&record["originalFloor"]["server"])?;
    let completed=chrono::DateTime::parse_from_rfc3339(text(&ceased["completedAt"])?).map_err(|_|fail())?;
    let created=chrono::DateTime::parse_from_rfc3339(text(&record["createdAt"])?).map_err(|_|fail())?;
    if completed>created{return Err(fail());}
    for pin in ceased["rawEvidence"].as_array().filter(|v|!v.is_empty()).ok_or_else(fail)? {read_pinned(&safe_path(text(&pin["path"])?,Path::new(""))?,digest(&pin["sha256"])?,16777216)?;}
    let review=pinned_json(&record["review"],131072)?;
    if review["kind"]!="native-fixture-producer-source-review"||review["status"]!="passed"||review["blockingFindings"]!=0||review["originalFloor"]!=record["originalFloor"]{return Err(fail());}
    for pin in review["sourcePins"].as_array().filter(|v|!v.is_empty()).ok_or_else(fail)? {read_pinned(&safe_path(text(&pin["path"])?,Path::new(""))?,digest(&pin["sha256"])?,16777216)?;}
    let response=pinned_json(&record["fixture"]["response"],16777216)?;
    let response_class=if record["selector"]==SELECTOR{"SYNTHETIC-PAID-RESPONSE"}else{"SYNTHETIC-NATIVE-CORPUS"};
    if !exact(&response,&["classification","result"])||response["classification"]!=response_class||!response["result"].is_object(){return Err(fail());}
    let owner=&record["fixedOwner"];
    let admission=Admission{identity:RuntimeIdentity{account:text(&owner["account"])?.into(),runtime_id:text(&owner["runtimeId"])?.into(),release_sha256:digest(&owner["releaseSha256"])?.into()},
        startup:Value::Null,predecessor:None,predecessor_launch:None,targets:RwLock::new(Vec::new()),trace_release:Some(TraceReleasePins{source_checkpoint_sha256:digest(&record["originalFloor"]["checkpoint"]["sha256"])?.into(),binary_sha256:digest(&core["binarySha256"])?.into(),core_sha256:digest(&record["originalFloor"]["core"]["sha256"])?.into()})};
    Ok(VerifiedFixtureProducer{timeout_ms:record["barrier"]["timeoutMs"].as_u64().unwrap(),admission,record,admission_pin:json!({"path":path,"sha256":sha}),response:response["result"].clone(),data_root,cas_root,barrier})
}
impl VerifiedFixtureProducer {
    pub(crate) fn require_selector(&self,selector:&str)->ApiResult<()> {
        if self.record["selector"]!=selector{return Err(fail());}Ok(())
    }
    pub(crate) fn require_workspace(&self,d:&Value)->ApiResult<OwnerToken> {
        let token=runtime_lifecycle::bound_admission_token(d,self.admission.identity(),AdmissionClass::Preparation)?;
        if token!=runtime_lifecycle::parse_token(&self.record["fixedOwner"])?||runtime_lifecycle::ledger_digest(d)?!=text(&self.record["preLedgerSha256"])?{return Err(fail());}Ok(token)
    }
    pub(crate) async fn guard_existing_database(&self,url:&str)->ApiResult<()> {
        let options=sqlx::postgres::PgConnectOptions::from_str(url).map_err(|_|fail())?;let db=&self.record["database"];
        if options.get_host()!=text(&db["host"])?||u64::from(options.get_port())!=db["port"].as_u64().ok_or_else(fail)?
            ||options.get_database()!=Some(text(&db["name"])?)||options.get_username()!=text(&db["role"])?{return Err(fail());}
        let mut connection=sqlx::PgConnection::connect_with(&options).await.map_err(|_|fail())?;
        let result:ApiResult<()>=async {
            sqlx::query("SET default_transaction_read_only=on").execute(&mut connection).await.map_err(|_|fail())?;
            let row=sqlx::query("SELECT current_database() AS db,current_user AS role,inet_server_addr()::text AS host,inet_server_port() AS port,(SELECT system_identifier::text FROM pg_control_system()) AS cluster,(SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname=current_database()) AS owner")
                .fetch_one(&mut connection).await.map_err(|_|fail())?;
            if row.try_get::<String,_>("db").map_err(|_|fail())?!=text(&db["name"])?||row.try_get::<String,_>("role").map_err(|_|fail())?!=text(&db["role"])?
                ||row.try_get::<String,_>("owner").map_err(|_|fail())?!=text(&db["role"])?||row.try_get::<String,_>("host").map_err(|_|fail())?!=text(&db["host"])?
                ||row.try_get::<i32,_>("port").map_err(|_|fail())? as u64!=db["port"].as_u64().ok_or_else(fail)?||row.try_get::<String,_>("cluster").map_err(|_|fail())?!=text(&db["clusterSystemId"])?{return Err(fail());}
            let schemas:Vec<i32>=sqlx::query_scalar("SELECT version FROM communityhero.schema_migrations ORDER BY version").fetch_all(&mut connection).await.map_err(|_|fail())?;
            if !schemas.contains(&crate::db_guards::REQUIRED_SCHEMA){return Err(fail());}
            let metadata:String=sqlx::query_scalar("SELECT metadata::text FROM communityhero.workspaces WHERE id='local-pilot'").fetch_one(&mut connection).await.map_err(|_|fail())?;
            let metadata=json(metadata.as_bytes())?;
            if metadata["account"]!="BAW Russia"||metadata["runtimeLifecycle"]["phase"]!="running"||metadata["runtimeLifecycle"]["owner"]!=self.record["fixedOwner"]{return Err(fail());}Ok(())
        }.await;
        let closed=connection.close().await.map_err(|_|fail());result?;closed?;
        let snapshot=crate::storage::read_native_fixture_workspace_snapshot(url,crate::accounts::Profile::BawRussia).await?;
        self.require_workspace(&snapshot)?;Ok(())
    }
    pub(crate) fn mark_attempt(&self)->ApiResult<()> {
        write_new(&self.data_root.join(format!("{}.attempt.json",text(&self.record["attemptId"])?)),&json!({"kind":"native-fixture-paid-producer-attempt","admission":self.admission_pin,"attemptId":self.record["attemptId"],"status":"started-unresolved"}))
    }
    pub(crate) async fn acquire_app(self,url:&str)->ApiResult<crate::App> {
        self.guard_existing_database(url).await?;self.mark_attempt()?;
        let db=crate::Database::postgres(url).await.map_err(|_|fail())?;
        let snapshot=match db.read().await {Ok(value)=>value,Err(error)=>{db.close().await;return Err(error);}};
        if self.require_workspace(&snapshot).is_err() {
            db.close().await;return Err(fail());
        }
        let profile=crate::accounts::Profile::BawRussia;
        let(events,_)=crate::broadcast::channel(8);let admission=std::sync::Arc::new(self.admission);
        Ok(crate::App{lifecycle_task_count:Default::default(),lifecycle_owner:std::sync::Arc::new(admission.identity().clone()),lifecycle_admission:admission,
            lifecycle_provider_token:Default::default(),lifecycle_work:Default::default(),media_discovery:Default::default(),preparation_wake:Default::default(),provider_session:Default::default(),
            account:profile,navigation:crate::account_navigation::Navigation::root(),db,gate:std::sync::Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:std::sync::Arc::new(crate::Mutex::new(())),
            preparation_workers:Default::default(),editorial_gate:Default::default(),assistant_gate:std::sync::Arc::new(crate::Mutex::new(())),assistant_chat_gate:std::sync::Arc::new(crate::Mutex::new(())),
            events,csrf:"isolated-floor-characterization".into(),auth:None,public_origin:None,external_writes:false,port:0,data:self.data_root.clone(),
            bridge:self.data_root.join("PROVIDER_MODEL_EXECUTION_FORBIDDEN.mjs"),node:self.data_root.join("MODEL_EXECUTION_FORBIDDEN.exe"),
            tasks:std::sync::Arc::new(crate::Mutex::new(std::collections::HashMap::new())),bootstrap_cache:std::sync::Arc::new(crate::bootstrap_cache::Cache::default())})
    }
}
pub(crate) fn read_fixture_input(path:&str,sha:&str)->ApiResult<Vec<u8>> {
    read_pinned(&safe_path(path,Path::new(""))?,sha,131072)
}
pub(crate) fn fresh_fixture_output(value:&str)->ApiResult<PathBuf> {
    let output=safe_path(value,Path::new(""))?;
    let repo=Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(|p|p.parent()).ok_or_else(fail)?;
    if !output.is_absolute()||!output.starts_with(repo)||output.exists(){return Err(fail());}
    for ancestor in output.parent().ok_or_else(fail)?.ancestors() {
        let metadata=std::fs::symlink_metadata(ancestor).map_err(|_|fail())?;
        if metadata.file_type().is_symlink(){return Err(fail());}
        #[cfg(windows)] {use std::os::windows::fs::MetadataExt;if metadata.file_attributes()&0x400!=0{return Err(fail());}}
    }Ok(output)
}
pub(crate) fn write_new(path:&Path,value:&Value)->ApiResult<()> {
    let bytes=value.to_string();let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|_|fail())?;
    file.write_all(bytes.as_bytes()).map_err(|_|fail())?;file.sync_all().map_err(|_|fail())?;drop(file);
    if std::fs::read(path).map_err(|_|fail())?!=bytes.as_bytes(){return Err(fail());}Ok(())
}
#[cfg(test)]mod tests {
    use super::*;
    #[test]fn closed_role_rejects_fixture_identity_wrong_selector_and_unknown_keys_before_io(){
        let mut record=json!({"schemaVersion":1,"kind":"root-reviewed-native-fixture-paid-producer-admission","selector":"other"});
        assert!(require_record(&record,chrono::Utc::now()).is_err());record["fixtureAdmission"]=json!(true);
        assert!(require_record(&record,chrono::Utc::now()).is_err());
    }
}
