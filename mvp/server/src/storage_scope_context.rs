//! Read-only ownership controls. Historical requests/results stay in their
//! canonical job records; these controls are never persisted as job payloads.
use super::*;
use serde_json::json;

const UNFINISHED:&str=r#"(COALESCE((payload->'recovery')<>'null'::jsonb,false) OR payload#>>'{scopeModelAttempt,status}' IN ('running','unknown') OR payload#>>'{preparationStages,first,status}' IN ('running','unknown') OR payload#>>'{preparationStages,review,status}' IN ('running','unknown')
 OR EXISTS(SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(payload#>'{preparationStages,reviewChunks,chunks}')='array' THEN payload#>'{preparationStages,reviewChunks,chunks}' ELSE '[]'::jsonb END) c
 CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(c->'attempts')='array' THEN c->'attempts' ELSE '[]'::jsonb END) a WHERE a->>'status' IN ('running','unknown')))"#;
const FIRST_ADMISSION_UNRESOLVED:&str=r#"(COALESCE(payload#>'{preparationStages,firstAdmission}'<>'null'::jsonb,false)
 AND NOT COALESCE(payload#>>'{preparationStages,first,status}'='completed' AND jsonb_typeof(payload#>'{preparationStages,first,result}')='object',false)
 AND NOT COALESCE(
 payload#>'{preparationStages,firstAdmission}'=jsonb_build_object('version',1,'status','reserved','requestSha256',payload#>'{prepareBundle,digest}','owner',payload#>'{preparationStages,initialAdmission,owner}','reservedAt',payload#>'{preparationStages,firstAdmission,reservedAt}')
 AND jsonb_typeof(payload#>'{preparationStages,firstAdmission,version}')='number'
 AND payload#>>'{preparationStages,firstAdmission,version}'='1'
 AND jsonb_typeof(payload#>'{preparationStages,firstAdmission,reservedAt}')='string'
 AND payload#>>'{preparationStages,firstAdmission,requestSha256}' ~ '^[0-9a-f]{64}$'
 AND payload#>'{preparationStages,firstAdmission,requestSha256}'=payload#>'{scopeReservation,prepareBundleDigest}'
 AND payload#>'{preparationStages,initialAdmission}'=jsonb_build_object('version',1,'status','scheduled','requestSha256',payload#>'{preparationStages,firstAdmission,requestSha256}','owner',payload#>'{preparationStages,firstAdmission,owner}','admittedAt',payload#>'{preparationStages,initialAdmission,admittedAt}')
 AND jsonb_typeof(payload#>'{preparationStages,initialAdmission,version}')='number'
 AND payload#>>'{preparationStages,initialAdmission,version}'='1'
 AND jsonb_typeof(payload#>'{preparationStages,initialAdmission,admittedAt}')='string'
 AND payload#>'{preparationStages,firstAdmission,owner}'=jsonb_build_object('account',payload#>'{preparationStages,firstAdmission,owner,account}','runtimeId',payload#>'{preparationStages,firstAdmission,owner,runtimeId}','releaseSha256',payload#>'{preparationStages,firstAdmission,owner,releaseSha256}','epoch',payload#>'{preparationStages,firstAdmission,owner,epoch}')
 AND jsonb_typeof(payload#>'{preparationStages,firstAdmission,owner,account}')='string'
 AND jsonb_typeof(payload#>'{preparationStages,firstAdmission,owner,runtimeId}')='string'
 AND jsonb_typeof(payload#>'{preparationStages,firstAdmission,owner,releaseSha256}')='string'
 AND jsonb_typeof(payload#>'{preparationStages,firstAdmission,owner,epoch}')='number'
 AND payload#>>'{preparationStages,firstAdmission,owner,epoch}' ~ '^[1-9][0-9]*$'
 AND (length(payload#>>'{preparationStages,firstAdmission,owner,epoch}')<20 OR
      (length(payload#>>'{preparationStages,firstAdmission,owner,epoch}')=20 AND payload#>>'{preparationStages,firstAdmission,owner,epoch}'<='18446744073709551615'))
 AND jsonb_typeof(payload#>'{scopeReservation,keysDigest}')='string'
 AND jsonb_typeof(payload#>'{scopeFailure,version}')='number'
 AND payload#>>'{scopeFailure,version}'='1'
 AND payload->'scopeFailure'=jsonb_build_object('version',1,'ownerJobId',payload->'id','keysDigest',payload#>'{scopeReservation,keysDigest}','prepareBundleDigest',payload#>'{scopeReservation,prepareBundleDigest}','kind','known_failure_without_result','category','ASSISTANT_BUSY'),false))"#;

fn repair_sql(input:&str)->String {
    format!("(COALESCE({input}->>'purpose'='answering_repair',false) OR COALESCE({input}->'originatingAnsweringAttemptId'<>'null'::jsonb,false) OR COALESCE({input}->'repairPaidIntent'<>'null'::jsonb,false) OR COALESCE({input}->'answeringRepairPlan'<>'null'::jsonb,false) OR COALESCE({input}#>'{{preparationStages,repairBudget}}'<>'null'::jsonb,false) OR COALESCE({input}#>'{{preparationStages,answeringRepairs}}'<>'null'::jsonb,false) OR COALESCE({input}->'repairMergeReceipt'<>'null'::jsonb,false))")
}
fn auxiliary_owners(workspace:&Value)->ApiResult<&[Value]> {
    // The canonical full workspace has no projection-only owner collection.
    // A supplied collection must remain a valid control, never an empty hint.
    let Some(value)=workspace.get("scopeOwners") else{return Ok(&[]);};
    let owners=value.as_array().ok_or_else(||internal("Invalid auxiliary reservation owners"))?;
    if owners.iter().any(|owner|!owner.is_object()||owner["id"].as_str().is_none_or(str::is_empty)){
        return Err(internal("Invalid auxiliary reservation owner"));
    }
    Ok(owners)
}
fn repair_proposal(workspace:&Value,p:&Value)->ApiResult<bool> {
    let owners=auxiliary_owners(workspace)?;
    Ok([&p["prepareRunId"],&p["origin"]["prepareRunId"],&p["recovery"]["prepareRunId"]].iter().filter_map(|id|id.as_str()).any(|id|crate::list(workspace,"jobs").iter().chain(owners.iter()).any(|job|job["id"]==id&&crate::preparation_reservations::repair_owned(job))))
}
fn compact_proposal(workspace:&Value,p:&Value)->ApiResult<Value> {
    if repair_proposal(workspace,p)?{return Ok(p.clone());}
    let mut value=json!({"id":p["id"],"itemId":p["itemId"],"status":p["status"]});
    for field in ["prepareRunId","account","accountId","connectorBinding","routeTarget","staleReason",
        "prepareBundleId","prepareBundleDigest","reviewContextDigest","itemRevision","kind","revision"] {
        if let Some(v)=p.get(field){value[field]=v.clone();}
    }
    for field in ["origin","recovery"] {if let Some(run)=p[field].get("prepareRunId") {value[field]=json!({"prepareRunId":run});}}
    if !p["prepareBundleId"].is_null()||!p["generationMetadata"].is_null(){value["paidGeneration"]=json!(true);}
    strip_nulls(&mut value);Ok(value)
}
fn strip_nulls(v:&mut Value){match v{Value::Object(o)=>{o.retain(|_,v|!v.is_null());for v in o.values_mut(){strip_nulls(v);}},Value::Array(a)=>for v in a{strip_nulls(v);},_=>()}}
fn active_external_job(j:&Value)->bool {
    matches!(j["kind"].as_str(),Some("execute"|"reconcile"))
        &&matches!(j["status"].as_str(),Some("running"|"queued"|"pending"))
}
fn external_job_control(j:&Value)->Value {
    json!({"id":j["id"],"kind":j["kind"],"status":j["status"],"refId":j["refId"]})
}
pub(super) fn project(workspace:&Value,view:&mut Value)->ApiResult<()> {
    auxiliary_owners(workspace)?;
    view["scopeProposals"]=json!(crate::list(workspace,"proposals").iter().map(|p|compact_proposal(workspace,p)).collect::<ApiResult<Vec<_>>>()?);
    let owners=crate::list(workspace,"jobs").iter().filter(|j|crate::preparation_reservations::requires_control(j))
        .map(|job|{let mut owner=crate::preparation_reservations::compact_owner(workspace,job)?;if !crate::preparation_reservations::repair_owned(job){strip_nulls(&mut owner);}Ok(owner)}).collect::<ApiResult<Vec<_>>>()?;
    view["scopeOwners"]=json!(owners);
    retain_repair_dependencies(workspace,view)?;
    view["activeExternalJobs"]=json!({"version":1,"complete":true,"jobs":crate::list(workspace,"jobs").iter().filter(|j|active_external_job(j)).map(external_job_control).collect::<Vec<_>>()});
    Ok(())
}
fn repair_roots(view:&Value)->ApiResult<Vec<Value>> {
    let mut roots=auxiliary_owners(view)?.iter().filter(|job|crate::preparation_reservations::repair_owned(job)).cloned().collect::<Vec<_>>();
    for proposal in crate::list(view,"scopeProposals"){if repair_proposal(view,proposal)?{roots.push(proposal.clone());}}
    Ok(roots)
}
fn merge_jobs(view:&mut Value,jobs:Vec<Value>)->ApiResult<()> {
    let current=view["jobs"].as_array_mut().ok_or_else(||internal("Repair dependency jobs missing"))?;
    for job in jobs {
        if let Some(old)=current.iter().find(|old|old["id"]==job["id"]){if old!=&job{return Err(internal("Repair dependency disagrees with loaded native job"));}}
        else {current.push(job);}
    }
    // A complete promoted canonical row is the mutable authority. Keeping its
    // old identical full control beside it would turn a later legitimate repair
    // settlement into a conflicting duplicate in native proof reconstruction.
    let promoted=current.iter().filter(|job|crate::preparation_reservations::repair_owned(job)).filter_map(|job|job["id"].as_str().map(str::to_owned)).collect::<HashSet<_>>();
    if let Some(owners)=view["scopeOwners"].as_array_mut(){owners.retain(|owner|!owner["id"].as_str().is_some_and(|id|promoted.contains(id)));}
    Ok(())
}
fn retain_repair_dependencies(workspace:&Value,view:&mut Value)->ApiResult<()> {
    let roots=repair_roots(view)?;if roots.is_empty(){return Ok(());}
    let mut jobs=Vec::new();
    loop {
        let(ids,bundles,full)=super::source_snapshot::scoped_job_dependencies(&jobs,&roots);
        let next=crate::list(workspace,"jobs").iter().filter(|job|full||job["id"].as_str().is_some_and(|id|ids.iter().any(|selected|selected==id))||job["prepareBundle"]["id"].as_str().is_some_and(|id|bundles.iter().any(|selected|selected==id))).cloned().collect::<Vec<_>>();
        if jobs==next{return merge_jobs(view,next);}
        jobs=next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    #[test]
    fn canonical_workspace_absent_auxiliary_owners_matches_explicit_empty_and_rejects_malformed_controls(){
        let full=terminal_auto_fixture();let original=full.clone();
        assert!(full.get("scopeOwners").is_none());
        let mut absent=full.clone();absent["jobs"]=json!([]);absent["proposals"]=json!([]);
        project(&full,&mut absent).unwrap();
        let mut explicit=full.clone();explicit["scopeOwners"]=json!([]);
        let mut equivalent=full.clone();equivalent["jobs"]=json!([]);equivalent["proposals"]=json!([]);
        project(&explicit,&mut equivalent).unwrap();assert_eq!(absent,equivalent);
        for recipient in ["a","d"]{
            assert_eq!(crate::preparation_reservations::assert_available(&absent,&[recipient.into()],None).is_ok(),crate::preparation_reservations::assert_available(&full,&[recipient.into()],None).is_ok());
        }
        assert_eq!(full,original,"read projection retains original frozen capture and saved proposal");
        for malformed in [Value::Null,json!({}),json!(true),json!([null]),json!([{}]),json!([{"id":""}])]{
            let mut hostile=full.clone();hostile["scopeOwners"]=malformed;
            let mut target=absent.clone();let unchanged=target.clone();
            assert!(project(&hostile,&mut target).is_err());assert_eq!(target,unchanged,"malformed supplied authority fails before projection writes");
        }
    }
    #[tokio::test]
    async fn canonical_native_paid_proposal_sqlite_read_keeps_exact_material_owner_without_auxiliary_controls(){
        let(mut d,batch,result,native)=super::super::hot_admission::tests::native_editorial_material_fixture();
        crate::editorial_review::admit(&mut d,&batch,&result,"2026-10-06T00:00:01Z").unwrap();normalize(&mut d);
        assert!(d.get("scopeOwners").is_none());
        let proposal=d["proposals"][0].clone();let body=json!({"proposals":[{"id":proposal["id"],"revision":proposal["revision"]}]});
        let folder=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&folder.path().join("scope-control-native.sqlite")).await.unwrap());
        db.change(|stored|{*stored=d;Ok(())}).await.unwrap();let full=db.read().await.unwrap();
        let view=db.read_operator_editorial(&body).await.unwrap();
        assert_eq!(crate::row(&view,"jobs",&native).unwrap(),crate::row(&full,"jobs",&native).unwrap(),"material proof owner remains a complete canonical journal");
        let full_current=crate::proposal_current(&full,&proposal).unwrap();
        assert_eq!(crate::proposal_current(&view,&proposal).unwrap(),full_current);
        assert_eq!(db.read().await.unwrap(),full,"read admission creates no receipt or operation");db.close().await;
    }
    #[test]
    fn repair_controls_preserve_exact_frozen_nulls_content_and_child_paid_closure(){
        let(mut d,origin,plan)=crate::answering_repair_plan::tests::fresh_revalidation();
        let child=crate::answering_repair_plan::tests::settle_revalidation_child(&mut d,&origin,&plan,true,false);
        crate::answering_repair_plan::tests::merge_revalidation(&mut d,&origin);normalize(&mut d);
        let mut view=d.clone();view["jobs"]=json!([]);view["proposals"]=json!([]);
        project(&d,&mut view).unwrap();
        for id in [&origin,&child]{
            assert!(crate::row(&view,"scopeOwners",id).is_err(),"promoted full repair authority must not retain a stale mutable duplicate");
            assert_eq!(crate::row(&view,"jobs",id).unwrap(),crate::row(&d,"jobs",id).unwrap(),"paid dependencies are complete canonical rows");
        }
        let proposal=d["proposals"][1].clone();
        assert_eq!(crate::row(&view,"scopeProposals",proposal["id"].as_str().unwrap()).unwrap(),&proposal,"repair content/history must authenticate native settlement SHA");
        let request=&crate::row(&d,"jobs",&child).unwrap()["prepareBundle"]["request"];
        assert_eq!(crate::preparation_materials::hash(request),crate::row(&view,"jobs",&child).unwrap()["prepareBundle"]["digest"]);
        assert_eq!(crate::preparation_reservations::assert_available(&view,&["i".into()],None).is_ok(),crate::preparation_reservations::assert_available(&d,&["i".into()],None).is_ok());
        assert_eq!(d["approvals"],json!([]));assert_eq!(d["operations"],json!([]));
    }
    #[tokio::test]
    #[ignore="ROOT-owned fresh isolated BAW PostgreSQL fixture"]
    async fn postgres_repair_full_canonical_controls_dependency_and_proposal_parity(){
        let url=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
        let expected=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_DATABASE").expect("explicit isolated fixture name");
        let db=super::super::preparation::writer_v51_fixture_db_for_profile_with(&url,&expected,crate::accounts::Profile::BawRussia).await;
        let(mut d,origin,plan)=crate::answering_repair_plan::tests::fresh_revalidation();
        let child=crate::answering_repair_plan::tests::settle_revalidation_child(&mut d,&origin,&plan,true,false);crate::answering_repair_plan::tests::merge_revalidation(&mut d,&origin);normalize(&mut d);
        db.change(|workspace|{*workspace=d;Ok(())}).await.unwrap();let full=db.read().await.unwrap();
        let view=db.read_preparation_context(&origin).await.unwrap();let mut oracle=full.clone();oracle["jobs"]=json!([]);project(&full,&mut oracle).unwrap();
        assert_eq!(view["scopeOwners"],oracle["scopeOwners"]);assert_eq!(view["scopeProposals"],oracle["scopeProposals"]);
        for id in [&origin,&child]{assert_eq!(crate::row(&view,"jobs",id).unwrap(),crate::row(&full,"jobs",id).unwrap());assert!(crate::row(&view,"scopeOwners",id).is_err());}
        let proposal=&full["proposals"][1];assert_eq!(crate::row(&view,"scopeProposals",proposal["id"].as_str().unwrap()).unwrap(),proposal);
        assert_eq!(db.read().await.unwrap(),full);db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
    async fn postgres_first_busy_versions_match_native_integer_only_discharge() {
        let db = super::super::preparation::writer_v51_fixture_db().await;
        let mut d = terminal_auto_fixture();
        // This fixture needs a no-output new run, not the retained paid sibling.
        d["proposals"] = json!([]);
        d["jobs"][0]["status"] = json!("running");
        d["jobs"][0]["preparationStages"] = json!({"first":null,"review":null});
        let identity=crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone();
        crate::runtime_lifecycle_startup::initialize_fixture(&mut d,&identity).unwrap();
        let token = crate::runtime_lifecycle::admission_token(&d,crate::runtime_lifecycle::AdmissionClass::Preparation).unwrap();
        let request = d["jobs"][0]["prepareBundle"]["request"].clone();
        crate::preparation_review::record_initial_admission(&mut d,&token,"native-auto",&crate::now()).unwrap();
        crate::preparation_review::reserve_first_admitted(&mut d,&token,"native-auto",&request,&crate::now()).unwrap();
        d["jobs"][0]["scopeFailure"] = crate::preparation_reservations::capture_failed_no_result(&d,"native-auto","ASSISTANT_BUSY").unwrap();
        d["jobs"][0]["status"] = json!("failed");
        let Database::Postgres{reader,..} = &db else { unreachable!() };
        let mut tx = reader.begin().await.unwrap();
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await.unwrap();
        let statement = format!("SELECT COALESCE({FIRST_ADMISSION_UNRESOLVED},false) FROM (SELECT $1::jsonb AS payload) fixture");
        for location in [None,Some("firstAdmission"),Some("initialAdmission"),Some("scopeFailure")] {
            let mut changed = d.clone();
            if let Some(field) = location {
                if field == "scopeFailure" { changed["jobs"][0][field]["version"] = json!(1.0); }
                else { changed["jobs"][0]["preparationStages"][field]["version"] = json!(1.0); }
            }
            let compact = crate::preparation_reservations::compact_owner(&changed,&changed["jobs"][0]).unwrap();
            let native = compact["scopeOwnerProjection"]["firstAdmissionUnresolved"].as_bool().unwrap();
            let sql:bool = sqlx::query_scalar(sqlx::AssertSqlSafe(statement.as_str())).bind(changed["jobs"][0].to_string()).fetch_one(&mut *tx).await.unwrap();
            assert_eq!(sql,native,"{location:?}: SQL/full Rust witness parity");
            assert_eq!(sql,location.is_some(),"integer versions alone discharge the first reservation");
        }
        tx.commit().await.unwrap();
        db.close().await;
    }

    fn terminal_auto_fixture() -> Value {
        let mut d = crate::empty();
        normalize(&mut d);
        crate::accounts::initialize(&mut d, crate::accounts::Profile::LikeAvto).unwrap();
        let binding = d["connectorBinding"].clone();
        d["branches"]=json!([{"id":"branch-a"},{"id":"branch-d"}]);
        d["items"] = json!([
            {"id":"a","branchId":"branch-a","objectId":"post-a","itemId":"external-a","postKey":"post-a","conversationKey":"thread-a","connectorBinding":binding},
            {"id":"d","branchId":"branch-d","objectId":"post-d","itemId":"external-d","postKey":"post-d","conversationKey":"thread-d","connectorBinding":binding}]);
        let request = json!({"account":d["account"],"connectorBinding":binding,"items":d["items"]});
        let digest = format!("{:x}",sha2::Sha256::digest(request.to_string().as_bytes()));
        d["jobs"] = json!([{"id":"native-auto","kind":"assistant","purpose":"auto_prepare","refId":"d","status":"completed",
            "prepareBundle":{"id":"auto-bundle","version":1,"itemIds":["a","d"],"request":request,"digest":digest},
            "preparationStages":{"first":{"status":"completed"},"review":{"status":"completed"}},
            "prepareOutcome":{"status":"needs_attention","items":[{"itemId":"a","status":"stale","reason":"Context changed during automatic preparation"},
                {"itemId":"d","status":"prepared"}],"admission":{"candidates":[{"itemId":"d"}]}}}]);
        d["jobs"][0]["scopeReservation"] = crate::preparation_reservations::capture(&d, "native-auto").unwrap();
        d["proposals"] = json!([{"id":"paid-d","itemId":"d","prepareRunId":"native-auto","status":"draft","routeTarget":d["items"][1]}]);
        validate(&d).unwrap();
        d
    }

    #[test]
    fn terminal_auto_control_projection_retains_independent_paid_sibling_and_original_history() {
        let full = terminal_auto_fixture();
        let mut view = full.clone(); view["jobs"] = json!([]); view["proposals"] = json!([]);
        project(&full, &mut view).unwrap();
        assert_eq!(view["scopeOwners"][0]["scopeOwnerProjection"]["autoTerminalItemIds"],json!(["a"]));
        assert!(crate::preparation_reservations::assert_available(&view,&["a".into()],None).is_ok());
        assert!(crate::preparation_reservations::assert_available(&view,&["d".into()],None).is_err());
        assert_eq!(full["jobs"][0]["prepareOutcome"]["items"][0]["status"],"stale");
        assert_eq!(full["proposals"][0]["status"],"draft");
    }

    #[test]
    fn external_job_controls_prove_complete_active_history_without_paid_payloads(){
        let mut full=terminal_auto_fixture();
        crate::list_mut(&mut full,"jobs").extend([
            json!({"id":"execute-original","kind":"execute","status":"running","refId":"old-approval","result":{"secretPaidOutput":"omit"}}),
            json!({"id":"reconcile-original","kind":"reconcile","status":"queued","refId":"old-operation"}),
            json!({"id":"pending-original","kind":"execute","status":"pending","refId":"old-approval"}),
            json!({"id":"ended-original","kind":"reconcile","status":"failed","refId":"old-operation"})]);
        let mut view=full.clone();view["jobs"]=json!([]);project(&full,&mut view).unwrap();
        assert_eq!(view["activeExternalJobs"]["version"],1);assert_eq!(view["activeExternalJobs"]["complete"],true);
        assert_eq!(view["activeExternalJobs"]["jobs"].as_array().unwrap().len(),3);
        assert_eq!(view["activeExternalJobs"]["jobs"][0]["refId"],"old-approval");
        assert!(!view["activeExternalJobs"].to_string().contains("secretPaidOutput"));
        assert_eq!(full["jobs"][1]["result"]["secretPaidOutput"],"omit","projection keeps the original paid job untouched");
    }

    #[tokio::test]
    #[ignore = "requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
    async fn postgres_terminal_auto_control_projection_matches_native_dispositions() {
        let db = super::super::preparation::writer_v51_fixture_db().await;
        let mut initial = terminal_auto_fixture();
        crate::list_mut(&mut initial,"jobs").extend([
            json!({"id":"active-execute","kind":"execute","status":"running","refId":"old-approval"}),
            json!({"id":"queued-readback","kind":"reconcile","status":"queued","refId":"old-operation"}),
            json!({"id":"pending-execute","kind":"execute","status":"pending","refId":"old-approval"}),
            json!({"id":"ended-readback","kind":"reconcile","status":"failed","refId":"old-operation"})]);
        db.change(|d| { *d = initial; Ok(()) }).await.unwrap();
        let full = db.read().await.unwrap();
        let mut expected = full.clone(); expected["jobs"] = json!([]); expected["proposals"] = json!([]);
        project(&full, &mut expected).unwrap();
        let mut actual = full.clone(); actual["jobs"] = json!([]); actual["proposals"] = json!([]);
        let Database::Postgres{reader,..} = &db else { unreachable!() };
        let mut tx = reader.begin().await.unwrap();
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await.unwrap();
        load(&mut tx, &mut actual).await.unwrap(); tx.commit().await.unwrap();
        assert_eq!(actual["scopeOwners"], expected["scopeOwners"], "SQL/pure terminal disposition parity");
        assert_eq!(actual["scopeProposals"], expected["scopeProposals"]);
        assert_eq!(actual["activeExternalJobs"],expected["activeExternalJobs"],"SQL/pure active transport completeness parity");
        assert_eq!(actual["activeExternalJobs"]["jobs"].as_array().unwrap().len(),3);
        assert!(crate::preparation_reservations::assert_available(&actual,&["a".into()],None).is_ok());
        assert!(crate::preparation_reservations::assert_available(&actual,&["d".into()],None).is_err());
        assert_eq!(db.read().await.unwrap(), full, "read-only controls never rewrite original paid history");
        db.close().await;
    }
}

// Rows own their bytes after the repeatable-read transaction is released.
// Keep SQL capture separate from JSON/provenance checks for preview readers.
pub(super) struct CapturedControls {
    proposals:Vec<sqlx::postgres::PgRow>,
    owners:Vec<sqlx::postgres::PgRow>,
    external:Vec<sqlx::postgres::PgRow>,
    repair_jobs:Vec<Value>,
}
pub(super) async fn capture(connection:&mut PgConnection)->ApiResult<CapturedControls> {
    let proposal_repair=format!("EXISTS(SELECT 1 FROM communityhero.jobs repair_owner WHERE repair_owner.workspace_id=$1 AND (repair_owner.id=p.payload->>'prepareRunId' OR repair_owner.id=p.payload#>>'{{origin,prepareRunId}}' OR repair_owner.id=p.payload#>>'{{recovery,prepareRunId}}') AND {})",repair_sql("repair_owner.payload"));
    let proposal_statement=r#"SELECT id,item_id,status,
      CASE WHEN {repair} THEN payload ELSE jsonb_strip_nulls(jsonb_build_object('id',payload->'id','itemId',payload->'itemId','status',payload->'status',
        'prepareRunId',payload->'prepareRunId','origin',CASE WHEN payload#>'{origin,prepareRunId}' IS NOT NULL THEN jsonb_build_object('prepareRunId',payload#>'{origin,prepareRunId}') END,
        'recovery',CASE WHEN payload#>'{recovery,prepareRunId}' IS NOT NULL THEN jsonb_build_object('prepareRunId',payload#>'{recovery,prepareRunId}') END,
        'account',payload->'account','accountId',payload->'accountId','connectorBinding',payload->'connectorBinding','routeTarget',payload->'routeTarget','staleReason',payload->'staleReason',
        'prepareBundleId',payload->'prepareBundleId','prepareBundleDigest',payload->'prepareBundleDigest',
        'reviewContextDigest',payload->'reviewContextDigest','itemRevision',payload->'itemRevision','kind',payload->'kind','revision',payload->'revision',
        'paidGeneration',CASE WHEN COALESCE((payload->'prepareBundleId')<>'null'::jsonb,false) OR COALESCE((payload->'generationMetadata')<>'null'::jsonb,false) THEN true END)) END::text AS control
      FROM communityhero.proposals p WHERE workspace_id=$1 ORDER BY ordinal"#.replace("payload","p.payload").replace("{repair}",&proposal_repair);
    let proposals=sqlx::query(sqlx::AssertSqlSafe(proposal_statement.as_str())).bind(WORKSPACE).fetch_all(&mut *connection).await?;
    #[cfg(test)] crate::performance::r3_sql_read();
    // JSON reduction happens in PostgreSQL before text transfer or serde. The
    // original request digest is retained only as a control binding for legacy
    // keys; ordinary model/proposal provenance still loads the full bound job.
    let repair=repair_sql("j.payload");
    let unfinished=format!("({UNFINISHED} OR {FIRST_ADMISSION_UNRESOLVED} OR COALESCE(payload#>>'{{repairPaidIntent,status}}' IN ('reserved','running','unknown'),false))");
    let predicate=format!("({repair} OR COALESCE(payload->'scopeReservation'<>'null'::jsonb,false) OR ((payload->>'purpose' IN ('engine_prepare','auto_prepare','auto_revalidate') OR ((kind='assistant' OR payload->>'kind'='assistant') AND (ref_id='engine_prepare' OR payload->>'refId'='engine_prepare'))) AND (status IN ('running','queued','interrupted') OR payload->>'status' IN ('running','queued','interrupted') OR COALESCE({unfinished},false))))");
    let statement=format!(r#"SELECT id,kind,ref_id,status,
      CASE WHEN {repair} THEN j.payload ELSE jsonb_strip_nulls(jsonb_build_object('id',j.payload->'id','kind',j.payload->'kind','purpose',j.payload->'purpose',
        'refId',j.payload->'refId','status',j.payload->'status','error',j.payload->'error','account',j.payload->'account',
        'accountId',j.payload->'accountId','connectorBinding',j.payload->'connectorBinding','scopeReservation',j.payload->'scopeReservation','scopeFailure',j.payload->'scopeFailure',
        'prepareOutcome',CASE WHEN j.payload->>'purpose'='auto_prepare' AND j.payload->>'status'='completed' THEN jsonb_build_object(
          'itemId',j.payload#>'{{prepareOutcome,itemId}}','status',j.payload#>'{{prepareOutcome,status}}',
          'proposalId',j.payload#>'{{prepareOutcome,proposalId}}',
          'reason',CASE WHEN j.payload#>>'{{prepareOutcome,reason}}'='Context changed during automatic preparation' THEN j.payload#>'{{prepareOutcome,reason}}' END,
          'items',CASE WHEN jsonb_typeof(j.payload#>'{{prepareOutcome,items}}')='array' THEN
            COALESCE((SELECT jsonb_agg(jsonb_build_object('itemId',i->'itemId','status',i->'status','proposalId',i->'proposalId',
              'reason',CASE WHEN i->>'reason'='Context changed during automatic preparation' THEN i->'reason' END))
              FROM jsonb_array_elements(j.payload#>'{{prepareOutcome,items}}') i),'[]'::jsonb) END,
          'admission',jsonb_build_object('candidates',COALESCE((SELECT jsonb_agg(jsonb_build_object('itemId',c->'itemId'))
            FROM jsonb_array_elements(CASE WHEN jsonb_typeof(j.payload#>'{{prepareOutcome,admission,candidates}}')='array'
              THEN j.payload#>'{{prepareOutcome,admission,candidates}}' ELSE '[]'::jsonb END) c),'[]'::jsonb))) END,
        'scopeOwnerProjection',jsonb_build_object('version',1,
          'reservationWasSaved',payload ? 'scopeReservation' AND payload->'scopeReservation'<>'null'::jsonb,
          'unfinishedModel',COALESCE({unfinished},false),
          'firstAdmissionUnresolved',COALESCE({FIRST_ADMISSION_UNRESOLVED},false),
          'noSelected',j.payload->'selectedItemIds'='[]'::jsonb AND NOT COALESCE((j.payload->'prepareBundle')<>'null'::jsonb,false)
            AND NOT COALESCE((j.payload#>'{{preparationStages,firstAdmission}}')<>'null'::jsonb,false)
            AND NOT COALESCE((j.payload#>'{{preparationStages,first}}')<>'null'::jsonb,false) AND NOT COALESCE((j.payload#>'{{preparationStages,review}}')<>'null'::jsonb,false)
            AND NOT COALESCE((j.payload#>'{{preparationStages,reviewChunks}}')<>'null'::jsonb,false) AND NOT COALESCE((j.payload->'scopeModelAttempt')<>'null'::jsonb,false),
          'supersedes',CASE WHEN jsonb_typeof(j.payload#>'{{prepareBundle,request,previousDecision}}')='object' THEN jsonb_build_object('prepareRunId',j.payload#>'{{prepareBundle,request,previousDecision,prepareRunId}}','proposalId',j.payload#>'{{prepareBundle,request,previousDecision,proposalId}}','itemId',j.payload#>'{{prepareBundle,request,previousDecision,itemId}}') END,
          'retainedOutput',COALESCE((j.payload->'result')<>'null'::jsonb,false)
            OR COALESCE((j.payload#>'{{preparationStages,first}}')<>'null'::jsonb,false)
            OR COALESCE((j.payload#>'{{preparationStages,review}}')<>'null'::jsonb,false)
            OR COALESCE((j.payload#>'{{preparationStages,reviewChunks}}')<>'null'::jsonb,false)
            OR COALESCE((j.payload->'scopeModelAttempt')<>'null'::jsonb,false) OR COALESCE((j.payload->'recovery')<>'null'::jsonb,false)
            OR EXISTS(SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(j.payload#>'{{preparationStages,groupAdmission}}')='array' THEN j.payload#>'{{preparationStages,groupAdmission}}' ELSE '[]'::jsonb END) g WHERE COALESCE((g->'admission')<>'null'::jsonb,false)),
          'cancelledUnpaid',j.payload->>'status'='cancelled' AND j.payload->'scopeCancellation'='{{"version":1,"modelDispatchPrevented":true,"noResult":true}}'::jsonb
            AND NOT COALESCE((j.payload#>'{{preparationStages,firstAdmission}}')<>'null'::jsonb,false)
            AND NOT COALESCE((j.payload->'result')<>'null'::jsonb,false) AND NOT COALESCE((j.payload#>'{{preparationStages,first}}')<>'null'::jsonb,false)
            AND NOT COALESCE((j.payload#>'{{preparationStages,review}}')<>'null'::jsonb,false) AND NOT COALESCE((j.payload#>'{{preparationStages,reviewChunks}}')<>'null'::jsonb,false)
            AND NOT COALESCE((j.payload->'scopeModelAttempt')<>'null'::jsonb,false)
            AND NOT EXISTS(SELECT 1 FROM communityhero.operations o WHERE o.workspace_id=$1 AND o.payload->>'prepareRunId'=j.id),
          'attentionItemIds',COALESCE((SELECT jsonb_agg(a->'itemId') FROM jsonb_array_elements(CASE WHEN jsonb_typeof(j.payload#>'{{preparationStages,groupAdmission}}')='array' THEN j.payload#>'{{preparationStages,groupAdmission}}' ELSE '[]'::jsonb END) g
            CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(g#>'{{admission,finalAssessments}}')='array' THEN g#>'{{admission,finalAssessments}}' ELSE '[]'::jsonb END) a
            WHERE j.payload->>'status'='completed' AND g->>'status'='admitted' AND a->>'outcome'='needs_attention' AND (g->'itemIds') @> jsonb_build_array(a->'itemId')
            AND NOT EXISTS(SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(g#>'{{admission,candidates}}')='array' THEN g#>'{{admission,candidates}}' ELSE '[]'::jsonb END) c WHERE c->'itemId'=a->'itemId')),'[]'::jsonb)
            || CASE WHEN j.payload->>'status'='completed' AND j.payload#>>'{{prepareOutcome,status}}'='needs_attention' THEN jsonb_build_array(j.payload->'refId') ELSE '[]'::jsonb END),
        'prepareBundle',CASE WHEN COALESCE(j.payload->'prepareBundle'<>'null'::jsonb,false) AND NOT COALESCE(j.payload->'scopeReservation'<>'null'::jsonb,false) THEN jsonb_build_object(
          'id',j.payload#>'{{prepareBundle,id}}','version',j.payload#>'{{prepareBundle,version}}','digest',j.payload#>'{{prepareBundle,digest}}','itemIds',j.payload#>'{{prepareBundle,itemIds}}',
          'request',jsonb_build_object('account',j.payload#>'{{prepareBundle,request,account}}','connectorBinding',j.payload#>'{{prepareBundle,request,connectorBinding}}',
             'items',COALESCE((SELECT jsonb_agg(jsonb_strip_nulls(jsonb_build_object('id',i->'id','branchId',i->'branchId','objectId',i->'objectId','itemId',i->'itemId','postKey',i->'postKey','conversationKey',i->'conversationKey','connectorBinding',i->'connectorBinding','account',i->'account','accountId',i->'accountId')))
                 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(j.payload#>'{{prepareBundle,request,items}}')='array' THEN j.payload#>'{{prepareBundle,request,items}}' ELSE '[]'::jsonb END) i),'[]'::jsonb))) END)) END::text AS control
      FROM communityhero.jobs j WHERE workspace_id=$1 AND {predicate} ORDER BY ordinal"#);
    let owners=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).fetch_all(&mut *connection).await?;
    #[cfg(test)] crate::performance::r3_sql_read();
    // This control is complete for the company transaction, independently of
    // the selected proposal's generation jobs. It never carries paid output.
    let external=sqlx::query("SELECT id,kind,status,ref_id,jsonb_build_object('id',payload->'id','kind',payload->'kind','status',payload->'status','refId',payload->'refId')::text AS control FROM communityhero.jobs WHERE workspace_id=$1 AND (kind IN ('execute','reconcile') OR payload->>'kind' IN ('execute','reconcile')) AND (status IN ('running','queued','pending') OR payload->>'status' IN ('running','queued','pending')) ORDER BY ordinal")
        .bind(WORKSPACE).fetch_all(&mut *connection).await?;
    #[cfg(test)] {
        crate::performance::r3_sql_read();
        let mut span=crate::performance::Span::new("r3.scope.controls.materialized");
        let rows=proposals.len()+owners.len()+external.len();
        let mut bytes=0;
        for row in proposals.iter().chain(owners.iter()).chain(external.iter()) { bytes+=row.try_get::<&str,_>("control")?.len(); }
        span.counts(rows,bytes,3);
    }
    // Preview callers decode controls only after releasing their reader. Resolve
    // exact repair bodies now, in the same RR transaction, rather than losing
    // child/paid dependencies in that delayed decode route.
    let mut roots=Vec::new();
    for row in &owners {let job=parse(row.try_get::<&str,_>("control")?)?;if crate::preparation_reservations::repair_owned(&job){roots.push(job);}}
    let repair_jobs=if roots.is_empty(){Vec::new()}else{
        let owner_ids=roots.iter().filter_map(|root|root["id"].as_str().map(str::to_owned)).collect::<HashSet<_>>();
        for row in &proposals {let proposal=parse(row.try_get::<&str,_>("control")?)?;
            if [&proposal["prepareRunId"],&proposal["origin"]["prepareRunId"],&proposal["recovery"]["prepareRunId"]].iter().filter_map(|id|id.as_str()).any(|id|owner_ids.contains(id)){roots.push(proposal);}
        }
        super::reads::dispatch::dependency_jobs_pg(connection,&roots).await?
    };
    Ok(CapturedControls{proposals,owners,external,repair_jobs})
}
pub(super) fn decode(captured:CapturedControls,view:&mut Value)->ApiResult<()> {
    let mut controls=Vec::with_capacity(captured.proposals.len());
    for row in captured.proposals {let p=parse(row.try_get::<&str,_>("control")?)?;
        if row.try_get::<&str,_>("id")?!=text(&p,"id")?
            ||row.try_get::<Option<String>,_>("item_id")?.as_deref()!=p["itemId"].as_str()
            ||row.try_get::<Option<String>,_>("status")?.as_deref()!=p["status"].as_str(){return Err(internal("Reservation proposal projection mismatch"));}
        controls.push(p);
    }
    view["scopeProposals"]=json!(controls);
    let mut owners=Vec::with_capacity(captured.owners.len());
    for row in captured.owners {let job=parse(row.try_get::<&str,_>("control")?)?;
        if row.try_get::<&str,_>("id")?!=text(&job,"id")?{return Err(internal("Reservation owner identity mismatch"));}
        for(column,field)in projection("jobs"){if row.try_get::<Option<String>,_>(*column)?.as_deref()!=job[*field].as_str(){return Err(internal("Reservation owner projection mismatch"));}}
        let mut owner=crate::preparation_reservations::compact_owner(view,&job)?;if !crate::preparation_reservations::repair_owned(&job){strip_nulls(&mut owner);}owners.push(owner);
    }
    view["scopeOwners"]=json!(owners);
    merge_jobs(view,captured.repair_jobs)?;
    let mut jobs=Vec::with_capacity(captured.external.len());
    for row in captured.external {
        let job=parse(row.try_get::<&str,_>("control")?)?;
        if row.try_get::<&str,_>("id")?!=text(&job,"id")?{return Err(internal("External writer identity mismatch"));}
        for(column,field)in projection("jobs"){
            if row.try_get::<Option<String>,_>(*column)?.as_deref()!=job[*field].as_str(){return Err(internal("External writer projection mismatch"));}
        }
        jobs.push(job);
    }
    view["activeExternalJobs"]=json!({"version":1,"complete":true,"jobs":jobs});
    Ok(())
}
pub(super) async fn load(connection:&mut PgConnection,view:&mut Value)->ApiResult<()> {
    decode(capture(connection).await?,view)
}
