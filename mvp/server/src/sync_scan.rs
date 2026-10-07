//! Durable, bounded pagination. Exhaustion is traversal evidence, not an atomic
//! snapshot of a changing provider queue. Missing records never imply deletion.
use super::*;
const PAGES_PER_MODE: usize = 8;
const CLOSED_PAGES_PER_RUN:usize = 1;
const EXACT_REFRESH_LIMIT: usize = 4;
const CONTINUE_AFTER_SECONDS: i64 = 5;
const MONITOR_AFTER_SECONDS: i64 = 60;
const MAX_ERROR_BACKOFF_SECONDS: i64 = 900;
// A bounded evidence budget, not an import limit. Keep overflow visible above it.
const ACCOUNTING_TARGET_LIMIT: usize = 10_000;
const OPEN_BATCH_PAGES:usize=2;
const OPEN_BATCH_ROWS:usize=200;
const OPEN_BATCH_BYTES:usize=8*1024*1024;

#[cfg(test)]
#[path="sync_scan_batch_tests.rs"]
mod batch_tests;

fn omitted_read_policy()->Value {json!({"options":{},"capabilities":null,"selection":"unverified_omitted"})}
fn read_options(lane:&Value)->ApiResult<Value> {
    let options=lane.get("readOptions").cloned().unwrap_or_else(||json!({}));
    if options.as_object().is_none_or(|fields|fields.keys().any(|key|key!="pageSize"))
        ||options.get("pageSize").is_some_and(|size|size.as_u64().is_none_or(|n|n==0||n>OPEN_BATCH_ROWS as u64)) {
        return Err(conflict("Invalid durable OPEN read options"));
    }
    Ok(options)
}
fn read_policy_from_caps(binding:&ConnectorBinding,account:&str,caps:&Value)->Value {
    let read=&caps["local"]["read"];
    if caps["account"]!=account||read["account"]!=account||read["binding"]!=binding.to_json()
        ||read["version"]!=1||read["verified"]!=true||read["mode"]!="open"
        ||read["objectScope"].as_array().is_none_or(|ids|ids.is_empty()||ids.len()>12
            ||ids.iter().any(|id|id.as_str().is_none_or(|id|id.is_empty()))) {
        return omitted_read_policy();
    }
    let Some(sizes)=read["pageSizes"].as_array() else{return omitted_read_policy();};
    let objects=read["objectScope"].as_array().unwrap().len() as u64;
    let allowed=|n:u64|n>0&&n<=OPEN_BATCH_ROWS as u64/objects;
    let supported=sizes.iter().filter_map(Value::as_u64).filter(|n|allowed(*n)).collect::<Vec<_>>();
    let size=match read["preferredPageSize"].as_u64().filter(|n|supported.contains(n)) {
        Some(size)=>size,None=>match supported.into_iter().max(){Some(size)=>size,None=>return omitted_read_policy()},
    };
    json!({"options":{"pageSize":size},"capabilities":read,"selection":"verified_connector"})
}
async fn new_read_policy(app:&App,binding:&ConnectorBinding,account:&str)->Value {
    match app.bridge("caps",json!({"account":account,"binding":binding.to_json()})).await {
        Ok(caps)=>read_policy_from_caps(binding,account,&caps),
        Err(_)=>omitted_read_policy(), // Unknown support never invents an option or another connector.
    }
}
fn pin_read_policy(lane:&mut Value,policy:&Value) {
    lane["readOptions"]=policy["options"].clone();
    lane["readCapabilities"]=policy["capabilities"].clone();
    lane["readOptionSelection"]=policy["selection"].clone();
}
fn scan_can_resume(d:&Value,binding:&Value)->bool {
    let old=&d["sync"]["scan"];
    old["binding"]==*binding&&continuation_pending(d)&&old["window"].is_object()&&old["invalidatedAt"].is_null()
}
fn frontier_can_resume(d:&Value,binding:&ConnectorBinding)->bool {
    let old=&d["sync"]["openFrontier"];
    old["binding"]==binding.to_json()&&old["scanId"]==d["sync"]["scan"]["id"]
        &&old["done"]==false&&old["scope"]=="all-open"&&old["window"].is_null()&&old["invalidatedAt"].is_null()
}

fn open_page_rows(snapshot:&Value)->ApiResult<usize> {
    let imported=snapshot["items"].as_array().ok_or_else(||internal("Provider omitted items"))?.len()
        +snapshot["skipped"].as_array().map_or(0,Vec::len);
    if snapshot["queueAccounting"]["version"]==1 {
        return snapshot["queueAccounting"]["observations"].as_array().map(|rows|rows.len().max(imported))
            .ok_or_else(||bad("OPEN page omitted queue observations"));
    }
    Ok(imported)
}
fn validate_open_page(lane:&Value,binding:&ConnectorBinding,account:&str,cursor:&Value,
    seen:&[Value],snapshot:&Value)->ApiResult<()> {
    let options=read_options(lane)?;
    for (value,expected) in [(snapshot.get("account"),json!(account)),
        (snapshot["accountBinding"].get("accountKey"),json!(account)),
        (snapshot.get("mode"),json!("open")),(snapshot.get("window"),Value::Null)] {
        if value.is_some_and(|value|*value!=expected){return Err(conflict("OPEN page response scope changed"));}
    }
    if snapshot.get("readOptions").is_some_and(|returned|*returned!=options)
        ||(!options.as_object().unwrap().is_empty()&&snapshot.get("readOptions")!=Some(&options)) {
        return Err(conflict("OPEN page read options changed"));
    }
    if !lane["readCapabilities"].is_null() {
        if snapshot["readCapabilities"]!=lane["readCapabilities"]
            ||snapshot["readCapabilities"]["binding"]!=binding.to_json()
            ||snapshot["accountBinding"]["accountKey"]!=account {
            return Err(conflict("OPEN page connector capability changed"));
        }
        let scope=lane["readCapabilities"]["objectScope"].as_array().ok_or_else(||conflict("OPEN read scope missing"))?;
        for table in ["items","skipped"] {
            for row in list(snapshot,table) {if !scope.contains(&row["objectId"]){return Err(conflict("OPEN page returned a foreign object"));}}
        }
        for row in list(snapshot,"posts") {
            if row.get("objectId").is_some_and(|object|!scope.contains(object)){return Err(conflict("OPEN page returned a foreign publication"));}
        }
        for branch in list(snapshot,"branches") {
            for message in list(branch,"messages") {
                if message.get("providerObjectId").is_some_and(|object|!scope.contains(object)){return Err(conflict("OPEN page returned a foreign branch message"));}
            }
        }
        for row in list(&snapshot["queueAccounting"],"observations") {
            if !scope.contains(&row["objectId"]){return Err(conflict("OPEN page observed a foreign object"));}
        }
    }
    let more=snapshot["hasMore"].as_bool().ok_or_else(||internal("Provider omitted pagination coverage"))?;
    let next=&snapshot["cursor"];
    if more&&(next.as_str().is_none_or(|value|value.is_empty())||next==cursor||seen.contains(next)) {
        return Err(internal("Provider cursor did not advance"));
    }
    if !more&&!next.is_null(){return Err(internal("Provider pagination is inconsistent"));}
    Ok(())
}
fn validate_open_batch(lane:&Value,binding:&ConnectorBinding,account:&str,start:&Value,pages:&[Value])->ApiResult<()> {
    if pages.is_empty()||pages.len()>OPEN_BATCH_PAGES{return Err(bad("Invalid ordered OPEN batch"));}
    let mut cursor=start.clone();let mut seen=list(lane,"seenCursors").to_vec();
    let mut rows=0usize;let mut bytes=0usize;
    for (index,page) in pages.iter().enumerate() {
        validate_open_page(lane,binding,account,&cursor,&seen,page)?;
        rows=rows.checked_add(open_page_rows(page)?).ok_or_else(||bad("OPEN batch row budget exceeded"))?;
        bytes=bytes.checked_add(page.to_string().len()).ok_or_else(||bad("OPEN batch byte budget exceeded"))?;
        if rows>OPEN_BATCH_ROWS||bytes>OPEN_BATCH_BYTES{return Err(bad("OPEN batch exceeds row or byte budget"));}
        if page["hasMore"]==false&&index+1!=pages.len(){return Err(internal("OPEN batch continued after terminal page"));}
        cursor=page["cursor"].clone();if !cursor.is_null(){seen.push(cursor.clone());}
    }
    Ok(())
}
async fn stage_open_batch(app:&App,binding:&ConnectorBinding,account:&str,lane:&Value,max_pages:usize)->ApiResult<Vec<Value>> {
    let options=read_options(lane)?;let mut cursor=lane["cursor"].clone();
    let mut seen=list(lane,"seenCursors").to_vec();let mut pages=Vec::new();
    let mut rows=0usize;let mut bytes=0usize;
    for _ in 0..max_pages.min(OPEN_BATCH_PAGES) {
        let mut args=json!({"account":account,"mode":"open","binding":binding.to_json()});
        if let Some(size)=options.get("pageSize"){args["pageSize"]=size.clone();}
        if !cursor.is_null(){args["cursor"]=cursor.clone();}
        let mut page=app.bridge("read",args).await?;
        validate_open_page(lane,binding,account,&cursor,&seen,&page)?;
        for item in page["items"].as_array_mut().ok_or_else(||internal("Provider omitted items"))? {
            *item=bound_item(binding,item)?;
        }
        let next_rows=open_page_rows(&page)?;let next_bytes=page.to_string().len();
        if next_rows>OPEN_BATCH_ROWS||next_bytes>OPEN_BATCH_BYTES {
            if pages.is_empty(){return Err(bad("OPEN page exceeds row or byte budget; complete context retained"));}
            break; // Commit the smaller complete prefix. This page's cursor is not admitted.
        }
        if rows+next_rows>OPEN_BATCH_ROWS||bytes+next_bytes>OPEN_BATCH_BYTES {break;}
        rows+=next_rows;bytes+=next_bytes;cursor=page["cursor"].clone();
        let done=page["hasMore"]==false;pages.push(page);
        if done||rows==OPEN_BATCH_ROWS||bytes==OPEN_BATCH_BYTES {break;}
        seen.push(cursor.clone());
    }
    validate_open_batch(lane,binding,account,&lane["cursor"],&pages)?;Ok(pages)
}
fn admit_open_batch(d:&mut Value,scan_id:&str,binding:&ConnectorBinding,frontier_id:Option<&str>,
    start:&Value,expected_options:&Value,expected_capabilities:&Value,pages:&[Value])->ApiResult<()> {
    let lane=if frontier_id.is_some(){&d["sync"]["openFrontier"]}else{&d["sync"]["scan"]["open"]};
    if read_options(lane)?!=*expected_options||lane["readCapabilities"]!=*expected_capabilities {
        return Err(conflict("OPEN read contract changed before batch admission"));
    }
    let account=bridge_account(binding)?;
    validate_open_batch(lane,binding,account,start,pages)?;
    let mut cursor=start.clone();let mut receipts=Vec::new();
    for (ordinal,page) in pages.iter().enumerate() {
        use sha2::{Digest,Sha256};
        match frontier_id {
            Some(frontier)=>admit_open_frontier(d,scan_id,binding,frontier,&cursor,page)?,
            None=>admit(d,scan_id,binding,"open",&cursor,page)?,
        }
        let encoded=page.to_string();
        receipts.push(json!({"ordinal":ordinal,"cursor":cursor,"nextCursor":page["cursor"],
            "hasMore":page["hasMore"],"logicalRows":open_page_rows(page)?,"utf8Bytes":encoded.len(),
            "snapshotSha256":format!("{:x}",Sha256::digest(encoded.as_bytes()))}));
        cursor=page["cursor"].clone();
    }
    // Immutable ordered receipts live in the existing audit collection. Source
    // projections exclude its history; no unbounded receipt array in metadata.
    let receipt=json!({"id":id(),"at":now(),"action":"sync.open.batch","refId":frontier_id.unwrap_or(scan_id),
        "scanId":scan_id,"frontierId":frontier_id,"binding":binding.to_json(),"readOptions":expected_options,
        "readCapabilities":expected_capabilities,
        "maxPages":OPEN_BATCH_PAGES,"maxLogicalRows":OPEN_BATCH_ROWS,"maxUtf8Bytes":OPEN_BATCH_BYTES,"pages":receipts});
    d["sync"]["lastOpenBatchReceiptId"]=receipt["id"].clone();list_mut(d,"audit").push(receipt);Ok(())
}

/// This marker records that accounting began at the head, not that the provider
/// supplied evidence. Missing page evidence must not keep rewinding a new pass.
const ACCOUNTING_CHECKPOINT_VERSION: u64 = 1;

fn fresh_lane(open: bool) -> Value {
    let mut lane = json!({"cursor":null,"done":false,"pages":0,"seenCursors":[],
        "accountingCheckpointVersion":ACCOUNTING_CHECKPOINT_VERSION});
    if open { lane["scope"] = json!("all-open"); }
    lane
}

fn accounting_recovery_reason(lane: &Value) -> Option<&'static str> {
    let progressed = lane["pages"].as_u64().unwrap_or(0) > 0
        || !lane["cursor"].is_null() || lane["done"] == true
        || lane["seenCursors"].as_array().is_some_and(|v| !v.is_empty());
    if !progressed { return None; }
    let a = &lane["accounting"];
    if a.is_null() { return Some("MISSING_CHECKPOINT_ACCOUNTING"); }
    if a["version"] != 1 { return Some("INCOMPATIBLE_CHECKPOINT_ACCOUNTING"); }
    let valid = || -> Option<()> {
        a["overflow"].as_bool()?;
        a["unverifiedPages"].as_u64()?;
        for field in ["observations", "duplicateObservations", "excludedObservations", "untrackedObservations"] {
            a[field].as_u64()?;
        }
        let rows = a["records"].as_array().filter(|rows| rows.len() <= ACCOUNTING_TARGET_LIMIT)?;
        let mut identities = std::collections::HashSet::new();
        for record in rows {
            if !identities.insert(accounting_key(record).ok()?)
                || !matches!(record["contextStatus"].as_str(), Some("imported" | "pending" | "failed")) {
                return None;
            }
        }
        let imported = rows.iter().filter(|r| r["contextStatus"] == "imported").count();
        for (field, expected) in [("trackedUnique", rows.len()), ("importedUnique", imported),
            ("unresolvedUnique", rows.len() - imported)] {
            // A page without provider evidence returns before these summaries
            // are generated. A verified page must have consistent summaries.
            if (a.get(field).is_some() || a["unverifiedPages"] == 0)
                && a[field].as_u64() != Some(expected as u64) { return None; }
        }
        Some(())
    };
    if valid().is_none() { return Some("MALFORMED_CHECKPOINT_ACCOUNTING"); }
    // An exhausted old budget cannot certify observations that it did not track.
    // Start one new pass at the expanded budget; never reset overflow at this budget.
    if a["overflow"] == true
        && a["limit"].as_u64().is_some_and(|limit| limit > 0 && limit < ACCOUNTING_TARGET_LIMIT as u64) {
        return Some("EXPANDED_ACCOUNTING_BUDGET");
    }
    if a["unverifiedPages"].as_u64().unwrap() > 0
        && lane["accountingCheckpointVersion"] != ACCOUNTING_CHECKPOINT_VERSION {
        return Some("LEGACY_UNVERIFIED_CHECKPOINT_ACCOUNTING");
    }
    None
}

