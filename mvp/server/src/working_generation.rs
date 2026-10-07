//! An immutable working-database episode, independent of runtime restarts,
//! connector credentials, source cursors and paid authorization grants.
use crate::*;
pub(crate) const HEADER:&str="x-communityhero-workspace-generation";

/// A selected pristine database cannot start through an unrelated legacy
/// endpoint. Check before lifecycle/account initialization performs any write.
pub(crate) async fn verify_startup(db:&storage::Database,profile:accounts::Profile,data:&std::path::Path,admission:&runtime_lifecycle_startup::Admission)->ApiResult<()> {
    let actual=db.read_working_generation().await?;
    let read=|key:&str|std::env::var(key).map(Some).or_else(|error|match error {
        std::env::VarError::NotPresent=>Ok(None),_=>Err(conflict("Working selection environment is invalid"))});
    let expected=read("COMMUNITYHERO_EXPECTED_STORAGE_GENERATION")?;
    let selection=read("COMMUNITYHERO_WORKING_SELECTION_PATH")?;
    let pin=read("COMMUNITYHERO_WORKING_SELECTION_SHA256")?;
    if actual.is_none()&&expected.is_none()&&selection.is_none()&&pin.is_none(){return Ok(());}
    if expected.as_deref()!=actual.as_deref()||actual.is_none(){return Err(conflict("Selected working database generation differs before startup"));}
    let path=selection.ok_or_else(||conflict("Pinned working selection is required for this database"))?;
    let pin=pin.filter(|v|v.len()==64&&v.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)))
        .ok_or_else(||conflict("Working selection SHA256 is required"))?;
    if !std::path::Path::new(&path).is_absolute(){return Err(conflict("Working selection requires an absolute path"));}
    let bytes=runtime_lifecycle_startup::read_pinned(std::path::Path::new(&path),&pin,1024*1024)?;
    let selected:Value=serde_json::from_slice(&bytes).map_err(|_|conflict("Working selection is invalid"))?;
    let url=read("COMMUNITYHERO_DATABASE_URL")?.ok_or_else(||conflict("Selected database endpoint is required"))?;
    validate_startup_selection(&selected,actual.as_deref().unwrap(),profile,data,&url)?;
    let case_path=read("COMMUNITYHERO_CONNECTION_ADMISSION_CASE_PATH")?.ok_or_else(||conflict("Selected startup requires the configured case"))?;
    let case_sha=read("COMMUNITYHERO_CONNECTION_ADMISSION_CASE_SHA256")?.ok_or_else(||conflict("Selected startup requires the configured case SHA"))?;
    let case:Value=serde_json::from_slice(&runtime_lifecycle_startup::read_pinned(std::path::Path::new(&case_path),&case_sha,4*1024*1024)?)
        .map_err(|_|conflict("Selected startup case is invalid"))?;
    validate_case_selection(&selected,&case,&case_sha,admission.identity())?;
    // Admission already checked the actual executable, complete runtime assets,
    // core, outer manifest and source checkpoint. Bind the selected package to
    // those same roles independently of the launcher before workspace writes.
    let release=admission.trace_release().ok_or_else(||conflict("Selected startup requires an admitted release manifest"))?;
    let manifest_path=read("COMMUNITYHERO_RELEASE_MANIFEST_PATH")?.ok_or_else(||conflict("Selected release manifest path missing"))?;
    let manifest_sha=read("COMMUNITYHERO_RELEASE_MANIFEST_SHA256")?.ok_or_else(||conflict("Selected release manifest SHA missing"))?;
    let manifest_path=std::path::Path::new(&manifest_path);
    let manifest:Value=serde_json::from_slice(&runtime_lifecycle_startup::read_pinned(manifest_path,&manifest_sha,16*1024*1024)?)
        .map_err(|_|conflict("Selected release manifest invalid"))?;
    let core_path=manifest["coreArtifact"]["path"].as_str().ok_or_else(||conflict("Selected core pin missing"))?;
    let core_path=if std::path::Path::new(core_path).is_absolute(){PathBuf::from(core_path)}else{
        manifest_path.parent().ok_or_else(||conflict("Selected release base missing"))?.join(core_path)};
    let binary=std::env::current_exe().map_err(|_|conflict("Actual selected executable unavailable"))?;
    for (path,sha) in [(manifest_path,manifest_sha.as_str()),(core_path.as_path(),release.core_sha256.as_str()),(binary.as_path(),release.binary_sha256.as_str())] {
        require_package_pin(&selected["currentPackage"],path,sha)?;
    }
    Ok(())
}
fn validate_case_selection(selected:&Value,case:&Value,case_sha:&str,identity:&runtime_lifecycle::RuntimeIdentity)->ApiResult<()> {
    if selected["caseSha256"]!=case_sha||case["kind"]!="communityhero-clean-start-case.v1"
        ||case["companyId"]!=selected["companyId"]||case["currentPackage"]!=selected["currentPackage"]
        ||case["newDatabase"]!=selected["database"]||case["newDataDir"]!=selected["dataDir"]
        ||case["bootstrap"]["companyId"]!=selected["companyId"]
        ||case["bootstrap"]["expectedStorageGeneration"]!=selected["storageGeneration"] {
        return Err(conflict("Selected startup differs from the configured case"));
    }
    let owner=runtime_lifecycle::parse_token(&case["bootstrap"]["owner"])?;
    if owner.account!=identity.account||owner.runtime_id!=identity.runtime_id||owner.release_sha256!=identity.release_sha256 {
        return Err(conflict("Selected startup case owner differs from admitted runtime"));
    }
    Ok(())
}
fn require_package_pin(package:&Value,path:&std::path::Path,sha:&str)->ApiResult<()> {
    let actual=std::fs::canonicalize(path).map_err(|_|conflict("Selected package role is unavailable"))?;
    let mut count=0;
    for pin in package.as_array().ok_or_else(||conflict("Selected package is invalid"))? {
        let fields=pin.as_object().filter(|v|v.contains_key("path")&&v.contains_key("sha256")
            &&v.keys().all(|key|matches!(key.as_str(),"path"|"sha256"|"bytes")))
            .ok_or_else(||conflict("Selected package pin is invalid"))?;
        if fields["sha256"].as_str().is_none_or(|v|v.len()!=64||!v.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)))
            ||fields.get("bytes").is_some_and(|v|v.as_u64().is_none()) {return Err(conflict("Selected package digest or size is invalid"));}
        let path=fields["path"].as_str().ok_or_else(||conflict("Selected package path is invalid"))?;
        if !std::path::Path::new(path).is_absolute(){return Err(conflict("Selected package pin must be absolute"));}
        if fields["sha256"]==sha&&std::fs::canonicalize(path).map_err(|_|conflict("Selected package member is unavailable"))?==actual {count+=1;}
    }
    if count!=1{return Err(conflict("Selected package lacks the unique admitted runtime role"));}
    Ok(())
}
fn validate_startup_selection(selected:&Value,actual:&str,profile:accounts::Profile,data:&std::path::Path,url:&str)->ApiResult<()> {
    use std::str::FromStr;
    let fields=["kind","companyId","selectionGeneration","database","dataDir","storageGeneration","currentPackage","caseSha256","gateState"];
    if profile!=accounts::Profile::BawRussia||!selected.as_object().is_some_and(|o|o.len()==fields.len()&&fields.iter().all(|f|o.contains_key(*f)))
        ||selected["kind"]!="communityhero-working-selection.v1"||selected["companyId"]!=profile.key()
        ||selected["gateState"]!="blocked"||selected["storageGeneration"]!=actual
        ||selected["selectionGeneration"].as_u64().is_none_or(|n|n==0)
        ||selected["currentPackage"].as_array().is_none_or(|pins|pins.is_empty()||pins.len()>64)
        ||selected["caseSha256"].as_str().is_none_or(|v|v.len()!=64||!v.bytes().all(|b|b.is_ascii_hexdigit())) {
        return Err(conflict("Working selection scope is invalid"));
    }
    let selected_data=selected["dataDir"].as_str().ok_or_else(||conflict("Selected data directory is invalid"))?;
    let selected_data=std::fs::canonicalize(selected_data).map_err(|_|conflict("Selected data directory is unavailable"))?;
    if selected_data!=std::fs::canonicalize(data).map_err(|_|conflict("Runtime data directory is unavailable"))? {
        return Err(conflict("Selected and runtime data directories differ"));
    }
    let options=sqlx::postgres::PgConnectOptions::from_str(url).map_err(|_|conflict("Selected database endpoint is invalid"))?;
    let endpoint=&selected["database"];
    // SQLx preserves URL brackets around IPv6. Remove that syntax only;
    // localhost, IPv4 and IPv6 remain distinct selected endpoints.
    let host=match options.get_host(){"[::1]"=>"::1",host=>host};
    if !matches!(host,"127.0.0.1"|"localhost"|"::1")||endpoint["host"]!=host
        ||endpoint["port"].as_u64()!=Some(options.get_port() as u64)
        ||endpoint["database"].as_str()!=options.get_database()||endpoint["user"]!=options.get_username() {
        return Err(conflict("Selected and runtime database endpoints differ"));
    }
    Ok(())
}

