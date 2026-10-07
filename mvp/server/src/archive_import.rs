//! Manual, deterministic closed-history import. Independent durable checkpoint;
//! shared sync job exclusion, with no automatic continuation or model calls.
use super::*;

const PAGES_PER_JOB: usize = 8;
const PAGE_SIZE: usize = 100;

fn window(body: &Value) -> ApiResult<Value> {
    let parse = |key| {
        chrono::DateTime::parse_from_rfc3339(required(body, key)?)
            .map(|v| v.with_timezone(&chrono::Utc))
            .map_err(|_| bad("Archive dates must be RFC3339 timestamps"))
    };
    let since = parse("since")?;
    let until = parse("until")?;
    if since >= until || until - since > chrono::Duration::days(31) {
        return Err(bad("Archive window must be positive and at most 31 days"));
    }
    Ok(json!({"since":since.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),
        "until":until.to_rfc3339_opts(chrono::SecondsFormat::Millis,true)}))
}

fn begin(d: &mut Value, requested: &Value, replace: bool) -> ApiResult<(Value, Option<String>)> {
    let binding = active_binding(d)?;
    bridge_account(&binding)?;
    let old = &d["sync"]["archive"];
    let same = old["window"] == *requested && old["binding"] == binding.to_json();
    if same && old["traversalComplete"] == true {
        return Ok((old.clone(), None));
    }
    let replacing = replace && !same;
    if replacing {
        let later_since = requested["since"].as_str()
            .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
            .zip(old["window"]["since"].as_str().and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok()))
            .is_some_and(|(new, previous)| new > previous);
        if old["binding"] != binding.to_json() || requested["until"] != old["window"]["until"] || !later_since {
            return Err(conflict("Replacement archive must narrow the existing window with the same end and binding"));
        }
        if list(d,"jobs").iter().any(|j|j["id"] == old["jobId"]
            && matches!(j["status"].as_str(),Some("running"|"queued"))) {
            return Err(conflict("Stop the current archive job before replacing its window"));
        }
    }
    if old.is_object() && !same && !replacing && old["traversalComplete"] != true {
        return Err(conflict("Resume the existing archive window before starting another"));
    }
    let replaced = replacing.then(|| old.clone());
    let archive = if same { old.clone() } else {
        json!({"id":id(),"binding":binding.to_json(),"window":requested,"mode":"closed",
            "pageSize":PAGE_SIZE,"pagesPerJob":PAGES_PER_JOB,"cursor":null,"seenCursors":[],
            "pages":0,"seenIds":[],"importedCount":0,"scannedCount":0,"outsideWindowCount":0,
            "skipped":0,"unknownDates":0,"rejectedStatusCount":0,"rejectedDateCount":0,
            "traversalComplete":false,"coverageComplete":false,"snapshotConsistent":false,"startedAt":now()})
    };
    let job = new_job(d, "sync", required(&archive, "id")?)?;
    row_mut(d, "jobs", &job)?["purpose"] = json!("archive_import");
    if let Some(mut previous) = replaced {
        previous["replacedAt"] = json!(now());
        previous["replacedBy"] = archive["id"].clone();
        if !d["sync"]["archiveHistory"].is_array() { d["sync"]["archiveHistory"] = json!([]); }
        d["sync"]["archiveHistory"].as_array_mut().unwrap().push(previous);
        audit(d,"archive_window_narrowed",required(&archive,"id")?);
    }
    d["sync"]["archive"] = archive;
    d["sync"]["archive"]["jobId"] = json!(job);
    d["sync"]["archive"]["status"] = json!("running");
    d["sync"]["archive"]["lastError"] = Value::Null;
    audit(d, "archive_import_started", &job);
    Ok((d["sync"]["archive"].clone(), Some(job)))
}

pub(super) async fn import(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let requested = window(&body)?;
    let replace = match body.get("replace") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(||bad("Archive replace must be boolean"))?,
    };
    let (archive, job) = app.change(|d| begin(d, &requested, replace)).await?;
    if let Some(job_id) = job.clone() {
        let worker = app.clone();
        app.spawn(job_id.clone(), async move { run(worker, job_id).await });
    }
    Ok(Json(json!({"jobId":job,"archive":archive})))
}