fn recovery_evidence(lane: &Value, reason: &str) -> Value {
    json!({"reason":reason,"pages":lane["pages"],"cursor":lane["cursor"],
        "done":lane["done"],"accountingVersion":lane["accounting"]["version"],
        "unverifiedPages":lane["accounting"]["unverifiedPages"],
        "previousLimit":lane["accounting"]["limit"],"limit":ACCOUNTING_TARGET_LIMIT})
}

fn accounting_key(value: &Value) -> ApiResult<String> {
    let safe = |key| value[key].as_str().filter(|s| !s.is_empty() && s.len() <= 200
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    let object = safe("objectId").ok_or_else(|| bad("Invalid queue accounting scope"))?;
    let item = safe("itemId").ok_or_else(|| bad("Invalid queue accounting identity"))?;
    Ok(format!("{object}:{item}"))
}

/// Record only the explicit requested IDs for this one traversal. The fixed cap
/// is an evidence limit, never a claim that untracked provider rows disappeared.
fn update_accounting(lane: &mut Value, snapshot: &Value) -> ApiResult<()> {
    let mut accounting = lane.get("accounting").cloned().unwrap_or_else(|| json!({
        "version":1,"limit":ACCOUNTING_TARGET_LIMIT,"records":[],"overflow":false,
        "unverifiedPages":u64::from(lane["pages"].as_u64().unwrap_or(0)>0),
        "observations":0,"duplicateObservations":0,"excludedObservations":0,"untrackedObservations":0}));
    // A nonoverflowed old pass can use the expanded budget without losing progress.
    // Overflowed old passes are recovered by prepare, before any new page admission.
    if accounting["overflow"] == false { accounting["limit"] = json!(ACCOUNTING_TARGET_LIMIT); }
    if lane["pages"].as_u64().unwrap_or(0) == 0 {
        lane["accountingCheckpointVersion"] = json!(ACCOUNTING_CHECKPOINT_VERSION);
    }
    let records = accounting["records"].as_array().filter(|rows| rows.len() <= ACCOUNTING_TARGET_LIMIT)
        .ok_or_else(|| bad("Invalid durable queue accounting"))?;
    let mut records = records.clone();
    let mut positions = HashMap::new();
    for (index, record) in records.iter().enumerate() {
        if positions.insert(accounting_key(record)?, index).is_some() { return Err(bad("Duplicate durable queue accounting identity")); }
    }
    let evidence = &snapshot["queueAccounting"];
    if evidence["version"] != 1 {
        // Legacy checkpoints/pages can continue importing safely, but cannot
        // acquire complete evidence retrospectively from cursor exhaustion.
        accounting["unverifiedPages"] = json!(accounting["unverifiedPages"].as_u64().unwrap_or(0).saturating_add(1));
        lane["accounting"] = accounting;
        return Ok(());
    }
    let observations = evidence["observations"].as_array().filter(|rows| rows.len() <= 1200)
        .ok_or_else(|| bad("Invalid page queue accounting"))?;
    let mut requested = HashMap::new();
    for observation in observations {
        let key = accounting_key(observation)?;
        let required = observation["contextRequired"].as_bool().ok_or_else(|| bad("Invalid context accounting disposition"))?;
        if requested.insert(key, required).is_some() { return Err(bad("Duplicate requested queue identity")); }
    }
    let mut imported = std::collections::HashSet::new();
    for item in snapshot["items"].as_array().ok_or_else(|| bad("Page accounting omitted imported contexts"))? {
        let key = accounting_key(item)?;
        if requested.get(&key) != Some(&true) { return Err(bad("Imported context was not requested by the queue page")); }
        if !imported.insert(key) { return Err(bad("Duplicate imported queue context")); }
    }
    let mut failures = HashMap::new();
    for failure in snapshot["skipped"].as_array().ok_or_else(|| bad("Page accounting omitted context failures"))? {
        let key = accounting_key(failure)?;
        let reason = failure["code"].as_str().filter(|code| matches!(*code,"RESPONSE_SCHEMA_ERROR"|"TARGET_IDENTITY_MISMATCH"))
            .ok_or_else(|| bad("Invalid queue context failure reason"))?;
        if requested.get(&key) != Some(&true) || imported.contains(&key) || failures.insert(key, reason).is_some() {
            return Err(bad("Ambiguous queue context failure identity"));
        }
    }
    let page_duplicates = evidence["duplicateQueueCount"].as_u64().filter(|count| *count <= 1200)
        .ok_or_else(|| bad("Invalid duplicate queue observation count"))?;
    let mut duplicates = page_duplicates;
    let mut excluded = 0_u64;
    let mut untracked = 0_u64;
    for observation in observations {
        if observation["contextRequired"] == false { excluded += 1; continue; }
        let key = accounting_key(observation)?;
        let index = match positions.get(&key) {
            Some(index) => { duplicates += 1; *index },
            None if records.len() == ACCOUNTING_TARGET_LIMIT => { untracked += 1; continue; },
            None => {
                let index = records.len(); positions.insert(key.clone(), index);
                records.push(json!({"objectId":observation["objectId"],"itemId":observation["itemId"],"contextStatus":"pending","reason":"CONTEXT_NOT_RETURNED"})); index
            }
        };
        if let Some(reason) = failures.get(&key) {
            records[index]["lastFailureReason"] = json!(reason);
            records[index]["failedObservations"] = json!(records[index]["failedObservations"].as_u64().unwrap_or(0).saturating_add(1));
        }
        if imported.contains(&key) {
            records[index]["contextStatus"] = json!("imported"); records[index]["reason"] = Value::Null;
        } else if records[index]["contextStatus"] != "imported" {
            records[index]["contextStatus"] = json!(if failures.contains_key(&key) {"failed"} else {"pending"});
            records[index]["reason"] = json!(failures.get(&key).copied().unwrap_or("CONTEXT_NOT_RETURNED"));
        }
    }
    accounting["observations"] = json!(accounting["observations"].as_u64().unwrap_or(0).saturating_add(observations.len() as u64).saturating_add(page_duplicates));
    for (key, increment) in [("duplicateObservations",duplicates),("excludedObservations",excluded),("untrackedObservations",untracked)] {
        accounting[key] = json!(accounting[key].as_u64().unwrap_or(0).saturating_add(increment));
    }
    accounting["overflow"] = json!(accounting["overflow"] == true || untracked > 0);
    accounting["trackedUnique"] = json!(records.len());
    accounting["importedUnique"] = json!(records.iter().filter(|r|r["contextStatus"]=="imported").count());
    accounting["unresolvedUnique"] = json!(records.iter().filter(|r|r["contextStatus"]!="imported").count());
    accounting["records"] = json!(records);
    lane["accounting"] = accounting;
    Ok(())
}

fn context_complete(lane: &Value) -> bool {
    let accounting = &lane["accounting"];
    accounting["version"] == 1 && accounting["overflow"] == false
        && accounting["unverifiedPages"].as_u64() == Some(0)
        && accounting["unresolvedUnique"].as_u64() == Some(0)
}

fn mark_coverage(lane: &mut Value) {
    lane["traversalComplete"] = json!(lane["done"] == true);
    lane["contextComplete"] = json!(context_complete(lane));
    lane["snapshotConsistent"] = json!(false);
    lane["coverageComplete"] = json!(lane["done"] == true && context_complete(lane)
        && lane["unknownDates"].as_u64().unwrap_or(0) == 0);
}

/// Cursor work and incomplete evidence are different: skipped records or unknown
/// dates must stay visible, but must not cause a tight rescan of exhausted pages.
fn continuation_pending(d: &Value) -> bool {
    let scan = &d["sync"]["scan"];
    let frontier = &d["sync"]["openFrontier"];
    scan.is_object()
        && (scan["traversalComplete"] != true
            || (frontier.is_object() && frontier["done"] == false))
}

fn due(d: &Value, at: i64) -> bool {
    !list(d, "jobs").iter().any(|j| {
        j["kind"] == "sync" && matches!(j["status"].as_str(), Some("running" | "queued"))
    }) && timestamp(&d["sync"]["background"]["nextRunAt"]).is_none_or(|next| next <= at)
}

/// Called in the same transaction that finishes a sync job. Deadlines and error
/// backoff survive restarts; page checkpoints are committed independently.
pub(super) fn schedule_next(d: &mut Value, failed: bool, at: i64) {
    let errors = if failed {
        d["sync"]["background"]["consecutiveErrors"].as_u64().unwrap_or(0).saturating_add(1)
    } else { 0 };
    let pending = continuation_pending(d);
    let delay = if failed {
        (MONITOR_AFTER_SECONDS * (1_i64 << errors.saturating_sub(1).min(4)))
            .min(MAX_ERROR_BACKOFF_SECONDS)
    } else if pending { CONTINUE_AFTER_SECONDS } else { MONITOR_AFTER_SECONDS };
    d["sync"]["background"] = json!({
        "enabled":true,"state":if failed {"backoff"} else if pending {"catching_up"} else {"monitoring"},
        "continuationPending":pending,"consecutiveErrors":errors,
        "nextRunAt":chrono::DateTime::from_timestamp(at+delay,0).unwrap().to_rfc3339(),
        "lastFinishedAt":chrono::DateTime::from_timestamp(at,0).unwrap().to_rfc3339(),
        "delaySeconds":delay
    });
}

pub(super) async fn tick(app: &App) -> ApiResult<()> {
    // One small read answers the three scheduling prechecks. Every actual
    // claim rechecks authoritative state inside its existing transaction.
    let schedule=app.db.read_schedule().await?;
    // These lanes must still run while the long archive job is in flight.
    if let Err(error)=fast_status::tick(app,&schedule).await {eprintln!("Fast source synchronization: {}",error.1);}
    let at = chrono::Utc::now().timestamp();
    // Avoid a write transaction and SSE refresh on every idle scheduler tick.
    if !due(&schedule, at) { return Ok(()); }
    let job = app.change_schedule(|d| {
        if !due(d, at) { return Ok(None); }
        let job = new_job(d, "sync", "")?;
        row_mut(d,"jobs",&job)?["purpose"] = json!("background_sync");
        d["sync"]["background"]["state"] = json!("running");
        d["sync"]["background"]["enabled"] = json!(true);
        Ok(Some(job))
    }).await?;
    if let Some(job) = job {
        let worker = app.clone();
        app.spawn(job, async move { run(worker).await });
    }
    Ok(())
}

fn timestamp(value: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|v| v.timestamp())
}

fn refresh_candidates(d: &Value, binding: &ConnectorBinding, at: i64) -> Vec<Value> {
    let mut candidates: Vec<(i64, Value)> = list(d, "items")
        .iter()
        .filter_map(|item| {
            if !["new", "inprogress"].contains(&item["providerStatus"].as_str().unwrap_or(""))
                && !["attention", "prepared", "waiting"]
                    .contains(&item["workflow"].as_str().unwrap_or(""))
            {
                return None;
            }
            let target = bound_item(binding, item).ok()?;
            let key = item["id"].as_str()?;
            let observed = timestamp(&item["providerObservedAt"]).unwrap_or(0);
            let attempted = timestamp(&d["sync"]["targetRefresh"][key]["attemptedAt"]).unwrap_or(0);
            // Open pages refresh records which are still open. A record missing
            // from those pages needs an exact status check on the next monitor
            // cycle, even when its creation date is outside closed history.
            // Absence alone never closes it; failed exact reads retain backoff.
            let failed = !d["sync"]["targetRefresh"][key]["error"].is_null();
            let retry_delay = if failed { 300 } else { MONITOR_AFTER_SECONDS };
            (observed <= at - MONITOR_AFTER_SECONDS && attempted <= at - retry_delay)
                .then_some((observed.max(attempted), target))
        })
        .collect();
    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1["id"].as_str().cmp(&b.1["id"].as_str()))
    });
    candidates
        .into_iter()
        .take(EXACT_REFRESH_LIMIT)
        .map(|(_, item)| item)
        .collect()
}

pub(super) fn admit_exact_refresh(
    d: &mut Value,
    binding: &ConnectorBinding,
    target: &Value,
    snapshot: &Value,
) -> ApiResult<()> {
    if active_binding(d)? != *binding {
        return Err(conflict("Connector changed during target refresh"));
    }
    let key = required(target, "id")?;
    let current = bound_item(binding, row(d, "items", key)?)?;
    for field in ["id", "objectId", "itemId"] {
        if current[field] != target[field] {
            return Err(conflict("Target changed during refresh"));
        }
    }
    let items = snapshot["items"]
        .as_array()
        .filter(|items| items.len() == 1)
        .ok_or_else(|| internal("Exact refresh omitted target"))?;
    let observed = bound_item(binding, &items[0])?;
    for field in ["id", "objectId", "itemId"] {
        if observed[field] != target[field] {
            return Err(conflict("Exact refresh returned another target"));
        }
    }
    if !["new", "inprogress", "closed", "deleted"]
        .contains(&observed["providerStatus"].as_str().unwrap_or(""))
    {
        return Err(internal("Exact refresh omitted provider status"));
    }
    let mut bound = snapshot.clone();
    bound["items"] = json!([observed]);
    merge_snapshot(d, &bound)?;
    d["sync"]["targetRefresh"][key]["observedAt"] = json!(now());
    d["sync"]["targetRefresh"][key]["completedAt"] = json!(now());
    d["sync"]["targetRefresh"][key]["error"] = Value::Null;
    Ok(())
}