pub(crate) fn check_request(current:Option<&str>,headers:&axum::http::HeaderMap,mutation:bool,session_route:bool)->ApiResult<()> {
    let values:Vec<_>=headers.get_all(HEADER).iter().collect();
    if values.len()>1{return Err(conflict("Workspace generation header is ambiguous"));}
    let supplied=values.first().map(|value|value.to_str().map_err(|_|conflict("Workspace generation header is invalid"))).transpose()?;
    if supplied.is_some()&&supplied!=current {return Err(conflict("Workspace generation changed; recover the original intent before starting a new one"));}
    if current.is_some()&&mutation&&!session_route&&supplied.is_none() {
        return Err(conflict("Workspace generation is required for this mutation"));
    }
    Ok(())
}

impl storage::Database {
    /// Small immutable identity read; no source/job/history or writer lease.
    pub(crate) async fn read_working_generation(&self)->ApiResult<Option<String>> {
        use sqlx::Row;
        let (value,present)=match self {
            Self::Sqlite(pool)=>{
                let row=sqlx::query("SELECT json_object('account',json_extract(payload,'$.account'),'connectorBinding',json(json_extract(payload,'$.connectorBinding')),'storageGeneration',json_extract(payload,'$.storageGeneration')) AS identity,(json_type(payload,'$.storageGeneration') IS NOT NULL) AS present FROM workspace WHERE id=1")
                    .fetch_one(pool).await?;
                (serde_json::from_str::<Value>(row.try_get::<&str,_>("identity")?).map_err(|_|internal("Working generation identity invalid"))?,row.try_get::<bool,_>("present")?)
            },
            Self::Postgres{reader,..}=>{
                let row=sqlx::query("SELECT jsonb_build_object('account',metadata->'account','connectorBinding',metadata->'connectorBinding','storageGeneration',metadata->'storageGeneration')::text AS identity,account,(metadata ? 'storageGeneration') AS present FROM communityhero.workspaces WHERE id='local-pilot'")
                    .fetch_one(reader).await?;
                let value=serde_json::from_str::<Value>(row.try_get::<&str,_>("identity")?).map_err(|_|internal("Working generation identity invalid"))?;
                if row.try_get::<String,_>("account")?!=value["account"].as_str().unwrap_or(""){return Err(internal("Working generation account projection differs"));}
                (value,row.try_get::<bool,_>("present")?)
            }
        };
        if !present{return Ok(None);}
        current(&value).map(|v|Some(v.to_owned()))
    }
}

