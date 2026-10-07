//! Durable, read/prepare-only queue. Never creates approvals or calls dispatch.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[path = "auto_revalidation.rs"]
pub(crate) mod revalidation;
#[path = "auto_prepare_scheduler.rs"]
mod scheduler;

const MAX_ATTEMPTS: u64 = 3;
const GROUP_REVIEW_REASON: &str = "Контекст групповой подготовки изменился; сохранённые решения требуют проверки, повторная генерация не запускалась.";
pub async fn configure(
    axum::extract::State(app): axum::extract::State<super::App>,
    axum::Json(body): axum::Json<Value>,
) -> super::ApiResult<axum::Json<Value>> {
    let mut review_body=body.clone();
    let facts=review_body.as_object_mut().and_then(|v|v.remove("publicFactFollowup"));
    if let Some(value)=&facts {crate::fact_followup::validate_automatic_setting(value)?;}
    let configuration=revalidation::validated_config(&review_body)?;
    app.change(|d| {
        d["settings"]["autoPreparation"]["revalidation"]=configuration;
        if let Some(value)=facts {d["settings"]["autoPreparation"]["publicFactFollowup"]=value;}
        super::audit(d,"preparation.revalidation_configured","local-pilot");
        Ok(axum::Json(revalidation::status(d,chrono::Utc::now().timestamp())))
    }).await
}
fn status_summary_query(query:&std::collections::HashMap<String,String>)->super::ApiResult<bool>{
    match query.get("eligibility").map(String::as_str){
        None|Some("full")=>Ok(false),Some("summary")=>Ok(true),
        _=>Err(super::bad("Invalid preparation eligibility query")),
    }
}
pub async fn status(axum::extract::State(app): axum::extract::State<super::App>,
    axum::extract::Query(query):axum::extract::Query<std::collections::HashMap<String,String>>) -> super::ApiResult<axum::Json<Value>> {
    let summary=status_summary_query(&query)?;
    let state=if summary {app.db.read_preparation_status_summary().await?}else{app.read().await?};
    Ok(axum::Json(status_view(&state,chrono::Utc::now().timestamp(),summary)))
}
pub(crate) fn status_view(state:&Value,now:i64,summary:bool)->Value {
    let mut value=if summary{revalidation::status_summary(state,now)}else{revalidation::status(state,now)};
    value["publicFactFollowup"]=json!({"version":1,"enabled":crate::fact_followup::automatic_enabled(state),"maxResearchAttempts":1,"maxContinuationsPerDependency":1});
    value["workerEnabled"]=json!(std::env::var("COMMUNITYHERO_BACKGROUND_DISABLED").as_deref()!=Ok("1")
        && std::env::var("COMMUNITYHERO_BACKGROUND_GENERATION_DISABLED").as_deref()!=Ok("1")
        && std::env::var("COMMUNITYHERO_COMMENT_PREPARATION_DISABLED").as_deref()!=Ok("1"));
    value["continuousPreparation"]=crate::continuous_preparation::status_view(state,&stamp(now),!summary);
    value["transcriptRequiredForVideo"]=json!(true);
    value["mediaWorkerEnabled"]=json!(super::media_queue::background_enabled());
    value["mediaOpenCommentsOnly"]=json!(std::env::var("COMMUNITYHERO_MEDIA_OPEN_COMMENTS_ONLY").as_deref()==Ok("1"));
    value["visualContextRequiredForVideo"]=json!(false);
    value["visualContextSamplingVersion"]=json!(1);
    // Configuration visibility is not a claim that every machine GPU client
    // participates or that shared model residency has been reconciled.
    value["gpuAdmissionConfigured"]=json!(std::env::var_os("COMMUNITYHERO_GPU_GATE_FILE").is_some());
    value
}
fn assessment_tags(assessment: &Value) -> super::ApiResult<Value> {
    let Some(value) = assessment.get("tags") else {
        return Ok(json!([]));
    };
    let tags = value
        .as_array()
        .filter(|tags| tags.len() <= 3)
        .ok_or_else(|| super::bad("Invalid assessment tags"))?;
    let mut seen = std::collections::HashSet::new();
    for tag in tags {
        let tag = tag
            .as_str()
            .ok_or_else(|| super::bad("Invalid assessment tags"))?;
        if ![
            "complaint",
            "needs_fact",
            "moderation",
            "missing_context",
            "purchase",
            "question",
            "feedback",
        ]
        .contains(&tag)
            || !seen.insert(tag)
        {
            return Err(super::bad("Invalid assessment tags"));
        }
    }
    Ok(value.clone())
}
fn time(value: &Value) -> Option<i64> {
    value
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.timestamp())
}
fn stamp(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .unwrap()
        .to_rfc3339()
}
fn waiting_for_media(d:&Value,item:&Value,now:i64)->bool {
    // A failed download or elapsed timer is not evidence about the video.
    // Initial preparation and revalidation share this admission barrier.
    crate::media_queue::preparation_state(d,item,&stamp(now))
        .map_or(true, |state| state.is_some())
}
fn outcome_for_item<'a>(job: &'a Value, item_id: &str) -> Option<&'a Value> {
    let outcome = &job["prepareOutcome"];
    if outcome["itemId"] == item_id { return Some(outcome); }
    outcome["items"].as_array()?.iter().find(|outcome| outcome["itemId"] == item_id)
}
pub(crate) fn eligible(d: &Value, item: &Value, now: i64) -> bool {
    eligible_with_media(d,item,now,waiting_for_media(d,item,now))
}
// Local draft preparation uses captured evidence, not a claim that a provider
// status is still current. Final dispatch independently refreshes exact context.
// Missing, malformed and future observations still fail closed.
fn valid_provider_observation(item:&Value,now:i64)->bool {
    time(&item["providerObservedAt"]).is_some_and(|at|at<=now+60)
}
fn eligible_with_media(d: &Value, item: &Value, now: i64, media_wait: bool) -> bool {
    // New captures can assess available text even while background media work
    // is incomplete. Their exact final judgments still declare dependencies;
    // historical captured jobs retain the pre-call media gate below.
    if !matches!(item["providerStatus"].as_str(), Some("new" | "inprogress"))
        || item["workflow"] != "attention"
        || !item["draft"].as_str().unwrap_or("").trim().is_empty()
        || item["draftEdited"] == true
        || !item["autoPreparation"]["humanOverrideAt"].is_null()
        || item["autoPreparation"]["requiresReview"] == true
        || item["autoPreparation"]["reviewResumeRequired"] == true
        || media_wait&&!crate::decision_media::may_assess(d,item).unwrap_or(false)
        || !valid_provider_observation(item,now)
    {
        return false;
    }
    let Ok(binding) = super::active_binding(d) else {
        return false;
    };
    if super::bridge_account(&binding).is_err() || super::bound_item(&binding, item).is_err() {
        return false;
    }
    if super::list(d, "proposals").iter().any(|p| {
        p["itemId"] == item["id"]
            && matches!(
                p["status"].as_str(),
                Some("draft" | "approved" | "dispatching" | "unknown")
            )
    }) {
        return false;
    }
    !super::list(d, "operations").iter().any(|op| {
        op["itemId"] == item["id"]
            && matches!(op["status"].as_str(), Some("unknown" | "dispatching"))
    })
}

/// Retire only unapproved automatic drafts. Never rebind a proposal or approval
/// to a newer item revision; a fresh preparation gets fresh provenance.
pub fn reconcile_stale(d: &mut Value, now: i64) {
    let stale: Vec<(String, String, String, String)> = {
      let context=super::prepare_bundle::EvidenceContext::new(d);
      super::list(d, "proposals")
        .iter()
        .filter_map(|p| {
            if p["status"] != "draft" {
                return None;
            }
            // Repair proposals retain their truthful child paid provenance;
            // only the proved merged root owns the automatic workflow pointer.
            let owner=crate::answering_repair_plan::automatic_proposal_origin(d,p)?;
            let error = super::proposal_current_with_context(p,&context).err()?;
            Some((
                p["id"].as_str()?.to_string(),
                p["itemId"].as_str()?.to_string(),
                error.1,
                owner,
            ))
        })
        .collect()
    }; // The borrowed validation index is gone before the first mutation.
    for (proposal_id, item_id, reason, owner) in stale {
        let p = super::row_mut(d, "proposals", &proposal_id).unwrap();
        let run=json!(owner);
        p["status"] = json!("stale");
        p["staleReason"] = json!(reason);
        p["staleAt"] = json!(stamp(now));
        super::bump(p);
        let protected = super::list(d, "proposals").iter().any(|p| {
            p["itemId"] == item_id
                && matches!(
                    p["status"].as_str(),
                    Some("approved" | "dispatching" | "unknown")
                )
        }) || super::list(d, "operations").iter().any(|o| {
            o["itemId"] == item_id
                && matches!(o["status"].as_str(), Some("dispatching" | "unknown"))
        });
        let valid_draft = super::list(d, "proposals").iter().any(|p| {
            p["itemId"] == item_id
                && p["status"] == "draft"
                && super::proposal_current(d, p).is_ok()
        });
        if protected || valid_draft {
            continue;
        }
        let Ok(item) = super::row_mut(d, "items", &item_id) else {
            continue;
        };
        if item["autoPreparation"]["jobId"] != run {
            continue;
        }
        let needs_review = item["workflow"] == "prepared"
            && item["draft"].as_str().unwrap_or("").trim().is_empty()
            && item["autoPreparation"]["humanOverrideAt"].is_null()
            && matches!(item["providerStatus"].as_str(), Some("new" | "inprogress"));
        item["autoPreparation"]["status"] = json!("stale");
        item["autoPreparation"]["requiresReview"] = json!(true);
        item["autoPreparation"]["savedProposalId"] = json!(proposal_id);
        item["autoPreparation"]["reason"] =
            json!("Контекст изменился. Сохранённый ответ требует перепроверки; текст сохранён.");
        item["autoPreparation"]["updatedAt"] = json!(stamp(now));
        item["autoPreparation"]["retryAt"] = Value::Null;
        if needs_review {
            item["workflow"] = json!("attention");
            item["decision"] = json!("needs_attention");
            item["reason"] = item["autoPreparation"]["reason"].clone();
            super::bump(item);
        }
    }
    hold_legacy_repreparations(d, now);
    explain_saved_review_holds(d);
    explain_group_review_holds(d);
}

// Only source reconciliation may supply this complete, ordered maintenance
// inventory. These headers never authorize generation, recovery or dispatch.
fn source_maintenance_jobs(d:&Value)->&[Value]{
    let inventory=&d["sourceJobControls"];
    if inventory["version"]==1&&inventory["complete"]==true{
        if let Some(jobs)=inventory["jobs"].as_array(){return jobs;}
    }
    super::list(d,"jobs")
}
fn source_maintenance_job<'a>(d:&'a Value,id:&str)->Option<&'a Value>{
    super::row(d,"jobs",id).ok().or_else(||source_maintenance_jobs(d).iter().find(|job|job["id"]==id))
}
fn explain_group_review_holds(d: &mut Value) {
    let ids: Vec<String> = super::list(d, "items").iter().filter_map(|item| {
        if item["autoPreparation"]["requiresReview"] != true || item["workflow"] != "attention"
            || item["draftEdited"] == true || !item["draft"].as_str().unwrap_or("").trim().is_empty()
            || !item["autoPreparation"]["humanOverrideAt"].is_null() { return None; }
        let id = item["id"].as_str()?;
        let job = source_maintenance_job(d,item["autoPreparation"]["jobId"].as_str()?)?;
        let ids = job["prepareBundle"]["itemIds"].as_array()?;
        if job["purpose"] != "auto_prepare" || ids.len() <= 1 || !ids.contains(&json!(id)) { return None; }
        if super::list(d, "proposals").iter().any(|p| p["itemId"] == id && matches!(p["status"].as_str(), Some("approved" | "dispatching" | "unknown")))
            || super::list(d, "operations").iter().any(|op| op["itemId"] == id && matches!(op["status"].as_str(), Some("dispatching" | "unknown"))) { return None; }
        Some(id.to_owned())
    }).collect();
    for id in ids {
        let item = super::row_mut(d, "items", &id).unwrap();
        item["autoPreparation"]["groupReviewRequired"] = json!(true);
        item["autoPreparation"]["reasonCode"] = json!("group_review_required");
        item["autoPreparation"]["reason"] = json!(GROUP_REVIEW_REASON);
        item["reason"] = json!(GROUP_REVIEW_REASON);
    }
}

// Also annotate already-held results after an upgrade. No proposal is promoted,
// no text is regenerated, and repeated reconciliation is a no-op.
fn explain_saved_review_holds(d: &mut Value) {
    let updates: Vec<(String, String)> = {
      let context=super::prepare_bundle::EvidenceContext::new(d);
      super::list(d, "items").iter().filter_map(|item| {
        if item["workflow"] != "attention" || item["autoPreparation"]["status"] != "stale"
            || item["autoPreparation"]["requiresReview"] != true { return None; }
        if item["autoPreparation"]["sourceChangeReason"].is_string()
            && item["autoPreparation"]["sourceChangeProposalId"] == item["autoPreparation"]["savedProposalId"]
            && !item["autoPreparation"]["reason"].as_str().unwrap_or("").contains("запускается оператором") {
            return None;
        }
        let id = item["id"].as_str()?;
        let proposal = super::row(d, "proposals", item["autoPreparation"]["savedProposalId"].as_str()?).ok()?;
        let run = proposal["prepareRunId"].as_str().or_else(|| proposal["recovery"]["prepareRunId"].as_str())?;
        let job = super::row(d, "jobs", run).ok()?;
        let reason = context.source_change_reason(&job["prepareBundle"], id)?;
        Some((id.to_owned(), reason.to_owned()))
      }).collect()
    };
    for (id, reason) in updates {
        let item = super::row_mut(d, "items", &id).unwrap();
        item["autoPreparation"]["sourceChangeReason"] = json!(reason);
        item["autoPreparation"]["sourceChangeProposalId"] = item["autoPreparation"]["savedProposalId"].clone();
        item["autoPreparation"]["reason"] = json!(format!("{reason} Сохранённое решение требует перепроверки; текст сохранён."));
        item["reason"] = item["autoPreparation"]["reason"].clone();
    }
}

/// Older releases queued stale proposals automatically and erased their input digest.
/// Preserve that paid-for result for review instead of spending another model run.
/// This also covers a failed/interrupted regeneration after recovery; genuinely new
/// comments with no prior automatic proposal retain their bounded retry policy.
fn hold_legacy_repreparations(d: &mut Value, now: i64) {
    let held: Vec<(String, String)> = super::list(d, "items").iter().filter_map(|item| {
        if item["workflow"] != "attention"
            || item["autoPreparation"]["requiresReview"] == true
            || item["autoPreparation"]["status"] == "running"
            || !item["draft"].as_str().unwrap_or("").trim().is_empty()
            || !item["autoPreparation"]["humanOverrideAt"].is_null()
            || super::list(d, "proposals").iter().any(|p| p["itemId"] == item["id"] && matches!(p["status"].as_str(), Some("draft" | "approved" | "dispatching" | "unknown")))
            || super::list(d, "operations").iter().any(|o| o["itemId"] == item["id"] && matches!(o["status"].as_str(), Some("dispatching" | "unknown"))) {
            return None;
        }
        let saved = super::list(d, "proposals").iter().rev().find(|p| {
            p["itemId"] == item["id"] && p["status"] == "stale"
                && p["prepareRunId"].as_str().and_then(|run|source_maintenance_job(d,run)).is_some_and(|j| j["purpose"] == "auto_prepare")
        })?;
        Some((item["id"].as_str()?.to_owned(), saved["id"].as_str()?.to_owned()))
    }).collect();
    for (item_id, proposal_id) in held {
        let item = super::row_mut(d, "items", &item_id).unwrap();
        item["autoPreparation"]["status"] = json!("stale");
        item["autoPreparation"]["requiresReview"] = json!(true);
        item["autoPreparation"]["savedProposalId"] = json!(proposal_id);
        item["autoPreparation"]["retryAt"] = Value::Null;
        item["autoPreparation"]["updatedAt"] = json!(stamp(now));
        item["autoPreparation"]["reason"] = json!("Сохранённый ответ требует перепроверки; текст сохранён.");
        item["reason"] = item["autoPreparation"]["reason"].clone();
        item["decision"] = json!("needs_attention");
        super::bump(item);
    }
    // Legacy claim() also erased jobId/inputDigest for accepted assessments with
    // no proposal. Recover their durable outcome from jobs before spending again.
    let assessments: Vec<(String, Value)> = super::list(d, "items").iter().filter_map(|item| {
        if item["workflow"] != "attention"
            || item["autoPreparation"]["requiresReview"] == true
            || !matches!(item["autoPreparation"]["status"].as_str(), Some("queued" | "error"))
            || !item["draft"].as_str().unwrap_or("").trim().is_empty()
            || !item["autoPreparation"]["humanOverrideAt"].is_null()
            || super::list(d, "proposals").iter().any(|p| p["itemId"] == item["id"] && matches!(p["status"].as_str(), Some("draft" | "approved" | "dispatching" | "unknown")))
            || super::list(d, "operations").iter().any(|o| o["itemId"] == item["id"] && matches!(o["status"].as_str(), Some("dispatching" | "unknown"))) {
            return None;
        }
        let previous = source_maintenance_jobs(d).iter().rev().find(|job| {
            job["purpose"] == "auto_prepare" && job["status"] == "completed"
                && outcome_for_item(job, item["id"].as_str().unwrap_or("")).is_some_and(|outcome|
                    outcome["status"] == "needs_attention" && outcome["reason"].as_str().is_some_and(|s| !s.is_empty()))
                && job["prepareBundle"]["itemIds"].as_array().is_some_and(|ids| ids.contains(&item["id"]))
                && job["prepareBundle"]["dependencyDigest"].is_string()
        })?;
        Some((item["id"].as_str()?.to_owned(), previous.clone()))
    }).collect();
    for (item_id, previous) in assessments {
        let item = super::row_mut(d, "items", &item_id).unwrap();
        item["autoPreparation"]["status"] = json!("needs_attention");
        item["autoPreparation"]["requiresReview"] = json!(true);
        item["autoPreparation"]["jobId"] = previous["id"].clone();
        item["autoPreparation"]["inputDigest"] = previous["autoPreparationInputs"][&item_id]["inputDigest"].as_str()
            .map(|digest| json!(digest)).unwrap_or_else(|| previous["prepareBundle"]["dependencyDigest"].clone());
        item["autoPreparation"]["retryAt"] = Value::Null;
        item["autoPreparation"]["updatedAt"] = json!(stamp(now));
        item["autoPreparation"]["reason"] = outcome_for_item(&previous, &item_id).unwrap()["reason"].clone();
        item["autoPreparation"]["reviewReason"] = json!("Сохранённый анализ восстановлен. Решение требует проверки по текущему контексту.");
        item["reason"] = item["autoPreparation"]["reason"].clone();
        item["decision"] = json!("needs_attention");
        super::bump(item);
    }
}