async fn refresh_open_targets(
    app: &App,
    binding: &ConnectorBinding,
    account: &str,
) -> ApiResult<()> {
    let candidates = refresh_candidates(
        &app.db.read_source_status().await?, binding, chrono::Utc::now().timestamp());
    for target in candidates {
        let key = required(&target, "id")?.to_string();
        let claimed = app
            .change_source_claim(|d| {
                if active_binding(d)? != *binding {
                    return Err(conflict("Connector changed during target refresh"));
                }
                if !refresh_candidates(d, binding, chrono::Utc::now().timestamp())
                    .iter()
                    .any(|v| v["id"] == key)
                {
                    return Ok(false);
                }
                d["sync"]["targetRefresh"][&key] = json!({"attemptedAt":now(),"error":null});
                Ok(true)
            })
            .await?;
        if !claimed {
            continue;
        }
        let outcome = match app.bridge("context", json!({"account":account,"objectId":target["objectId"],"itemId":target["itemId"],"snapshot":true})).await {
            Ok(snapshot) => app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshot(&snapshot),|d| admit_exact_refresh(d, binding, &target, &snapshot)).await,
            Err(error) => Err(error),
        };
        if let Err(error) = outcome {
            app.change_schedule(|d| {
                if active_binding(d)? != *binding {
                    return Err(conflict("Connector changed during target refresh"));
                }
                d["sync"]["targetRefresh"][&key]["completedAt"] = json!(now());
                d["sync"]["targetRefresh"][&key]["error"] = json!(error.1);
                Ok(())
            })
            .await?;
        }
    }
    Ok(())
}

fn prepare(d: &mut Value, binding: &Value) -> Value {
    prepare_with_read_policy(d,binding,&omitted_read_policy())
}
fn prepare_with_read_policy(d:&mut Value,binding:&Value,policy:&Value)->Value {
    let old = &d["sync"]["scan"];
    if scan_can_resume(d,binding) {
        let mut resumed = old.clone();
        let mut recovered = serde_json::Map::new();
        for mode in ["open", "closed"] {
            if let Some(reason) = accounting_recovery_reason(&resumed[mode]) {
                recovered.insert(mode.into(), recovery_evidence(&resumed[mode], reason));
                let prior=resumed[mode].clone();resumed[mode] = fresh_lane(mode == "open");
                for field in ["readOptions","readCapabilities","readOptionSelection"] {
                    if let Some(value)=prior.get(field){resumed[mode][field]=value.clone();}
                }
            }
        }
        if resumed["open"]["scope"] != "all-open" {
            recovered.entry("open".to_string()).or_insert_with(||
                recovery_evidence(&resumed["open"], "LEGACY_OPEN_SCOPE"));
            let prior=resumed["open"].clone();resumed["open"] = fresh_lane(true);
            for field in ["readOptions","readCapabilities","readOptionSelection"] {
                if let Some(value)=prior.get(field){resumed["open"][field]=value.clone();}
            }
        }
        if !recovered.is_empty() {
            // Rotate the admission generation even if the old cursor was null.
            // Retain the unaffected lane and its exact closed-history window.
            let previous_id = resumed["id"].clone();
            resumed["id"] = json!(id());
            resumed["traversalComplete"] = json!(false);
            resumed["contextComplete"] = json!(false);
            resumed["coverageComplete"] = json!(false);
            d["sync"]["accountingRecovery"]["scan"] = json!({"at":now(),
                "fromId":previous_id,"toId":resumed["id"],"binding":binding,
                "version":ACCOUNTING_CHECKPOINT_VERSION,"lanes":recovered});
            let frontier = &mut d["sync"]["openFrontier"];
            if frontier["binding"] == *binding && frontier["scanId"] == previous_id {
                frontier["scanId"] = resumed["id"].clone();
            }
        }
        d["sync"]["scan"] = resumed.clone();
        d["sync"]["openCoverage"] = open_coverage(d);
        return resumed;
    }
    let until = chrono::Utc::now();
    let mut open=fresh_lane(true);pin_read_policy(&mut open,policy);
    let scan = json!({"id":id(),"binding":binding,"window":{"since":(until-chrono::Duration::hours(48)).to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"until":until.to_rfc3339_opts(chrono::SecondsFormat::Millis,true)},"open":open,"closed":fresh_lane(false),"skipped":0,"unknownDates":0,"traversalComplete":false,"snapshotConsistent":false,"seenIds":[]});
    d["sync"]["scan"] = scan.clone();
    d["sync"]["openCoverage"] = open_coverage(d);
    scan
}

fn prepare_open_frontier(d: &mut Value, binding: &ConnectorBinding) -> Value {
    prepare_open_frontier_with_read_policy(d,binding,&omitted_read_policy())
}
fn prepare_open_frontier_with_read_policy(d:&mut Value,binding:&ConnectorBinding,policy:&Value)->Value {
    let old = &d["sync"]["openFrontier"];
    if frontier_can_resume(d,binding) {
        let mut resumed = old.clone();
        if let Some(reason) = accounting_recovery_reason(old) {
            let evidence = recovery_evidence(old, reason);
            let previous_id = old["id"].clone();
            resumed = fresh_lane(true);
            resumed["id"] = json!(id());
            resumed["scanId"] = d["sync"]["scan"]["id"].clone();
            resumed["binding"] = binding.to_json();
            resumed["window"] = Value::Null;
            resumed["skipped"] = json!(0);
            resumed["unknownDates"] = json!(0);
            resumed["snapshotConsistent"] = json!(false);
            for field in ["readOptions","readCapabilities","readOptionSelection"] {
                if let Some(value)=old.get(field){resumed[field]=value.clone();}
            }
            d["sync"]["accountingRecovery"]["openFrontier"] = json!({"at":now(),
                "fromId":previous_id,"toId":resumed["id"],"binding":binding.to_json(),
                "scanId":resumed["scanId"],"version":ACCOUNTING_CHECKPOINT_VERSION,"lane":evidence});
        }
        d["sync"]["openFrontier"] = resumed.clone();
        d["sync"]["openCoverage"] = open_coverage(d);
        return resumed;
    }
    let mut frontier = json!({"id":id(),"scanId":d["sync"]["scan"]["id"],"binding":binding.to_json(),"scope":"all-open","window":null,"cursor":null,"done":false,"pages":0,"seenCursors":[],"skipped":0,"unknownDates":0,"snapshotConsistent":false,"accountingCheckpointVersion":ACCOUNTING_CHECKPOINT_VERSION});
    pin_read_policy(&mut frontier,policy);
    d["sync"]["openFrontier"] = frontier.clone();
    d["sync"]["openCoverage"] = open_coverage(d);
    frontier
}

fn admit_open_frontier(
    d: &mut Value,
    scan_id: &str,
    binding: &ConnectorBinding,
    frontier_id: &str,
    cursor: &Value,
    snapshot: &Value,
) -> ApiResult<()> {
    let f = &d["sync"]["openFrontier"];
    if active_binding(d)? != *binding
        || d["sync"]["scan"]["id"] != scan_id
        || d["sync"]["scan"]["binding"] != binding.to_json()
        || f["binding"] != binding.to_json()
        || f["scanId"] != scan_id
        || !d["sync"]["scan"]["invalidatedAt"].is_null()
        || f["id"] != frontier_id
        || f["cursor"] != *cursor
        || !f["invalidatedAt"].is_null()
    {
        return Err(conflict("Open frontier changed before admission"));
    }
    let more = snapshot["hasMore"]
        .as_bool()
        .ok_or_else(|| internal("Provider omitted pagination coverage"))?;
    let next = &snapshot["cursor"];
    if more
        && (next.as_str().is_none_or(|s| s.is_empty())
            || next == cursor
            || f["seenCursors"].as_array().unwrap().contains(next))
    {
        return Err(internal("Provider cursor did not advance"));
    }
    if !more && !next.is_null() {
        return Err(internal("Provider pagination is inconsistent"));
    }
    if snapshot.get("window").is_some() && snapshot["window"] != f["window"] {
        return Err(conflict("Provider window changed"));
    }
    let mut accounted = f.clone();
    update_accounting(&mut accounted, snapshot)?;
    merge_snapshot(d, snapshot)?;
    d["sync"]["openFrontier"] = accounted;
    let f = &mut d["sync"]["openFrontier"];
    if more {
        f["seenCursors"].as_array_mut().unwrap().push(next.clone());
    }
    f["cursor"] = next.clone();
    f["done"] = json!(!more);
    f["pages"] = json!(f["pages"].as_u64().unwrap_or(0) + 1);
    f["skipped"] = json!(
        f["skipped"].as_u64().unwrap_or(0)
            + snapshot["skipped"].as_array().map_or(0, |v| v.len() as u64)
    );
    f["unknownDates"] = json!(
        f["unknownDates"].as_u64().unwrap_or(0)
            + snapshot["unknownDateCount"].as_u64().unwrap_or(0)
    );
    mark_coverage(f);
    f["lastPageAt"] = json!(now());
    d["sync"]["openCoverage"] = open_coverage(d);
    Ok(())
}

async fn refresh_open_frontier(
    app: &App,
    scan_id: &str,
    binding: &ConnectorBinding,
    account: &str,
) -> ApiResult<()> {
    let state=app.db.read_metadata().await?;
    let policy=if frontier_can_resume(&state,binding){omitted_read_policy()}else{new_read_policy(app,binding,account).await};
    let f = app
        .change_schedule(|d| {
            if active_binding(d)? != *binding || d["sync"]["scan"]["id"] != scan_id {
                return Err(conflict("Sync scan superseded"));
            }
            Ok(prepare_open_frontier_with_read_policy(d, binding,&policy))
        })
        .await?;
    let frontier_id = required(&f, "id")?;
    let mut admitted_pages=0usize;
    while admitted_pages<PAGES_PER_MODE {
        let state = app.db.read_metadata().await?;
        let current = &state["sync"]["openFrontier"];
        if current["id"] != frontier_id || state["sync"]["scan"]["id"] != scan_id {
            return Err(conflict("Open frontier superseded"));
        }
        if current["done"] == true {
            break;
        }
        let cursor = current["cursor"].clone();
        let options=read_options(current)?;let capabilities=current["readCapabilities"].clone();
        let result = match stage_open_batch(app,binding,account,current,PAGES_PER_MODE-admitted_pages).await {
            Ok(snapshots) => {
                let count=snapshots.len();
                let result=app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshots(&snapshots),|d|
                    admit_open_batch(d,scan_id,binding,Some(frontier_id),&cursor,&options,&capabilities,&snapshots)).await;
                if result.is_ok(){admitted_pages+=count;}
                result
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            if let Some(reason) = checkpoint_failure_reason(&error) {
                app.change_schedule(|d| {
                    if active_binding(d)? == *binding
                        && d["sync"]["scan"]["id"] == scan_id
                        && d["sync"]["openFrontier"]["id"] == frontier_id
                        && d["sync"]["openFrontier"]["cursor"] == cursor
                    {
                        d["sync"]["openFrontier"]["invalidatedAt"] = json!(now());
                        d["sync"]["openFrontier"]["invalidationReason"] = json!(reason);
                        d["sync"]["openCoverage"] = open_coverage(d);
                    }
                    Ok(())
                })
                .await?;
            }
            return Err(error);
        }
    }
    Ok(())
}

fn checkpoint_failure_reason(error: &ApiError) -> Option<&'static str> {
    match error.1.as_str() {
        "Adapter failed (INVALID_CURSOR)" => Some("INVALID_CURSOR"),
        "Adapter failed (INVALID_PROVIDER_CURSOR)" => Some("INVALID_PROVIDER_CURSOR"),
        "Provider cursor did not advance" => Some("CURSOR_DID_NOT_ADVANCE"),
        "Provider pagination is inconsistent" => Some("INCONSISTENT_PAGINATION"),
        _ => None,
    }
}

fn invalidate_checkpoint(
    d: &mut Value,
    scan_id: &str,
    binding: &ConnectorBinding,
    mode: &str,
    expected_cursor: &Value,
    error: &ApiError,
) -> bool {
    let Some(reason) = checkpoint_failure_reason(error) else {
        return false;
    };
    // A delayed failure must never invalidate a replacement scan or a page
    // which another admitted run has already advanced.
    if active_binding(d).ok().as_ref() != Some(binding)
        || d["sync"]["scan"]["id"] != scan_id
        || d["sync"]["scan"][mode]["cursor"] != *expected_cursor
    {
        return false;
    }
    let at = now();
    d["sync"]["scan"]["invalidatedAt"] = json!(at);
    d["sync"]["scan"]["invalidationReason"] = json!(reason);
    d["sync"]["lastInvalidatedScan"] = json!({"id":scan_id,"at":at,"reason":reason,"mode":mode});
    d["sync"]["openCoverage"] = open_coverage(d);
    true
}

async fn record_checkpoint_failure(
    app: &App,
    scan_id: &str,
    binding: &ConnectorBinding,
    mode: &str,
    cursor: &Value,
    error: &ApiError,
) -> ApiResult<()> {
    if checkpoint_failure_reason(error).is_some() {
        // Separate successful transaction: mutations in a rejected page's
        // transaction are rolled back by the store.
        app.change_schedule(|d| {
            Ok(invalidate_checkpoint(
                d, scan_id, binding, mode, cursor, error,
            ))
        })
        .await?;
    }
    Ok(())
}