pub(super) async fn status(State(app): State<App>) -> ApiResult<Json<Value>> {
    let d = app.read().await?;
    let mut archive = d["sync"]["archive"].clone();
    if archive["status"] == "running" {
        let state = list(&d, "jobs").iter().find(|j| j["id"] == archive["jobId"])
            .map(|j| j["status"].clone()).unwrap_or(Value::Null);
        if state != "running" && state != "queued" {
            archive["status"] = json!("interrupted");
            archive["jobStatus"] = state;
        }
    }
    Ok(Json(json!({"archive":archive})))
}

pub(super) fn finish(d: &mut Value, error: Option<&str>) {
    let a = &mut d["sync"]["archive"];
    a["status"] = json!(if error.is_some() { "error" } else if a["traversalComplete"] == true {
        if a["coverageComplete"] == true { "completed" } else { "incomplete" }
    } else { "partial" });
    a["lastError"] = json!(error);
    a["lastFinishedAt"] = json!(now());
}

fn admit(d: &mut Value, binding: &ConnectorBinding, job: &str, cursor: &Value, snapshot: &Value) -> ApiResult<()> {
    let a = &d["sync"]["archive"];
    if active_binding(d)? != *binding || a["binding"] != binding.to_json()
        || a["jobId"] != job || a["cursor"] != *cursor
        || row(d,"jobs",job)?["status"] != "running" {
        return Err(conflict("Archive checkpoint changed before page admission"));
    }
    if snapshot["window"] != a["window"] {
        return Err(conflict("Provider archive window changed"));
    }
    let more = snapshot["hasMore"].as_bool().ok_or_else(|| internal("Provider omitted pagination coverage"))?;
    let next = &snapshot["cursor"];
    if more && (next.as_str().is_none_or(|s|s.is_empty()) || next == cursor
        || a["seenCursors"].as_array().unwrap().contains(next)) {
        return Err(internal("Provider archive cursor did not advance"));
    }
    if !more && !next.is_null() { return Err(internal("Provider archive pagination is inconsistent")); }
    let since = chrono::DateTime::parse_from_rfc3339(required(&a["window"],"since")?).unwrap();
    let until = chrono::DateTime::parse_from_rfc3339(required(&a["window"],"until")?).unwrap();
    let mut rejected_status = 0;
    let mut rejected_date = 0;
    let mut items = Vec::new();
    for item in snapshot["items"].as_array().ok_or_else(|| internal("Provider omitted items"))? {
        let item = bound_item(binding,item)?;
        if item["providerStatus"] != "closed" { rejected_status += 1; continue; }
        if !item["createdAt"].as_str().and_then(|v|chrono::DateTime::parse_from_rfc3339(v).ok())
            .is_some_and(|date|date >= since && date <= until) {
            rejected_date += 1; continue;
        }
        items.push(item);
    }
    // Do not admit context belonging only to a rejected row.
    let mut admitted = json!({"items":items});
    for (collection, reference) in [("posts","postId"),("branches","branchId")] {
        admitted[collection] = json!(snapshot[collection].as_array().into_iter().flatten()
            .filter(|v|v["id"].is_string() && items.iter().any(|i|i[reference] == v["id"]))
            .cloned().collect::<Vec<_>>());
    }
    merge_snapshot(d,&admitted)?;
    let a = &mut d["sync"]["archive"];
    if more { a["seenCursors"].as_array_mut().unwrap().push(next.clone()); }
    a["cursor"] = next.clone();
    for item in items {
        let seen = a["seenIds"].as_array_mut().unwrap();
        if !seen.contains(&item["id"]) { seen.push(item["id"].clone()); }
    }
    a["importedCount"] = json!(a["seenIds"].as_array().unwrap().len());
    for (field, amount) in [
        ("pages",1),("scannedCount",snapshot["scannedCount"].as_u64().unwrap_or(0)),
        ("outsideWindowCount",snapshot["outsideWindowCount"].as_u64().unwrap_or(0)),
        ("skipped",snapshot["skipped"].as_array().map_or(0,|v|v.len() as u64)),
        ("unknownDates",snapshot["unknownDateCount"].as_u64().unwrap_or(0)),
        ("rejectedStatusCount",rejected_status),("rejectedDateCount",rejected_date),
    ] { a[field] = json!(a[field].as_u64().unwrap_or(0)+amount); }
    a["traversalComplete"] = json!(!more);
    a["coverageComplete"] = json!(!more && ["skipped","unknownDates","rejectedStatusCount","rejectedDateCount"]
        .iter().all(|field|a[*field] == 0));
    a["lastPageAt"] = json!(now());
    Ok(())
}