fn failed(d: &mut Value, item_id: &str, job: &str, reason: &str, transient: bool, now: i64) {
    let review_resume_required=super::row(d,"jobs",job).is_ok_and(crate::preparation_review::chunks::present);
    let reserved=super::row(d,"jobs",job).is_ok_and(|j|!j["scopeReservation"].is_null());
    let no_dispatch=matches!(reason,"ASSISTANT_BUSY"|"Adapter failed (ASSISTANT_BUSY)");
    let recovery_required=reserved&&!no_dispatch;
    let protected = super::row(d,"items",item_id).is_ok_and(|item|
        item["draftEdited"]==true || !item["draft"].as_str().unwrap_or("").trim().is_empty()
            || !item["autoPreparation"]["humanOverrideAt"].is_null() || item["workflow"]!="attention")
        || super::list(d,"proposals").iter().any(|p|p["itemId"]==item_id
            && matches!(p["status"].as_str(),Some("draft"|"approved"|"dispatching"|"unknown")))
        || super::list(d,"operations").iter().any(|o|o["itemId"]==item_id
            && matches!(o["status"].as_str(),Some("dispatching"|"unknown")));
    if let Ok(item) = super::row_mut(d, "items", item_id) {
        if item["autoPreparation"]["jobId"] != job {
            return;
        }
        let attempts = item["autoPreparation"]["attempts"]
            .as_u64()
            .unwrap_or(MAX_ATTEMPTS);
        let retry = transient && attempts < MAX_ATTEMPTS && !protected && !review_resume_required && !recovery_required;
        item["autoPreparation"]["status"] = json!("error");
        if review_resume_required {item["autoPreparation"]["reviewResumeRequired"]=json!(true);}
        item["autoPreparation"]["reason"] = json!(reason);
        item["autoPreparation"]["updatedAt"] = json!(stamp(now));
        item["autoPreparation"]["retryAt"] = if retry {
            json!(stamp(now + 60 * attempts.max(1) as i64))
        } else {
            Value::Null
        };
        if !protected {
            if recovery_required {
                item["autoPreparation"]["requiresReview"]=json!(true);
                item["autoPreparation"]["reasonCode"]=json!("paid_attempt_recovery_required");
                item["autoPreparation"]["reviewReason"]=json!("Исходная попытка требует проверки и восстановления; автоматический повтор не запускается.");
                item["reason"]=json!(format!("Автоподготовка: {reason}. Исходная попытка требует проверки и восстановления; автоматический повтор не запускается."));
            }else{item["reason"] = json!(format!("Автоподготовка: {reason}"));}
            item["decision"] = json!("needs_attention");
        }
    }
    // Only the trusted returned-worker failure path can save a no-result
    // witness. Recovery/partial output/uncertain attempts fail this capture and
    // retain their reservation. The ordinary attempt budget still owns retry.
    if transient && !protected && no_dispatch {
        if let Ok(witness)=super::preparation_reservations::capture_failed_no_result(d,job,reason) {
            if let Ok(saved)=super::row_mut(d,"jobs",job) {saved["scopeFailure"]=witness;}
        }
    }
}

/// Project interrupted job state even when background generation is disabled.
/// This only updates recovery metadata; it never claims or starts model work.
pub(crate) fn recover_jobs(d: &mut Value, now: i64) {
    let running: Vec<Value> = super::list(d, "items")
        .iter()
        .filter(|i| i["autoPreparation"]["status"] == "running")
        .cloned()
        .collect();
    for item in running {
        let Some(item_id) = item["id"].as_str() else {
            continue;
        };
        let job_id = item["autoPreparation"]["jobId"].as_str().unwrap_or("");
        let status = super::row(d, "jobs", job_id)
            .ok()
            .and_then(|j| j["status"].as_str())
            .unwrap_or("missing")
            .to_owned();
        if status == "running" || status == "queued" {
            continue;
        }
        // An operator decision made while the old job was running owns the
        // displayed workflow/reason. Correct the obsolete spinner only.
        let protected = item["draftEdited"] == true
            || !item["draft"].as_str().unwrap_or("").trim().is_empty()
            || !item["autoPreparation"]["humanOverrideAt"].is_null()
            || item["workflow"] != "attention"
            || super::list(d, "proposals").iter().any(|p| p["itemId"] == item_id
                && matches!(p["status"].as_str(), Some("draft" | "approved" | "dispatching" | "unknown")))
            || super::list(d, "operations").iter().any(|o| o["itemId"] == item_id
                && matches!(o["status"].as_str(), Some("dispatching" | "unknown")));
        let resumable=super::row(d,"jobs",job_id).is_ok_and(crate::preparation_review::chunks::present);
        if protected {
            let auto = &mut super::row_mut(d, "items", item_id).unwrap()["autoPreparation"];
            if resumable { auto["reviewResumeRequired"] = json!(true); }
            auto["status"] = json!("error");
            auto["reason"] = json!("Предыдущая подготовка прервана; сохранённое решение оставлено");
            auto["updatedAt"] = json!(stamp(now));
            auto["retryAt"] = Value::Null;
            continue;
        }
        failed(
            d,
            item_id,
            job_id,
            if status == "cancelled" {
                "Подготовка отменена оператором"
            } else {
                "Предыдущая подготовка прервана; результат не принят"
            },
            !resumable && (status == "interrupted" || status == "failed" || status == "missing"),
            now,
        );
    }
    let interrupted_reviews: Vec<String> = super::list(d, "items").iter().filter_map(|item| {
        if item["autoRevalidation"]["status"] != "running" { return None; }
        let job = item["autoRevalidation"]["jobId"].as_str()?;
        let status = super::row(d, "jobs", job).ok()
            .and_then(|j| j["status"].as_str()).unwrap_or("missing");
        (!matches!(status, "running" | "queued"))
            .then(|| item["id"].as_str().map(str::to_owned)).flatten()
    }).collect();
    for id in interrupted_reviews {
        let state = &mut super::row_mut(d, "items", &id).unwrap()["autoRevalidation"];
        state["status"] = json!("held");
        state["reason"] = json!("Перепроверка прервана. Сохранённое решение оставлено; автоматический повтор не запускается.");
        state["finishedAt"] = json!(stamp(now));
    }
}

// Jobs are appended in durable admission order. Only automatic preparation
// classes choose the next turn; personal discussion and manual jobs do not.
// This has no independent scheduler state to lose on restart or rollback.
fn revalidation_turn(d: &Value) -> bool {
    super::list(d, "jobs").iter().rev().find(|job| job["kind"]=="assistant"
        && matches!(job["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate")))
        .is_some_and(|job|job["purpose"]=="auto_prepare")
}

/// Refresh durable eligibility and claim at most one preparation slot.
pub fn claim(d: &mut Value, now: i64) -> super::ApiResult<Option<(String, Value)>> {
    claim_with_capacity(d,now,None)
}

/// A capacity plan narrows only unclaimed new recipients. Existing paid jobs,
/// prior attempts and the immutable source/operation guards stay authoritative.
fn claim_with_capacity(d:&mut Value,now:i64,capacity:Option<(&[String],&[String])>)->super::ApiResult<Option<(String,Value)>> {
    reconcile_claim_state(d,now)?;
    claim_reconciled(d,now,capacity,1)
}
pub(crate) fn ordinary_claim_ready(d:&Value,now:i64,workers:usize)->super::ApiResult<bool>{
    let mut preview=d.clone();reconcile_claim_state(&mut preview,now)?;
    Ok(claim_reconciled(&mut preview,now,None,workers)?.is_some())
}
fn reconcile_claim_state(d:&mut Value,now:i64)->super::ApiResult<()> {
    // Register source work in the same transaction before checking its wait
    // window; independently scheduled media/assistant ticks must not race.
    crate::media_queue::reconcile(d,&stamp(now))?;
    reconcile_stale(d, now);
    recover_jobs(d, now);
    hold_legacy_repreparations(d, now);
    Ok(())
}
// Availability succeeds monotonically for subsets of one captured selection.
// Check healthy queues with one ledger scan; split only conflicting ranges to
// preserve available siblings, their order and the existing reservation rules.
pub(crate) fn retain_available(ids:&[String],check:&mut impl FnMut(&[String])->bool,out:&mut Vec<String>) {
    if ids.is_empty() {return;}
    if check(ids) {out.extend_from_slice(ids);}
    else if ids.len()>1 {
        let middle=ids.len()/2;
        retain_available(&ids[..middle],check,out);
        retain_available(&ids[middle..],check,out);
    }
}
fn claim_reconciled(d:&mut Value,now:i64,capacity:Option<(&[String],&[String])>,workers:usize)->super::ApiResult<Option<(String,Value)>> {
    claim_reconciled_excluding(d,now,capacity,workers,&std::collections::BTreeSet::new())
}
/// Wake-local graph exclusions advise selection only. Reconciliation, current
/// dependencies, reservations and durable admission remain the ordinary reducer.
fn claim_reconciled_excluding(d:&mut Value,now:i64,capacity:Option<(&[String],&[String])>,workers:usize,
    excluded:&std::collections::BTreeSet<String>)->super::ApiResult<Option<(String,Value)>> {
    // The actual tick requires a configured finite native policy. Scoped pure
    // reducer fixtures retain legacy discovery; configured policies guard every
    // claim, including speculative capacity discovery and late durable commit.
    if d["settings"]["autoPreparation"]["continuousPreparation"].is_object()
        && crate::continuous_preparation::admission_reason(d).is_some(){return Ok(None);}
    // Alternate ready classes within this same claim transaction. A blocked
    // review (including its stability delay) must not leave initial work idle.
    let review_first=revalidation_turn(d);
    if review_first {
        if let Some(claimed)=revalidation::claim(d,now)? {return Ok(Some(claimed));}
        // Stop refilling only for a stable, actually claimable review. The
        // existing exclusive review path runs when current owners finish.
        if revalidation::ready(d,now)? {return Ok(None);}
    }
    let admission=crate::preparation_workers::AutomaticAdmission::capture(d,workers);
    if !admission.available() {return Ok(None);}
    let attention:Vec<Value>=super::list(d,"items").iter().filter(|i|i["workflow"]=="attention").cloned().collect();
    let media_states=crate::media_queue::preparation_states(d,&attention,&stamp(now))?;
    let media_wait:Vec<(String,Option<&'static str>)>=super::list(d,"items").iter().filter_map(|item|{
        let id=item["id"].as_str()?;
        let state=if item["workflow"]=="attention" && item["draftEdited"]!=true
            && item["draft"].as_str().unwrap_or("").trim().is_empty()
            && item["autoPreparation"]["humanOverrideAt"].is_null() {
                media_states.get(id).copied().unwrap_or(Some("media_unavailable"))
            }else{None};
        // Missing default media is a background task until the exact semantic
        // decision depends on it. Do not display a preparation-wide wait.
        let state=state.filter(|_|!crate::decision_media::may_assess(d,item).unwrap_or(false));
        (state.is_some() || item.get("preparationMediaWait").is_some()).then(||(id.to_owned(),state))
    }).collect();
    for (id,state) in media_wait {
        let item=super::row_mut(d,"items",&id)?;
        if let Some(state)=state {item["preparationMediaWait"]=json!({"status":state,"reason":if state=="media_unavailable" {"Для подготовки пока нет всех обязательных доказательств: полного аудиотекста либо визуального контекста, если он отдельно потребован владельцем."}else{"Ждём проверенный полный аудиотекст и, если он отдельно потребован владельцем, визуальный контекст."}});}
        else {item.as_object_mut().unwrap().remove("preparationMediaWait");}
    }
    let mut candidates: Vec<Value> = super::list(d, "items")
        .iter()
        .filter(|i| eligible_with_media(d, i, now,media_states.get(i["id"].as_str().unwrap_or("")).map_or(true,Option::is_some)))
        .cloned()
        .collect();
    candidates.sort_by_key(|i| {
        (
            i["createdAt"].as_str().unwrap_or("").to_owned(),
            i["id"].as_str().unwrap_or("").to_owned(),
        )
    });
    // The following application loop changes only autoPreparation, reason,
    // decision and revision. Evidence excludes the first three; dependency_digest
    // removes revision. Thus all candidate fingerprints may be computed against
    // this immutable phase before those bookkeeping writes, in the same order.
    // Time-sensitive source selection is still evaluated on every fingerprint.
    let evaluated:Vec<_>={
        let context=super::prepare_bundle::EvidenceContext::new(d);
        candidates.into_iter().filter_map(|snapshot|{
            let id=snapshot["id"].as_str()?;
            if snapshot["autoPreparation"]["status"]=="running"{return None;}
            let fingerprint=context.fingerprint(id);
            Some((snapshot,fingerprint))
        }).collect()
    };
    let mut ready = Vec::new();
    for (snapshot,fingerprint) in evaluated {
        let item_id=snapshot["id"].as_str().unwrap();
        let fingerprint = match fingerprint {
            Ok(fingerprint) => fingerprint,
            Err(reason) => {
                if snapshot["autoPreparation"]["attempts"].as_u64().unwrap_or(0) > 0
                    || snapshot["autoPreparation"]["jobId"].is_string() {
                    // Invalid new evidence must not erase an earlier paid
                    // attempt and become fresh work when that evidence repairs.
                    let item = super::row_mut(d, "items", item_id)?;
                    item["autoPreparation"]["requiresReview"] = json!(true);
                    item["autoPreparation"]["retryAt"] = Value::Null;
                    item["autoPreparation"]["reason"] = json!(reason);
                    item["autoPreparation"]["updatedAt"] = json!(stamp(now));
                    item["reason"] = json!(reason);
                    continue;
                }
                let evidence = json!([
                    snapshot["id"],
                    snapshot["branchId"],
                    snapshot["contextEvidenceDigest"],
                    snapshot["branchContextDigest"],
                    reason
                ]);
                let fallback = format!("{:x}", Sha256::digest(evidence.to_string().as_bytes()));
                if snapshot["autoPreparation"]["inputDigest"] == fallback
                    && snapshot["autoPreparation"]["status"] == "needs_attention"
                {
                    continue;
                }
                let item = super::row_mut(d, "items", item_id)?;
                item["autoPreparation"] = json!({"status":"needs_attention","inputDigest":fallback,"jobId":null,"attempts":0,"reason":reason,"updatedAt":stamp(now)});
                item["reason"] = json!(reason);
                item["decision"] = json!("needs_attention");
                continue;
            }
        };
        let old = &snapshot["autoPreparation"];
        let same = old["inputDigest"] == fingerprint;
        if !same && old["status"] == "needs_attention"
            && old["jobId"].as_str().and_then(|job| super::row(d, "jobs", job).ok()).is_some_and(|job| {
                job["purpose"] == "auto_prepare" && job["status"] == "completed"
                    && outcome_for_item(job, item_id).is_some_and(|outcome| outcome["status"] == "needs_attention")
            }) {
            // A successful assessment is a durable result even when it proposed no
            // reply. A changed schema/context is not an unlimited reanalysis budget.
            let item = super::row_mut(d, "items", item_id)?;
            item["autoPreparation"]["requiresReview"] = json!(true);
            item["autoPreparation"]["updatedAt"] = json!(stamp(now));
            item["autoPreparation"]["reason"] = json!("Контекст изменился после анализа. Сохранённое решение требует перепроверки; прежний анализ сохранён.");
            item["reason"] = item["autoPreparation"]["reason"].clone();
            super::bump(item);
            continue;
        }
        // A changed input is a new generation, not another bounded attempt at
        // the original input. W2 does not grant an inter-generation spend budget.
        // Unattempted capacity holds can still become eligible after source repair.
        if !same && (old["attempts"].as_u64().unwrap_or(0) > 0 || old["jobId"].is_string()) {
            let item = super::row_mut(d, "items", item_id)?;
            item["autoPreparation"]["requiresReview"] = json!(true);
            item["autoPreparation"]["retryAt"] = Value::Null;
            item["autoPreparation"]["updatedAt"] = json!(stamp(now));
            item["autoPreparation"]["reason"] = json!("Контекст изменился после попытки подготовки. Нужна проверка перед новым запуском.");
            item["reason"] = item["autoPreparation"]["reason"].clone();
            continue;
        }
        if same {
            match old["status"].as_str() {
                Some("prepared" | "needs_attention") => continue,
                Some("error")
                    if old["attempts"].as_u64().unwrap_or(MAX_ATTEMPTS) >= MAX_ATTEMPTS
                        || !time(&old["retryAt"]).is_some_and(|t| t <= now) =>
                {
                    continue;
                }
                _ => (),
            }
        }
        let attempts = if same {
            old["attempts"].as_u64().unwrap_or(0)
        } else {
            0
        };
        let item = super::row_mut(d, "items", item_id)?;
        if !same || old["status"] != "queued" {
            item["autoPreparation"] = json!({"status":"queued","inputDigest":fingerprint,"jobId":null,"attempts":attempts,"reason":"Ожидает автоматической подготовки","updatedAt":stamp(now)});
        }
        ready.push(item_id.to_owned());
    }
    explain_group_review_holds(d);
    // Exclude occupied families and recipient/branch aliases before the planner
    // selects its first family. Reservations outlive completed/UNKNOWN jobs.
    // These deferred recipients retain their queue state and spend no attempt.
    ready.retain(|id| super::row(d,"items",id).is_ok_and(|item|admission.permits(d,item)));
    let mut available=Vec::new();
    retain_available(&ready,&mut |ids|super::preparation_reservations::assert_available(d,ids,None).is_ok(),&mut available);
    ready=available;
    ready.retain(|id|!excluded.contains(id));
    if let Some((selected,oversized))=capacity {
        for id in oversized.iter().filter(|id|ready.contains(id)) {
            let item=super::row_mut(d,"items",id)?;
            item["autoPreparation"]["status"]=json!("needs_attention");
            item["autoPreparation"]["reason"]=json!("model_context_capacity_exceeded");
            item["reason"]=json!("model_context_capacity_exceeded");item["decision"]=json!("needs_attention");
        }
        ready.retain(|id|selected.contains(id));
    }
    let (bundles,held)=super::prepare_plan::automatic_packed_bundles(d,&ready);
    for (item_id, reason) in held {
        let item = super::row_mut(d, "items", &item_id)?;
        item["autoPreparation"]["status"] = json!("needs_attention");
        item["autoPreparation"]["reason"] = json!(reason);
        item["reason"] = json!(reason);
        item["decision"] = json!("needs_attention");
    }
    if let Some(bundle)=bundles.into_iter().next() {
        let groups=super::prepare_bundle::capture_groups(d,&bundle).map_err(super::bad)?;
        let item_ids = bundle_item_ids(&bundle)?;
        if super::preparation_reservations::assert_available(d,&item_ids,None).is_err() {return Ok(None);}
        let request = bundle["request"].clone();
        let inputs: serde_json::Map<String, Value> = item_ids.iter().map(|id| {
            let auto = &super::row(d, "items", id).unwrap()["autoPreparation"];
            (id.clone(), json!({"inputDigest":auto["inputDigest"],"attempt":auto["attempts"].as_u64().unwrap_or(0)+1}))
        }).collect();
        let job = super::new_job(d, "assistant", &item_ids[0])?;
        let stored = super::row_mut(d, "jobs", &job)?;
        stored["purpose"] = json!("auto_prepare");
        stored["requestedItemIds"] = json!(item_ids);
        stored["autoPreparationInputs"] = json!(inputs);
        stored["preparationStages"]["groupAdmission"]=groups;
        stored["prepareBundle"] = bundle;
        if let Some(policy)=crate::fact_followup::capture_automatic_policy(d,super::row(d,"jobs",&job)?) {
            super::row_mut(d,"jobs",&job)?["automaticFactPolicy"]=policy;
        }
        let reservation=super::preparation_reservations::capture(d,&job)?;
        super::preparation_reservations::check(d,&reservation,Some(&job))?;
        super::row_mut(d,"jobs",&job)?["scopeReservation"]=reservation;
        if let Some(scope)=crate::preparation_workers::capture(d,super::row(d,"jobs",&job)?) {
            super::row_mut(d,"jobs",&job)?["preparationWorkerScope"]=scope;
        }
        for item_id in item_ids {
            let item = super::row_mut(d, "items", &item_id)?;
            item["autoPreparation"]["status"] = json!("running");
            item["autoPreparation"]["jobId"] = json!(job);
            item["autoPreparation"]["attempts"] =
                json!(item["autoPreparation"]["attempts"].as_u64().unwrap_or(0) + 1);
            item["autoPreparation"]["reason"] = json!("Ассистент проверяет контекст");
        }
        if crate::continuous_preparation::enabled(d){crate::continuous_preparation::stamp_background_job(d,&job,None)?;}
        return Ok(Some((job, request)));
    }
    if review_first || capacity.is_some() {Ok(None)} else {revalidation::claim(d, now)}
}