pub(crate) fn current(data:&Value)->ApiResult<&str> {
    // No caller-supplied account or default connection can mint this identity.
    accounts::Profile::from_workspace(data)?;
    active_binding(data)?;
    let text=data["storageGeneration"].as_str().ok_or_else(||conflict("Working database generation is not admitted"))?;
    let value=uuid::Uuid::parse_str(text).map_err(|_|conflict("Working database generation is invalid"))?;
    if value.get_version_num()!=4 || value.to_string()!=text {
        return Err(conflict("Working database generation must be a canonical UUIDv4"));
    }
    Ok(text)
}

pub(crate) fn validate_change(before:&Value,after:&Value)->ApiResult<()> {
    // Initial creation belongs only to reviewed offline pristine provisioning.
    // A normal writer may neither create, delete nor rotate this field.
    if before.get("storageGeneration")!=after.get("storageGeneration") {
        return Err(conflict("Working database generation is immutable; explicit pristine provisioning required"));
    }
    if after.get("storageGeneration").is_some() {current(after)?;}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn workspace()->Value {
        json!({"account":"BAW Russia","connectorBinding":accounts::Profile::BawRussia.binding(),
            "storageGeneration":"e4e1d9f2-49b8-4b48-846f-ab113f519f89"})
    }
    #[test]
    fn restart_keeps_working_generation_and_generic_writer_cannot_mint_or_rotate_it(){
        let before=workspace(); let mut after=before.clone();
        after["runtimeLifecycle"]=json!({"epoch":20});
        validate_change(&before,&after).unwrap();assert_eq!(current(&before).unwrap(),current(&after).unwrap());
        after["storageGeneration"]=json!("7b02e571-572b-4b41-87a6-40d2c2a36bc3");
        assert!(validate_change(&before,&after).is_err());
        let mut legacy=before.clone();legacy.as_object_mut().unwrap().remove("storageGeneration");
        assert!(validate_change(&legacy,&before).is_err());assert!(validate_change(&before,&legacy).is_err());
        validate_change(&legacy,&legacy).unwrap();assert!(current(&legacy).is_err());
    }
    #[test]
    fn malformed_or_foreign_identity_never_creates_a_queue_epoch(){
        let before=workspace();
        for value in [Value::Null,json!("client-epoch"),json!("E4E1D9F2-49B8-4B48-846F-AB113F519F89")]{
            let mut invalid=before.clone();invalid["storageGeneration"]=value;assert!(current(&invalid).is_err());
        }
        let mut foreign=before;foreign["account"]=json!("unconfigured-company");assert!(current(&foreign).is_err());
    }
    #[test]
    fn stale_read_and_mutation_headers_never_switch_saved_intents(){
        let current="e4e1d9f2-49b8-4b48-846f-ab113f519f89";
        let mut headers=axum::http::HeaderMap::new();
        assert!(check_request(Some(current),&headers,false,false).is_ok());
        assert!(check_request(Some(current),&headers,true,false).is_err());
        assert!(check_request(Some(current),&headers,true,true).is_ok());
        headers.insert(HEADER,current.parse().unwrap());
        assert!(check_request(Some(current),&headers,true,false).is_ok());
        assert!(check_request(None,&headers,false,false).is_err());
        assert!(check_request(Some("7b02e571-572b-4b41-87a6-40d2c2a36bc3"),&headers,false,false).is_err());
        headers.append(HEADER,current.parse().unwrap());
        assert!(check_request(Some(current),&headers,true,false).is_err());
    }
    #[test]
    fn native_startup_rejects_selection_endpoint_directory_company_or_generation_drift(){
        let dir=tempfile::tempdir().unwrap();let other=tempfile::tempdir().unwrap();let current=workspace()["storageGeneration"].as_str().unwrap().to_owned();
        let selected=json!({"kind":"communityhero-working-selection.v1","companyId":"baw-russia","selectionGeneration":1,
            "database":{"host":"127.0.0.1","port":55499,"database":"communityhero_fixture","user":"fixture"},
            "dataDir":dir.path().to_string_lossy(),"storageGeneration":current,"currentPackage":[{"sha256":"a".repeat(64)}],
            "caseSha256":"b".repeat(64),"gateState":"blocked"});
        let url="postgresql://fixture@127.0.0.1:55499/communityhero_fixture";
        validate_startup_selection(&selected,&current,accounts::Profile::BawRussia,dir.path(),url).unwrap();
        for (host,url) in [("localhost","postgresql://fixture@localhost:55499/communityhero_fixture"),("::1","postgresql://fixture@[::1]:55499/communityhero_fixture")] {
            let mut loopback=selected.clone();loopback["database"]["host"]=json!(host);
            validate_startup_selection(&loopback,&current,accounts::Profile::BawRussia,dir.path(),url).unwrap();
            assert!(validate_startup_selection(&loopback,&current,accounts::Profile::BawRussia,dir.path(),"postgresql://fixture@127.0.0.1:55499/communityhero_fixture").is_err());
        }
        assert!(validate_startup_selection(&selected,&current,accounts::Profile::BawRussia,other.path(),url).is_err());
        assert!(validate_startup_selection(&selected,&current,accounts::Profile::LikeAvto,dir.path(),url).is_err());
        for pointer in ["/storageGeneration","/database/database","/database/user","/gateState"] {
            let mut changed=selected.clone();*changed.pointer_mut(pointer).unwrap()=json!("different");
            assert!(validate_startup_selection(&changed,&current,accounts::Profile::BawRussia,dir.path(),url).is_err(),"{pointer}");
        }
    }
    #[test]
    fn startup_case_and_package_roles_cannot_be_retargeted_independently(){
        let owner=json!({"account":"BAW Russia","runtimeId":"selected-native","releaseSha256":"a".repeat(64),"epoch":1});
        let identity=runtime_lifecycle::RuntimeIdentity{account:"BAW Russia".into(),runtime_id:"selected-native".into(),release_sha256:"a".repeat(64)};
        let selected=json!({"companyId":"baw-russia","storageGeneration":workspace()["storageGeneration"],"caseSha256":"b".repeat(64),
            "currentPackage":[{"path":"selected-package","sha256":"c".repeat(64)}],"database":{"database":"selected-N"},"dataDir":"selected-N-data"});
        let case=json!({"kind":"communityhero-clean-start-case.v1","companyId":"baw-russia","currentPackage":selected["currentPackage"],
            "newDatabase":selected["database"],"newDataDir":selected["dataDir"],"bootstrap":{"owner":owner,"companyId":"baw-russia","expectedStorageGeneration":selected["storageGeneration"]}});
        validate_case_selection(&selected,&case,&"b".repeat(64),&identity).unwrap();
        assert!(validate_case_selection(&selected,&case,&"d".repeat(64),&identity).is_err());
        for pointer in ["/companyId","/currentPackage","/newDatabase","/newDataDir","/bootstrap/companyId","/bootstrap/expectedStorageGeneration","/bootstrap/owner/runtimeId","/bootstrap/owner/releaseSha256"] {
            let mut changed=case.clone();*changed.pointer_mut(pointer).unwrap()=json!("different");
            assert!(validate_case_selection(&selected,&changed,&"b".repeat(64),&identity).is_err(),"{pointer}");
        }
        let dir=tempfile::tempdir().unwrap();let core=dir.path().join("core.json");let foreign=dir.path().join("foreign.json");
        std::fs::write(&core,b"admitted-role").unwrap();std::fs::write(&foreign,b"same-label-other-role").unwrap();
        let pin=json!({"path":core.to_string_lossy(),"sha256":"a".repeat(64),"bytes":13});
        require_package_pin(&json!([pin.clone()]),&core,&"a".repeat(64)).unwrap();
        assert!(require_package_pin(&json!([pin.clone()]),&foreign,&"a".repeat(64)).is_err());
        assert!(require_package_pin(&json!([pin.clone()]),&core,&"d".repeat(64)).is_err());
        assert!(require_package_pin(&json!([pin.clone(),pin]),&core,&"a".repeat(64)).is_err());
    }
}