fn admit(
    d: &mut Value,
    scan_id: &str,
    binding: &ConnectorBinding,
    mode: &str,
    expected_cursor: &Value,
    snapshot: &Value,
) -> ApiResult<()> {
    if active_binding(d)? != *binding
        || d["sync"]["scan"]["id"] != scan_id
        || d["sync"]["scan"]["binding"] != binding.to_json()
        || !d["sync"]["scan"]["invalidatedAt"].is_null()
        || d["sync"]["scan"][mode]["cursor"] != *expected_cursor
    {
        return Err(conflict("Sync scan changed before page admission"));
    }
    let more = snapshot["hasMore"]
        .as_bool()
        .ok_or_else(|| internal("Provider omitted pagination coverage"))?;
    let next = &snapshot["cursor"];
    if more
        && (next.as_str().is_none_or(|s| s.is_empty())
            || next == expected_cursor
            || d["sync"]["scan"][mode]["seenCursors"]
                .as_array()
                .unwrap()
                .contains(next))
    {
        return Err(internal("Provider cursor did not advance"));
    }
    if !more && !next.is_null() {
        return Err(internal("Provider pagination is inconsistent"));
    }
    let expected_window = if mode == "closed" {
        d["sync"]["scan"]["window"].clone()
    } else {
        Value::Null
    };
    if snapshot.get("window").is_some() && snapshot["window"] != expected_window {
        return Err(conflict("Provider window changed"));
    }
    let mut accounted = d["sync"]["scan"][mode].clone();
    update_accounting(&mut accounted, snapshot)?;
    merge_snapshot(d, snapshot)?;
    d["sync"]["scan"][mode] = accounted;
    d["connectorBinding"] = binding.to_json();
    let scan = &mut d["sync"]["scan"];
    if more {
        scan[mode]["seenCursors"]
            .as_array_mut()
            .unwrap()
            .push(next.clone());
    }
    scan[mode]["cursor"] = next.clone();
    scan[mode]["done"] = json!(!more);
    scan[mode]["pages"] = json!(scan[mode]["pages"].as_u64().unwrap_or(0) + 1);
    scan["skipped"] = json!(
        scan["skipped"].as_u64().unwrap_or(0)
            + snapshot["skipped"].as_array().map_or(0, |v| v.len() as u64)
    );
    scan["unknownDates"] = json!(
        scan["unknownDates"].as_u64().unwrap_or(0)
            + snapshot["unknownDateCount"].as_u64().unwrap_or(0)
    );
    scan[mode]["skipped"] = json!(scan[mode]["skipped"].as_u64().unwrap_or(0)
        + snapshot["skipped"].as_array().map_or(0, |rows| rows.len() as u64));
    scan[mode]["unknownDates"] = json!(scan[mode]["unknownDates"].as_u64().unwrap_or(0)
        + snapshot["unknownDateCount"].as_u64().unwrap_or(0));
    mark_coverage(&mut scan[mode]);
    if scan["seenIds"].as_array().is_some_and(|seen|seen.len()>ACCOUNTING_TARGET_LIMIT) {
        scan["seenIdsOverflow"] = json!(true);
        scan["seenIds"].as_array_mut().unwrap().truncate(ACCOUNTING_TARGET_LIMIT);
    }
    for item in snapshot["items"]
        .as_array()
        .ok_or_else(|| internal("Provider omitted items"))?
    {
        let seen = scan["seenIds"].as_array_mut().unwrap();
        if !seen.contains(&item["id"]) {
            if seen.len() < ACCOUNTING_TARGET_LIMIT {
                seen.push(item["id"].clone());
            } else {
                scan["seenIdsOverflow"] = json!(true);
            }
        }
    }
    let done = scan["open"]["done"] == true && scan["closed"]["done"] == true;
    scan["traversalComplete"] = json!(done);
    scan["contextComplete"] = json!(context_complete(&scan["open"]) && context_complete(&scan["closed"]));
    scan["coverageComplete"] = json!(scan["open"]["coverageComplete"] == true && scan["closed"]["coverageComplete"] == true);
    scan["lastPageAt"] = json!(now());
    // Keep normal page controls compatible without confusing them with the
    // separately bound rolling-window scan cursor.
    d["sync"]["lastSyncedAt"] = json!(now());
    if mode == "open" { d["sync"]["openCoverage"] = open_coverage(d); }
    Ok(())
}

/// The initial all-open walk and later refreshes share one explicit consumer
/// contract. An old completed frontier must not certify a newer pending scan.
fn open_coverage(d: &Value) -> Value {
    let scan=&d["sync"]["scan"];
    let frontier=&d["sync"]["openFrontier"];
    let refresh=scan["open"]["done"]==true && frontier.is_object()
        && frontier["scanId"]==scan["id"];
    let source=if refresh {frontier} else {&scan["open"]};
    let skipped=source["skipped"].as_u64().unwrap_or_else(||scan["skipped"].as_u64().unwrap_or(0));
    let unknown=source["unknownDates"].as_u64().unwrap_or_else(||scan["unknownDates"].as_u64().unwrap_or(0));
    let valid=source["scope"]=="all-open" && scan["invalidatedAt"].is_null()
        && source["invalidatedAt"].is_null();
    json!({"id":if refresh {frontier["id"].clone()} else {scan["id"].clone()},
        "scope":source["scope"],"scanId":scan["id"],"cursor":source["cursor"],
        "done":source["done"],"pages":source["pages"],"skipped":skipped,"unknownDates":unknown,
        "invalidatedAt":if scan["invalidatedAt"].is_null(){source["invalidatedAt"].clone()}else{scan["invalidatedAt"].clone()},
        "traversalComplete":valid && source["done"]==true,
        "contextComplete":valid && context_complete(source),"snapshotConsistent":false,
        "accounting":accounting_summary(source),
        "coverageComplete":valid && source["done"]==true && context_complete(source) && unknown==0})
}

fn accounting_summary(lane: &Value) -> Value {
    let a = &lane["accounting"];
    json!({"version":a["version"],"limit":ACCOUNTING_TARGET_LIMIT,
        "trackedUnique":a["trackedUnique"],"importedUnique":a["importedUnique"],
        "unresolvedUnique":a["unresolvedUnique"],"overflow":a["overflow"],
        "unverifiedPages":a["unverifiedPages"],"observations":a["observations"],
        "duplicateObservations":a["duplicateObservations"],"excludedObservations":a["excludedObservations"],
        "untrackedObservations":a["untrackedObservations"]})
}

async fn refresh_heads(app: &App, binding: &ConnectorBinding, account: &str) -> ApiResult<()> {
    // Poll both open heads because provider direction is not a verified creation
    // order. These cursorless observations cannot rewind the durable backlog.
    // Full traversal remains necessary; a head is never proof of full coverage.
    {
        let until = chrono::Utc::now();
        let window = json!({"since":(until-chrono::Duration::hours(48)).to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"until":until.to_rfc3339_opts(chrono::SecondsFormat::Millis,true)});
        for (mode, reverse) in [("open", false), ("open", true), ("closed", true)] {
            let mut args = json!({"account":account,"mode":mode,"reverse":reverse,"binding":binding.to_json()});
            if mode == "closed" {
                args["window"] = window.clone();
            }
            let mut page = app.bridge("read", args).await?;
            for item in page["items"]
                .as_array_mut()
                .ok_or_else(|| internal("Provider omitted items"))?
            {
                *item = bound_item(&binding, item)?;
            }
            app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshot(&page),|d| {
                if active_binding(d)? != *binding {return Err(conflict("Connector changed during head refresh"));}
                merge_snapshot(d,&page)?;
                let observation=json!({"at":now(),"scope":if mode=="open" {"all-open"}else{"dated-closed-history"},"reverse":reverse,"window":if mode=="closed" {window.clone()}else{Value::Null},"coverage":page["coverage"],"hasMore":page["hasMore"]});
                d["sync"]["frontier"][mode]=observation.clone();
                d["sync"]["heads"][mode][if reverse {"reverse"}else{"forward"}]=observation;
                Ok(())
            }).await?;
        }
    }
    Ok(())
}

pub(super) async fn run(app: App) -> ApiResult<Value> {
    let state=app.db.read_metadata().await?;
    let binding = active_binding(&state)?;
    let account = bridge_account(&binding)?;
    let policy=if scan_can_resume(&state,&binding.to_json()){omitted_read_policy()}else{new_read_policy(&app,&binding,account).await};
    let scan = app.change_schedule(|d| {
        if active_binding(d)? != binding { return Err(conflict("Connector changed before sync preparation")); }
        Ok(prepare_with_read_policy(d, &binding.to_json(),&policy))
    }).await?;
    let scan_id = required(&scan, "id")?.to_string();
    // Once the initial open traversal ends, keep rescanning its own fresh,
    // resumable unwindowed queue independently of the still-running closed history.
    // Heads below still admit arrivals on every successful bounded open turn.
    if scan["open"]["done"] == true {
        refresh_open_frontier(&app, &scan_id, &binding, account).await?;
    }
    let mut head_error = None;
    for mode in ["open", "closed"] {
        if mode == "closed" {
            // Commit the primary open work first. A failing auxiliary head must
            // neither rewind it nor prevent the closed lane from getting a turn.
            let heads = refresh_heads(&app, &binding, account).await;
            app.change_schedule(|d| {
                if active_binding(d)? != binding { return Err(conflict("Connector changed after head refresh")); }
                d["sync"]["auxiliaryHeads"] = json!({"attemptedAt":now(),
                    "status":if heads.is_ok(){"completed"}else{"error"},
                    "error":heads.as_ref().err().map(|e|e.1.clone())});
                Ok(())
            }).await?;
            head_error = heads.err();
            refresh_open_targets(&app, &binding, account).await?;
        }
        let page_limit=if mode=="closed"{CLOSED_PAGES_PER_RUN}else{PAGES_PER_MODE};
        let mut admitted_pages=0usize;
        while admitted_pages<page_limit {
            let state = app.db.read_metadata().await?;
            if state["sync"]["scan"]["id"] != scan_id
                || !state["sync"]["scan"]["invalidatedAt"].is_null()
            {
                return Err(conflict("Sync scan superseded"));
            }
            if state["sync"]["scan"][mode]["done"] == true {
                break;
            }
            let cursor = state["sync"]["scan"][mode]["cursor"].clone();
            if mode=="open" {
                let lane=&state["sync"]["scan"]["open"];
                let options=read_options(lane)?;let capabilities=lane["readCapabilities"].clone();
                let outcome=match stage_open_batch(&app,&binding,account,lane,page_limit-admitted_pages).await {
                    Ok(snapshots)=>{
                        let count=snapshots.len();
                        let outcome=app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshots(&snapshots),|d|
                            admit_open_batch(d,&scan_id,&binding,None,&cursor,&options,&capabilities,&snapshots)).await;
                        if outcome.is_ok(){admitted_pages+=count;}
                        outcome
                    },
                    Err(error)=>Err(error),
                };
                if let Err(error)=outcome {
                    record_checkpoint_failure(&app,&scan_id,&binding,mode,&cursor,&error).await?;return Err(error);
                }
                continue;
            }
            let mut args = json!({"account":account,"mode":mode,"binding":binding.to_json()});
            if mode == "closed" {
                args["window"] = scan["window"].clone();
            }
            if !cursor.is_null() {
                args["cursor"] = cursor.clone();
            }
            let mut snapshot = match app.bridge("read", args).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    record_checkpoint_failure(&app, &scan_id, &binding, mode, &cursor, &error)
                        .await?;
                    return Err(error);
                }
            };
            for item in snapshot["items"]
                .as_array_mut()
                .ok_or_else(|| internal("Provider omitted items"))?
            {
                *item = bound_item(&binding, item)?;
            }
            if let Err(error) = app
                .change_source_snapshot_scoped(storage::SourceReadIntent::Snapshot(&snapshot),|d| admit(d, &scan_id, &binding, mode, &cursor, &snapshot))
                .await
            {
                record_checkpoint_failure(&app, &scan_id, &binding, mode, &cursor, &error).await?;
                return Err(error);
            }
            admitted_pages+=1;
        }
    }
    if let Some(error) = head_error { return Err(error); }
    let state = app.change_schedule(|d| {
        d["sync"]["openCoverage"] = open_coverage(d);
        Ok(d["sync"].clone())
    }).await?;
    Ok(result_summary(&state))
}