pub fn complete(d: &mut Value, job_id: &str, result: &Value, now: i64) -> super::ApiResult<Value> {
    if super::row(d, "jobs", job_id)?["purpose"] == "auto_revalidate" {
        return revalidation::complete(d, job_id, result, now);
    }
    complete_initial(d, job_id, result, now)
}
// Shared by the full revalidation fallback and the bounded initial-preparation
// storage path. This is settlement of returned work, not new paid admission.
pub(crate) fn settle_success(d:&mut Value,run:&str,result:&Value,reviewed:bool,at:Option<&str>)->super::ApiResult<Value>{
    if reviewed {crate::preparation_review::chunks::current(d,super::row(d,"jobs",run)?)?;}
    // Production samples after acquiring the writer lock, just as the previous
    // broad closure did. Explicit timestamps are for deterministic reducers.
    let timestamp=if let Some(at)=at {chrono::DateTime::parse_from_rfc3339(at).map_err(|_|super::bad("Invalid preparation completion time"))?.timestamp()}
        else{chrono::Utc::now().timestamp()};
    let outcome=complete(d,run,result,timestamp)?;
    if reviewed {crate::preparation_review::record_review(d,run,Ok(result),&at.map(str::to_owned).unwrap_or_else(super::now))?;}
    Ok(outcome)
}
fn bundle_item_ids(bundle: &Value) -> super::ApiResult<Vec<String>> {
    let ids = bundle["itemIds"].as_array()
        .filter(|ids| !ids.is_empty() && ids.len() <= super::prepare_plan::AUTO_GROUP_MAX_ITEMS)
        .ok_or_else(|| super::bad("Automatic preparation targets missing"))?;
    let mut seen = std::collections::HashSet::new();
    ids.iter().map(|id| {
        let id = id.as_str().filter(|id| !id.is_empty())
            .ok_or_else(|| super::bad("Automatic preparation target invalid"))?;
        if !seen.insert(id) { return Err(super::bad("Automatic preparation duplicate target")); }
        Ok(id.to_owned())
    }).collect()
}

fn fail_claimed(d: &mut Value, job_id: &str, reason: &str, transient: bool, now: i64) -> super::ApiResult<()> {
    let job=super::row(d,"jobs",job_id)?;
    let ids=if job["preparationStages"]["first"]["status"]=="completed"
        &&job["preparationStages"]["groupAdmission"].is_array(){
        job["preparationStages"]["groupAdmission"].as_array().unwrap().iter().filter(|g|g["status"]=="pending")
            .flat_map(|g|g["itemIds"].as_array().cloned().unwrap_or_default())
            .filter_map(|v|v.as_str().map(str::to_owned)).collect()
    }else{bundle_item_ids(&job["prepareBundle"])?};
    for id in ids { failed(d, &id, job_id, reason, transient, now); }
    Ok(())
}

fn decision_matches(request:&Value,decision:&str,candidates:&[&Value])->bool{
    match decision {
        "needs_attention"=>candidates.is_empty(),
        "reply"|"close"=>candidates.len()==1&&candidates[0]["kind"]==if decision=="reply"{"reply_and_close"}else{"close"},
        "hide"|"delete" if request["preparationMode"]=="single_pass_v1"=>
            candidates.len()==1&&candidates[0]["kind"]==decision&&candidates[0]["text"]=="",
        _=>false,
    }
}
fn complete_initial(d: &mut Value, job_id: &str, result: &Value, now: i64) -> super::ApiResult<Value> {
    let job = super::row(d, "jobs", job_id)?.clone();
    if job["preparationStages"]["first"]["status"]=="completed"
        &&job["preparationStages"]["groupAdmission"].is_array(){
        return complete_grouped(d,job_id,result,now,job["preparationStages"]["first"]["reviewRequired"]==true,true);
    }
    let item_ids = bundle_item_ids(&job["prepareBundle"])?;
    if job["status"] != "running" || item_ids.iter().any(|id| {
        super::row(d, "items", id).map_or(true, |item|
            item["autoPreparation"]["jobId"] != job_id || item["autoPreparation"]["status"] != "running")
    }) {
        return Err(super::conflict(
            "Automatic preparation no longer owns this item",
        ));
    }
    // Recheck eligibility at admission: a human draft or active action wins this race.
    if super::prepare_bundle::current(d, &job["prepareBundle"]).is_err()
        || item_ids.iter().any(|id| {
            let item = super::row(d, "items", id).unwrap();
            !eligible(d, item, now) || super::list(&job["prepareBundle"]["request"], "items").iter()
                .find(|original| original["id"] == *id).is_none_or(|original| original["revision"] != item["revision"])
        }) {
        fail_claimed(d, job_id, "Контекст изменился во время подготовки", false, now)?;
        let items: Vec<Value> = item_ids.iter().map(|id| json!({"status":"stale","itemId":id,"reason":"Context changed during automatic preparation"})).collect();
        let outcome = if item_ids.len() == 1 { items[0].clone() }
            else { json!({"status":"stale","itemIds":item_ids,"items":items,"reason":"Context changed during automatic preparation"}) };
        super::row_mut(d, "jobs", job_id)?["prepareOutcome"] = outcome.clone();
        return Ok(outcome);
    }
    let assessments = result["assessments"]
        .as_array()
        .filter(|a| a.len() == item_ids.len())
        .ok_or_else(|| super::bad("Automatic preparation requires one assessment per target"))?;
    let proposals = result["proposals"]
        .as_array()
        .ok_or_else(|| super::bad("Automatic preparation proposals missing"))?;
    if proposals.iter().any(|p| !item_ids.iter().any(|id| p["itemId"] == *id)) {
        return Err(super::bad("Automatic preparation proposal target mismatch"));
    }
    // Validate the full result before admitting any member. No duplicate/missing
    // assessment or foreign proposal can turn a partial result into completion.
    let mut validated = Vec::new();
    for item_id in &item_ids {
        let matching: Vec<&Value> = assessments.iter().filter(|a| a["itemId"] == *item_id).collect();
        if matching.len() != 1 { return Err(super::bad("Automatic preparation assessment target mismatch")); }
        let assessment = matching[0];
        let tags = assessment_tags(assessment)?;
        let reason = assessment["reason"].as_str().filter(|s| !s.trim().is_empty() && s.len() <= 12000)
            .ok_or_else(|| super::bad("Automatic preparation reason missing"))?;
        let decision = assessment["outcome"].as_str().unwrap_or("");
        let candidates: Vec<&Value> = proposals.iter().filter(|p| p["itemId"] == *item_id).collect();
        let valid = decision_matches(&job["prepareBundle"]["request"],decision,&candidates);
        if !valid { return Err(super::bad("Automatic preparation assessment and proposal disagree")); }
        validated.push((item_id.clone(), tags, reason, decision));
    }
    let outcome = super::prepare_bundle::admit_to(d, job_id, None, result)?;
    let mut items = Vec::new();
    for (item_id, tags, reason, decision) in validated {
        let prepared = outcome["candidates"].as_array()
            .is_some_and(|rows| rows.iter().any(|v| v["itemId"] == item_id && v["status"] == "review"));
        let item = super::row_mut(d, "items", &item_id)?;
        item["autoPreparation"]["status"] = json!(if prepared { "prepared" } else { "needs_attention" });
        item["autoPreparation"]["reason"] = json!(reason);
        item["autoPreparation"]["updatedAt"] = json!(stamp(now));
        item["autoPreparation"]["retryAt"] = Value::Null;
        item["decision"] = json!(decision);
        // Descriptive evidence for the operator/model; never action authorization.
        item["triageTags"] = tags;
        item["reason"] = json!(reason);
        if prepared { item["workflow"] = json!("prepared"); }
        items.push(json!({"status":item["autoPreparation"]["status"],"itemId":item_id,"reason":reason}));
    }
    let final_outcome = if items.len() == 1 {
        let mut single = items.remove(0); single["admission"] = outcome; single
    } else {
        json!({"status":if items.iter().all(|item| item["status"] == "prepared") { "prepared" } else { "needs_attention" },
            "itemIds":item_ids,"items":items,"admission":outcome})
    };
    super::row_mut(d, "jobs", job_id)?["prepareOutcome"] = final_outcome.clone();
    Ok(final_outcome)
}

/// Settle routine groups from an already persisted original first response.
/// Recovery callers do not dispatch a model or finalize pending review groups.
pub(crate) fn settle_first_groups(d:&mut Value,job_id:&str,result:&Value,at:i64)->super::ApiResult<Value>{
    complete_grouped(d,job_id,result,at,false,false)
}

fn complete_grouped(d:&mut Value,job_id:&str,result:&Value,now:i64,reviewed:bool,finalize:bool)->super::ApiResult<Value>{
    let job=super::row(d,"jobs",job_id)?.clone();
    if job["purpose"]!="auto_prepare"||job["status"]!="running"{return Err(super::conflict("Automatic group owner changed"));}
    let review_ids:std::collections::BTreeSet<String>=crate::preparation_review::plan_review_for_job(&job)
        .map_err(super::bad)?.and_then(|v|v["items"].as_array().cloned()).unwrap_or_default()
        .iter().filter_map(|v|v["id"].as_str().map(str::to_owned)).collect();
    let groups=job["preparationStages"]["groupAdmission"].as_array().ok_or_else(||super::conflict("Automatic group plan missing"))?;
    for (index,group) in groups.iter().enumerate(){
        if group["status"]!="pending"{continue;}
        let ids=group["itemIds"].as_array().ok_or_else(||super::bad("Automatic group recipients missing"))?;
        let needs_review=ids.iter().any(|id|id.as_str().is_some_and(|v|review_ids.contains(v)));
        if needs_review!=reviewed{continue;}
        let scoped=super::engine_prepare::group_result(result,ids);
        let assessments=scoped["assessments"].as_array().filter(|a|a.len()==ids.len())
            .ok_or_else(||super::bad("Automatic group assessment coverage mismatch"))?;
        let mut validated=Vec::new();
        for id in ids {
            let id=id.as_str().ok_or_else(||super::bad("Automatic group recipient invalid"))?;
            let matches:Vec<_>=assessments.iter().filter(|a|a["itemId"]==id).collect();
            if matches.len()!=1{return Err(super::bad("Automatic group assessment mismatch"));}
            let assessment=matches[0];let tags=assessment_tags(assessment)?;
            let reason=assessment["reason"].as_str().filter(|s|!s.trim().is_empty()&&s.len()<=12000)
                .ok_or_else(||super::bad("Automatic group reason missing"))?.to_owned();
            let decision=assessment["outcome"].as_str().unwrap_or("");
            let proposals:Vec<_>=scoped["proposals"].as_array().into_iter().flatten().filter(|p|p["itemId"]==id).collect();
            if !decision_matches(&job["prepareBundle"]["request"],decision,&proposals){
                return Err(super::bad("Automatic group proposal mismatch"));
            }
            validated.push((id.to_owned(),tags,reason,decision.to_owned()));
        }
        let stale=super::prepare_bundle::current_group(d,&job["prepareBundle"],group).is_err()
            ||validated.iter().any(|(id,_,_,_)|super::row(d,"items",id).map_or(true,|item|
                item["autoPreparation"]["jobId"]!=job_id||item["autoPreparation"]["status"]!="running"
                    ||!eligible(d,item,now)||super::list(&job["prepareBundle"]["request"],"items").iter()
                        .find(|original|original["id"]==*id).is_none_or(|original|original["revision"]!=item["revision"])));
        let mut admission=if stale{json!({"status":"stale","reason":"Context changed during automatic preparation","candidates":[]})}
            else{super::prepare_bundle::admit_group(d,job_id,&scoped,group)?};
        let mut items=Vec::new();
        for (id,tags,reason,decision) in validated {
            if admission["status"]=="stale" {
                failed(d,&id,job_id,"Контекст изменился во время подготовки",false,now);
                items.push(json!({"status":"stale","itemId":id,"reason":"Context changed during automatic preparation"}));
                continue;
            }
            let prepared=admission["candidates"].as_array().is_some_and(|rows|rows.iter().any(|v|v["itemId"]==id&&v["status"]=="review"));
            let item=super::row_mut(d,"items",&id)?;
            item["autoPreparation"]["status"]=json!(if prepared{"prepared"}else{"needs_attention"});
            item["autoPreparation"]["reason"]=json!(reason);
            item["autoPreparation"]["updatedAt"]=json!(stamp(now));item["autoPreparation"]["retryAt"]=Value::Null;
            item["decision"]=json!(decision);item["triageTags"]=tags;item["reason"]=json!(reason);
            if prepared{item["workflow"]=json!("prepared");}
            items.push(json!({"status":item["autoPreparation"]["status"],"itemId":id,"reason":reason}));
        }
        admission["items"]=json!(items);
        let saved=&mut super::row_mut(d,"jobs",job_id)?["preparationStages"]["groupAdmission"][index];
        saved["status"]=json!(if admission["status"]=="stale"{"stale"}else{"admitted"});saved["admission"]=admission;
    }
    if !finalize{return Ok(json!({"status":"partial","jobId":job_id}));}
    let job=super::row(d,"jobs",job_id)?.clone();
    let groups=job["preparationStages"]["groupAdmission"].as_array().unwrap();
    if groups.iter().any(|g|g["status"]=="pending"){return Err(super::conflict("Automatic group still pending"));}
    let items:Vec<Value>=groups.iter().flat_map(|g|g["admission"]["items"].as_array().cloned().unwrap_or_default()).collect();
    let admission=json!({"status":if groups.iter().any(|g|g["status"]=="stale"){"stale"}else{"review"},
        "candidates":groups.iter().flat_map(|g|g["admission"]["candidates"].as_array().cloned().unwrap_or_default()).collect::<Vec<_>>()});
    let outcome=if items.len()==1{let mut single=items[0].clone();single["admission"]=admission;single}
        else{json!({"status":if items.iter().all(|item|item["status"]=="prepared"){"prepared"}else if items.iter().any(|item|item["status"]=="stale"){"stale"}else{"needs_attention"},
            "itemIds":job["prepareBundle"]["itemIds"],"items":items,"admission":admission})};
    super::row_mut(d,"jobs",job_id)?["prepareOutcome"]=outcome.clone();Ok(outcome)
}

fn transient(reason: &str) -> bool {
    [
        "ASSISTANT_BUSY",
        "ASSISTANT_FAILED",
        "ADAPTER_TIMEOUT",
        "ADAPTER_PROCESS_FAILED",
        "Adapter timed out",
        "Adapter process failed",
    ]
    .iter()
    .any(|part| reason.contains(part))
}
fn model_preflight(d:&Value,run:&str)->super::ApiResult<()> {
    let job=super::row(d,"jobs",run)?;
    if job["status"]!="running" {return Err(super::conflict("Automatic preparation cancelled before model call"));}
    let grouped=job["purpose"]=="auto_prepare"&&job["preparationStages"]["first"]["status"]=="completed"
        &&job["preparationStages"]["groupAdmission"].is_array();
    if !grouped{super::prepare_bundle::current(d,&job["prepareBundle"]).map_err(super::conflict)?;}
    let ids=if grouped{job["preparationStages"]["groupAdmission"].as_array().unwrap().iter()
        .filter(|g|g["status"]=="pending").flat_map(|g|g["itemIds"].as_array().cloned().unwrap_or_default())
        .filter_map(|v|v.as_str().map(str::to_owned)).collect::<Vec<_>>()}
        else{bundle_item_ids(&job["prepareBundle"])?};
    if job.get("scopeReservation").is_some() {
        super::preparation_reservations::assert_available(d,&ids,Some(run))?;
    }
    for id in ids {
        if grouped {
            let group=job["preparationStages"]["groupAdmission"].as_array().unwrap().iter()
                .find(|g|g["itemIds"].as_array().is_some_and(|ids|ids.contains(&json!(id))))
                .ok_or_else(||super::conflict("Automatic preparation group missing"))?;
            super::prepare_bundle::current_group(d,&job["prepareBundle"],group).map_err(super::conflict)?;
        }
        let item=super::row(d,"items",&id)?;
        if !crate::decision_media::enabled(&job["prepareBundle"]["request"]) && waiting_for_media(d,item,chrono::Utc::now().timestamp()) {
            return Err(super::conflict("Сначала требуется расшифровка видео; модель не запускалась"));
        }
        if super::list(&job["prepareBundle"]["request"], "items").iter()
            .find(|original| original["id"] == id).is_none_or(|original| original["revision"] != item["revision"])
            || (job["purpose"] == "auto_prepare" && (item["autoPreparation"]["jobId"] != run
                || item["autoPreparation"]["status"] != "running")) {
            return Err(super::conflict("Automatic preparation changed before model call"));
        }
    }
    Ok(())
}