async fn run(app: App, job: String) -> ApiResult<Value> {
    let binding = active_binding(&app.read().await?)?;
    let account = bridge_account(&binding)?;
    for _ in 0..PAGES_PER_JOB {
        let d = app.read().await?;
        let a = &d["sync"]["archive"];
        if a["jobId"] != job || a["binding"] != binding.to_json() || row(&d,"jobs",&job)?["status"] != "running" {
            return Err(conflict("Archive import superseded or cancelled"));
        }
        if a["traversalComplete"] == true { break; }
        let cursor = a["cursor"].clone();
        let mut args = json!({"account":account,"mode":"closed","window":a["window"],
            "binding":binding.to_json(),"pageSize":PAGE_SIZE});
        if !cursor.is_null() { args["cursor"] = cursor.clone(); }
        let snapshot = app.bridge("read",args).await?;
        app.change(|d|admit(d,&binding,&job,&cursor,&snapshot)).await?;
    }
    let a = app.read().await?["sync"]["archive"].clone();
    Ok(json!({"archiveId":a["id"],"partial":a["traversalComplete"] != true,
        "traversalComplete":a["traversalComplete"],"coverageComplete":a["coverageComplete"],
        "importedCount":a["importedCount"],"pages":a["pages"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dates() -> Value { json!({"since":"2026-09-08T00:00:00Z","until":"2026-09-22T00:00:00Z"}) }
    fn item(key: &str) -> Value {
        json!({"id":key,"itemId":key,"objectId":"11391","postKey":"11391:p","conversationKey":"11391:c",
            "createdAt":"2026-09-15T00:00:00Z","providerStatus":"closed","draft":"provider cannot set draft"})
    }
    fn setup() -> (Value,ConnectorBinding,String) {
        let mut d = empty();
        let b = active_binding(&d).unwrap();
        let (_,job) = begin(&mut d,&window(&dates()).unwrap(),false).unwrap();
        (d,b,job.unwrap())
    }
    fn page(d: &Value, items: Value, cursor: Value) -> Value {
        json!({"items":items,"window":d["sync"]["archive"]["window"],"hasMore":!cursor.is_null(),"cursor":cursor})
    }
    #[test]
    fn strict_window_normalizes_and_rejects_invalid_ranges() {
        assert_eq!(window(&dates()).unwrap()["since"],"2026-09-08T00:00:00.000Z");
        for v in [json!({"since":"2026-09-08","until":"2026-09-22"}),
            json!({"since":"2026-09-22T00:00:00Z","until":"2026-09-08T00:00:00Z"}),
            json!({"since":"2026-08-01T00:00:00Z","until":"2026-09-22T00:00:00Z"})] {
            assert!(window(&v).is_err());
        }
    }
    #[test]
    fn resume_is_durable_isolated_and_completed_window_is_idempotent() {
        let (mut d,b,job)=setup();
        d["sync"]["scan"] = json!({"cursor":"regular-position"});
        d["sync"]["background"] = json!({"nextRunAt":"keep-deadline"});
        assert!(begin(&mut d,&window(&dates()).unwrap(),false).is_err());
        let p=page(&d,json!([item("a")]),json!("next"));
        admit(&mut d,&b,&job,&Value::Null,&p).unwrap();
        row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("interrupted");
        d=serde_json::from_str(&d.to_string()).unwrap();
        let (a,next_job)=begin(&mut d,&window(&dates()).unwrap(),false).unwrap();
        assert_eq!(a["cursor"],"next");
        let next_job=next_job.unwrap();
        let p=page(&d,json!([item("a"),item("b")]),Value::Null);
        admit(&mut d,&b,&next_job,&json!("next"),&p).unwrap();
        finish(&mut d,None);
        assert_eq!(d["sync"]["archive"]["importedCount"],2);
        assert_eq!(d["sync"]["archive"]["status"],"completed");
        let before=d.clone();
        assert!(begin(&mut d,&window(&dates()).unwrap(),false).unwrap().1.is_none());
        assert_eq!(d,before);
        assert_eq!(d["sync"]["scan"]["cursor"],"regular-position");
        assert_eq!(d["sync"]["background"]["nextRunAt"],"keep-deadline");
    }
    #[test]
    fn reject_bad_pages_without_merging_and_skip_transitioned_rows() {
        let (mut d,b,job)=setup();
        let before=d.clone();
        let mut p=page(&d,json!([item("a")]),json!("next"));
        p["window"]["since"]=json!("2026-01-01T00:00:00Z");
        assert!(admit(&mut d,&b,&job,&Value::Null,&p).is_err());
        assert_eq!(d,before);
        let mut transitioned=item("open"); transitioned["providerStatus"]=json!("new");
        let mut outside=item("outside"); outside["createdAt"]=json!("2025-01-01T00:00:00Z");
        p=page(&d,json!([item("a"),transitioned,outside]),json!("next"));
        admit(&mut d,&b,&job,&Value::Null,&p).unwrap();
        assert_eq!(list(&d,"items").len(),1);
        assert_eq!(d["sync"]["archive"]["rejectedStatusCount"],1);
        assert_eq!(d["sync"]["archive"]["rejectedDateCount"],1);
        let before=d.clone();
        assert!(admit(&mut d,&b,&job,&json!("next"),&p).is_err());
        assert_eq!(d,before);
        p=page(&d,json!([]),Value::Null);
        admit(&mut d,&b,&job,&json!("next"),&p).unwrap();
        finish(&mut d,None);
        assert_eq!(d["sync"]["archive"]["status"],"incomplete");
    }
    #[test]
    fn preserves_existing_drafts_proposals_and_unrelated_items() {
        let (mut d,b,job)=setup();
        let mut existing=bound_item(&b,&item("a")).unwrap();
        existing["draft"]=json!("Human work");existing["draftEdited"]=json!(true);
        existing["workflow"]=json!("waiting");
        d["items"]=json!([existing,item("unrelated")]);
        d["proposals"]=json!([{"id":"proposal","itemId":"a","status":"draft","text":"Keep proposal"}]);
        let proposals=d["proposals"].clone();
        let unrelated=d["items"][1].clone();
        let p=page(&d,json!([item("a"),item("new-closed")]),Value::Null);
        admit(&mut d,&b,&job,&Value::Null,&p).unwrap();
        assert_eq!(d["items"][0]["draft"],"Human work");
        assert_eq!(d["items"][0]["draftEdited"],true);
        assert_eq!(d["items"][0]["workflow"],"waiting");
        assert_eq!(d["items"][1],unrelated);
        assert_eq!(d["proposals"],proposals);
        assert_eq!(d["items"][2]["workflow"],"closed");
        assert_eq!(d["items"][2]["draft"],"");
        assert!(list(&d,"approvals").is_empty());assert!(list(&d,"operations").is_empty());
    }
    #[test]
    fn cancelled_checkpoint_can_be_narrowed_after_reload_without_losing_imported_work() {
        let (mut d,b,job)=setup();
        let p=page(&d,json!([item("previously-imported")]),json!("old-window-cursor"));
        admit(&mut d,&b,&job,&Value::Null,&p).unwrap();
        d["items"][0]["draft"]=json!("Saved work");
        row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("cancelled");
        d=serde_json::from_str(&d.to_string()).unwrap();
        let old=d["sync"]["archive"].clone();
        let items=d["items"].clone();
        let week=window(&json!({"since":"2026-09-15T00:00:00Z","until":"2026-09-22T00:00:00Z"})).unwrap();
        assert!(begin(&mut d,&week,false).is_err());
        let (current,new_job)=begin(&mut d,&week,true).unwrap();
        assert_ne!(current["id"],old["id"]);
        assert_eq!(current["cursor"],Value::Null);
        assert_eq!(current["pages"],0);
        assert_eq!(d["sync"]["archiveHistory"][0]["cursor"],"old-window-cursor");
        assert_eq!(d["sync"]["archiveHistory"][0]["id"],old["id"]);
        assert_eq!(d["items"],items);
        // A delayed page from the cancelled generation cannot touch the new one.
        assert!(admit(&mut d,&b,&job,&json!("old-window-cursor"),&p).is_err());
        let new_job=new_job.unwrap();
        let p=page(&d,json!([item("new-week-item")]),Value::Null);
        admit(&mut d,&b,&new_job,&Value::Null,&p).unwrap();
        assert_eq!(d["items"][0]["draft"],"Saved work");
        assert_eq!(list(&d,"items").len(),2);
        let before=d.clone();
        assert!(begin(&mut d,&week,true).unwrap().1.is_none());
        assert_eq!(d,before);
    }
    #[test]
    fn replacement_rejects_active_job_expansion_or_changed_end_without_mutation() {
        let (mut d,_,job)=setup();
        let week=window(&json!({"since":"2026-09-15T00:00:00Z","until":"2026-09-22T00:00:00Z"})).unwrap();
        let before=d.clone();
        assert!(begin(&mut d,&week,true).is_err());assert_eq!(d,before);
        row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("cancelled");
        for request in [json!({"since":"2026-09-01T00:00:00Z","until":"2026-09-22T00:00:00Z"}),
            json!({"since":"2026-09-15T00:00:00Z","until":"2026-09-23T00:00:00Z"})] {
            let before=d.clone();
            assert!(begin(&mut d,&window(&request).unwrap(),true).is_err());
            assert_eq!(d,before);
        }
    }
    #[tokio::test]
    async fn bounded_worker_resumes_after_restart_and_only_reads_closed_pages() {
        let temp=tempfile::tempdir().unwrap();
        let db=open_db(&temp.path().join("workspace.sqlite")).await.unwrap();
        let (events,_)=broadcast::channel(8);
        let bridge=temp.path().join("archive-fixture.mjs");
        let log=temp.path().join("requests.jsonl");
        let script=r#"import {appendFile} from 'node:fs/promises';
let raw='';for await(const chunk of process.stdin)raw+=chunk;const r=JSON.parse(raw);
await appendFile(__LOG__,JSON.stringify(r)+'\n');
if(r.operation!=='read'||r.mode!=='closed'||r.pageSize!==100)throw Error('Wrong operation');
const n=Number(r.cursor||0),key='closed-'+n;
const item={id:key,itemId:key,objectId:'11391',postKey:'11391:p',conversationKey:'11391:c',providerStatus:'closed',createdAt:'2026-09-15T00:00:00Z'};
process.stdout.write(JSON.stringify({ok:true,result:{items:[item],hasMore:n<9,cursor:n<9?String(n+1):null,window:r.window}}));"#
            .replace("__LOG__",&json!(log.to_string_lossy()).to_string());
        std::fs::write(&bridge,script).unwrap();
        let mut app=App{lifecycle_task_count: Default::default(),lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)),lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()),lifecycle_provider_token: Default::default(),lifecycle_work: Default::default(),media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),
        preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),events,csrf:id(),auth:None,public_origin:None,external_writes:false,port:4186,
            data:temp.path().to_owned(),bridge,node:PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
        app.db.change(|d|crate::accounts::initialize(d,crate::accounts::Profile::LikeAvto)).await.unwrap();
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        for batch in 0..2 {
            let Json(response)=import(State(app.clone()),Json(dates())).await.unwrap();
            let job=response["jobId"].as_str().unwrap().to_owned();
            tokio::time::timeout(Duration::from_secs(30),async {
                loop {
                    let d=app.read().await.unwrap();
                    if row(&d,"jobs",&job).unwrap()["status"] != "running" {break;}
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }).await.unwrap();
            let d=app.read().await.unwrap();
            assert_eq!(row(&d,"jobs",&job).unwrap()["status"],"completed","{:#?}",d["jobs"]);
            assert_eq!(d["sync"]["archive"]["pages"],if batch==0{8}else{10});
            assert!(d["sync"]["scan"].is_null());assert!(d["sync"]["background"].is_null());
            app.db.close().await;
            app.db=Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
        }
        let d=app.read().await.unwrap();
        assert_eq!(d["sync"]["archive"]["status"],"completed");
        assert_eq!(d["sync"]["archive"]["importedCount"],10);
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(),10);
        assert!(list(&d,"jobs").iter().all(|j|j["kind"]=="sync"));
        assert!(list(&d,"approvals").is_empty());assert!(list(&d,"operations").is_empty());
        app.db.close().await;
    }
}