fn result_summary(state: &Value) -> Value {
    let scan = &state["scan"];
    let frontier = &state["openFrontier"];
    // A retained frontier belongs to its own scan and connector. It cannot
    // qualify a new initial traversal, including a scan completed in one turn.
    let current_frontier = frontier.is_object() && scan["open"]["done"] == true
        && frontier["scanId"] == scan["id"] && frontier["binding"] == scan["binding"]
        && frontier["scope"] == "all-open" && frontier["window"].is_null();
    let frontier_partial = current_frontier
        && (frontier["coverageComplete"] != true || !frontier["invalidatedAt"].is_null());
    let open_frontier = if current_frontier {
        let valid = frontier["invalidatedAt"].is_null();
        json!({"scope":"all-open","id":frontier["id"],"window":frontier["window"],
            "done":frontier["done"],"pages":frontier["pages"],"skipped":frontier["skipped"],
            "unknownDates":frontier["unknownDates"],"traversalComplete":valid && frontier["traversalComplete"]==true,
            "contextComplete":valid && frontier["contextComplete"]==true,"accounting":accounting_summary(frontier),
            "coverageComplete":valid && frontier["coverageComplete"]==true})
    } else { Value::Null };
    json!({"synced":true,"scanId":scan["id"],"partial":frontier_partial||scan["coverageComplete"]!=true,
        "traversalComplete":scan["traversalComplete"],"contextComplete":scan["contextComplete"],
        "coverageComplete":scan["coverageComplete"],"window":scan["window"],"openScope":"all-open",
        "closedWindow":scan["window"],"openFrontier":open_frontier,"snapshotConsistent":false})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_legacy(lane: &mut Value) {
        lane.as_object_mut().unwrap().remove("accountingCheckpointVersion");
        lane.as_object_mut().unwrap().remove("accounting");
        lane["pages"] = json!(87);
        lane["cursor"] = json!("legacy-page-88");
        lane["seenCursors"] = json!(["legacy-page-88"]);
    }

    #[test]
    fn legacy_scan_recovers_only_affected_lane_preserves_workspace_and_rejects_late_pages() {
        let (mut d, b, old_id) = start();
        admit(&mut d, &old_id, &b, "closed", &Value::Null,
            &accounted_page(json!({"items":[],"hasMore":true,"cursor":"valid-closed"}))).unwrap();
        make_legacy(&mut d["sync"]["scan"]["open"]);
        d["items"] = json!([{"id":"retained","draft":"owner text","workflow":"prepared"}]);
        d["operations"] = json!([{"id":"uncertain","status":"UNKNOWN"}]);
        d["approvals"] = json!([{"id":"approved","target":"retained"}]);
        let before = d.clone();
        let resumed = prepare(&mut d, &b.to_json());
        assert_ne!(resumed["id"], old_id);
        assert_eq!(resumed["open"]["pages"], 0);
        assert!(resumed["open"]["cursor"].is_null());
        assert_eq!(resumed["closed"], before["sync"]["scan"]["closed"]);
        assert_eq!(resumed["window"], before["sync"]["scan"]["window"]);
        for key in ["items", "operations", "approvals"] { assert_eq!(d[key], before[key]); }
        assert_eq!(d["sync"]["accountingRecovery"]["scan"]["lanes"]["open"]["reason"], "MISSING_CHECKPOINT_ACCOUNTING");
        assert_eq!(d["sync"]["accountingRecovery"]["scan"]["lanes"]["open"]["pages"], 87);
        let recovered = d.clone();
        for cursor in [Value::Null, json!("legacy-page-88")] {
            assert!(admit(&mut d, &old_id, &b, "open", &cursor,
                &json!({"items":[{"id":"late"}],"hasMore":false,"cursor":null})).is_err());
            assert!(!invalidate_checkpoint(&mut d, &old_id, &b, "open", &cursor,
                &internal("Adapter failed (INVALID_CURSOR)")));
            assert_eq!(d, recovered);
        }
        assert_eq!(prepare(&mut d, &b.to_json()), resumed);
    }

    #[test]
    fn legacy_frontier_recovers_once_even_when_provider_never_supplies_evidence() {
        let (mut d, b, scan_id) = start();
        let original = prepare_open_frontier(&mut d, &b);
        make_legacy(&mut d["sync"]["openFrontier"]);
        let scan_before = d["sync"]["scan"].clone();
        let recovered = prepare_open_frontier(&mut d, &b);
        assert_ne!(recovered["id"], original["id"]);
        assert_eq!(d["sync"]["scan"], scan_before);
        let frontier_id = recovered["id"].as_str().unwrap();
        let receipt = d["sync"]["accountingRecovery"].clone();
        let before = d.clone();
        assert!(admit_open_frontier(&mut d, &scan_id, &b, original["id"].as_str().unwrap(),
            &Value::Null, &json!({"items":[],"hasMore":false,"cursor":null})).is_err());
        assert_eq!(d, before);
        for page in 0..3 {
            let cursor = d["sync"]["openFrontier"]["cursor"].clone();
            admit_open_frontier(&mut d, &scan_id, &b, frontier_id, &cursor,
                &json!({"items":[],"hasMore":true,"cursor":format!("next-{page}")})).unwrap();
            d = serde_json::from_str(&d.to_string()).unwrap();
            let resumed = prepare_open_frontier(&mut d, &b);
            assert_eq!(resumed["id"], frontier_id);
            assert_eq!(resumed["pages"], page + 1);
            assert_eq!(resumed["contextComplete"], false);
            assert_eq!(d["sync"]["accountingRecovery"], receipt);
        }
    }

    #[test]
    fn compatible_pre_upgrade_and_new_checkpoints_resume_after_serialization() {
        for old_format in [false, true] {
            let (mut d, b, scan_id) = start();
            admit(&mut d, &scan_id, &b, "open", &Value::Null,
                &accounted_page(json!({"items":[],"hasMore":true,"cursor":"current"}))).unwrap();
            let frontier = prepare_open_frontier(&mut d, &b);
            admit_open_frontier(&mut d, &scan_id, &b, frontier["id"].as_str().unwrap(), &Value::Null,
                &accounted_page(json!({"items":[],"hasMore":true,"cursor":"current-frontier"}))).unwrap();
            if old_format {
                d["sync"]["scan"]["open"].as_object_mut().unwrap().remove("accountingCheckpointVersion");
                d["sync"]["openFrontier"].as_object_mut().unwrap().remove("accountingCheckpointVersion");
            }
            d = serde_json::from_str(&d.to_string()).unwrap();
            let before = d.clone();
            assert_eq!(prepare(&mut d, &b.to_json()), before["sync"]["scan"]);
            assert_eq!(prepare_open_frontier(&mut d, &b), before["sync"]["openFrontier"]);
            assert!(d["sync"]["accountingRecovery"].is_null());
        }
    }

    #[tokio::test]
    async fn accounting_recovery_and_current_progress_survive_database_reopen() {
        let (mut fixture, b, original_id) = start();
        admit(&mut fixture, &original_id, &b, "open", &Value::Null,
            &accounted_page(json!({"items":[],"hasMore":true,"cursor":"valid-after-restart"}))).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("workspace.sqlite");
        let db = Database::Sqlite(open_db(&path).await.unwrap());
        db.change(|d| { d["sync"] = fixture["sync"].clone(); Ok(()) }).await.unwrap();
        db.close().await;
        let db = Database::Sqlite(open_db(&path).await.unwrap());
        let resumed = db.change(|d| Ok(prepare(d, &b.to_json()))).await.unwrap();
        assert_eq!(resumed["id"], original_id);
        assert_eq!(resumed["open"]["cursor"], "valid-after-restart");
        let repaired = db.change(|d| {
            make_legacy(&mut d["sync"]["scan"]["closed"]);
            Ok(prepare(d, &b.to_json()))
        }).await.unwrap();
        assert_ne!(repaired["id"], original_id);
        let receipt = db.read_metadata().await.unwrap()["sync"]["accountingRecovery"].clone();
        db.close().await;
        let db = Database::Sqlite(open_db(&path).await.unwrap());
        assert_eq!(db.change(|d| Ok(prepare(d, &b.to_json()))).await.unwrap(), repaired);
        assert_eq!(db.read_metadata().await.unwrap()["sync"]["accountingRecovery"], receipt);
        assert_eq!(repaired["open"]["cursor"], "valid-after-restart");
        db.close().await;
    }

    #[test]
    fn malformed_and_midpass_accounting_recover_without_resetting_pristine_lanes() {
        for variation in 0..7 {
            let (mut d, b, old_id) = start();
            admit(&mut d, &old_id, &b, "closed", &Value::Null,
                &accounted_page(json!({"items":[],"hasMore":true,"cursor":"old"}))).unwrap();
            let lane = &mut d["sync"]["scan"]["closed"];
            match variation {
                0 => lane["accounting"] = Value::Null,
                1 => lane["accounting"]["version"] = json!(2),
                2 => lane["accounting"]["version"] = json!("1"),
                3 => lane["accounting"]["records"] = json!({}),
                4 => lane["accounting"]["unverifiedPages"] = json!(-1),
                5 => lane["accounting"]["trackedUnique"] = json!(99),
                _ => {
                    lane["accounting"]["unverifiedPages"] = json!(1);
                    lane.as_object_mut().unwrap().remove("accountingCheckpointVersion");
                }
            }
            let open_before = d["sync"]["scan"]["open"].clone();
            let resumed = prepare(&mut d, &b.to_json());
            assert_ne!(resumed["id"], old_id, "variation {variation}");
            assert_eq!(resumed["open"], open_before);
            assert_eq!(resumed["closed"]["pages"], 0);
            let scan_id = resumed["id"].as_str().unwrap();
            // Recovery is useful, not just a reset: a verified replay can finish.
            admit(&mut d, scan_id, &b, "closed", &Value::Null,
                &accounted_page(json!({"items":[],"hasMore":false,"cursor":null}))).unwrap();
            assert_eq!(d["sync"]["scan"]["closed"]["coverageComplete"], true);
            assert_eq!(prepare(&mut d, &b.to_json())["id"], scan_id);
        }
    }

    #[test]
    fn recovered_scan_keeps_valid_frontier_but_never_accepts_old_generation_or_binding() {
        let (mut d, b, old_id) = start();
        admit(&mut d, &old_id, &b, "open", &Value::Null,
            &accounted_page(json!({"items":[],"hasMore":false,"cursor":null}))).unwrap();
        let frontier = prepare_open_frontier(&mut d, &b);
        let frontier_id = frontier["id"].as_str().unwrap();
        admit_open_frontier(&mut d, &old_id, &b, frontier_id, &Value::Null,
            &accounted_page(json!({"items":[],"hasMore":true,"cursor":"valid"}))).unwrap();
        let frontier_before = d["sync"]["openFrontier"].clone();
        make_legacy(&mut d["sync"]["scan"]["closed"]);
        let resumed = prepare(&mut d, &b.to_json());
        let mut expected = frontier_before;
        expected["scanId"] = resumed["id"].clone();
        assert_eq!(prepare_open_frontier(&mut d, &b), expected);
        let before = d.clone();
        assert!(admit_open_frontier(&mut d, &old_id, &b, frontier_id, &json!("valid"),
            &json!({"items":[],"hasMore":false,"cursor":null})).is_err());
        assert_eq!(d, before);
        d["sync"]["openFrontier"]["binding"] = json!({"foreign":"company"});
        let before = d.clone();
        assert!(admit_open_frontier(&mut d, resumed["id"].as_str().unwrap(), &b, frontier_id,
            &json!("valid"), &json!({"items":[],"hasMore":false,"cursor":null})).is_err());
        assert_eq!(d, before);
        let new_frontier = prepare_open_frontier(&mut d, &b);
        assert_ne!(new_frontier["id"], frontier_id);
        assert_eq!(new_frontier["binding"], b.to_json());
    }

    fn accounted_page(mut page: Value) -> Value {
        let observations: Vec<Value> = list(&page,"items").iter().map(|item|
            json!({"objectId":item["objectId"],"itemId":item["itemId"],"contextRequired":true})).collect();
        page["queueAccounting"] = json!({"version":1,"observations":observations,"duplicateQueueCount":0});
        if page["skipped"].is_null() {page["skipped"] = json!([]);}
        page
    }
    fn certify_empty_lane(lane: &mut Value) {
        lane["accounting"] = Value::Null;
        // Fixture represents a verified empty traversal, not a legacy checkpoint.
        lane.as_object_mut().unwrap().remove("accounting");
        let pages = lane["pages"].clone();
        lane["pages"] = json!(0);
        update_accounting(lane,&accounted_page(json!({"items":[]}))).unwrap();
        lane["pages"] = pages;
        mark_coverage(lane);
    }
    fn accounting_page(ids: &[&str], failed: Option<&str>) -> Value {
        let observations: Vec<Value> = ids.iter().map(|key|json!({"objectId":"11391","itemId":key,"contextRequired":true})).collect();
        let items: Vec<Value> = ids.iter().filter(|key|Some(**key)!=failed)
            .map(|key|json!({"objectId":"11391","itemId":key})).collect();
        let skipped: Vec<Value> = failed.into_iter().map(|key|json!({"objectId":"11391","itemId":key,"code":"TARGET_IDENTITY_MISMATCH"})).collect();
        json!({"items":items,"skipped":skipped,"queueAccounting":{"version":1,"observations":observations,"duplicateQueueCount":0}})
    }
    #[test]
    fn scoped_context_failure_prevents_exhaustion_claim_and_overlap_retry_resolves_it() {
        let mut lane=json!({"pages":0,"done":true});
        update_accounting(&mut lane,&accounting_page(&["same","bad"],Some("bad"))).unwrap();
        mark_coverage(&mut lane);
        assert_eq!(lane["traversalComplete"],true);
        assert_eq!(lane["contextComplete"],false);
        assert_eq!(lane["coverageComplete"],false);
        assert_eq!(lane["accounting"]["records"][1],json!({"objectId":"11391","itemId":"bad","contextStatus":"failed","reason":"TARGET_IDENTITY_MISMATCH","lastFailureReason":"TARGET_IDENTITY_MISMATCH","failedObservations":1}));
        update_accounting(&mut lane,&accounting_page(&["same","bad"],None)).unwrap();
        mark_coverage(&mut lane);
        assert_eq!(lane["accounting"]["trackedUnique"],2);
        assert_eq!(lane["accounting"]["duplicateObservations"],2);
        assert_eq!(lane["accounting"]["records"][1]["lastFailureReason"],"TARGET_IDENTITY_MISMATCH");
        assert_eq!(lane["coverageComplete"],true);
        assert_eq!(lane["snapshotConsistent"],false);
        // Moving pagination and a later failed duplicate do not undo evidence
        // that this ID was successfully read once in this non-atomic traversal.
        update_accounting(&mut lane,&accounting_page(&["bad"],Some("bad"))).unwrap();
        mark_coverage(&mut lane);
        assert_eq!(lane["contextComplete"],true);
        assert_eq!(lane["accounting"]["records"][1]["failedObservations"],2);
        assert_eq!(lane["snapshotConsistent"],false);
    }
    #[test]
    fn accounting_rejects_wrong_scope_unrequested_context_or_ambiguous_page_before_mutation() {
        let (mut d,b,scan_id)=start();
        for variation in 0..3 {
            let mut page=accounting_page(&["requested"],None);
            page["hasMore"]=json!(false);page["cursor"]=Value::Null;
            if variation==0 {page["items"][0]["objectId"]=json!("other");}
            if variation==1 {page["items"][0]["itemId"]=json!("foreign");}
            if variation==2 {let observation=page["queueAccounting"]["observations"][0].clone();page["queueAccounting"]["observations"].as_array_mut().unwrap().push(observation);}
            let before=d.clone();
            assert!(admit(&mut d,&scan_id,&b,"open",&Value::Null,&page).is_err());
            assert_eq!(d,before);
        }
    }
    #[test]
    fn accounting_cap_and_legacy_evidence_stay_incomplete_and_reset_with_new_pass() {
        let mut lane=json!({"pages":0,"done":true});
        for offset in (0..ACCOUNTING_TARGET_LIMIT+1000).step_by(1000) {
            let ids: Vec<String>=(offset..offset+1000).map(|n|format!("id-{n}")).collect();
            let refs: Vec<&str>=ids.iter().map(String::as_str).collect();
            update_accounting(&mut lane,&accounting_page(&refs,None)).unwrap();
        }
        mark_coverage(&mut lane);
        assert_eq!(lane["accounting"]["records"].as_array().unwrap().len(),ACCOUNTING_TARGET_LIMIT);
        assert_eq!(lane["accounting"]["untrackedObservations"],1000);
        assert_eq!(lane["accounting"]["overflow"],true);
        assert_eq!(lane["coverageComplete"],false);
        let mut legacy=json!({"pages":1,"done":true});
        update_accounting(&mut legacy,&accounted_page(json!({"items":[]}))).unwrap();
        mark_coverage(&mut legacy);
        assert_eq!(legacy["accounting"]["unverifiedPages"],1);
        assert_eq!(legacy["contextComplete"],false);
        let (mut d,b,scan_id)=start();
        d["sync"]["openFrontier"]=json!({"id":"old","scanId":scan_id,"binding":b.to_json(),"done":true,"accounting":lane["accounting"]});
        let fresh=prepare_open_frontier(&mut d,&b);
        assert!(fresh["accounting"].is_null());
        update_accounting(&mut d["sync"]["openFrontier"],&accounted_page(json!({"items":[]}))).unwrap();
        assert_eq!(d["sync"]["openFrontier"]["accounting"]["trackedUnique"],0);
        assert_eq!(d["sync"]["openFrontier"]["accounting"]["overflow"],false);
    }
    #[test]
    fn expanded_accounting_tracks_3001_after_restart_and_repairs_unresolved_duplicate() {
        let mut lane=fresh_lane(true);
        for offset in (0..3000).step_by(1000) {
            let ids:Vec<String>=(offset..offset+1000).map(|n|format!("id-{n}")).collect();
            let refs:Vec<&str>=ids.iter().map(String::as_str).collect();
            update_accounting(&mut lane,&accounting_page(&refs,None)).unwrap();
            lane["pages"]=json!(offset/1000+1);
            lane=serde_json::from_str(&lane.to_string()).unwrap();
            assert!(accounting_recovery_reason(&lane).is_none());
        }
        update_accounting(&mut lane,&accounting_page(&["last"],Some("last"))).unwrap();
        lane["done"]=json!(true);mark_coverage(&mut lane);
        assert_eq!(lane["accounting"]["trackedUnique"],3001);
        assert_eq!(lane["accounting"]["unresolvedUnique"],1);
        assert_eq!(lane["coverageComplete"],false);
        lane=serde_json::from_str(&lane.to_string()).unwrap();
        update_accounting(&mut lane,&accounting_page(&["id-0","last"],None)).unwrap();
        mark_coverage(&mut lane);
        assert_eq!(lane["accounting"]["trackedUnique"],3001);
        assert_eq!(lane["accounting"]["duplicateObservations"],2);
        assert_eq!(lane["coverageComplete"],true);
        assert_eq!(lane["snapshotConsistent"],false);
    }

    #[test]
    fn expanded_budget_preserves_progress_and_recovers_old_overflow_only_once() {
        let (mut d,b,scan)=start();
        let mut lane=fresh_lane(true);
        update_accounting(&mut lane,&accounting_page(&["kept"],None)).unwrap();
        lane["pages"]=json!(17);lane["cursor"]=json!("kept-cursor");
        lane["accounting"]["limit"]=json!(2000);
        d["sync"]["scan"]["open"]=lane.clone();
        assert_eq!(prepare(&mut d,&b.to_json())["open"],lane);
        assert_eq!(d["sync"]["scan"]["id"],scan);
        // Persisted overflow at the old budget requires one fresh evidence pass.
        d["sync"]["scan"]["open"]["accounting"]["overflow"]=json!(true);
        d["sync"]["scan"]["open"]["accounting"]["untrackedObservations"]=json!(1);
        let recovered=prepare(&mut d,&b.to_json());
        assert_ne!(recovered["id"],scan);
        assert_eq!(recovered["open"]["pages"],0);
        assert_eq!(d["sync"]["accountingRecovery"]["scan"]["lanes"]["open"]["reason"],"EXPANDED_ACCOUNTING_BUDGET");
        let mut overflow=lane;overflow["accounting"]["limit"]=json!(ACCOUNTING_TARGET_LIMIT);
        overflow["accounting"]["overflow"]=json!(true);
        overflow["accounting"]["untrackedObservations"]=json!(1);
        d["sync"]["scan"]["open"]=overflow.clone();
        for _ in 0..3 {
            d=serde_json::from_str(&d.to_string()).unwrap();
            let next=prepare(&mut d,&b.to_json());
            assert_eq!(next["id"],recovered["id"]);
            assert_eq!(next["open"],overflow);
        }
    }

    #[test]
    fn accounting_budget_metadata_size_is_bounded_at_maximum_identity_width() {
        let mut lane=fresh_lane(true);
        for offset in (0..ACCOUNTING_TARGET_LIMIT).step_by(1000) {
            let ids:Vec<String>=(offset..offset+1000).map(|n|format!("{:0>200}",n)).collect();
            let refs:Vec<&str>=ids.iter().map(String::as_str).collect();
            let mut page=accounting_page(&refs,None);
            for key in ["items"] {for row in page[key].as_array_mut().unwrap(){row["objectId"]=json!("o".repeat(200));}}
            for row in page["queueAccounting"]["observations"].as_array_mut().unwrap(){row["objectId"]=json!("o".repeat(200));}
            update_accounting(&mut lane,&page).unwrap();
        }
        lane["pages"]=json!(10);lane["done"]=json!(true);mark_coverage(&mut lane);
        assert_eq!(lane["accounting"]["trackedUnique"],ACCOUNTING_TARGET_LIMIT);
        assert_eq!(lane["coverageComplete"],true);
        let clock=std::time::Instant::now();
        let encoded=serde_json::to_vec(&lane).unwrap();
        let reloaded:Value=serde_json::from_slice(&encoded).unwrap();
        let copied=reloaded.clone();
        assert!(accounting_recovery_reason(&copied).is_none());
        assert!(encoded.len()<6*1024*1024);
        eprintln!("accounting_budget_benchmark limit={} bytes={} serialize_reload_clone_validate_ms={}",ACCOUNTING_TARGET_LIMIT,encoded.len(),clock.elapsed().as_millis());
    }

    #[test]
    fn window_exclusions_do_not_consume_context_identity_budget() {
        let mut lane=json!({"pages":0,"done":true});
        let mut page=accounting_page(&["old"],None);
        page["items"]=json!([]);
        page["queueAccounting"]["observations"][0]["contextRequired"]=json!(false);
        update_accounting(&mut lane,&page).unwrap();mark_coverage(&mut lane);
        assert_eq!(lane["accounting"]["excludedObservations"],1);
        assert_eq!(lane["accounting"]["trackedUnique"],0);
        assert_eq!(lane["contextComplete"],true);
    }
    #[test]
    fn initial_and_refreshed_open_coverage_never_reuse_an_old_scan() {
        let mut d=json!({"sync":{"scan":{"id":"current","open":{"scope":"all-open","done":true,"pages":1,"skipped":0,"unknownDates":0},"skipped":2,"unknownDates":1}}});
        certify_empty_lane(&mut d["sync"]["scan"]["open"]);
        assert_eq!(open_coverage(&d)["coverageComplete"],true);
        d["sync"]["openFrontier"]=json!({"id":"stale","scanId":"old","scope":"all-open","done":true,"pages":1,"skipped":0,"unknownDates":0});
        certify_empty_lane(&mut d["sync"]["openFrontier"]);
        d["sync"]["scan"]["open"]["done"]=json!(false);
        d["sync"]["scan"]["open"]["cursor"]=json!("next");
        assert_eq!(open_coverage(&d)["done"],false);
        assert_eq!(open_coverage(&d)["cursor"],"next");
        assert_eq!(open_coverage(&d)["coverageComplete"],false);
        d["sync"]["scan"]["open"]["done"]=json!(true);
        d["sync"]["openFrontier"]["scanId"]=json!("current");
        d["sync"]["openFrontier"]["done"]=json!(false);
        assert_eq!(open_coverage(&d)["coverageComplete"],false);
        d["sync"]["openFrontier"]["done"]=json!(true);
        assert_eq!(open_coverage(&d)["coverageComplete"],true);
        d["sync"]["openFrontier"]["accounting"]["unresolvedUnique"]=json!(1);
        assert_eq!(open_coverage(&d)["coverageComplete"],false);
    }
    #[test]
    fn public_open_coverage_tracks_pending_frontier_and_each_admitted_page() {
        let (mut d,b,scan_id)=start();
        admit(&mut d,&scan_id,&b,"open",&Value::Null,
            &accounted_page(json!({"items":[],"hasMore":false,"cursor":null}))).unwrap();
        assert_eq!(crate::bootstrap_view(d.clone(),"csrf")["sync"]["openCoverage"]["coverageComplete"],true);
        let frontier=prepare_open_frontier(&mut d,&b);
        let frontier_id=frontier["id"].as_str().unwrap();
        let public=crate::bootstrap_view(d.clone(),"csrf");
        let pending=&public["sync"]["openCoverage"];
        assert_eq!(pending["id"],frontier["id"]);
        assert_eq!(pending["done"],false);
        assert_eq!(pending["pages"],0);
        assert_eq!(pending["coverageComplete"],false);
        admit_open_frontier(&mut d,&scan_id,&b,frontier_id,&Value::Null,
            &accounted_page(json!({"items":[],"hasMore":true,"cursor":"next"}))).unwrap();
        assert_eq!(d["sync"]["openCoverage"]["pages"],1);
        assert_eq!(d["sync"]["openCoverage"]["cursor"],"next");
        assert_eq!(d["sync"]["openCoverage"]["coverageComplete"],false);
        admit_open_frontier(&mut d,&scan_id,&b,frontier_id,&json!("next"),
            &accounted_page(json!({"items":[],"hasMore":false,"cursor":null}))).unwrap();
        assert_eq!(d["sync"]["openCoverage"]["pages"],2);
        assert_eq!(d["sync"]["openCoverage"]["coverageComplete"],true);
        d["sync"]["scan"]["closed"]["done"]=json!(true);
        d["sync"]["scan"]["traversalComplete"]=json!(true);
        let new_scan=prepare(&mut d,&b.to_json());
        assert_ne!(new_scan["id"],scan_id);
        assert_eq!(d["sync"]["openCoverage"]["scanId"],new_scan["id"]);
        assert_eq!(d["sync"]["openCoverage"]["pages"],0);
        assert_eq!(d["sync"]["openCoverage"]["coverageComplete"],false);
    }
    #[test]
    fn scheduler_resumes_backlog_but_incomplete_evidence_does_not_busy_loop() {
        let (mut d, _, _) = start();
        let at = chrono::Utc::now().timestamp();
        schedule_next(&mut d, false, at);
        assert_eq!(d["sync"]["background"]["state"], "catching_up");
        assert!(!due(&d, at + 4));
        assert!(due(&d, at + 5));
        let job = new_job(&mut d, "sync", "").unwrap();
        assert!(!due(&d, at + 500));
        row_mut(&mut d,"jobs",&job).unwrap()["status"] = json!("completed");
        d["sync"]["scan"]["traversalComplete"] = json!(true);
        d["sync"]["scan"]["skipped"] = json!(2);
        d["sync"]["scan"]["unknownDates"] = json!(1);
        schedule_next(&mut d, false, at);
        assert_eq!(d["sync"]["background"]["state"], "monitoring");
        assert!(!due(&d, at + 59));
        assert!(due(&d, at + 60));
    }

    #[test]
    fn scheduler_error_backoff_is_durable_capped_and_resets_after_success() {
        let (mut d, _, _) = start();
        let at = chrono::Utc::now().timestamp();
        for delay in [60,120,240,480,900,900] {
            schedule_next(&mut d, true, at);
            // Reload serialized state, as a restarted process would.
            d = serde_json::from_str(&d.to_string()).unwrap();
            assert_eq!(d["sync"]["background"]["delaySeconds"], delay);
            assert!(!due(&d, at + delay - 1));
            assert!(due(&d, at + delay));
        }
        schedule_next(&mut d, false, at);
        assert_eq!(d["sync"]["background"]["consecutiveErrors"], 0);
        assert_eq!(d["sync"]["background"]["delaySeconds"], 5);
        assert!(list(&d,"approvals").is_empty());
        assert!(list(&d,"operations").is_empty());
    }

    #[test]
    fn completing_closed_history_does_not_discard_unfinished_open_frontier() {
        let (mut d,b,scan_id) = start();
        d["sync"]["scan"]["traversalComplete"] = json!(true);
        d["sync"]["scan"]["open"]["done"] = json!(true);
        d["sync"]["scan"]["closed"]["done"] = json!(true);
        certify_empty_lane(&mut d["sync"]["scan"]["open"]);
        certify_empty_lane(&mut d["sync"]["scan"]["closed"]);
        let frontier=prepare_open_frontier(&mut d,&b);
        admit_open_frontier(&mut d,&scan_id,&b,frontier["id"].as_str().unwrap(),&Value::Null,
            &json!({"items":[],"hasMore":true,"cursor":"continue-open"})).unwrap();
        assert_eq!(prepare(&mut d,&b.to_json())["id"],scan_id);
        assert_eq!(prepare_open_frontier(&mut d,&b)["cursor"],"continue-open");
        schedule_next(&mut d,false,chrono::Utc::now().timestamp());
        assert_eq!(d["sync"]["background"]["continuationPending"],true);
    }

    #[tokio::test]
    async fn background_tick_drains_multiple_batches_without_manual_sync_and_monitors_both_heads() {
        let temp = tempfile::tempdir().unwrap();
        let db = open_db(&temp.path().join("workspace.sqlite")).await.unwrap();
        let (events, _) = broadcast::channel(8);
        let bridge = temp.path().join("automatic-fixture.mjs");
        let log = temp.path().join("requests.jsonl");
        let script=r#"import {appendFile} from 'node:fs/promises';
let raw='';for await(const chunk of process.stdin)raw+=chunk;const r=JSON.parse(raw);
if(r.operation==='caps'){process.stdout.write(JSON.stringify({ok:true,result:{account:r.account,local:{}}}));process.exit(0);}
await appendFile(__LOG__,JSON.stringify(r)+'\n');
if(r.operation==='head'){process.stdout.write(JSON.stringify({ok:true,result:{kind:'open-status-head',observedAt:new Date().toISOString(),items:[],errors:[],hasMore:false}}));process.exit(0);}
if(r.operation!=='read')throw Error('Unexpected operation');
const n=Number(r.cursor||0),latest=r.reverse===true;
const key=latest?'fresh-head':'backlog-'+n;
const item={id:key,itemId:key,objectId:'11391',postKey:'11391:p',conversationKey:'11391:c',providerStatus:n===1?'inprogress':'new',createdAt:'2020-01-01T00:00:00Z',workflow:'attention',draft:''};
const result=r.mode==='closed'?{items:[],hasMore:false,cursor:null,window:r.window}:{items:[item],hasMore:latest?false:n<9,cursor:latest||n===9?null:String(n+1),window:r.window};
process.stdout.write(JSON.stringify({ok:true,result}));"#.replace("__LOG__",&json!(log.to_string_lossy()).to_string());
        std::fs::write(&bridge,script).unwrap();
        let app=App{lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),
        preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),events,csrf:id(),
                auth: None,
                public_origin: None, external_writes: false,port:4186,
            data:temp.path().to_owned(),bridge,node:PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        for batch in 0..2 {
            tick(&app).await.unwrap();
            tokio::time::timeout(Duration::from_secs(30),async {
                loop {
                    let d=app.read().await.unwrap();
                    if !list(&d,"jobs").iter().any(|j|j["status"]=="running") {break;}
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }).await.unwrap();
            let d=app.read().await.unwrap();
            assert!(list(&d,"jobs").iter().all(|j|j["status"]=="completed"),"{:#?}",d["jobs"]);
            if batch==0 {
                assert_eq!(d["sync"]["scan"]["open"]["pages"],8);
                assert_eq!(d["sync"]["background"]["state"],"catching_up");
                tick(&app).await.unwrap();
                assert_eq!(list(&app.read().await.unwrap(),"jobs").iter().filter(|j|j["kind"]=="sync").count(),1);
                app.change(|d|{d["sync"]["background"]["nextRunAt"]=Value::Null;Ok(())}).await.unwrap();
            }
        }
        let d=app.read().await.unwrap();
        assert_eq!(d["sync"]["scan"]["open"]["pages"],10);
        assert_eq!(d["sync"]["scan"]["traversalComplete"],true);
        assert_eq!(d["sync"]["background"]["state"],"monitoring");
        assert_eq!(list(&d,"items").len(),11);
        assert_eq!(row(&d,"items","backlog-1").unwrap()["providerStatus"],"inprogress");
        assert_eq!(row(&d,"items","backlog-1").unwrap()["workflow"],"attention");
        let requests:Vec<Value>=std::fs::read_to_string(log).unwrap().lines().map(|s|serde_json::from_str(s).unwrap()).collect();
        assert_eq!(requests.iter().filter(|r|r["operation"]=="read").count(),17); // fixed bounded pages, no API spin
        for reverse in [false,true] {
            assert_eq!(requests.iter().filter(|r|r["mode"]=="open"&&r["reverse"]==reverse&&r["cursor"].is_null()).count(),2);
        }
        assert!(requests.iter().all(|r|r["operation"]=="read"||r["operation"]=="head"));
        assert!(list(&d,"approvals").is_empty());
        assert!(list(&d,"operations").is_empty());
        app.db.close().await;
    }
    #[tokio::test]
    async fn failing_closed_head_cannot_starve_open_progress_or_closed_lane() {
        let temp=tempfile::tempdir().unwrap();
        let db=open_db(&temp.path().join("workspace.sqlite")).await.unwrap();
        let (events,_)=broadcast::channel(8);
        let bridge=temp.path().join("failed-head.mjs");
        let log=temp.path().join("requests.jsonl");
        let script=r#"import {appendFile} from 'node:fs/promises';
let raw='';for await(const c of process.stdin)raw+=c;const r=JSON.parse(raw);
if(r.operation==='caps'){process.stdout.write(JSON.stringify({ok:true,result:{account:r.account,local:{}}}));process.exit(0);}
await appendFile(__LOG__,JSON.stringify({mode:r.mode,reverse:r.reverse,cursor:r.cursor})+'\n');
if(r.mode==='closed'&&r.reverse===true){process.stdout.write(JSON.stringify({ok:false,error:{code:'PROVIDER_UNAVAILABLE'}}));process.exit(0);}
const n=Number(r.cursor||0),open=r.mode==='open',key='c'+n;
const item={id:key,itemId:key,objectId:'11391',postKey:'11391:p',conversationKey:'11391:t',providerStatus:'new',workflow:'attention'};
const items=open?[item]:[],more=open&&n<9;
process.stdout.write(JSON.stringify({ok:true,result:{items,hasMore:more,cursor:more?String(n+1):null,skipped:[],queueAccounting:{version:1,observations:items.map(i=>({objectId:i.objectId,itemId:i.itemId,contextRequired:true})),duplicateQueueCount:0}}}));"#.replace("__LOG__",&json!(log.to_string_lossy()).to_string());
        std::fs::write(&bridge,script).unwrap();
        let app=App{lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),
        preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),events,csrf:id(),
                auth: None,
                public_origin: None, external_writes: false,port:4186,
            data:temp.path().to_owned(),bridge,node:PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        assert!(run(app.clone()).await.is_err());
        let first=app.db.read().await.unwrap();
        assert_eq!(first["sync"]["scan"]["open"]["pages"],8);
        assert_eq!(first["sync"]["scan"]["closed"]["done"],true);
        assert_eq!(first["sync"]["auxiliaryHeads"]["status"],"error");
        assert!(run(app.clone()).await.is_err());
        let second=app.db.read().await.unwrap();
        assert_eq!(second["sync"]["scan"]["open"]["pages"],10);
        assert_eq!(second["sync"]["scan"]["open"]["done"],true);
        let requests:Vec<Value>=std::fs::read_to_string(log).unwrap().lines().map(|l|serde_json::from_str(l).unwrap()).collect();
        assert!(requests[..8].iter().all(|r|r["mode"]=="open"&&r["reverse"].is_null()));
        assert_eq!(requests.iter().filter(|r|r["mode"]=="closed"&&r["reverse"]==true).count(),2);
        assert!(requests.iter().any(|r|r["mode"]=="closed"&&r["reverse"].is_null()));
        assert!(list(&second,"operations").is_empty());
        app.db.close().await;
    }

    #[test]
    fn dated_open_checkpoint_migrates_without_restarting_closed_history() {
        let (mut d, b, scan_id) = start();
        d["sync"]["scan"]["open"] =
            json!({"cursor":"old-window-cursor","done":true,"pages":3,"seenCursors":[]});
        d["sync"]["scan"]["closed"]["cursor"] = json!("closed-checkpoint");
        certify_empty_lane(&mut d["sync"]["scan"]["closed"]);
        let closed = d["sync"]["scan"]["closed"].clone();
        let migrated = prepare(&mut d, &b.to_json());
        assert_ne!(migrated["id"], scan_id);
        let scan_id = migrated["id"].as_str().unwrap().to_string();
        assert_eq!(migrated["closed"], closed);
        assert_eq!(migrated["open"]["scope"], "all-open");
        assert_eq!(migrated["open"]["done"], false);
        assert!(migrated["open"]["cursor"].is_null());
        d["sync"]["openFrontier"] = json!({"id":"dated","binding":b.to_json(),"done":false,"window":{"since":"old","until":"old"},"cursor":"dated-cursor"});
        let f = prepare_open_frontier(&mut d, &b);
        assert_ne!(f["id"], "dated");
        assert!(f["window"].is_null());
        for n in 0..PAGES_PER_MODE {
            let cursor = d["sync"]["openFrontier"]["cursor"].clone();
            admit_open_frontier(
                &mut d,
                &scan_id,
                &b,
                f["id"].as_str().unwrap(),
                &cursor,
                &json!({"items":[],"hasMore":true,"cursor":format!("page-{}",n+1),"window":null}),
            )
            .unwrap();
        }
        let resumed = prepare_open_frontier(&mut d, &b);
        assert_eq!(resumed["id"], f["id"]);
        assert_eq!(resumed["cursor"], "page-8");
        assert_eq!(resumed["done"], false);
    }

    #[test]
    fn exact_refresh_accepts_unknown_creation_date_with_stale_provider_observation() {
        let (mut d, b, _) = start();
        let at = chrono::Utc::now().timestamp();
        let mut item = refresh_item("unknown-date", at);
        item["createdAt"] = Value::Null;
        d["items"] = json!([item]);
        assert_eq!(refresh_candidates(&d, &b, at).len(), 1);
    }
    #[tokio::test]
    async fn resumed_run_fetches_second_open_page_after_original_open_completed() {
        let temp = tempfile::tempdir().unwrap();
        let db = open_db(&temp.path().join("workspace.sqlite"))
            .await
            .unwrap();
        let (events, _) = broadcast::channel(8);
        let bridge = temp.path().join("frontier-fixture.mjs");
        let log = temp.path().join("requests.jsonl");
        let script=r#"import {appendFile} from 'node:fs/promises';
let raw='';for await(const chunk of process.stdin)raw+=chunk;const r=JSON.parse(raw);
if(r.operation==='caps'){process.stdout.write(JSON.stringify({ok:true,result:{account:r.account,local:{}}}));process.exit(0);}
await appendFile(__LOG__,JSON.stringify(r)+'\n');
if(r.operation!=='read')throw Error('Unexpected operation');
const second=r.mode==='open'&&r.cursor==='open-second';
const itemId=second?'arrival-page-two':'head-item';
const item={id:itemId,itemId,objectId:'11391',postKey:'11391:p',conversationKey:'11391:c',providerStatus:'new',createdAt:second?'2020-01-01T00:00:00Z':null,workflow:'attention',draft:''};
const result=r.mode==='open'?{items:[item],hasMore:!second,cursor:second?null:'open-second',window:r.window}:{items:[],hasMore:true,cursor:'closed-'+(Number((r.cursor||'closed-0').split('-')[1])+1),window:r.window};
process.stdout.write(JSON.stringify({ok:true,result}));"#.replace("__LOG__",&json!(log.to_string_lossy()).to_string());
        std::fs::write(&bridge, script).unwrap();
        let app = App {lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),
            account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),
            db: Database::Sqlite(db),
            gate: Arc::new(crate::writer_gate::WriterGate::default()), execution_gate: Arc::new(Mutex::new(())),
        preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),
            events,
            csrf: id(),
                auth: None,
                public_origin: None, external_writes: false,
            port: 4186,
            data: temp.path().to_owned(),
            bridge,
            node: PathBuf::from(
                "C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe",
            ),
            tasks: Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default()),
        };
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        let original = app
            .change(|d| {
                let binding = active_binding(d)?;
                let scan = prepare(d, &binding.to_json());
                let scan_id = required(&scan, "id")?;
                admit(
                    d,
                    scan_id,
                    &binding,
                    "open",
                    &Value::Null,
                    &json!({"items":[],"hasMore":false,"cursor":null}),
                )?;
                Ok(scan_id.to_string())
            })
            .await
            .unwrap();
        let result = run(app.clone()).await.unwrap();
        let state = app.read().await.unwrap();
        assert_eq!(state["sync"]["scan"]["id"], original);
        assert_eq!(state["sync"]["scan"]["closed"]["done"], false);
        assert!(
            list(&state, "items")
                .iter()
                .any(|i| i["id"] == "arrival-page-two")
        );
        assert_eq!(result["openFrontier"]["done"], true);
        assert_eq!(result["openFrontier"]["pages"], 2);
        assert_eq!(result["partial"], true);
        let requests: Vec<Value> = std::fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(
            requests
                .iter()
                .any(|r| r["mode"] == "open" && r["cursor"] == "open-second")
        );
        assert!(requests.iter().all(|r| r["operation"] == "read"));
        assert!(
            requests
                .iter()
                .filter(|r| r["mode"] == "open")
                .all(|r| r.get("window").is_none())
        );
        assert!(
            requests
                .iter()
                .filter(|r| r["mode"] == "closed")
                .all(|r| r["window"].is_object())
        );
        assert_eq!(
            state["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["id"] == "arrival-page-two")
                .unwrap()["createdAt"],
            "2020-01-01T00:00:00Z"
        );
        app.db.close().await;
    }
    #[test]
    fn open_frontier_reaches_new_second_page_while_old_closed_scan_remains_incomplete() {
        let (mut d, b, scan_id) = start();
        admit(
            &mut d,
            &scan_id,
            &b,
            "open",
            &Value::Null,
            &json!({"items":[],"hasMore":false,"cursor":null}),
        )
        .unwrap();
        assert_eq!(d["sync"]["scan"]["closed"]["done"], false);
        let f = prepare_open_frontier(&mut d, &b);
        let frontier_id = f["id"].as_str().unwrap();
        admit_open_frontier(
            &mut d,
            &scan_id,
            &b,
            frontier_id,
            &Value::Null,
            &accounted_page(json!({"items":[],"hasMore":true,"cursor":"new-page-two"})),
        )
        .unwrap();
        let resumed = prepare_open_frontier(&mut d, &b);
        assert_eq!(resumed["id"], f["id"]);
        assert_eq!(resumed["window"], f["window"]);
        assert_eq!(resumed["cursor"], "new-page-two");
        let arrival = bound_item(
            &b,
            &refresh_item("new-second-page", chrono::Utc::now().timestamp()),
        )
        .unwrap();
        admit_open_frontier(
            &mut d,
            &scan_id,
            &b,
            frontier_id,
            &json!("new-page-two"),
            &accounted_page(json!({"items":[arrival],"hasMore":false,"cursor":null})),
        )
        .unwrap();
        assert!(
            list(&d, "items")
                .iter()
                .any(|item| item["id"] == "new-second-page")
        );
        assert_eq!(d["sync"]["scan"]["id"], scan_id);
        assert_eq!(d["sync"]["scan"]["closed"]["done"], false);
        assert_eq!(d["sync"]["openFrontier"]["coverageComplete"], true);
        assert_ne!(prepare_open_frontier(&mut d, &b)["id"], f["id"]);
    }

    #[test]
    fn frontier_resume_keeps_coverage_holes_and_rejects_cycles_or_superseded_scan() {
        let (mut d, b, scan_id) = start();
        let f = prepare_open_frontier(&mut d, &b);
        let frontier_id = f["id"].as_str().unwrap();
        admit_open_frontier(
            &mut d,
            &scan_id,
            &b,
            frontier_id,
            &Value::Null,
            &json!({"items":[],"hasMore":true,"cursor":"next","unknownDateCount":1}),
        )
        .unwrap();
        let before = d.clone();
        assert!(
            admit_open_frontier(
                &mut d,
                &scan_id,
                &b,
                frontier_id,
                &json!("next"),
                &json!({"items":[],"hasMore":true,"cursor":"next"})
            )
            .is_err()
        );
        assert_eq!(d, before);
        assert!(
            admit_open_frontier(
                &mut d,
                "superseded",
                &b,
                frontier_id,
                &json!("next"),
                &json!({"items":[],"hasMore":false,"cursor":null})
            )
            .is_err()
        );
        assert_eq!(d, before);
        admit_open_frontier(
            &mut d,
            &scan_id,
            &b,
            frontier_id,
            &json!("next"),
            &json!({"items":[],"hasMore":false,"cursor":null}),
        )
        .unwrap();
        assert_eq!(d["sync"]["openFrontier"]["coverageComplete"], false);
    }

    #[test]
    fn exact_refresh_admits_explicit_deleted_but_rejects_unknown_status() {
        let (mut d, b, _) = start();
        let target = refresh_item("deleted-target", chrono::Utc::now().timestamp());
        d["items"] = json!([target.clone()]);
        let mut observed = target.clone();
        observed["providerStatus"] = json!("unexpected");
        assert!(
            admit_exact_refresh(&mut d, &b, &target, &json!({"items":[observed.clone()]})).is_err()
        );
        observed["providerStatus"] = json!("deleted");
        admit_exact_refresh(&mut d, &b, &target, &json!({"items":[observed]})).unwrap();
        assert_eq!(d["items"][0]["providerStatus"], "deleted");
        assert_eq!(d["items"][0]["draft"], "manual draft");
    }
    fn refresh_item(key: &str, at: i64) -> Value {
        json!({"id":key,"itemId":key,"objectId":"11391","postKey":"11391:post","conversationKey":"11391:thread","createdAt":chrono::DateTime::from_timestamp(at-60,0).unwrap().to_rfc3339(),"providerStatus":"new","workflow":"prepared","draft":"manual draft","revision":2})
    }

    #[tokio::test]
    async fn scoped_sync_reads_keep_cursor_and_refresh_decisions() {
        let (mut fixture, binding, scan_id) = start();
        let at = chrono::Utc::now().timestamp();
        fixture["sync"]["scan"]["open"]["cursor"] = json!("next-page");
        fixture["items"] = json!([
            refresh_item("eligible", at),
            {"id":"closed","itemId":"closed","objectId":"11391","providerStatus":"closed","workflow":"closed","draft":"protected"}
        ]);
        let folder = tempfile::tempdir().unwrap();
        let db = Database::Sqlite(open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
        db.change(|workspace| {
            workspace["sync"] = fixture["sync"].clone();
            workspace["items"] = fixture["items"].clone();
            Ok(())
        }).await.unwrap();
        let full = db.read().await.unwrap();
        let metadata = db.read_metadata().await.unwrap();
        let source = db.read_source_status().await.unwrap();
        assert_eq!(metadata["sync"], full["sync"]);
        assert_eq!(metadata["sync"]["scan"]["id"], scan_id);
        assert_eq!(metadata["sync"]["scan"]["open"]["cursor"], "next-page");
        assert!(metadata.get("items").is_none());
        assert!(source["items"][0].get("draft").is_none());
        let keys = |workspace: &Value| refresh_candidates(workspace, &binding, at)
            .into_iter().map(|item| item["id"].clone()).collect::<Vec<_>>();
        assert_eq!(keys(&source), keys(&full));
        assert_eq!(keys(&source), vec![json!("eligible")]);
        db.close().await;
    }

    #[test]
    fn exact_refresh_includes_old_open_is_bounded_fair_and_cools_down_failures() {
        let (mut d, b, _) = start();
        let at = chrono::Utc::now().timestamp();
        d["items"] = json!(
            (0..12)
                .map(|i| refresh_item(&format!("item-{i:02}"), at))
                .collect::<Vec<_>>()
        );
        let candidates = refresh_candidates(&d, &b, at);
        assert_eq!(candidates.len(), EXACT_REFRESH_LIMIT);
        assert_eq!(candidates[0]["id"], "item-00");
        d["sync"]["targetRefresh"]["item-00"] = json!({"attemptedAt":chrono::DateTime::from_timestamp(at,0).unwrap().to_rfc3339(),"error":"network failed"});
        d["items"][1]["providerObservedAt"] = json!(
            chrono::DateTime::from_timestamp(at - 30, 0)
                .unwrap()
                .to_rfc3339()
        );
        d["items"][2]["createdAt"] = json!(
            chrono::DateTime::from_timestamp(at - 49 * 3600, 0)
                .unwrap()
                .to_rfc3339()
        );
        d["items"][3]["providerStatus"] = json!("closed");
        d["items"][3]["workflow"] = json!("closed");
        let candidates = refresh_candidates(&d, &b, at);
        assert_eq!(candidates.len(), EXACT_REFRESH_LIMIT);
        assert_eq!(candidates[0]["id"], "item-02");
        for item in &candidates {
            let key = item["id"].as_str().unwrap();
            d["sync"]["targetRefresh"][key] =
                json!({"attemptedAt":chrono::DateTime::from_timestamp(at,0).unwrap().to_rfc3339()});
        }
        // A smaller batch preserves fairness: never-attempted records still
        // precede the failed target even after its cooldown has expired.
        let mut retried = false;
        for round in 0..4 {
            let attempt_at = at + 301 + round;
            let batch = refresh_candidates(&d, &b, attempt_at);
            assert!(batch.len() <= EXACT_REFRESH_LIMIT);
            retried |= batch.iter().any(|v| v["id"] == "item-00");
            for item in batch {
                let key = item["id"].as_str().unwrap();
                d["sync"]["targetRefresh"][key] = json!({"attemptedAt":chrono::DateTime::from_timestamp(attempt_at,0).unwrap().to_rfc3339()});
            }
            if retried { break; }
        }
        assert!(retried, "Failed target must re-enter after cooldown without starving older targets");
    }

    #[test]
    fn externally_closed_old_record_is_checked_next_cycle_without_retrying_failed_reads() {
        let (mut d, b, _) = start();
        let at = chrono::Utc::now().timestamp();
        let mut item = refresh_item("old-open", at);
        item["createdAt"] = json!("2020-01-01T00:00:00Z");
        item["providerObservedAt"] = json!(chrono::DateTime::from_timestamp(at-61,0).unwrap().to_rfc3339());
        d["items"] = json!([item.clone()]);
        assert_eq!(refresh_candidates(&d,&b,at).len(),1);
        d["sync"]["targetRefresh"]["old-open"] = json!({"attemptedAt":item["providerObservedAt"],"error":"network failed"});
        assert!(refresh_candidates(&d,&b,at).is_empty());
        assert_eq!(refresh_candidates(&d,&b,at+240).len(),1);
        assert_eq!(d["items"][0]["workflow"],"prepared");
        assert_eq!(d["items"][0]["draft"],"manual draft");
        d["sync"]["targetRefresh"]["old-open"]["error"] = Value::Null;
        let mut observed=item.clone();
        observed["providerStatus"]=json!("closed");
        observed["draft"]=json!("");
        admit_exact_refresh(&mut d,&b,&item,&json!({"items":[observed]})).unwrap();
        assert_eq!(d["items"][0]["workflow"],"closed");
        assert_eq!(d["items"][0]["draft"],"manual draft");
        assert!(refresh_candidates(&d,&b,at+600).is_empty());
    }

    #[test]
    fn exact_refresh_merges_observed_closure_keeps_manual_draft_and_rejects_wrong_identity() {
        let (mut d, b, _) = start();
        let target = refresh_item("cached", chrono::Utc::now().timestamp());
        d["items"] = json!([target.clone()]);
        let mut observed = target.clone();
        observed["providerStatus"] = json!("closed");
        observed["draft"] = json!("");
        let before = d.clone();
        let mut wrong = observed.clone();
        wrong["itemId"] = json!("other");
        assert!(admit_exact_refresh(&mut d, &b, &target, &json!({"items":[wrong]})).is_err());
        assert_eq!(d, before);
        assert!(admit_exact_refresh(&mut d, &b, &target, &json!({"items":[]})).is_err());
        assert_eq!(d, before);
        admit_exact_refresh(&mut d, &b, &target, &json!({"items":[observed]})).unwrap();
        assert_eq!(d["items"][0]["providerStatus"], "closed");
        assert_eq!(d["items"][0]["workflow"], "closed");
        assert_eq!(d["items"][0]["draft"], "manual draft");
        assert!(d["items"][0]["providerObservedAt"].is_string());
        assert!(d["sync"]["targetRefresh"]["cached"]["error"].is_null());
    }
    fn start() -> (Value, ConnectorBinding, String) {
        let mut d = empty();
        let b = active_binding(&d).unwrap();
        let s = prepare(&mut d, &b.to_json());
        (d, b, s["id"].as_str().unwrap().into())
    }
    #[tokio::test]
    async fn completed_new_scan_result_excludes_retained_old_frontier_after_database_reopen() {
        let (mut d, binding, old_id) = start();
        let old_frontier = prepare_open_frontier(&mut d, &binding);
        admit_open_frontier(&mut d, &old_id, &binding, old_frontier["id"].as_str().unwrap(),
            &Value::Null, &accounted_page(json!({"items":[],"hasMore":true,"cursor":"old-pending"}))).unwrap();
        d["sync"]["scan"]["invalidatedAt"] = json!(now());
        let next = prepare(&mut d, &binding.to_json());
        let next_id = next["id"].as_str().unwrap();
        for mode in ["open", "closed"] {
            admit(&mut d, next_id, &binding, mode, &Value::Null,
                &accounted_page(json!({"items":[],"hasMore":false,"cursor":null}))).unwrap();
        }
        assert_ne!(next_id, old_id);
        assert_eq!(d["sync"]["openFrontier"]["id"], old_frontier["id"]);
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("workspace.sqlite");
        let db = Database::Sqlite(open_db(&path).await.unwrap());
        db.change(|saved| { saved["sync"] = d["sync"].clone(); Ok(()) }).await.unwrap();
        db.close().await;
        let db = Database::Sqlite(open_db(&path).await.unwrap());
        let saved = db.read_metadata().await.unwrap();
        let result = result_summary(&saved["sync"]);
        assert_eq!(result["scanId"], next_id);
        assert_eq!(result["coverageComplete"], true);
        assert_eq!(result["partial"], false);
        assert!(result["openFrontier"].is_null());
        assert_eq!(saved["sync"]["openFrontier"]["cursor"], "old-pending");
        db.close().await;
    }

    #[test]
    fn result_frontier_requires_current_binding_and_keeps_current_incompleteness() {
        let (mut d, binding, scan_id) = start();
        for mode in ["open", "closed"] {
            admit(&mut d, &scan_id, &binding, mode, &Value::Null,
                &accounted_page(json!({"items":[],"hasMore":false,"cursor":null}))).unwrap();
        }
        let frontier = prepare_open_frontier(&mut d, &binding);
        let before = d.clone();
        let result = result_summary(&d["sync"]);
        assert_eq!(result["partial"], true);
        assert_eq!(result["openFrontier"]["id"], frontier["id"]);
        d["sync"]["openFrontier"]["binding"]["revision"] = json!(999);
        let foreign = result_summary(&d["sync"]);
        assert_eq!(foreign["partial"], false);
        assert!(foreign["openFrontier"].is_null());
        d = before;
        admit_open_frontier(&mut d, &scan_id, &binding, frontier["id"].as_str().unwrap(),
            &Value::Null, &accounted_page(json!({"items":[],"hasMore":false,"cursor":null}))).unwrap();
        assert_eq!(result_summary(&d["sync"])["partial"], false);
        d["sync"]["openFrontier"]["invalidatedAt"] = json!(now());
        let invalidated = result_summary(&d["sync"]);
        assert_eq!(invalidated["partial"], true);
        assert_eq!(invalidated["openFrontier"]["coverageComplete"], false);
        assert_eq!(invalidated["openFrontier"]["traversalComplete"], false);
    }

    #[test]
    fn cursor_checkpoint_survives_resume_and_cycle_is_rejected() {
        let (mut d, b, id) = start();
        let page = json!({"items":[],"hasMore":true,"cursor":"two"});
        admit(&mut d, &id, &b, "open", &Value::Null, &page).unwrap();
        assert_eq!(prepare(&mut d, &b.to_json())["id"], id);
        assert_eq!(d["sync"]["scan"]["open"]["cursor"], "two");
        assert!(admit(&mut d, &id, &b, "open", &json!("two"), &page).is_err());
    }
    #[test]
    fn traversal_never_marks_absent_item_closed_or_claims_snapshot() {
        let (mut d, b, id) = start();
        d["items"] = json!([{"id":"absent","providerStatus":"new"}]);
        let page = json!({"items":[],"hasMore":false,"cursor":null});
        for mode in ["open", "closed"] {
            admit(&mut d, &id, &b, mode, &Value::Null, &page).unwrap();
        }
        assert_eq!(d["sync"]["scan"]["traversalComplete"], true);
        assert_eq!(d["sync"]["scan"]["snapshotConsistent"], false);
        assert_eq!(d["items"][0]["providerStatus"], "new");
        assert_ne!(prepare(&mut d, &b.to_json())["id"], id);
    }

    #[test]
    fn invalid_cursor_restarts_scan_but_retains_failure_reason() {
        let (mut d, b, id) = start();
        admit(
            &mut d,
            &id,
            &b,
            "open",
            &Value::Null,
            &json!({"items":[],"hasMore":true,"cursor":"checkpoint"}),
        )
        .unwrap();
        let error = internal("Adapter failed (INVALID_CURSOR)");
        assert!(invalidate_checkpoint(
            &mut d,
            &id,
            &b,
            "open",
            &json!("checkpoint"),
            &error
        ));
        assert_eq!(d["sync"]["scan"]["invalidationReason"], "INVALID_CURSOR");
        let next = prepare(&mut d, &b.to_json());
        assert_ne!(next["id"], id);
        assert!(next["open"]["cursor"].is_null());
        assert_eq!(next["open"]["pages"], 0);
        assert_eq!(d["sync"]["lastInvalidatedScan"]["id"], id);
        assert_eq!(d["sync"]["lastInvalidatedScan"]["reason"], "INVALID_CURSOR");
    }

    #[test]
    fn repeated_and_cyclic_cursors_invalidate_without_admitting_the_bad_page() {
        for cyclic in [false, true] {
            let (mut d, b, id) = start();
            admit(
                &mut d,
                &id,
                &b,
                "open",
                &Value::Null,
                &json!({"items":[],"hasMore":true,"cursor":"one"}),
            )
            .unwrap();
            if cyclic {
                admit(
                    &mut d,
                    &id,
                    &b,
                    "open",
                    &json!("one"),
                    &json!({"items":[],"hasMore":true,"cursor":"two"}),
                )
                .unwrap();
            }
            let cursor = d["sync"]["scan"]["open"]["cursor"].clone();
            let before = d.clone();
            let error = admit(
                &mut d,
                &id,
                &b,
                "open",
                &cursor,
                &json!({"items":[{"id":"must-not-merge"}],"hasMore":true,"cursor":"one"}),
            )
            .unwrap_err();
            assert_eq!(d, before);
            assert!(invalidate_checkpoint(
                &mut d, &id, &b, "open", &cursor, &error
            ));
            assert_eq!(
                d["sync"]["scan"]["invalidationReason"],
                "CURSOR_DID_NOT_ADVANCE"
            );
            assert_ne!(prepare(&mut d, &b.to_json())["id"], id);
        }
    }

    #[test]
    fn transient_failures_keep_exact_checkpoint_and_stale_failures_cannot_reset_progress() {
        let (mut d, b, id) = start();
        admit(
            &mut d,
            &id,
            &b,
            "open",
            &Value::Null,
            &json!({"items":[],"hasMore":true,"cursor":"one"}),
        )
        .unwrap();
        let before = d.clone();
        for message in [
            "Adapter process failed",
            "Adapter timed out; action outcome may be unknown",
            "Adapter failed (RATE_LIMITED)",
            "Adapter failed (NETWORK_ERROR)",
        ] {
            assert!(!invalidate_checkpoint(
                &mut d,
                &id,
                &b,
                "open",
                &json!("one"),
                &internal(message)
            ));
            assert_eq!(d, before);
            assert_eq!(prepare(&mut d, &b.to_json())["id"], id);
        }
        let invalid = internal("Adapter failed (INVALID_CURSOR)");
        assert!(!invalidate_checkpoint(
            &mut d,
            &id,
            &b,
            "open",
            &Value::Null,
            &invalid
        ));
        assert!(!invalidate_checkpoint(
            &mut d,
            "superseded",
            &b,
            "open",
            &json!("one"),
            &invalid
        ));
        assert_eq!(d, before);
    }
}