fn completed_recovery_candidate(d: &Value) -> Option<(String, String)> {
    super::list(d, "jobs").iter().find_map(|job| {
        if !crate::preparation_review::chunks::completed_candidate(job) { return None; }
        if crate::preparation_workers::pending_conflict(d,job,false) { return None; }
        let plan = job["preparationStages"]["reviewChunks"]["planDigest"].as_str()?;
        if job["completedRecoveryHold"]["expectedPlanDigest"] == plan { return None; }
        Some((job["id"].as_str()?.to_owned(), plan.to_owned()))
    })
}

async fn record_completed_recovery_hold(app: &super::App, run: &str, plan: &str, reason: &str) -> super::ApiResult<()> {
    // This failure only annotates one existing job. Preserve its fresh locked
    // plan/status check without loading unrelated workspace history into writer.
    app.change_job(run, |d| {
        let job = super::row_mut(d, "jobs", run)?;
        if job["preparationStages"]["reviewChunks"]["planDigest"] == plan
            && matches!(job["status"].as_str(), Some("failed" | "interrupted")) {
            job["completedRecoveryHold"] = json!({"expectedPlanDigest":plan,"reason":reason,"heldAt":super::now()});
        }
        Ok(())
    }).await
}

pub async fn tick(app: &super::App) -> super::ApiResult<()> {
    let metadata=app.db.read_metadata().await?;
    if !crate::continuous_preparation::enabled(&metadata){return Ok(());}
    drop(metadata);
    // Fully settled results get one local-only recovery attempt per exact plan.
    // A profile/currentness/ownership conflict is a durable hold, not a fresh
    // generation or a metadata request repeated on every queue tick.
    crate::media_fullframes::refresh(app).await?;
    let mut tail_state=app.read().await?;
    if let Some((run, plan)) = completed_recovery_candidate(&tail_state) {
        if let Err(error) = crate::preparation_review::chunks::recover_completed_job(app, &run, &plan).await {
          if error.1 != "RECOVERY_BUSY" {
            record_completed_recovery_hold(app, &run, &plan, &error.1).await?;
          }
        }
        tail_state=app.read().await?;
    }
    crate::continuous_preparation::tick(app).await?;
    tail_state=app.read().await?;
    // Recovery/current ready projection above remains local even when disabled.
    // An absent or exhausted sustained policy never starts fresh paid work.
    if crate::continuous_preparation::admission_reason(&tail_state).is_some(){return Ok(());}
    // One native tail step uses the same completion wake and durable job ledger.
    // Ready revalidation retains its existing turn; no extra producer is spawned.
    if crate::fact_followup::automatic_enabled(&tail_state) {
        // Fact-only turns must not prevent review stability from being observed.
        // This transaction admits no assistant job or preparation attempt.
        app.change_preparation_claim(|d|{
            let at=chrono::Utc::now().timestamp();reconcile_claim_state(d,at)?;
            revalidation::observe(d,at)
        }).await?;
        tail_state=app.read().await?;
        if !(revalidation_turn(&tail_state)&&revalidation::ready(&tail_state,chrono::Utc::now().timestamp())?)
            && crate::fact_followup::automatic_tick(app,&tail_state).await? {return Ok(());}
    }
    // Independent unpaid capacity checks complete out of order. Every ready
    // result still takes the ordinary fresh, atomic durable claim path.
    scheduler::fill(app).await
}
/// A first paid call needs a new current owner token and its exact durable
/// initial-admission receipt. The caller owns the existing writer rollback.
pub(super) fn reserve_initial_call_captured(d:&mut Value,token:&crate::runtime_lifecycle::OwnerToken,
    run:&str,request:&Value,keys:Option<&std::collections::BTreeSet<String>>,at:&str)->super::ApiResult<()> {
    model_preflight(d,run)?;
    if let Some(expected)=keys {
        if crate::preparation_workers::keys(d,super::row(d,"jobs",run)?).as_ref()!=Some(expected) {
            return Err(super::conflict("Preparation family ownership changed while waiting; model not started"));
        }
    }
    crate::preparation_review::reserve_first_admitted(d,token,run,request,at)
}
pub(super) async fn reserve_initial_call(app:&super::App,run:&str,request:&Value,
    keys:Option<&std::collections::BTreeSet<String>>)->super::ApiResult<()> {
    let token=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await?;
    app.change_preparation_first(run,|d|reserve_initial_call_captured(d,&token,run,request,keys,&super::now())).await
}
pub(crate) fn spawn_review_resume(app:&super::App,job:String){spawn_worker(app,job,None);}
async fn first_response(app:&super::App,run:&str,request:&Value,
    keys:Option<&std::collections::BTreeSet<String>>,retained:Option<Value>,
    token:Option<&crate::runtime_lifecycle::OwnerToken>)->super::ApiResult<Value>{
    if let Some(result)=retained{return Ok(result);}
    let token=token.ok_or_else(||super::conflict("First-pass lifecycle admission missing"))?;
    let native=app.lifecycle_work.begin(crate::runtime_owned_work::Kind::Preparation)?;
    let admitted=app.change_preparation_first(run,|d|reserve_initial_call_captured(d,token,run,request,keys,&super::now())).await;
    if let Err(error)=admitted{native.settled();return Err(error);}
    app.bridge_admitted("assistant",request.clone(),native).await
}
// This function owns the worker slot through FIRST settlement and repair.
// Its caller releases the slot and assistant gate before any manual wait.
async fn run_worker_owned(worker:&super::App,run:&str,request:Option<Value>,retained:Option<Value>,
    keys:Option<std::collections::BTreeSet<String>>,token:Option<&crate::runtime_lifecycle::OwnerToken>)
    ->(bool,super::ApiResult<Value>){
    let preflight=worker.db.read_preparation_context(run).await.and_then(|d|{
        model_preflight(&d,run)?;
        if keys.is_some()&&crate::preparation_workers::keys(&d,super::row(&d,"jobs",run)?)!=keys {
            return Err(super::conflict("Preparation family ownership changed while waiting; model not started"));
        }
        Ok((request,retained))
    });
        let generated=match preflight {
            Err(error)=>Err(error),
            Ok((request,retained))=>match request {
                None=>{
                    let early=worker.change(|d|{
                        let job=super::row(d,"jobs",&run)?.clone();
                        if job["purpose"]=="auto_prepare"&&job["preparationStages"]["groupAdmission"].is_array(){
                            complete_grouped(d,&run,&job["preparationStages"]["first"]["result"],chrono::Utc::now().timestamp(),false,false)?;
                        }Ok(())
                    }).await;
                    match early{Err(error)=>Err(error),Ok(())=>crate::preparation_review::chunks::run(&worker,&run,model_preflight).await.map(|r|(r,true))}
                },
                Some(request)=>match first_response(worker,run,&request,keys.as_ref(),retained,token).await {
                Err(error)=>Err(error),
                Ok(first)=>{
                    // Persist the actual first pass before spending on the stronger
                    // one. A crash cannot silently erase the first-pass evidence.
                    let review=worker.change_preparation_first(&run, |d| {
                        crate::preparation_review::settle_first(d,&run,&request,&first,&super::now())
                    }).await;
                    match review {
                        Err(error)=>Err(error),
                        Ok(None)=>Ok((first,false)),
                        Ok(Some(_))=>{
                            let early=worker.change(|d|{
                                if super::row(d,"jobs",&run)?["purpose"]=="auto_prepare"{
                                    complete_grouped(d,&run,&first,chrono::Utc::now().timestamp(),false,false)?;
                                }Ok(())
                            }).await;
                            match early{Err(error)=>Err(error),Ok(())=>crate::preparation_review::chunks::run(&worker,&run,model_preflight).await.map(|r|(r,true))}
                        },
                    }
                }
            }}
        };
        let mut original_settled=false;
        let result = match generated {
            Ok((result,reviewed)) => {
                match worker.settle_preparation_success(&run,&result,reviewed).await {
                    Err(error)=>Err(error),
                    Ok(outcome)=>{original_settled=true;match crate::answering_repair_plan::run_pending(&worker,&run).await {
                        Err(error)=>Err(error),
                        Ok(None)=>Ok(outcome),
                        Ok(Some(_))=>worker.change(|d| {
                            let saved=super::row(d,"jobs",&run)?["prepareOutcome"].clone();
                            crate::answering_repair_plan::merge_outcome(d,&run,saved)
                        }).await,
                    }},
                }
            }
            Err(error) => Err(error),
        };
    (original_settled,result)
}
fn spawn_worker(app:&super::App,job:String,request:Option<Value>){
    let worker=app.clone();
    let run=job.clone();
    app.spawn(job,async move {
        let mut original_settled=false;
        let result=async {
            // Paid recovery always precedes material acquisition. A missing
            // retained result cannot clear a reserved FIRST admission.
            let retained=match request.as_ref(){
                Some(request)=>crate::preparation_review::recover_first_if_retained(&worker,&run,request).await?,
                None=>None,
            };
            let token=if request.is_some()&&retained.is_none(){
                Some(worker.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await?)
            }else{None};
            let mut material_wait=token.clone().map(crate::engine_prepare::manual_wait::Wait::new);
            let mut request=request;
            loop {
                if let (Some(wait),Some(original))=(material_wait.as_mut(),request.as_ref()){
                    request=Some(crate::engine_prepare::manual_wait::acquire(&worker,&run,original,wait,model_preflight).await?);
                }
                let state=worker.db.read_preparation_context(&run).await?;
                model_preflight(&state,&run)?;
                let keys=crate::preparation_workers::keys(&state,super::row(&state,"jobs",&run)?);drop(state);
                let (settled,result)={
                    let lease=worker.preparation_workers.acquire(keys.clone()).await;
                    let _assistant_guard=if lease.slot()==0{Some(worker.assistant_gate.clone().lock_owned().await)}else{None};
                    lease.scope(run_worker_owned(&worker,&run,request.clone(),retained.clone(),keys,token.as_ref())).await
                };
                original_settled|=settled;
                if !settled&&result.as_ref().is_err_and(|e|crate::manual_frame_request::is_wait_reason(&e.1)){
                    if let (Some(wait),Some(request))=(material_wait.as_mut(),request.as_ref()){
                        wait.admission_race(request);
                        continue;
                    }
                }
                return result;
            }
        }.await;
        if let Err(error) = &result {
            worker
                .change(|d| {
                    if original_settled {
                        // Preserve the original decisions/drafts. Only the
                        // extra frame/repair work failed or remains uncertain.
                        return crate::answering_repair_plan::record_work_failure(d,&run,&error.1);
                    }
                    if super::row(d, "jobs", &run)?["purpose"] == "auto_revalidate" {
                        return revalidation::failed(d, &run, &error.1, chrono::Utc::now().timestamp());
                    }
                    let resumable=super::row(d,"jobs",&run).is_ok_and(crate::preparation_review::chunks::present);
                    fail_claimed(
                        d,
                        &run,
                        &error.1,
                        !resumable&&transient(&error.1),
                        chrono::Utc::now().timestamp(),
                    )?;
                    Ok(())
                })
                .await?;
        }
        result
    });
}

