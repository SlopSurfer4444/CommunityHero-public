//! Durable, bounded pagination. Exhaustion is traversal evidence, not an atomic
//! snapshot of a changing provider queue. Missing records never imply deletion.
use super::*;
const PAGES_PER_MODE: usize = 8;
const CLOSED_PAGES_PER_RUN:usize = 1;
const EXACT_REFRESH_LIMIT: usize = 4;
const CONTINUE_AFTER_SECONDS: i64 = 5;
const MONITOR_AFTER_SECONDS: i64 = 60;
const MAX_ERROR_BACKOFF_SECONDS: i64 = 900;

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

fn admit_exact_refresh(
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
    let candidates =
        refresh_candidates(&app.read().await?, binding, chrono::Utc::now().timestamp());
    for target in candidates {
        let key = required(&target, "id")?.to_string();
        let claimed = app
            .change(|d| {
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
            Ok(snapshot) => app.change(|d| admit_exact_refresh(d, binding, &target, &snapshot)).await,
            Err(error) => Err(error),
        };
        if let Err(error) = outcome {
            app.change(|d| {
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
    let old = &d["sync"]["scan"];
    if old["binding"] == *binding
        && continuation_pending(d)
        && old["window"].is_object()
        && old["invalidatedAt"].is_null()
    {
        let mut resumed = old.clone();
        if resumed["open"]["scope"] != "all-open" {
            resumed["open"] =
                json!({"scope":"all-open","cursor":null,"done":false,"pages":0,"seenCursors":[]});
            d["sync"]["scan"] = resumed.clone();
        }
        return resumed;
    }
    let until = chrono::Utc::now();
    let scan = json!({"id":id(),"binding":binding,"window":{"since":(until-chrono::Duration::hours(48)).to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"until":until.to_rfc3339_opts(chrono::SecondsFormat::Millis,true)},"open":{"scope":"all-open","cursor":null,"done":false,"pages":0,"seenCursors":[]},"closed":{"cursor":null,"done":false,"pages":0,"seenCursors":[]},"skipped":0,"unknownDates":0,"traversalComplete":false,"snapshotConsistent":false,"seenIds":[]});
    d["sync"]["scan"] = scan.clone();
    scan
}

fn prepare_open_frontier(d: &mut Value, binding: &ConnectorBinding) -> Value {
    let old = &d["sync"]["openFrontier"];
    if old["binding"] == binding.to_json()
        && old["scanId"] == d["sync"]["scan"]["id"]
        && old["done"] == false
        && old["scope"] == "all-open"
        && old["window"].is_null()
        && old["invalidatedAt"].is_null()
    {
        return old.clone();
    }
    let frontier = json!({"id":id(),"scanId":d["sync"]["scan"]["id"],"binding":binding.to_json(),"scope":"all-open","window":null,"cursor":null,"done":false,"pages":0,"seenCursors":[],"skipped":0,"unknownDates":0,"snapshotConsistent":false});
    d["sync"]["openFrontier"] = frontier.clone();
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
    merge_snapshot(d, snapshot)?;
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
    f["coverageComplete"] = json!(!more && f["skipped"] == 0 && f["unknownDates"] == 0);
    f["lastPageAt"] = json!(now());
    Ok(())
}

async fn refresh_open_frontier(
    app: &App,
    scan_id: &str,
    binding: &ConnectorBinding,
    account: &str,
) -> ApiResult<()> {
    let f = app
        .change(|d| {
            if active_binding(d)? != *binding || d["sync"]["scan"]["id"] != scan_id {
                return Err(conflict("Sync scan superseded"));
            }
            Ok(prepare_open_frontier(d, binding))
        })
        .await?;
    let frontier_id = required(&f, "id")?;
    for _ in 0..PAGES_PER_MODE {
        let state = app.read().await?;
        let current = &state["sync"]["openFrontier"];
        if current["id"] != frontier_id || state["sync"]["scan"]["id"] != scan_id {
            return Err(conflict("Open frontier superseded"));
        }
        if current["done"] == true {
            break;
        }
        let cursor = current["cursor"].clone();
        let mut args = json!({"account":account,"mode":"open","binding":binding.to_json()});
        if !cursor.is_null() {
            args["cursor"] = cursor.clone();
        }
        let result = match app.bridge("read", args).await {
            Ok(mut snapshot) => {
                for item in snapshot["items"]
                    .as_array_mut()
                    .ok_or_else(|| internal("Provider omitted items"))?
                {
                    *item = bound_item(binding, item)?;
                }
                app.change(|d| {
                    admit_open_frontier(d, scan_id, binding, frontier_id, &cursor, &snapshot)
                })
                .await
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            if let Some(reason) = checkpoint_failure_reason(&error) {
                app.change(|d| {
                    if active_binding(d)? == *binding
                        && d["sync"]["scan"]["id"] == scan_id
                        && d["sync"]["openFrontier"]["id"] == frontier_id
                        && d["sync"]["openFrontier"]["cursor"] == cursor
                    {
                        d["sync"]["openFrontier"]["invalidatedAt"] = json!(now());
                        d["sync"]["openFrontier"]["invalidationReason"] = json!(reason);
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
        app.change(|d| {
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
    merge_snapshot(d, snapshot)?;
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
    for item in snapshot["items"]
        .as_array()
        .ok_or_else(|| internal("Provider omitted items"))?
    {
        let seen = scan["seenIds"].as_array_mut().unwrap();
        if !seen.contains(&item["id"]) {
            seen.push(item["id"].clone());
        }
    }
    let done = scan["open"]["done"] == true && scan["closed"]["done"] == true;
    scan["traversalComplete"] = json!(done);
    scan["lastPageAt"] = json!(now());
    // Keep normal page controls compatible without confusing them with the
    // separately bound rolling-window scan cursor.
    d["sync"]["lastSyncedAt"] = json!(now());
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
        "coverageComplete":valid && source["done"]==true && skipped==0 && unknown==0})
}

pub(super) async fn run(app: App) -> ApiResult<Value> {
    let binding = active_binding(&app.read().await?)?;
    let account = bridge_account(&binding)?;
    let scan = app.change(|d| Ok(prepare(d, &binding.to_json()))).await?;
    let scan_id = required(&scan, "id")?.to_string();
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
            app.change(|d| {
                if active_binding(d)? != binding {return Err(conflict("Connector changed during head refresh"));}
                merge_snapshot(d,&page)?;
                let observation=json!({"at":now(),"scope":if mode=="open" {"all-open"}else{"dated-closed-history"},"reverse":reverse,"window":if mode=="closed" {window.clone()}else{Value::Null},"coverage":page["coverage"],"hasMore":page["hasMore"]});
                d["sync"]["frontier"][mode]=observation.clone();
                d["sync"]["heads"][mode][if reverse {"reverse"}else{"forward"}]=observation;
                Ok(())
            }).await?;
        }
    }
    // Once the initial open traversal ends, keep rescanning its own fresh,
    // resumable unwindowed queue independently of the still-running closed history.
    // The head refresh above also admits arrivals during a long frontier run.
    if scan["open"]["done"] == true {
        refresh_open_frontier(&app, &scan_id, &binding, account).await?;
    }
    for mode in ["open", "closed"] {
        if mode == "closed" {
            refresh_open_targets(&app, &binding, account).await?;
        }
        for _ in 0..if mode=="closed"{CLOSED_PAGES_PER_RUN}else{PAGES_PER_MODE} {
            let state = app.read().await?;
            if state["sync"]["scan"]["id"] != scan_id
                || !state["sync"]["scan"]["invalidatedAt"].is_null()
            {
                return Err(conflict("Sync scan superseded"));
            }
            if state["sync"]["scan"][mode]["done"] == true {
                break;
            }
            let cursor = state["sync"]["scan"][mode]["cursor"].clone();
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
                .change(|d| admit(d, &scan_id, &binding, mode, &cursor, &snapshot))
                .await
            {
                record_checkpoint_failure(&app, &scan_id, &binding, mode, &cursor, &error).await?;
                return Err(error);
            }
        }
    }
    let state = app.change(|d| {
        d["sync"]["openCoverage"] = open_coverage(d);
        Ok(d.clone())
    }).await?;
    let scan = &state["sync"]["scan"];
    let open_frontier = &state["sync"]["openFrontier"];
    let frontier_partial = open_frontier.is_object()
        && (open_frontier["done"] != true
            || open_frontier["skipped"].as_u64().unwrap_or(0) > 0
            || open_frontier["unknownDates"].as_u64().unwrap_or(0) > 0);
    Ok(
        json!({"synced":true,"scanId":scan_id,"partial":frontier_partial||scan["traversalComplete"]!=true||scan["skipped"].as_u64().unwrap_or(0)>0||scan["unknownDates"].as_u64().unwrap_or(0)>0,"window":scan["window"],"openScope":"all-open","closedWindow":scan["window"],"openFrontier":{"scope":"all-open","id":open_frontier["id"],"window":open_frontier["window"],"done":open_frontier["done"],"pages":open_frontier["pages"],"skipped":open_frontier["skipped"],"unknownDates":open_frontier["unknownDates"],"coverageComplete":open_frontier["coverageComplete"]},"snapshotConsistent":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_and_refreshed_open_coverage_never_reuse_an_old_scan() {
        let mut d=json!({"sync":{"scan":{"id":"current","open":{"scope":"all-open","done":true,"pages":1,"skipped":0,"unknownDates":0},"skipped":2,"unknownDates":1}}});
        assert_eq!(open_coverage(&d)["coverageComplete"],true);
        d["sync"]["openFrontier"]=json!({"id":"stale","scanId":"old","scope":"all-open","done":true,"pages":1,"skipped":0,"unknownDates":0});
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
        d["sync"]["openFrontier"]["skipped"]=json!(1);
        assert_eq!(open_coverage(&d)["coverageComplete"],false);
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
await appendFile(__LOG__,JSON.stringify(r)+'\n');
if(r.operation==='head'){process.stdout.write(JSON.stringify({ok:true,result:{kind:'open-status-head',observedAt:new Date().toISOString(),items:[],errors:[],hasMore:false}}));process.exit(0);}
if(r.operation!=='read')throw Error('Unexpected operation');
const n=Number(r.cursor||0),latest=r.reverse===true;
const key=latest?'fresh-head':'backlog-'+n;
const item={id:key,itemId:key,objectId:'11391',postKey:'11391:p',conversationKey:'11391:c',providerStatus:n===1?'inprogress':'new',createdAt:'2020-01-01T00:00:00Z',workflow:'attention',draft:''};
const result=r.mode==='closed'?{items:[],hasMore:false,cursor:null,window:r.window}:{items:[item],hasMore:latest?false:n<9,cursor:latest||n===9?null:String(n+1),window:r.window};
process.stdout.write(JSON.stringify({ok:true,result}));"#.replace("__LOG__",&json!(log.to_string_lossy()).to_string());
        std::fs::write(&bridge,script).unwrap();
        let app=App{account:crate::accounts::Profile::LikeAvto,db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),
        assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),events,csrf:id(),
                auth: None,
                public_origin: None, external_writes: false,port:4186,
            data:temp.path().to_owned(),bridge,node:PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
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
    #[test]
    fn dated_open_checkpoint_migrates_without_restarting_closed_history() {
        let (mut d, b, scan_id) = start();
        d["sync"]["scan"]["open"] =
            json!({"cursor":"old-window-cursor","done":true,"pages":3,"seenCursors":[]});
        d["sync"]["scan"]["closed"]["cursor"] = json!("closed-checkpoint");
        let closed = d["sync"]["scan"]["closed"].clone();
        let migrated = prepare(&mut d, &b.to_json());
        assert_eq!(migrated["id"], scan_id);
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
await appendFile(__LOG__,JSON.stringify(r)+'\n');
if(r.operation!=='read')throw Error('Unexpected operation');
const second=r.mode==='open'&&r.cursor==='open-second';
const itemId=second?'arrival-page-two':'head-item';
const item={id:itemId,itemId,objectId:'11391',postKey:'11391:p',conversationKey:'11391:c',providerStatus:'new',createdAt:second?'2020-01-01T00:00:00Z':null,workflow:'attention',draft:''};
const result=r.mode==='open'?{items:[item],hasMore:!second,cursor:second?null:'open-second',window:r.window}:{items:[],hasMore:true,cursor:'closed-'+(Number((r.cursor||'closed-0').split('-')[1])+1),window:r.window};
process.stdout.write(JSON.stringify({ok:true,result}));"#.replace("__LOG__",&json!(log.to_string_lossy()).to_string());
        std::fs::write(&bridge, script).unwrap();
        let app = App {
            account:crate::accounts::Profile::LikeAvto,
            db: Database::Sqlite(db),
            gate: Arc::new(crate::writer_gate::WriterGate::default()), execution_gate: Arc::new(Mutex::new(())),
        assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),
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
            &json!({"items":[],"hasMore":true,"cursor":"new-page-two"}),
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
            &json!({"items":[arrival],"hasMore":false,"cursor":null}),
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