#[cfg(test)]
#[path = "auto_prepare_manual_wait_tests.rs"]
mod manual_wait_tests;
#[cfg(test)]
#[path = "auto_prepare_admission_tests.rs"]
mod admission_tests;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn baw_source_headers_preserve_latest_pointerless_assessment_without_model_authority(){
        let mut full=crate::empty();crate::accounts::initialize(&mut full,crate::accounts::Profile::BawRussia).unwrap();
        full["items"]=json!([{"id":"baw-comment","revision":1,"workflow":"attention","draft":"",
            "autoPreparation":{"status":"queued"}}]);
        for (id,reason) in [("older","old saved analysis"),("latest","latest saved analysis")]{
            crate::list_mut(&mut full,"jobs").push(json!({"id":id,"kind":"assistant","purpose":"auto_prepare","status":"completed",
                "prepareBundle":{"itemIds":["baw-comment"],"dependencyDigest":"a".repeat(64)},
                "prepareOutcome":{"itemId":"baw-comment","status":"needs_attention","reason":reason}}));
        }
        let mut scoped=full.clone();
        scoped["sourceJobControls"]=json!({"version":1,"complete":true,"jobs":full["jobs"]});
        scoped["jobs"]=json!([]);
        hold_legacy_repreparations(&mut full,NOW);
        hold_legacy_repreparations(&mut scoped,NOW);
        assert_eq!(scoped["items"],full["items"]);
        assert_eq!(scoped["items"][0]["autoPreparation"]["jobId"],"latest");
        assert_eq!(scoped["items"][0]["reason"],"latest saved analysis");
        assert!(crate::row(&scoped,"jobs","latest").is_err(),"maintenance headers never become canonical model jobs");
        let mut incomplete=scoped.clone();incomplete["sourceJobControls"]["complete"]=json!(false);
        assert!(source_maintenance_job(&incomplete,"latest").is_none());
    }
    #[test]
    fn source_maintenance_lookup_prefers_complete_native_job(){
        let d=json!({"jobs":[{"id":"same","purpose":"engine_prepare"}],
            "sourceJobControls":{"version":1,"complete":true,"jobs":[{"id":"same","purpose":"auto_prepare"}]}});
        assert_eq!(source_maintenance_job(&d,"same").unwrap()["purpose"],"engine_prepare");
    }
    #[test]
    fn preparation_status_query_keeps_full_default_and_explicit_summary_only(){
        use std::collections::HashMap;
        assert!(!status_summary_query(&HashMap::new()).unwrap());
        assert!(!status_summary_query(&HashMap::from([("eligibility".into(),"full".into())])).unwrap());
        assert!(status_summary_query(&HashMap::from([("eligibility".into(),"summary".into())])).unwrap());
        for value in ["","Summary","cheap","false"]{
            assert_eq!(status_summary_query(&HashMap::from([("eligibility".into(),value.into())])).unwrap_err().0,axum::http::StatusCode::BAD_REQUEST);
        }
    }
    fn without_media_schedule(mut d:Value)->Value {
        // Media reconciliation maintains its own cache; it is not preparation,
        // proposal, source, or operator state.
        d.as_object_mut().unwrap().remove("mediaQueue");
        d
    }
    #[test]
    fn explicit_owner_transcript_requirement_survives_timeout_or_download_failure() {
        for failed_media in [false,true] {
            let mut d=fixture();
            d["posts"][0]["postKey"]=json!("p");
            d["posts"][0]["attachments"]=json!([{"type":"video"}]);
            let post=d["posts"][0].clone();
            d["settings"]["postMediaPolicies"]=json!({(post["id"].as_str().unwrap()):{
                "version":1,"revision":1,"status":"active","postId":post["id"],"mode":"full_audio_only",
                "account":d["account"],"connectorBinding":crate::active_binding(&d).unwrap().to_json(),
                "sourceVersion":crate::media_fullframes::source_version(&post,d["account"].as_str().unwrap())}});
            assert_eq!(crate::post_media_policy::effective_for_preparation(&d,&post).unwrap()["decisionBasis"]["kind"],"exact_owner_override");
            let group=crate::knowledge::media_group_key(&d["posts"][0],"LikeAvto").unwrap();
            d["jobs"].as_array_mut().unwrap().push(json!({"id":"media","kind":"media","purpose":"auto_media","status":"running","groupKey":group,"startedAt":stamp(NOW)}));
            assert!(claim(&mut d,NOW+1).unwrap().is_none());
            assert!(d["items"][0]["preparationMediaWait"].is_object());
            if failed_media {d["jobs"][0]["status"]=json!("failed");}
            assert!(claim(&mut d,if failed_media{NOW+2}else{NOW+301}).unwrap().is_none());
            assert!(d["items"][0]["preparationMediaWait"].is_object());
            assert!(super::super::list(&d,"jobs").iter().all(|job|job["kind"]!="assistant"));
        }
    }
    #[test]
    fn model_preflight_rejects_changed_source_cancel_and_operator_revision() {
        for change in ["source","cancel","revision"] {
            let mut d=fixture();let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
            assert!(model_preflight(&d,&job).is_ok());
            match change {
                "source"=>d["branches"][0]["messages"][0]["text"]=json!("changed"),
                "cancel"=>crate::row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("cancelled"),
                _=>d["items"][0]["revision"]=json!(2),
            }
            assert!(model_preflight(&d,&job).is_err());
        }
    }
    #[test]
    fn video_detected_after_claim_blocks_model_and_result_admission() {
        let mut d=fixture();
        let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
        let result=captured_result(&mut d,&job,&response("reply")).unwrap();
        // A provider enrichment may reveal a video without changing its title.
        d["posts"][0]["attachments"]=json!([{"type":"video"}]);
        assert!(model_preflight(&d,&job).is_err());
        let outcome=super::complete(&mut d,&job,&result,NOW+1).unwrap();
        assert_eq!(outcome["status"],"stale");
        assert!(super::super::list(&d,"proposals").is_empty());
    }
    #[test]
    fn optional_triage_tags_are_validated_persisted_and_do_not_authorize_actions() {
        assert_eq!(assessment_tags(&json!({})).unwrap(), json!([]));
        for invalid in [
            json!(null),
            json!(["invented"]),
            json!(["question", "question"]),
            json!(["question", "feedback", "purchase", "complaint"]),
        ] {
            assert!(assessment_tags(&json!({"tags":invalid})).is_err());
        }
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        let mut result = response("needs_attention");
        result["assessments"][0]["tags"] = json!(["purchase", "needs_fact"]);
        complete_with_captured_result(&mut d, &job, &result, NOW).unwrap();
        assert_eq!(
            d["items"][0]["triageTags"],
            json!(["purchase", "needs_fact"])
        );
        assert!(d["approvals"].as_array().unwrap().is_empty());
        assert!(d["operations"].as_array().unwrap().is_empty());
        let incoming = d["items"][0].clone();
        super::super::merge_snapshot(&mut d, &json!({"items":[incoming]})).unwrap();
        assert_eq!(
            d["items"][0]["triageTags"],
            json!(["purchase", "needs_fact"])
        );
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        let mut reply = response("reply");
        reply["assessments"][0]["tags"] = json!(["feedback"]);
        complete_with_captured_result(&mut d, &job, &reply, NOW).unwrap();
        assert!(
            super::super::proposal_current(&d, &d["proposals"][0]).is_ok(),
            "Generated tags must not invalidate their own proposal provenance"
        );
    }
    #[test]
    fn all_open_ages_and_unknown_creation_dates_can_be_prepared_when_freshly_observed() {
        for created in [
            json!(stamp(NOW - 365 * 24 * 3600)),
            Value::Null,
            json!("unparseable"),
        ] {
            let mut d = fixture();
            d["items"][0]["createdAt"] = created;
            assert!(claim(&mut d, NOW).unwrap().is_some());
        }
    }

    #[test]
    fn open_queue_claims_oldest_first_without_starving_following_items() {
        let mut d = fixture();
        let mut older = d["items"][0].clone();
        older["id"] = json!("older");
        older["itemId"] = json!("older");
        older["postId"] = json!("older-post");
        older["postKey"] = json!("older-post");
        older["branchId"] = json!("older-branch");
        older["conversationKey"] = json!("older-branch");
        older["createdAt"] = json!(stamp(NOW - 30 * 24 * 3600));
        d["items"].as_array_mut().unwrap().push(older);
        d["posts"].as_array_mut().unwrap().push(json!({"id":"older-post","text":"Earlier independent post","attachments":[]}));
        d["branches"].as_array_mut().unwrap().push(json!({"id":"older-branch","postId":"older-post","messages":[{"id":"older","text":"Hello"}],"contextComplete":true}));
        let (job, request) = claim(&mut d, NOW).unwrap().unwrap();
        assert_eq!(request["items"].as_array().unwrap().len(),1);
        assert_eq!(super::super::row(&d,"jobs",&job).unwrap()["requestedItemIds"],json!(["older"]));
        let result = group_response(&[("older","needs_attention")]);
        complete_with_captured_result(&mut d, &job, &result, NOW).unwrap();
        super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("completed");
        let (next,request)=claim(&mut d,NOW).unwrap().unwrap();
        assert_ne!(next,job);assert_eq!(request["items"][0]["id"],"i");
        complete_with_captured_result(&mut d,&next,&response("needs_attention"),NOW).unwrap();
        super::super::row_mut(&mut d,"jobs",&next).unwrap()["status"]=json!("completed");
        assert!(claim(&mut d,NOW).unwrap().is_none());
        assert_eq!(d["items"][0]["autoPreparation"]["status"],"needs_attention");
        assert_eq!(d["items"][1]["autoPreparation"]["status"],"needs_attention");
    }
    pub(super) const NOW: i64 = 1_800_000_000;
    #[test]
    fn branch_digest_ignores_adapter_provenance_but_not_role() {
        let mut d=fixture();
        super::super::merge_snapshot(&mut d,&json!({})).unwrap();
        let digest=d["items"][0]["branchContextDigest"].clone();
        d["branches"][0]["messages"][0]["providerOfficial"]=json!(false);
        d["branches"][0]["messages"][0]["roleEvidence"]=Value::Null;
        d["branches"][0]["observedMessages"]=d["branches"][0]["messages"].clone();
        super::super::merge_snapshot(&mut d,&json!({})).unwrap();
        assert_eq!(d["items"][0]["branchContextDigest"],digest);
        d["branches"][0]["messages"][0]["role"]=json!("brand");
        d["branches"][0]["observedMessages"]=d["branches"][0]["messages"].clone();
        super::super::merge_snapshot(&mut d,&json!({})).unwrap();
        assert_ne!(d["items"][0]["branchContextDigest"],digest);
    }
    #[test]
    fn metadata_recovery_preserves_saved_text_and_never_approves() {
        let mut d=fixture();
        super::super::merge_snapshot(&mut d,&json!({})).unwrap();
        let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
        complete_with_captured_result(&mut d,&job,&response("reply"),NOW).unwrap();
        let original=d["proposals"][0].clone();
        let paid_history=crate::row(&d,"jobs",&job).unwrap()["retainedEvidence"].clone();
        let material_history=crate::row(&d,"jobs",&job).unwrap()["modelMaterialReceipts"].clone();
        d["items"][0]["branchContextDigest"]=json!("adapter-metadata-only");
        reconcile_stale(&mut d,NOW+1);
        let stale=d["proposals"][0].clone();
        assert_eq!(stale["status"],"stale");
        let preview=super::super::recover_equivalent_prepared(&mut d,false).unwrap();
        assert_eq!(preview["results"][0]["result"],"equivalent");
        assert_eq!(d["proposals"].as_array().unwrap().len(),1);
        super::super::recover_equivalent_prepared(&mut d,true).unwrap();
        assert_eq!(d["proposals"][0],stale);
        assert_eq!(d["proposals"][1]["text"],original["text"]);
        assert_eq!(d["proposals"][1]["status"],"draft");
        assert_eq!(d["proposals"][1]["modelMaterialReceipt"],original["modelMaterialReceipt"]);
        assert_eq!(d["proposals"][1]["mandatoryMaterialContract"],original["mandatoryMaterialContract"]);
        assert_eq!(crate::row(&d,"jobs",&job).unwrap()["retainedEvidence"],paid_history);
        assert_eq!(crate::row(&d,"jobs",&job).unwrap()["modelMaterialReceipts"],material_history);
        super::super::proposal_current(&d,&d["proposals"][1]).expect("equivalent recovery must preserve current material provenance");
        assert!(super::super::list(&d,"approvals").is_empty());
        assert!(super::super::list(&d,"operations").is_empty());
        let snapshot=d.clone();
        super::super::recover_equivalent_prepared(&mut d,true).unwrap();
        assert_eq!(d,snapshot);
        for field in ["deleted","textUnavailable"] {
            let mut removed=snapshot.clone();
            removed["branches"][0]["observedMessages"][0][field]=json!(true);
            super::super::merge_snapshot(&mut removed,&json!({})).unwrap();
            assert_eq!(removed["proposals"][1]["status"],"stale");
            assert!(super::super::proposal_current(&removed,&removed["proposals"][1]).is_err());
        }
        d["branches"][0]["observedMessages"][0]["text"]=json!("A real new statement");
        super::super::merge_snapshot(&mut d,&json!({})).unwrap();
        assert_eq!(d["proposals"][1]["status"],"stale");
        assert!(super::super::proposal_current(&d,&d["proposals"][1]).is_err());
    }
    #[test]
    fn metadata_recovery_holds_missing_or_corrupt_material_receipt_without_replacing_paid_history() {
        for change in ["missing", "corrupt"] {
            let mut d=fixture();
            crate::merge_snapshot(&mut d,&json!({})).unwrap();
            let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
            complete_with_captured_result(&mut d,&job,&response("reply"),NOW).unwrap();
            d["items"][0]["branchContextDigest"]=json!("adapter-metadata-only");
            reconcile_stale(&mut d,NOW+1);
            assert_eq!(d["proposals"][0]["staleReason"],"Review source context changed");
            if change=="missing" {
                d["proposals"][0].as_object_mut().unwrap().remove("modelMaterialReceipt");
            } else {
                d["proposals"][0]["modelMaterialReceipt"]["pointerSha256"]=json!("corrupt");
            }
            let before=d.clone();
            let preview=crate::recover_equivalent_prepared(&mut d,false).unwrap();
            assert_eq!(preview["results"][0]["result"],"materials_unproven","{change}");
            assert_eq!(d,before,"preview stays read-only: {change}");
            let outcome=crate::recover_equivalent_prepared(&mut d,true).unwrap();
            assert_eq!(outcome["results"][0]["result"],"materials_unproven","{change}");
            assert_eq!(d["proposals"],before["proposals"],"no replacement draft: {change}");
            assert_eq!(d["jobs"],before["jobs"],"paid and material histories are immutable: {change}");
            assert_eq!(d["approvals"],before["approvals"]);
            assert_eq!(d["operations"],before["operations"]);
        }
    }
    #[test]
    fn metadata_recovery_rejects_changed_speaker_or_reply() {
        for field in ["role","text","deleted","textUnavailable","attachments"] {
            let mut d=fixture();
            d["branches"][0]["messages"]=json!([{"id":"message","role":"customer","text":"hello"}]);
            super::super::merge_snapshot(&mut d,&json!({})).unwrap();
            let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
            complete_with_captured_result(&mut d,&job,&response("reply"),NOW).unwrap();
            d["branches"][0]["messages"][0][field]=match field {"role"=>json!("brand"),"deleted"|"textUnavailable"=>json!(true),"attachments"=>json!([{"type":"image"}]),_=>json!("different")};
            d["branches"][0]["observedMessages"]=d["branches"][0]["messages"].clone();
            reconcile_stale(&mut d,NOW+1);
            super::super::merge_snapshot(&mut d,&json!({})).unwrap();
            let before=d.clone();
            let report=super::super::recover_equivalent_prepared(&mut d,true).unwrap();
            assert!(report["results"].as_array().unwrap().iter().all(|r|r["result"]=="source_changed"));
            assert_eq!(d,before);
        }
    }
    pub(super) fn fixture() -> Value {
        let mut d = super::super::empty();
        // Bind this pristine synthetic workspace before adding any records.
        let profile = crate::accounts::Profile::from_workspace(&d).unwrap();
        crate::accounts::initialize(&mut d, profile).unwrap();
        d["items"] = json!([{"id":"i","itemId":"c","objectId":"o","postKey":"p","conversationKey":"thread","branchId":"b","postId":"post","revision":1,"draft":"","workflow":"attention","providerStatus":"new","createdAt":stamp(NOW-60),"providerObservedAt":stamp(NOW)}]);
        d["branches"] = json!([{"id":"b","postId":"post","messages":[{"id":"c","text":"Hi"}],"contextComplete":false}]);
        d["posts"] = json!([{"id":"post","text":"Post","attachments":[]}]);
        d
    }
    pub(super) fn captured_result(d: &mut Value, job: &str, result: &Value) -> crate::ApiResult<Value> {
        let request = crate::row(d, "jobs", job)?["prepareBundle"]["request"].clone();
        assert_eq!(request["preparationMode"], "single_pass_v1");
        let mut captured = if result.get("runMetadata").is_some() {
            result.clone()
        } else {
            crate::engine_prepare::tests::single_pass_result(result.clone())
        };
        // This uses isolated CAS paid evidence and the normal material validators.
        crate::model_material_receipt::fixture_result(d, job, &request, &mut captured)?;
        Ok(captured)
    }
    pub(super) fn complete_with_captured_result(
        d: &mut Value, job: &str, result: &Value, at: i64,
    ) -> crate::ApiResult<Value> {
        let captured = captured_result(d, job, result)?;
        super::complete(d, job, &captured, at)
    }
    fn response(outcome: &str) -> Value {
        json!({"text":"Review","sources":[],"assessments":[{"itemId":"i","outcome":outcome,"reason":"Friendly comment"}],"proposals":if outcome=="reply"{json!([{"itemId":"i","kind":"reply_and_close","text":"Спасибо!"}])}else{json!([])}})
    }
    pub(super) fn add_recipient(d: &mut Value, id: &str, post: &str) {
        let mut item = d["items"][0].clone();
        item["id"] = json!(id); item["itemId"] = json!(id);
        item["postId"] = json!(post); item["postKey"] = json!(post);
        item["branchId"] = json!(format!("branch-{id}")); item["conversationKey"] = item["branchId"].clone();
        item.as_object_mut().unwrap().remove("autoPreparation");
        item["workflow"] = json!("attention"); item["draft"] = json!("");
        d["items"].as_array_mut().unwrap().push(item);
        if !super::super::list(d, "posts").iter().any(|p| p["id"] == post) {
            d["posts"].as_array_mut().unwrap().push(json!({"id":post,"text":"Independent source","attachments":[]}));
        }
        d["branches"].as_array_mut().unwrap().push(json!({"id":format!("branch-{id}"),"postId":post,"messages":[{"id":id,"text":"Thanks"}],"contextComplete":true}));
    }
    fn group_response(ids: &[(&str, &str)]) -> Value {
        json!({"text":"Review","sources":[],
            "assessments":ids.iter().map(|(id,decision)|json!({"itemId":id,"outcome":decision,"reason":format!("Assessment for {id}")})).collect::<Vec<_>>(),
            "proposals":ids.iter().filter(|(_,decision)| *decision == "reply").map(|(id,_)|json!({"itemId":id,"kind":"reply_and_close","text":"Спасибо!"})).collect::<Vec<_>>()})
    }
    pub(super) fn legacy_two_pass_request(d:&mut Value,job:&str)->Value {
        // These tests model persisted captures predating single-pass materials
        // and strict groups; this helper must not be used for fresh results.
        super::super::row_mut(d,"jobs",job).unwrap().as_object_mut().unwrap().remove("scopeReservation");
        let bundle=&mut super::super::row_mut(d,"jobs",job).unwrap()["prepareBundle"];
        let request=bundle["request"].as_object_mut().unwrap();
        for field in ["preparationMode","responseContract","modelContextContract","researchPolicy","researchLimitContract","recoveryEvidenceContract","factDependencyContract","visualNeedContract","visualSelection",
            "strictGroupContract","strictGroup","mandatoryMaterialContract","postContextBundle","materialReadiness"] {request.remove(field);}
        bundle["digest"]=json!(format!("{:x}",Sha256::digest(bundle["request"].to_string().as_bytes())));
        bundle["request"].clone()
    }
    #[test]
    fn capacity_preview_and_commit_use_same_reconciled_source_phase(){
        let mut original=fixture();add_recipient(&mut original,"j","other");
        original["posts"][0]["attachments"]=json!([{"type":"video","url":"https://example.invalid/offline-only.mp4"}]);
        let mut snapshot=original.clone();reconcile_claim_state(&mut snapshot,NOW).unwrap();
        assert_ne!(snapshot,original,"fixture must exercise reducer writes before capacity capture");
        let mut preview=snapshot.clone();let (_,captured)=claim_reconciled(&mut preview,NOW,None,1).unwrap().unwrap();
        let ids:Vec<Value>=captured["items"].as_array().unwrap().iter().map(|item|item["id"].clone()).collect();
        let expected=crate::engine_prepare::build_request(&snapshot,&ids,None).unwrap()["request"].clone();
        assert_eq!(captured,expected,"reconciled snapshot represents the exact pre-claim source");
        let selected=ids.iter().map(|id|id.as_str().unwrap().to_owned()).collect::<Vec<_>>();
        reconcile_claim_state(&mut original,NOW).unwrap();
        let (_,actual)=claim_reconciled(&mut original,NOW,Some((&selected,&[])),1).unwrap().unwrap();
        crate::engine_prepare::capacity::same_capture(Some(&expected),Some(&actual)).unwrap();
    }
    #[test]
    fn capacity_selection_claims_only_checked_group_and_does_not_spend_held_attempt(){
        let mut d=fixture();add_recipient(&mut d,"j","other");
        let selected=vec!["i".to_owned()];let oversized=vec!["j".to_owned()];
        let (job,request)=claim_with_capacity(&mut d,NOW,Some((&selected,&oversized))).unwrap().unwrap();
        assert_eq!(request["items"].as_array().unwrap().len(),1);
        assert_eq!(request["items"][0]["id"],"i");
        let held=crate::row(&d,"items","j").unwrap();
        assert_eq!(held["autoPreparation"]["status"],"needs_attention");
        assert_eq!(held["autoPreparation"]["attempts"],0);assert!(held["autoPreparation"]["jobId"].is_null());
        assert_eq!(crate::row(&d,"jobs",&job).unwrap()["prepareBundle"]["itemIds"],json!(["i"]));
        assert!(crate::list(&d,"operations").is_empty());
    }
    #[test]
    fn grouped_claim_settles_one_family_and_claims_queued_tail_without_actions() {
        let mut d=fixture(); add_recipient(&mut d,"j","post"); add_recipient(&mut d,"z","other");
        let (job,request)=claim(&mut d,NOW).unwrap().unwrap();
        assert_eq!(request["items"].as_array().unwrap().len(),2);
        assert_eq!(request["posts"].as_array().unwrap().len(),1);
        assert_eq!(request["preparationMode"],"single_pass_v1");
        assert_eq!(request["modelContextContract"],"shared_moderation_v1");
        assert_eq!(request["researchPolicy"],"context_sufficient_v1");
        let stored=super::super::row(&d,"jobs",&job).unwrap();
        assert_eq!(stored["prepareBundle"]["itemIds"],json!(["i","j"]));
        for id in ["i","j"] {
            let auto=&super::super::row(&d,"items",id).unwrap()["autoPreparation"];
            assert_eq!(auto["jobId"],job); assert_eq!(auto["attempts"],1);
            assert_eq!(stored["autoPreparationInputs"][id]["inputDigest"],auto["inputDigest"]);
        }
        let tail=&super::super::row(&d,"items","z").unwrap()["autoPreparation"];
        assert_eq!(tail["status"],"queued");assert_eq!(tail["attempts"],0);assert!(tail["jobId"].is_null());
        assert!(claim(&mut d,NOW).unwrap().is_none());
        let outcome=complete_with_captured_result(&mut d,&job,&group_response(&[("j","needs_attention"),("i","reply")]),NOW).unwrap();
        assert_eq!(outcome["items"][0]["status"],"prepared");
        assert_eq!(outcome["items"][1]["status"],"needs_attention");
        assert_eq!(d["items"][0]["workflow"],"prepared");
        assert_eq!(d["items"][1]["reason"],"Assessment for j");
        assert!(super::super::proposal_current(&d,&d["proposals"][0]).is_ok());
        super::super::row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("completed");
        let (next,request)=claim(&mut d,NOW+1).unwrap().unwrap();
        assert_ne!(next,job);assert_eq!(request["items"].as_array().unwrap().len(),1);
        assert_eq!(request["items"][0]["id"],"z");
        assert_eq!(d["items"][0]["autoPreparation"]["jobId"],job);
        assert_eq!(d["items"][1]["autoPreparation"]["jobId"],job);
        let outcome=complete_with_captured_result(&mut d,&next,&group_response(&[("z","needs_attention")]),NOW+1).unwrap();
        assert_eq!(outcome["status"],"needs_attention");
        assert!(super::super::list(&d,"approvals").is_empty()); assert!(super::super::list(&d,"operations").is_empty());
    }
    #[test]
    fn automatic_routine_branch_survives_unrelated_review_failure(){
        let mut d=fixture();
        super::super::accounts::initialize(&mut d,super::super::accounts::Profile::LikeAvto).unwrap();
        add_recipient(&mut d,"j","post");
        let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
        let request=legacy_two_pass_request(&mut d,&job);
        let first=group_response(&[("i","reply"),("j","needs_attention")]);
        let plan=crate::preparation_review::settle_first(&mut d,&job,&request,&first,&stamp(NOW)).unwrap().unwrap();
        assert_eq!(plan["items"].as_array().unwrap().len(),1);
        assert_eq!(plan["items"][0]["id"],"j");
        complete_grouped(&mut d,&job,&first,NOW,false,false).unwrap();
        assert_eq!(d["proposals"].as_array().unwrap().len(),1);
        assert_eq!(d["items"][0]["autoPreparation"]["status"],"prepared");
        complete_grouped(&mut d,&job,&first,NOW,false,false).unwrap();
        assert_eq!(d["proposals"].as_array().unwrap().len(),1,"settled branch is not replayed");
        d["branches"][1]["messages"][0]["text"]=json!("Source changed before stronger review");
        fail_claimed(&mut d,&job,"Review failed",false,NOW+1).unwrap();
        assert_eq!(d["items"][0]["autoPreparation"]["status"],"prepared");
        assert_eq!(d["items"][1]["autoPreparation"]["status"],"error");
        assert_eq!(d["proposals"].as_array().unwrap().len(),1);
    }
    #[test]
    fn completed_a_media_held_b_and_independent_c_progress_across_restart() {
        let mut d=fixture(); add_recipient(&mut d,"b","video"); add_recipient(&mut d,"c","last");
        // Establish the same bound catalog/owner source shape before capturing
        // A, so restart initialization is not itself a source migration.
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap();
        // The seed's provider itemId is "c". Give A its own provider recipient:
        // different local posts must not silently alias the same object/itemId.
        d["items"][0]["itemId"]=json!("original-recipient");
        // Declare A/B/C queue order explicitly: add_recipient otherwise copies
        // one timestamp and the deterministic ID tie-breaker puts c before i.
        for (index, age) in [180, 120, 60].into_iter().enumerate() {
            d["items"][index]["createdAt"]=json!(stamp(NOW-age));
        }
        d["posts"][1]["attachments"]=json!([{"type":"video"}]);
        let post=d["posts"][1].clone();
        d["settings"]["postMediaPolicies"]=json!({(post["id"].as_str().unwrap()):{
            "version":1,"revision":1,"status":"active","postId":post["id"],"mode":"full_audio_only",
            "account":d["account"],"connectorBinding":crate::active_binding(&d).unwrap().to_json(),
            "sourceVersion":crate::media_fullframes::source_version(&post,d["account"].as_str().unwrap())}});
        assert_eq!(crate::post_media_policy::effective_for_preparation(&d,&post).unwrap()["decisionBasis"]["kind"],"exact_owner_override");
        let (a,request)=claim(&mut d,NOW).unwrap().unwrap();
        assert_eq!(request["items"].as_array().unwrap().len(),1);
        assert_eq!(super::super::row(&d,"jobs",&a).unwrap()["requestedItemIds"],json!(["i"]));
        let accepted_a=complete_with_captured_result(&mut d,&a,&group_response(&[("i","reply")]),NOW).unwrap();
        assert_eq!(accepted_a["status"],"prepared","A must be admitted before testing unchanged restart: {accepted_a}");
        super::super::row_mut(&mut d,"jobs",&a).unwrap()["status"]=json!("completed");
        d=serde_json::from_str(&d.to_string()).unwrap(); crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
        let (c,request)=claim(&mut d,NOW+1).unwrap().expect("independent queued tail survives restart");
        assert_eq!(request["items"].as_array().unwrap().len(),1);assert_eq!(request["items"][0]["id"],"c");
        assert_ne!(a,c);assert_eq!(d["items"][0]["autoPreparation"]["jobId"],a);
        let accepted_c=complete_with_captured_result(&mut d,&c,&group_response(&[("c","reply")]),NOW+1).unwrap();
        assert_eq!(accepted_c["status"],"prepared","independent C must be admitted while video B remains held: {accepted_c}");
        super::super::row_mut(&mut d,"jobs",&c).unwrap()["status"]=json!("completed");
        assert!(claim(&mut d,NOW+2).unwrap().is_none());
        assert_eq!(d["items"][0]["autoPreparation"]["status"],"prepared");
        assert!(d["items"][1]["preparationMediaWait"].is_object());
        assert!(d["items"][1]["autoPreparation"]["jobId"].is_null());
        assert_eq!(d["items"][2]["autoPreparation"]["status"],"prepared");
    }
    #[test]
    fn group_failure_and_restart_touch_exact_members_and_keep_individual_retry_budgets() {
        let mut d=fixture(); add_recipient(&mut d,"j","post"); add_recipient(&mut d,"z","post");
        let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
        assert_eq!(super::super::row(&d,"jobs",&job).unwrap()["requestedItemIds"],json!(["i","j","z"]));
        d["items"][1]["autoPreparation"]["attempts"]=json!(3);
        // BUSY proves this attempt was refused before dispatch; a timeout
        // instead retains the original reservation for recovery.
        fail_claimed(&mut d,&job,"ASSISTANT_BUSY",true,NOW+1).unwrap();
        assert_eq!(d["items"][0]["autoPreparation"]["retryAt"],stamp(NOW+61));
        assert!(d["items"][1]["autoPreparation"]["retryAt"].is_null());
        assert_eq!(d["items"][2]["autoPreparation"]["retryAt"],stamp(NOW+61));
        super::super::row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("failed");
        assert!(claim(&mut d,NOW+2).unwrap().is_none());
        let (_,request)=claim(&mut d,NOW+61).unwrap().unwrap();
        assert_eq!(request["items"].as_array().unwrap().len(),2);
        let mut interrupted=fixture(); add_recipient(&mut interrupted,"j","post");
        let (run,_)=claim(&mut interrupted,NOW).unwrap().unwrap();
        super::super::row_mut(&mut interrupted,"jobs",&run).unwrap()["status"]=json!("interrupted");
        recover_jobs(&mut interrupted,NOW+1);
        for item in super::super::list(&interrupted,"items") { assert_eq!(item["autoPreparation"]["status"],"error"); assert_eq!(item["autoPreparation"]["jobId"],run); }
        let recovered=interrupted.clone(); recover_jobs(&mut interrupted,NOW+2); assert_eq!(interrupted,recovered);
    }
    #[test]
    fn group_human_edit_shared_source_unknown_and_lost_owner_prevent_admission() {
        for change in ["human","source","unknown","approved","owner","revision"] {
            let mut d=fixture(); add_recipient(&mut d,"j","post");
            let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
            let captured=captured_result(&mut d,&job,&group_response(&[("i","reply"),("j","reply")])).unwrap();
            match change {
                "human"=>{d["items"][1]["draft"]=json!("Human draft"); d["items"][1]["reason"]=json!("Human decision");},
                "source"=>d["posts"][0]["text"]=json!("Changed shared source"),
                "unknown"=>d["operations"]=json!([{"id":"unknown","itemId":"j","status":"unknown"}]),
                "approved"=>d["proposals"]=json!([{"id":"approved","itemId":"j","status":"approved"}]),
                "owner"=>d["items"][1]["autoPreparation"]["jobId"]=json!("new-owner"),
                _=>d["items"][1]["revision"]=json!(2),
            }
            let protected=(d["operations"].clone(),d["proposals"].clone(),d["items"][1]["draft"].clone());
            let result=super::complete(&mut d,&job,&captured,NOW+1);
            if change=="owner" { assert!(result.is_err()); } else { assert_eq!(result.unwrap()["status"],"stale"); }
            assert_eq!(d["operations"],protected.0); assert_eq!(d["proposals"],protected.1); assert_eq!(d["items"][1]["draft"],protected.2);
            if change=="human" { assert_eq!(d["items"][1]["reason"],"Human decision"); }
        }
    }
    #[test]
    fn group_result_requires_exact_unique_members_before_any_admission() {
        for invalid in ["missing","duplicate","foreign","proposal"] {
            let mut d=fixture(); add_recipient(&mut d,"j","post");
            let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
            let mut result=group_response(&[("i","reply"),("j","reply")]);
            match invalid {
                "missing"=>{result["assessments"].as_array_mut().unwrap().pop();},
                "duplicate"=>result["assessments"][1]["itemId"]=json!("i"),
                "foreign"=>result["assessments"][1]["itemId"]=json!("outside"),
                _=>result["proposals"][1]["itemId"]=json!("outside"),
            }
            let result=captured_result(&mut d,&job,&result).unwrap();
            let before=d.clone();
            let error=super::complete(&mut d,&job,&result,NOW).unwrap_err();
            assert_eq!(error.1,match invalid {
                "missing"=>"Automatic preparation requires one assessment per target",
                "proposal"=>"Automatic preparation proposal target mismatch",
                _=>"Automatic preparation assessment target mismatch",
            });
            assert_eq!(d,before);
        }
    }
    #[test]
    fn grouped_preflight_checks_nonfirst_revision_and_ownership() {
        for change in ["revision","owner"] {
            let mut d=fixture(); add_recipient(&mut d,"j","post");
            let (job,_)=claim(&mut d,NOW).unwrap().unwrap(); assert!(model_preflight(&d,&job).is_ok());
            if change=="revision" { d["items"][1]["revision"]=json!(2); }
            else { d["items"][1]["autoPreparation"]["jobId"]=json!("different"); }
            assert!(model_preflight(&d,&job).is_err());
        }
    }
    #[test]
    fn completed_recovery_selection_skips_held_plan_unknown_and_busy_work() {
        let mut d=fixture();
        let ready=json!({"id":"ready","kind":"assistant","purpose":"auto_prepare","status":"interrupted",
            "preparationStages":{"first":{"status":"completed","reviewRequired":true},
                "reviewChunks":{"status":"completed","planDigest":"plan","chunks":[{"result":{},"attempts":[{"status":"completed"}]}]}}});
        d["jobs"]=json!([ready]);
        assert_eq!(completed_recovery_candidate(&d),Some(("ready".into(),"plan".into())));
        d["jobs"][0]["completedRecoveryHold"]=json!({"expectedPlanDigest":"plan","reason":"RECOVERY_PROFILE_CHANGED"});
        assert!(completed_recovery_candidate(&d).is_none());
        d["jobs"][0]["preparationStages"]["reviewChunks"]["planDigest"]=json!("other-plan");
        assert!(completed_recovery_candidate(&d).is_some());
        d["jobs"][0]["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"][0]["status"]=json!("unknown");
        assert!(completed_recovery_candidate(&d).is_none());
        d["jobs"][0]["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"][0]["status"]=json!("completed");
        d["jobs"].as_array_mut().unwrap().push(json!({"id":"busy","kind":"assistant","purpose":"auto_prepare","status":"running"}));
        assert!(completed_recovery_candidate(&d).is_none());
    }
    #[test]
    fn grouped_attention_history_recovers_each_missing_pointer_without_regeneration() {
        let mut d=fixture(); add_recipient(&mut d,"j","post");
        let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
        complete_with_captured_result(&mut d,&job,&group_response(&[("i","needs_attention"),("j","needs_attention")]),NOW).unwrap();
        super::super::row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("completed");
        d["items"][1]["autoPreparation"]=json!({"status":"queued","attempts":0});
        d["posts"][0]["text"]=json!("Updated post");
        assert!(claim(&mut d,NOW+1).unwrap().is_none());
        for item in super::super::list(&d,"items") {
            assert_eq!(item["autoPreparation"]["jobId"],job);
            assert_eq!(item["autoPreparation"]["requiresReview"],true);
            assert_eq!(item["autoPreparation"]["reasonCode"],"group_review_required");
            assert_eq!(item["reason"],GROUP_REVIEW_REASON);
        }
        assert_eq!(super::super::list(&d,"jobs").len(),1);
    }
    #[test]
    fn changed_failed_group_is_held_without_resetting_attempts_or_reclaiming() {
        let mut d=fixture(); add_recipient(&mut d,"j","post");
        let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
        fail_claimed(&mut d,&job,"ADAPTER_TIMEOUT",true,NOW+1).unwrap();
        super::super::row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("failed");
        d["posts"][0]["text"]=json!("Changed source");
        assert!(claim(&mut d,NOW+61).unwrap().is_none());
        for item in super::super::list(&d,"items") { assert_eq!(item["autoPreparation"]["requiresReview"],true); assert_eq!(item["autoPreparation"]["attempts"],1); assert_eq!(item["autoPreparation"]["jobId"],job); }
        assert_eq!(super::super::list(&d,"jobs").len(),1);
    }
    #[test]
    fn broken_then_repaired_evidence_does_not_erase_spent_attempt_history() {
        let mut d=fixture(); let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
        failed(&mut d,"i",&job,"ADAPTER_TIMEOUT",true,NOW+1);
        assert!(d["items"][0]["autoPreparation"]["retryAt"].is_null());
        assert_eq!(d["items"][0]["autoPreparation"]["reasonCode"],"paid_attempt_recovery_required");
        assert!(d["items"][0]["reason"].as_str().unwrap().contains("автоматический повтор не запускается"));
        assert!(super::super::row(&d,"jobs",&job).unwrap().get("scopeFailure").is_none());
        super::super::row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("failed");
        let branches=d["branches"].clone(); d["branches"]=json!([]);
        assert!(claim(&mut d,NOW+61).unwrap().is_none());
        assert_eq!(d["items"][0]["autoPreparation"]["jobId"],job);
        assert_eq!(d["items"][0]["autoPreparation"]["attempts"],1);
        d["branches"]=branches;
        assert!(claim(&mut d,NOW+62).unwrap().is_none());
        assert_eq!(d["items"][0]["autoPreparation"]["requiresReview"],true);
        assert_eq!(super::super::list(&d,"jobs").len(),1);
    }
    #[test]
    fn grouped_claims_cannot_include_foreign_company_and_keep_same_ids_separate() {
        let mut like=fixture(); add_recipient(&mut like,"j","post");
        like["items"][1]["connectorBinding"]=json!({"accountId":"BAW Russia"});
        let (_,request)=claim(&mut like,NOW).unwrap().unwrap();
        assert_eq!(request["items"].as_array().unwrap().len(),1);
        assert_eq!(request["items"][0]["id"],"i");
        let mut baw=super::super::empty(); super::super::accounts::initialize(&mut baw,super::super::accounts::Profile::BawRussia).unwrap();
        let fixture=fixture(); for key in ["items","posts","branches"] { baw[key]=fixture[key].clone(); }
        let (_,request)=claim(&mut baw,NOW).unwrap().unwrap();
        assert_eq!(request["account"],baw["account"]); assert_ne!(like["account"],baw["account"]);
        assert_ne!(like["items"][0]["autoPreparation"]["jobId"],baw["items"][0]["autoPreparation"]["jobId"]);
    }
    fn prepared() -> Value {
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        complete_with_captured_result(&mut d, &job, &response("reply"), NOW).unwrap();
        super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("completed");
        d
    }

    #[test]
    fn stale_analysis_validates_catalog_once_for_the_whole_immutable_phase(){
        let mut d=fixture();
        d["materials"]=json!([{"id":"global","text":"Retained knowledge","revision":1}]);
        crate::knowledge::sync_catalog(&mut d,"2026-01-01T00:00:00Z").unwrap();
        let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
        complete_with_captured_result(&mut d,&job,&response("reply"),NOW).unwrap();
        crate::row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("completed");
        for n in 0..15{
            let mut proposal=d["proposals"][0].clone();proposal["id"]=json!(format!("same-source-{n}"));
            d["proposals"].as_array_mut().unwrap().push(proposal);
        }
        let before=d.clone();let validations=crate::knowledge::validation_count();
        reconcile_stale(&mut d,NOW+1);
        assert_eq!(d,before);
        assert_eq!(crate::knowledge::validation_count()-validations,1);
        d["branches"][0]["messages"][0]["text"]=json!("Source changed after the phase");
        reconcile_stale(&mut d,NOW+2);
        assert!(crate::list(&d,"proposals").iter().all(|p|p["status"]=="stale"));
        assert_eq!(d["items"][0]["autoPreparation"]["requiresReview"],true);
        assert_eq!(d["items"][0]["workflow"],"attention");
    }

    #[test]
    fn unchanged_restart_keeps_accepted_proposal_and_never_starts_another_job() {
        let mut d = fixture();
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap();
        super::super::knowledge::sync_catalog(&mut d, &stamp(NOW)).unwrap();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        let accepted=complete_with_captured_result(&mut d, &job, &response("reply"), NOW).unwrap();
        assert_eq!(accepted["status"],"prepared","restart fixture must contain an accepted proposal: {accepted}");
        assert!(super::super::proposal_current(&d,&d["proposals"][0]).is_ok());
        super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("completed");
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap();
        let before = d.clone();
        for n in 1..=3 {
            d = serde_json::from_str(&d.to_string()).unwrap();
            super::super::knowledge::sync_catalog(&mut d, &stamp(NOW + n)).unwrap();
            crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
            assert!(claim(&mut d, NOW + n).unwrap().is_none());
            assert_eq!(without_media_schedule(d.clone()), without_media_schedule(before.clone()));
            assert!(super::super::proposal_current(&d, &d["proposals"][0]).is_ok());
        }
    }
    #[test]
    fn transcript_hold_explains_saved_draft_and_is_idempotent() {
        let mut d = prepared();
        let old_text = d["proposals"][0]["text"].clone();
        d["materials"].as_array_mut().unwrap().push(json!({"id":"video","postKey":"p","kind":"transcript","text":"new video evidence"}));
        reconcile_stale(&mut d, NOW + 1);
        assert_eq!(d["items"][0]["workflow"], "attention");
        assert_eq!(d["items"][0]["autoPreparation"]["sourceChangeReason"], "Добавлена расшифровка видео.");
        assert_eq!(d["proposals"][0]["status"], "stale");
        assert_eq!(d["proposals"][0]["text"], old_text);
        let held = d.clone();
        reconcile_stale(&mut d, NOW + 2);
        assert_eq!(d, held);
        assert!(claim(&mut d, NOW + 2).unwrap().is_none());
        assert_eq!(d["jobs"].as_array().unwrap().len(), 1);
        // Existing generic review holds get the same explanation after upgrade.
        d["items"][0]["autoPreparation"].as_object_mut().unwrap().remove("sourceChangeReason");
        d["items"][0]["autoPreparation"]["reason"] = json!("Context changed");
        reconcile_stale(&mut d, NOW + 3);
        assert_eq!(d, held);
    }

    #[test]
    fn changed_knowledge_preserves_result_and_digest_without_automatic_regeneration() {
        let mut d = fixture();
        super::super::knowledge::sync_catalog(&mut d, &stamp(NOW)).unwrap();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        complete_with_captured_result(&mut d, &job, &response("reply"), NOW).unwrap();
        super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("completed");
        let old = d["proposals"][0].clone();
        let input_digest = d["items"][0]["autoPreparation"]["inputDigest"].clone();
        // Backdate only fixture validity; selection still uses the real clock.
        super::super::knowledge::save_instruction(&mut d, &json!({"requestId":"new-rule","title":"Style","text":"Use plain language"}), "2026-01-01T00:00:00Z").unwrap();
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
        assert_eq!(d["proposals"][0]["status"], "stale");
        assert_eq!(d["proposals"][0]["text"], old["text"]);
        assert_eq!(d["items"][0]["autoPreparation"]["inputDigest"], input_digest);
        assert_eq!(d["items"][0]["autoPreparation"]["savedProposalId"], old["id"]);
        assert!(super::super::proposal_current(&d, &d["proposals"][0]).is_err());
        let held = d.clone();
        for n in 1..=3 {
            crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
            assert!(claim(&mut d, NOW + n).unwrap().is_none());
            assert_eq!(without_media_schedule(d.clone()), without_media_schedule(held.clone()));
        }
        assert_eq!(d["jobs"].as_array().unwrap().len(), 1);
        assert!(super::super::list(&d, "operations").is_empty());
        // The existing explicit assistant path can generate a fresh reviewable
        // proposal; the held automatic scheduler does not own that choice.
        let manual_job = super::super::new_job(&mut d, "assistant", "operator-request").unwrap();
        let bundle = crate::engine_prepare::build_request(&d, &[json!("i")], None).unwrap();
        super::super::row_mut(&mut d, "jobs", &manual_job).unwrap()["prepareBundle"] = bundle;
        let result=captured_result(&mut d,&manual_job,&response("reply")).unwrap();
        super::super::prepare_bundle::admit_to(&mut d, &manual_job, None, &result).unwrap();
        assert_eq!(d["proposals"].as_array().unwrap().len(), 2);
        assert!(super::super::proposal_current(&d, &d["proposals"][1]).is_ok());
        assert_eq!(d["proposals"][0]["status"], "stale");
    }

    #[test]
    fn legacy_queued_stale_result_is_held_but_fresh_comment_still_prepares() {
        let mut d = prepared();
        // This tests pre-reservation queued recovery metadata. New interrupted
        // captures instead retain their original paid ownership.
        d["jobs"][0].as_object_mut().unwrap().remove("scopeReservation");
        let saved = d["proposals"][0].clone();
        d["proposals"][0]["status"] = json!("stale");
        d["items"][0]["workflow"] = json!("attention");
        d["items"][0]["autoPreparation"]["status"] = json!("queued");
        d["items"][0]["autoPreparation"]["inputDigest"] = Value::Null;
        d["items"][0]["autoPreparation"]["jobId"] = Value::Null;
        let mut fresh = fixture()["items"][0].clone();
        fresh["id"] = json!("fresh");
        fresh["itemId"] = json!("fresh-provider-id");
        d["items"].as_array_mut().unwrap().push(fresh);
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
        assert_eq!(d["items"][0]["autoPreparation"]["requiresReview"], true);
        assert_eq!(d["items"][0]["autoPreparation"]["savedProposalId"], saved["id"]);
        assert_eq!(d["proposals"][0]["text"], saved["text"]);
        let (_, request) = claim(&mut d, NOW + 1).unwrap().unwrap();
        assert_eq!(request["items"][0]["id"], "fresh");
        assert_eq!(d["jobs"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn revision_only_stale_auto_proposal_is_preserved_for_explicit_review() {
        let mut d = prepared();
        let old = d["proposals"][0].clone();
        let old_revision = d["items"][0]["revision"].clone();
        super::super::bump(&mut d["items"][0]);
        super::super::bump(&mut d["items"][0]);
        let claimed = claim(&mut d, NOW + 1).unwrap();
        assert!(claimed.is_none());
        assert_eq!(d["proposals"][0]["status"], "stale");
        assert_eq!(d["proposals"][0]["itemRevision"], old_revision);
        assert_eq!(d["proposals"][0]["text"], old["text"]);
        assert_eq!(d["items"][0]["workflow"], "attention");
        assert_eq!(d["items"][0]["autoPreparation"]["attempts"], 1);
        assert_eq!(d["items"][0]["autoPreparation"]["requiresReview"], true);
        assert_eq!(d["proposals"].as_array().unwrap().len(), 1);
        assert!(super::super::proposal_current(&d, &d["proposals"][0]).is_err());
        assert_eq!(d["proposals"][0]["status"], "stale");
        assert!(d["approvals"].as_array().unwrap().is_empty());
    }

    #[test]
    fn changed_source_reconciles_at_startup_and_keeps_manual_or_protected_work() {
        let mut d = prepared();
        d["posts"][0]["text"] = json!("Changed source");
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
        assert_eq!(d["proposals"][0]["status"], "stale");
        assert_eq!(d["items"][0]["workflow"], "attention");
        for protection in [
            "draft",
            "waiting",
            "humanOverride",
            "approved",
            "dispatching",
            "unknown",
        ] {
            let mut d = prepared();
            match protection {
                "draft" => d["items"][0]["draft"] = json!("Human text"),
                "waiting" => d["items"][0]["workflow"] = json!("waiting"),
                "humanOverride" => {
                    d["items"][0]["autoPreparation"]["humanOverrideAt"] = json!(stamp(NOW))
                }
                _ => d["proposals"][0]["status"] = json!(protection),
            }
            super::super::bump(&mut d["items"][0]);
            let before = d["items"][0].clone();
            assert!(claim(&mut d, NOW + 1).unwrap().is_none());
            assert_eq!(d["items"][0]["workflow"], before["workflow"]);
            assert_eq!(d["items"][0]["draft"], before["draft"]);
            if ["approved", "dispatching", "unknown"].contains(&protection) {
                assert_eq!(d["proposals"][0]["status"], protection);
            } else {
                assert_eq!(d["proposals"][0]["status"], "stale");
                assert_eq!(d["items"][0]["autoPreparation"]["status"], "stale");
            }
        }
    }

    #[test]
    fn author_history_identity_enrichment_preserves_prepared_proposal_but_text_change_invalidates() {
        let mut d=fixture();
        let at=chrono::Utc::now().timestamp();
        let mut snapshot=json!({"items":d["items"],"branches":d["branches"],"posts":d["posts"]});
        super::super::merge_snapshot(&mut d,&snapshot).unwrap();
        let (job,_)=claim(&mut d,at).unwrap().unwrap();
        complete_with_captured_result(&mut d,&job,&response("reply"),at).unwrap();
        super::super::row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("completed");
        let revision=d["items"][0]["revision"].clone();
        let digest=d["items"][0]["branchContextDigest"].clone();
        let proposal=d["proposals"][0].clone();
        assert!(digest.is_string());
        for identity in [Value::Null,json!("stable-author-42")] {
            snapshot["branches"][0]["messages"][0]["authorId"]=identity.clone();
            snapshot["items"][0]["authorId"]=identity.clone();
            super::super::merge_snapshot(&mut d,&snapshot).unwrap();
            assert_eq!(d["items"][0]["revision"],revision);
            assert_eq!(d["items"][0]["branchContextDigest"],digest);
            assert_eq!(d["proposals"][0],proposal);
            assert_eq!(d["items"][0]["workflow"],"prepared");
            assert_eq!(d["branches"][0]["messages"][0]["authorId"],identity);
            assert!(super::super::proposal_current(&d,&d["proposals"][0]).is_ok());
        }
        snapshot["branches"][0]["messages"][0]["text"]=json!("Changed substantive comment");
        super::super::merge_snapshot(&mut d,&snapshot).unwrap();
        assert_ne!(d["items"][0]["branchContextDigest"],digest);
        assert_ne!(d["items"][0]["revision"],revision);
        assert_eq!(d["proposals"][0]["status"],"stale");
        assert_eq!(d["proposals"][0]["text"],proposal["text"]);
        assert_eq!(d["items"][0]["workflow"],"attention");
        assert!(super::super::list(&d,"approvals").is_empty());
        assert!(super::super::list(&d,"operations").is_empty());
    }

    #[test]
    fn ordinary_sync_reconciles_changed_provider_context_and_preserves_proposal_text() {
        let mut d = prepared();
        let mut item = d["items"][0].clone();
        item["contextEvidenceDigest"] = json!("changed");
        super::super::merge_snapshot(&mut d, &json!({"items":[item]})).unwrap();
        assert_eq!(d["proposals"][0]["status"], "stale");
        assert_eq!(d["proposals"][0]["text"], "Спасибо!");
        assert_eq!(d["items"][0]["workflow"], "attention");
        assert_eq!(d["items"][0]["autoPreparation"]["status"], "stale");
    }
    #[test]
    fn claims_once_and_unknown_legacy_jobs_hold_the_preparation_slot() {
        let mut d = fixture();
        let (job, request) = claim(&mut d, NOW).unwrap().unwrap();
        assert_eq!(request["purpose"], "triage");
        assert!(claim(&mut d, NOW).unwrap().is_none());
        assert!(super::super::new_job(&mut d, "assistant", "chat").is_ok());
        assert_eq!(d["items"][0]["autoPreparation"]["jobId"], job);
        let mut d = fixture();
        super::super::new_job(&mut d, "assistant", "chat").unwrap();
        assert!(claim(&mut d, NOW).unwrap().is_none());
        // Occupied execution slot does not enqueue/rewrite otherwise idle work.
        assert!(d["items"][0]["autoPreparation"].is_null());
        let before=d.clone();
        assert!(claim(&mut d, NOW + 1).unwrap().is_none());
        assert_eq!(d,before);
    }
    #[test]
    fn image_capacity_preflight_holds_without_consuming_a_model_attempt_and_rechecks_source_changes() {
        let mut d=fixture();
        d["items"][0]["attachments"]=json!((0..17).map(|n|json!({"type":"photo","url":format!("https://images.example/{n}.png")})).collect::<Vec<_>>());
        assert!(claim(&mut d,NOW).unwrap().is_none());
        assert_eq!(d["items"][0]["autoPreparation"]["status"],"needs_attention");
        assert_eq!(d["items"][0]["autoPreparation"]["reason"],crate::engine_prepare::IMAGE_CAPACITY_ERROR);
        assert_eq!(d["items"][0]["autoPreparation"]["attempts"],0);
        assert!(d["items"][0]["autoPreparation"]["jobId"].is_null());
        assert!(crate::list(&d,"jobs").is_empty());
        assert!(crate::list(&d,"proposals").is_empty());assert!(crate::list(&d,"operations").is_empty());
        let unchanged=d.clone();
        assert!(claim(&mut d,NOW+1).unwrap().is_none());assert_eq!(d,unchanged);
        // A real source change may admit ordinary preparation; a timer alone may not.
        d["items"][0]["attachments"].as_array_mut().unwrap().pop();
        let (job,request)=claim(&mut d,NOW+2).unwrap().unwrap();
        assert_eq!(crate::engine_prepare::image_count(&request),16);
        assert_eq!(d["items"][0]["autoPreparation"]["attempts"],1);
        assert_eq!(d["items"][0]["autoPreparation"]["jobId"],job);
    }
    #[test]
    fn oversized_source_does_not_block_an_independent_ready_recipient() {
        let mut d=fixture();
        d["items"][0]["attachments"]=json!((0..17).map(|n|json!({"type":"photo","url":format!("https://images.example/{n}.png")})).collect::<Vec<_>>());
        let mut independent=d["items"][0].clone();
        independent["attachments"]=json!([]);
        independent["id"]=json!("z");independent["itemId"]=json!("z-comment");
        independent["postId"]=json!("z-post");independent["postKey"]=json!("z-key");
        independent["branchId"]=json!("z-branch");independent["conversationKey"]=json!("z-thread");
        d["items"].as_array_mut().unwrap().push(independent);
        d["posts"].as_array_mut().unwrap().push(json!({"id":"z-post","postKey":"z-key","text":"Independent post","attachments":[]}));
        d["branches"].as_array_mut().unwrap().push(json!({"id":"z-branch","postId":"z-post","messages":[{"id":"z-comment","text":"Thanks"}],"contextComplete":true}));
        let (job,request)=claim(&mut d,NOW).unwrap().unwrap();
        assert_eq!(request["items"].as_array().unwrap().len(),1);assert_eq!(request["items"][0]["id"],"z");
        assert_eq!(crate::row(&d,"jobs",&job).unwrap()["refId"],"z");
        assert_eq!(d["items"][0]["autoPreparation"]["status"],"needs_attention");
        assert_eq!(d["items"][0]["autoPreparation"]["attempts"],0);
        assert_eq!(d["items"][1]["autoPreparation"]["attempts"],1);
        assert_eq!(crate::list(&d,"jobs").len(),1);
    }
    #[test]
    fn personal_discussion_does_not_block_background_preparation() {
        let mut d = fixture();
        let chat_job=super::super::new_job(&mut d,"assistant","chat").unwrap();
        super::super::row_mut(&mut d,"jobs",&chat_job).unwrap()["purpose"]=json!("discussion");
        let (preparation,request)=claim(&mut d,NOW).unwrap().unwrap();
        assert_eq!(request["purpose"],"triage");
        assert_ne!(preparation,chat_job);
        assert!(claim(&mut d,NOW+1).unwrap().is_none());
        assert_eq!(super::super::row(&d,"jobs",&chat_job).unwrap()["status"],"running");
    }
    #[test]
    fn invalid_observation_manual_draft_and_active_action_are_excluded() {
        for observation in [Value::Null,json!("not-a-timestamp"),json!(stamp(NOW+61))] {
            let mut d = fixture();
            d["items"][0]["providerObservedAt"]=observation;
            assert!(claim(&mut d, NOW).unwrap().is_none());
        }
        let mut d=fixture();d["items"][0]["draft"]=json!("Human writing");
        assert!(claim(&mut d,NOW).unwrap().is_none());
        let mut d = fixture();
        d["operations"] = json!([{"itemId":"i","status":"unknown"}]);
        assert!(claim(&mut d, NOW).unwrap().is_none());
    }
    #[test]
    fn old_observations_prepare_local_drafts_without_age_expiry_or_external_actions() {
        for age in [601,3600,49*3600] {
            let mut d=fixture();let observed=stamp(NOW-age);
            d["items"][0]["providerObservedAt"]=json!(observed);
            let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
            let outcome=complete_with_captured_result(&mut d,&job,&response("reply"),NOW+3600).unwrap();
            assert_eq!(outcome["status"],"prepared","observation age: {age}");
            assert_eq!(d["items"][0]["providerObservedAt"],observed,"preparation never claims a fresh external observation");
            assert_eq!(d["proposals"][0]["status"],"draft");
            for table in ["approvals","operations"]{assert!(super::super::list(&d,table).is_empty());}
        }
        let mut d=fixture();d["items"][0]["providerObservedAt"]=json!(stamp(NOW+60));
        assert!(claim(&mut d,NOW).unwrap().is_some(),"existing future clock tolerance is preserved");
    }
    #[test]
    fn old_observation_does_not_bypass_semantic_revision_operator_media_or_unknown_guards() {
        for change in ["post","branch","revision","draft","override","media","unknown"] {
            let mut d=fixture();d["items"][0]["providerObservedAt"]=json!(stamp(NOW-3600));
            let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
            let result=captured_result(&mut d,&job,&response("reply")).unwrap();
            match change {
                "post"=>d["posts"][0]["text"]=json!("Changed source"),
                "branch"=>d["branches"][0]["messages"][0]["text"]=json!("Changed branch"),
                "revision"=>d["items"][0]["revision"]=json!(2),
                "draft"=>d["items"][0]["draft"]=json!("Operator draft"),
                "override"=>d["items"][0]["autoPreparation"]["humanOverrideAt"]=json!(stamp(NOW)),
                "media"=>d["posts"][0]["attachments"]=json!([{"type":"video"}]),
                _=>d["operations"]=json!([{"itemId":"i","status":"unknown"}]),
            }
            assert_eq!(super::complete(&mut d,&job,&result,NOW+3600).unwrap()["status"],"stale","{change}");
            assert!(super::super::list(&d,"proposals").is_empty(),"{change}");
        }
    }
    #[test]
    fn attention_result_survives_refresh_and_changed_context_without_automatic_reanalysis() {
        let mut d = fixture();
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        let accepted=complete_with_captured_result(&mut d, &job, &response("needs_attention"), NOW).unwrap();
        assert_eq!(accepted["status"],"needs_attention","saved attention outcome must exist before refresh: {accepted}");
        d["jobs"][0]["status"] = json!("completed");
        d["items"][0]["providerObservedAt"] = json!(stamp(NOW + 60));
        assert!(claim(&mut d, NOW + 60).unwrap().is_none());
        let old_digest = d["items"][0]["autoPreparation"]["inputDigest"].clone();
        d["posts"][0]["text"] = json!("Changed post");
        assert!(claim(&mut d, NOW + 60).unwrap().is_none());
        assert_eq!(d["items"][0]["autoPreparation"]["requiresReview"], true);
        assert_eq!(d["items"][0]["autoPreparation"]["inputDigest"], old_digest);
        assert_eq!(d["jobs"].as_array().unwrap().len(), 1);
        assert_eq!(d["jobs"][0]["prepareOutcome"]["reason"], "Friendly comment");
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap();
        let held = d.clone();
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
        assert!(claim(&mut d, NOW + 61).unwrap().is_none());
        assert_eq!(d, held);
    }
    #[test]
    fn legacy_queued_assessment_recovers_history_without_proposal_or_job_pointer() {
        for retry_state in ["queued", "running", "cancelled"] {
            let mut d = fixture();
            let (accepted_job, _) = claim(&mut d, NOW).unwrap().unwrap();
            complete_with_captured_result(&mut d, &accepted_job, &response("needs_attention"), NOW).unwrap();
            super::super::row_mut(&mut d, "jobs", &accepted_job).unwrap()["status"] = json!("completed");
            let digest = d["jobs"][0]["prepareBundle"]["dependencyDigest"].clone();
            d["posts"][0]["text"] = json!("New schema or new context");
            d["items"][0]["autoPreparation"] = json!({"status":"queued","jobId":null,"inputDigest":null,"attempts":0});
            if retry_state != "queued" {
                let retry = super::super::new_job(&mut d, "assistant", "i").unwrap();
                super::super::row_mut(&mut d, "jobs", &retry).unwrap()["purpose"] = json!("auto_prepare");
                if retry_state == "cancelled" { super::super::row_mut(&mut d, "jobs", &retry).unwrap()["status"] = json!("cancelled"); }
                d["items"][0]["autoPreparation"] = json!({"status":"running","jobId":retry,"inputDigest":"new-fingerprint","attempts":1});
            }
            crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
            assert!(claim(&mut d, NOW + 1).unwrap().is_none());
            assert_eq!(d["items"][0]["autoPreparation"]["requiresReview"], true);
            assert_eq!(d["items"][0]["autoPreparation"]["jobId"], accepted_job);
            assert_eq!(d["items"][0]["autoPreparation"]["inputDigest"], digest);
            assert_eq!(d["items"][0]["reason"], "Friendly comment");
            assert_eq!(super::super::list(&d, "jobs").len(), if retry_state != "queued" {2} else {1});
            assert!(super::super::list(&d, "proposals").is_empty());
            let restored = d.clone();
            crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
            assert!(claim(&mut d, NOW + 2).unwrap().is_none());
            assert_eq!(d, restored);
        }
    }
    #[test]
    fn prepares_proposal_without_chat_or_external_action() {
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        complete_with_captured_result(&mut d, &job, &response("reply"), NOW).unwrap();
        assert_eq!(d["items"][0]["workflow"], "prepared");
        assert_eq!(d["proposals"][0]["status"], "draft");
        assert!(d["conversations"].as_array().unwrap().is_empty());
        assert!(d["approvals"].as_array().unwrap().is_empty());
        assert!(d["operations"].as_array().unwrap().is_empty());
        assert!(super::super::proposal_current(&d, &d["proposals"][0]).is_ok());
    }
    #[test]
    fn interrupted_reserved_preparation_retains_original_job_without_fresh_paid_retry() {
        let mut d = fixture();
        let (job, _) = claim(&mut d,NOW).unwrap().unwrap();
        super::super::row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("interrupted");
        let saved=super::super::row(&d,"jobs",&job).unwrap().clone();
        // V76 reserves the original paid request. A crash is not a confirmed
        // no-result failure, and elapsed backoff cannot authorize fresh spend.
        for at in [NOW+1,NOW+181,NOW+550,NOW+86_400] {assert!(claim(&mut d,at).unwrap().is_none());}
        assert_eq!(super::super::list(&d,"jobs").len(),1);
        assert_eq!(super::super::row(&d,"jobs",&job).unwrap(),&saved);
        assert_eq!(d["items"][0]["autoPreparation"]["attempts"],1);
    }
    #[test]
    fn startup_projects_interrupted_preparation_without_scheduler() {
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        // Exercise the actual startup path, with no preparation tick afterward.
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
        assert_eq!(super::super::row(&d, "jobs", &job).unwrap()["status"], "interrupted");
        assert_eq!(d["items"][0]["autoPreparation"]["status"], "error");
        assert_eq!(d["items"][0]["autoPreparation"]["jobId"], job);
        assert_eq!(d["jobs"].as_array().unwrap().len(), 1);
        assert!(super::super::list(&d, "proposals").is_empty());
        assert!(super::super::list(&d, "operations").is_empty());
        let recovered = d.clone();
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
        assert_eq!(d, recovered);
    }
    #[test]
    fn chunked_review_failure_requires_explicit_resume_even_after_context_changes() {
        for restart in [false, true] {
            let mut d = fixture();
            let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
            super::super::row_mut(&mut d, "jobs", &job).unwrap()["preparationStages"] =
                json!({"reviewChunks":{"version":1,"status":"held"}});
            if restart {
                crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
            } else {
                failed(&mut d, "i", &job, "ADAPTER_TIMEOUT", true, NOW + 1);
                super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("failed");
            }
            assert_eq!(d["items"][0]["autoPreparation"]["reviewResumeRequired"], true);
            assert!(d["items"][0]["autoPreparation"]["retryAt"].is_null());
            d["posts"][0]["text"] = json!("Changed context must not reset paid review budget");
            d["items"][0]["providerObservedAt"] = json!(stamp(NOW + 600));
            assert!(claim(&mut d, NOW + 600).unwrap().is_none());
            assert_eq!(d["jobs"].as_array().unwrap().len(), 1);
            assert!(super::super::list(&d, "proposals").is_empty());
            assert!(super::super::list(&d, "operations").is_empty());
        }
    }
    #[test]
    fn recovery_preserves_operator_decision_and_saved_proposal() {
        for guard in ["text", "cleared", "override", "proposal", "operation", "closed"] {
            let mut d = fixture();
            let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
            super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("interrupted");
            d["items"][0]["reason"] = json!("Operator decision");
            d["items"][0]["decision"] = json!("reply");
            match guard {
                "text" => d["items"][0]["draft"] = json!("Keep this draft"),
                "cleared" => d["items"][0]["draftEdited"] = json!(true),
                "override" => d["items"][0]["autoPreparation"]["humanOverrideAt"] = json!(stamp(NOW)),
                "proposal" => d["proposals"] = json!([{"id":"saved","itemId":"i","status":"draft","text":"Keep candidate"}]),
                "operation" => d["operations"] = json!([{"id":"op","itemId":"i","status":"unknown"}]),
                _ => d["items"][0]["workflow"] = json!("closed"),
            }
            let before = d.clone();
            recover_jobs(&mut d, NOW + 1);
            assert_eq!(d["items"][0]["autoPreparation"]["status"], "error");
            assert!(d["items"][0]["autoPreparation"]["retryAt"].is_null());
            // Only the obsolete automatic-progress metadata may change.
            let mut expected = before.clone();
            expected["items"][0]["autoPreparation"] = d["items"][0]["autoPreparation"].clone();
            assert_eq!(d, expected, "{guard}");
            assert_eq!(d["items"][0]["autoPreparation"]["humanOverrideAt"], before["items"][0]["autoPreparation"]["humanOverrideAt"]);
        }
    }
    #[test]
    fn startup_projects_interrupted_revalidation_without_reclaiming_it() {
        let mut d = fixture();
        d["items"][0]["autoRevalidation"] = json!({"status":"running","jobId":"review","pendingDigest":"source"});
        d["jobs"] = json!([{"id":"review","kind":"assistant","purpose":"auto_revalidate","status":"running"}]);
        d["items"][0]["draft"] = json!("Keep operator text");
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
        assert_eq!(d["items"][0]["autoRevalidation"]["status"], "held");
        assert_eq!(d["items"][0]["autoRevalidation"]["pendingDigest"], "source");
        assert_eq!(d["items"][0]["draft"], "Keep operator text");
        assert_eq!(d["jobs"].as_array().unwrap().len(), 1);
        let recovered = d.clone();
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();
        assert_eq!(d, recovered);
    }
    #[test]
    fn human_edit_during_model_run_wins() {
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        let result=captured_result(&mut d,&job,&response("reply")).unwrap();
        d["items"][0]["draft"] = json!("Human draft");
        assert_eq!(
            super::complete(&mut d, &job, &result, NOW).unwrap()["status"],
            "stale"
        );
        assert!(d["proposals"].as_array().unwrap().is_empty());
        assert_eq!(d["items"][0]["draft"], "Human draft");
    }
    #[test]
    fn model_failure_is_bounded_and_nontransient_failure_is_held() {
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        failed(&mut d, "i", &job, "ASSISTANT_UNAVAILABLE", false, NOW);
        d["jobs"][0]["status"] = json!("failed");
        assert!(claim(&mut d, NOW + 120).unwrap().is_none());
        assert_eq!(d["items"][0]["autoPreparation"]["status"], "error");
        assert!(!transient("ASSISTANT_INVALID_RESPONSE"));
        assert!(transient("Adapter failed (ASSISTANT_BUSY)"));
    }
    #[test]
    fn transient_model_failures_retry_exactly_three_times() {
        let mut d = fixture();
        let mut at = NOW;
        for attempt in 1..=3 {
            let (job, _) = claim(&mut d, at).unwrap().unwrap();
            failed(
                &mut d,
                "i",
                &job,
                "Adapter failed (ASSISTANT_BUSY)",
                true,
                at,
            );
            super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("failed");
            assert_eq!(d["items"][0]["autoPreparation"]["attempts"], attempt);
            assert!(claim(&mut d, at + 1).unwrap().is_none());
            at += 60 * attempt as i64;
        }
        assert!(claim(&mut d, at).unwrap().is_none());
        assert_eq!(d["jobs"].as_array().unwrap().len(), 3);
        assert!(d["items"][0]["autoPreparation"]["retryAt"].is_null());
    }
    #[test]
    fn transient_review_failures_keep_first_pass_and_bounded_retry_but_never_admit_it() {
        let mut d=fixture();let mut at=NOW;
        for attempt in 1..=3 {
            let (job,_)=claim(&mut d,at).unwrap().unwrap();
            legacy_two_pass_request(&mut d,&job);
            crate::preparation_review::record_first(&mut d,&job,&response("needs_attention"),&stamp(at)).unwrap();
            let cause="Adapter failed (ADAPTER_TIMEOUT)";
            crate::preparation_review::record_review(&mut d,&job,Err(cause),&stamp(at+1)).unwrap();
            let reason=crate::preparation_review::failure_message(cause);
            assert!(transient(&reason));failed(&mut d,"i",&job,&reason,transient(&reason),at+1);
            crate::row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("failed");
            assert_eq!(d["items"][0]["autoPreparation"]["attempts"],attempt);
            assert!(crate::list(&d,"proposals").is_empty());
            assert_eq!(crate::row(&d,"jobs",&job).unwrap()["preparationStages"]["first"]["status"],"completed");
            assert!(claim(&mut d,at+2).unwrap().is_none());at+=60*attempt+2;
        }
        assert!(claim(&mut d,at).unwrap().is_none());
        assert_eq!(crate::list(&d,"jobs").len(),3);
        assert_eq!(d["preparationResearch"].as_array().unwrap().len(),3);
        assert!(d["items"][0]["autoPreparation"]["retryAt"].is_null());
        for cause in ["ASSISTANT_INVALID_RESEARCH","ASSISTANT_RESEARCH_LIMIT","ASSISTANT_ISOLATION_FAILED","CANCELLED"] {
            assert!(!transient(&crate::preparation_review::failure_message(cause)));
        }
    }
    #[test]
    fn late_failure_preserves_operator_decision_and_closed_items_never_retry() {
        for guard in ["draft","cleared","override","closed","proposal","operation"] {
            let mut d=fixture();let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
            d["items"][0]["reason"]=json!("Operator selected this decision");
            d["items"][0]["decision"]=json!("close");
            match guard {
                "draft"=>d["items"][0]["draft"]=json!("Operator draft"),
                "cleared"=>d["items"][0]["draftEdited"]=json!(true),
                "override"=>d["items"][0]["autoPreparation"]["humanOverrideAt"]=json!(stamp(NOW)),
                "closed"=>{d["items"][0]["workflow"]=json!("closed");d["items"][0]["providerStatus"]=json!("closed");},
                "proposal"=>d["proposals"]=json!([{"id":"manual","itemId":"i","status":"draft","text":"Keep proposal"}]),
                _=>d["operations"]=json!([{"id":"op","itemId":"i","status":"unknown"}]),
            }
            let before=d.clone();
            failed(&mut d,"i",&job,"Adapter failed (ADAPTER_TIMEOUT)",true,NOW+1);
            let mut expected=before;expected["items"][0]["autoPreparation"]=d["items"][0]["autoPreparation"].clone();
            assert_eq!(d,expected,"{guard}");
            assert!(d["items"][0]["autoPreparation"]["retryAt"].is_null());
            crate::row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("failed");
            assert!(claim(&mut d,NOW+180).unwrap().is_none(),"{guard}");
        }
    }
    #[test]
    fn single_pass_singleton_and_grouped_moderation_use_canonical_rule_and_approval_path(){
        for (kind,platform,grouped) in [("delete","youtube",false),("hide","tiktok",false),("delete","youtube",true),("hide","tiktok",true)]{
            let mut d=fixture();d["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding();d["items"][0]["platform"]=json!(platform);
            let at="2026-01-01T00:00:00Z".to_owned();crate::knowledge::save_instruction(&mut d,&json!({"requestId":"test-rule","title":"Current company rule","text":"Moderate targeted insults with the permitted exact action."}),&at).unwrap();
            let (job,req)=claim(&mut d,NOW).unwrap().unwrap();
            assert_eq!(req["items"][0]["moderationCapabilities"][kind],"supported");
            let rule=req["moderationContext"]["ruleRefs"][0].clone();assert!(rule["hash"].is_string(),"manifest={} materials={} context={}",req["knowledgeManifest"],req["materials"],req["moderationContext"]);
            let mut r=json!({"text":"Moderation proposed","sources":[],"assessments":[{"itemId":"i","outcome":kind,"reason":"Current rule applies to exact comment","tags":["moderation"]}],
                "proposals":[{"itemId":"i","kind":kind,"text":""}],"moderationEvidence":{"version":1,"entries":[{"itemId":"i","kind":kind,"ruleRefs":[rule]}]},
                "editorialEvidence":{"version":1,"contract":crate::editorial_review::CONTRACT,"entries":[{"itemId":"i","kind":kind,"textSha256":crate::editorial_review::hash_text(""),"decision":"accept","reason":"Exact action checked","checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}}]},
                "runMetadata":{"schemaVersion":1,"model":crate::codex_model_policy::MODEL,"modelProfile":crate::codex_model_policy::PROFILE,"reasoningEffort":"high","promptVersion":"communityhero-preparation-v1-single-pass",
                    "instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),"cliSha256":crate::codex_model_policy::CLI_SHA256,"elapsedMs":1,"completedAt":stamp(NOW)}});
            r["runMetadata"]["decisionDependencies"]=json!({"version":1,"entries":[{"itemId":"i","dependsOnItemIds":[]}]});
            r["runMetadata"]["researchLimitContract"]=json!("uncapped_evidence_v1");
            r["factDependencies"]=json!([]);
            r=captured_result(&mut d,&job,&r).unwrap();
            let before=d.clone();let mut missing=d.clone();let mut no_proof=r.clone();no_proof.as_object_mut().unwrap().remove("moderationEvidence");
            assert_eq!(complete_initial(&mut missing,&job,&no_proof,NOW+1).unwrap_err().1,"Moderation rule evidence missing");
            assert_eq!(missing,before);
            if grouped{
                let groups=crate::prepare_bundle::capture_groups(&d,&crate::row(&d,"jobs",&job).unwrap()["prepareBundle"]).unwrap();
                crate::row_mut(&mut d,"jobs",&job).unwrap()["preparationStages"]["groupAdmission"]=groups;
                crate::preparation_review::record_first(&mut d,&job,&r,&stamp(NOW)).unwrap();
            }
            let out=complete_initial(&mut d,&job,&r,NOW+1).unwrap();assert_eq!(out["status"],"prepared", "{kind} grouped={grouped}: {out}");
            assert_eq!(d["proposals"][0]["kind"],kind);assert_eq!(d["proposals"][0]["status"],"draft");
            assert_eq!(d["proposals"][0]["generationMetadata"]["moderationEvidence"],r["moderationEvidence"]);
            assert!(crate::list(&d,"approvals").is_empty());assert!(crate::list(&d,"operations").is_empty());
            let mut legacy=before;
            legacy_two_pass_request(&mut legacy,&job);
            r.as_object_mut().unwrap().remove("moderationEvidence");assert!(complete_initial(&mut legacy,&job,&r,NOW+1).is_err());
        }
    }

    #[test]
    fn automatic_public_fact_authority_is_captured_only_on_new_opted_in_parent(){
        for enabled in [false,true]{
            let mut d=fixture();
            if enabled{d["settings"]["autoPreparation"]["publicFactFollowup"]=json!({"version":1,"enabled":true});}
            let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
            let parent=crate::row(&d,"jobs",&job).unwrap();
            assert_eq!(parent.get("automaticFactPolicy").is_some(),enabled);
            crate::fact_followup::validate_automatic_policy(&d,parent).unwrap();
            if enabled{
                assert_eq!(parent["automaticFactPolicy"]["parentJobId"],job);
                assert_eq!(parent["automaticFactPolicy"]["bundleDigest"],parent["prepareBundle"]["digest"]);
                assert_eq!(parent["automaticFactPolicy"]["maxResearchAttempts"],1);
            }else{
                d["settings"]["autoPreparation"]["publicFactFollowup"]=json!({"version":1,"enabled":true});
                assert!(crate::row(&d,"jobs",&job).unwrap().get("automaticFactPolicy").is_none(),"enabling does not retroactively grant old parents");
            }
        }
    }

    #[test]
    fn cancelled_native_claim_settles_unstarted_ticket_after_first_reserve_rejection() {
        let mut d = fixture();
        if d.get("connectorBinding").is_none() { d["connectorBinding"] = crate::active_binding(&d).unwrap().to_json(); }
        for key in ["jobs", "operations", "approvals", "audit", "materials", "knowledge_entries", "knowledge_versions"] {
            if d.get(key).is_none() { d[key] = json!([]); }
        }
        let token = crate::runtime_lifecycle::OwnerToken {
            account: d["account"].as_str().unwrap().into(), runtime_id: "offline-runtime".into(),
            release_sha256: "a".repeat(64), epoch: 1,
        };
        let ledger = crate::runtime_lifecycle::ledger_digest(&d).unwrap();
        crate::runtime_lifecycle::initialize(&mut d, token.clone(), &"b".repeat(64), &ledger).unwrap();
        let (run, request) = claim(&mut d, NOW).unwrap().unwrap();
        crate::preparation_review::record_initial_admission(&mut d, &token, &run, &stamp(NOW)).unwrap();
        let keys = crate::preparation_workers::keys(&d, crate::row(&d, "jobs", &run).unwrap());
        crate::row_mut(&mut d, "jobs", &run).unwrap()["status"] = json!("cancelled");
        let cancelled = d.clone();
        let registry = crate::runtime_owned_work::Registry::default();
        let native = registry.begin(crate::runtime_owned_work::Kind::Preparation).unwrap();
        assert_eq!(registry.snapshot().unwrap().active, 1);
        let error = match reserve_initial_call_captured(&mut d, &token, &run, &request, keys.as_ref(), &stamp(NOW + 1)) {
            Err(error) => { native.settled(); error }
            Ok(()) => panic!("Cancelled native claim cannot reserve a first assistant call"),
        };
        assert_eq!(error.0, axum::http::StatusCode::CONFLICT);
        assert_eq!(d, cancelled);
        assert!(crate::row(&d, "jobs", &run).unwrap()["preparationStages"]["firstAdmission"].is_null());
        let snapshot = registry.snapshot().unwrap();
        assert_eq!(snapshot.active, 0);
        assert_eq!(snapshot.unresolved, 0);
        let drain = registry.close().unwrap();
        registry.resume(&drain).unwrap();
    }

}
