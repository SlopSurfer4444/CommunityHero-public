//! Durable, read/prepare-only queue. Never creates approvals or calls dispatch.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[path = "auto_revalidation.rs"]
mod revalidation;

const MAX_ATTEMPTS: u64 = 3;
pub async fn configure(
    axum::extract::State(app): axum::extract::State<super::App>,
    axum::Json(body): axum::Json<Value>,
) -> super::ApiResult<axum::Json<Value>> {
    let configuration=revalidation::validated_config(&body)?;
    app.change(|d| {
        d["settings"]["autoPreparation"]["revalidation"]=configuration;
        super::audit(d,"preparation.revalidation_configured","local-pilot");
        Ok(axum::Json(revalidation::status(d,chrono::Utc::now().timestamp())))
    }).await
}
pub async fn status(axum::extract::State(app): axum::extract::State<super::App>) -> super::ApiResult<axum::Json<Value>> {
    let mut value=revalidation::status(&app.read().await?,chrono::Utc::now().timestamp());
    value["workerEnabled"]=json!(std::env::var("COMMUNITYHERO_BACKGROUND_DISABLED").as_deref()!=Ok("1")
        && std::env::var("COMMUNITYHERO_BACKGROUND_GENERATION_DISABLED").as_deref()!=Ok("1"));
    value["transcriptRequiredForVideo"]=json!(true);
    value["visualContextRequiredForVideo"]=json!(true);
    value["visualContextSamplingVersion"]=json!(1);
    Ok(axum::Json(value))
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
fn eligible(d: &Value, item: &Value, now: i64) -> bool {
    eligible_with_media(d,item,now,waiting_for_media(d,item,now))
}
fn eligible_with_media(d: &Value, item: &Value, now: i64, media_wait: bool) -> bool {
    if !matches!(item["providerStatus"].as_str(), Some("new" | "inprogress"))
        || item["workflow"] != "attention"
        || !item["draft"].as_str().unwrap_or("").trim().is_empty()
        || item["draftEdited"] == true
        || !item["autoPreparation"]["humanOverrideAt"].is_null()
        || item["autoPreparation"]["requiresReview"] == true
        || media_wait
        || !time(&item["providerObservedAt"]).is_some_and(|t| (now - 600..=now + 60).contains(&t))
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
    let stale: Vec<(String, String, String)> = {
      let context=super::prepare_bundle::EvidenceContext::new(d);
      super::list(d, "proposals")
        .iter()
        .filter_map(|p| {
            if p["status"] != "draft" {
                return None;
            }
            let run = p["prepareRunId"].as_str().or_else(||p["recovery"]["prepareRunId"].as_str())?;
            let job = super::row(d, "jobs", run).ok()?;
            if !matches!(job["purpose"].as_str(), Some("auto_prepare" | "auto_revalidate")) {
                return None;
            }
            let error = super::proposal_current_with_context(p,&context).err()?;
            Some((
                p["id"].as_str()?.to_string(),
                p["itemId"].as_str()?.to_string(),
                error.1,
            ))
        })
        .collect()
    }; // The borrowed validation index is gone before the first mutation.
    for (proposal_id, item_id, reason) in stale {
        let p = super::row_mut(d, "proposals", &proposal_id).unwrap();
        let run = p["prepareRunId"].as_str().or_else(||p["recovery"]["prepareRunId"].as_str()).map(|v|json!(v)).unwrap_or(Value::Null);
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
                && p["prepareRunId"].as_str().and_then(|run| super::row(d, "jobs", run).ok()).is_some_and(|j| j["purpose"] == "auto_prepare")
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
        let previous = super::list(d, "jobs").iter().rev().find(|job| {
            job["purpose"] == "auto_prepare" && job["status"] == "completed"
                && job["refId"] == item["id"]
                && job["prepareOutcome"]["itemId"] == item["id"]
                && job["prepareOutcome"]["status"] == "needs_attention"
                && job["prepareOutcome"]["reason"].as_str().is_some_and(|s| !s.is_empty())
                && job["prepareBundle"]["itemIds"].as_array().is_some_and(|ids| ids.as_slice() == [item["id"].clone()])
                && job["prepareBundle"]["dependencyDigest"].is_string()
        })?;
        Some((item["id"].as_str()?.to_owned(), previous.clone()))
    }).collect();
    for (item_id, previous) in assessments {
        let item = super::row_mut(d, "items", &item_id).unwrap();
        item["autoPreparation"]["status"] = json!("needs_attention");
        item["autoPreparation"]["requiresReview"] = json!(true);
        item["autoPreparation"]["jobId"] = previous["id"].clone();
        item["autoPreparation"]["inputDigest"] = previous["prepareBundle"]["dependencyDigest"].clone();
        item["autoPreparation"]["retryAt"] = Value::Null;
        item["autoPreparation"]["updatedAt"] = json!(stamp(now));
        item["autoPreparation"]["reason"] = previous["prepareOutcome"]["reason"].clone();
        item["autoPreparation"]["reviewReason"] = json!("Сохранённый анализ восстановлен. Решение требует проверки по текущему контексту.");
        item["reason"] = previous["prepareOutcome"]["reason"].clone();
        item["decision"] = json!("needs_attention");
        super::bump(item);
    }
}

fn failed(d: &mut Value, item_id: &str, job: &str, reason: &str, transient: bool, now: i64) {
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
        let retry = transient && attempts < MAX_ATTEMPTS && !protected;
        item["autoPreparation"]["status"] = json!("error");
        item["autoPreparation"]["reason"] = json!(reason);
        item["autoPreparation"]["updatedAt"] = json!(stamp(now));
        item["autoPreparation"]["retryAt"] = if retry {
            json!(stamp(now + 60 * attempts.max(1) as i64))
        } else {
            Value::Null
        };
        if !protected {
            item["reason"] = json!(format!("Автоподготовка: {reason}"));
            item["decision"] = json!("needs_attention");
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
        if protected {
            let auto = &mut super::row_mut(d, "items", item_id).unwrap()["autoPreparation"];
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
            status == "interrupted" || status == "failed" || status == "missing",
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
    // Register source work in the same transaction before checking its wait
    // window; independently scheduled media/assistant ticks must not race.
    crate::media_queue::reconcile(d,&stamp(now))?;
    reconcile_stale(d, now);
    recover_jobs(d, now);
    hold_legacy_repreparations(d, now);
    // Reconcile interrupted/stale state, but don't rebuild every queued bundle
    // while another preparation owns its execution slot. Personal discussions
    // run independently. Model admission
    // still rechecks the complete current context immediately before use.
    if super::list(d, "jobs").iter().any(|j| {
        j["kind"] == "assistant" && j["purpose"] != "discussion" && matches!(j["status"].as_str(), Some("running" | "queued"))
    }) { return Ok(None); }
    // Alternate ready classes within this same claim transaction. A blocked
    // review (including its stability delay) must not leave initial work idle.
    let review_first=revalidation_turn(d);
    if review_first {
        if let Some(claimed)=revalidation::claim(d,now)? {return Ok(Some(claimed));}
    }
    let attention:Vec<Value>=super::list(d,"items").iter().filter(|i|i["workflow"]=="attention").cloned().collect();
    let media_states=crate::media_queue::preparation_states(d,&attention,&stamp(now))?;
    let media_wait:Vec<(String,Option<&'static str>)>=super::list(d,"items").iter().filter_map(|item|{
        let id=item["id"].as_str()?;
        let state=if item["workflow"]=="attention" && item["draftEdited"]!=true
            && item["draft"].as_str().unwrap_or("").trim().is_empty()
            && item["autoPreparation"]["humanOverrideAt"].is_null() {
                media_states.get(id).copied().unwrap_or(Some("media_unavailable"))
            }else{None};
        (state.is_some() || item.get("preparationMediaWait").is_some()).then(||(id.to_owned(),state))
    }).collect();
    for (id,state) in media_wait {
        let item=super::row_mut(d,"items",&id)?;
        if let Some(state)=state {item["preparationMediaWait"]=json!({"status":state,"reason":if state=="media_unavailable" {"Аудио и визуальный контекст видео пока не получены полностью. Подготовка ждёт доступный ролик и завершённый анализ кадров."}else{"Сначала получаем аудио и визуальный контекст видео, включая финальные кадры. Ответ будет подготовлен после проверки."}});}
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
                    && job["prepareOutcome"]["status"] == "needs_attention"
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
    if super::list(d, "jobs").iter().any(|j| {
        j["kind"] == "assistant" && j["purpose"] != "discussion" && matches!(j["status"].as_str(), Some("running" | "queued"))
    }) {
        return Ok(None);
    }
    for item_id in ready {
        let bundle = match super::prepare_bundle::triage(d, &item_id) {
            Ok(bundle) => bundle,
            Err(reason) => {
                let item = super::row_mut(d, "items", &item_id)?;
                item["autoPreparation"]["status"] = json!("needs_attention");
                item["autoPreparation"]["reason"] = json!(reason);
                item["reason"] = json!(reason);
                item["decision"] = json!("needs_attention");
                continue;
            }
        };
        let request = bundle["request"].clone();
        let job = super::new_job(d, "assistant", &item_id)?;
        let stored = super::row_mut(d, "jobs", &job)?;
        stored["purpose"] = json!("auto_prepare");
        stored["prepareBundle"] = bundle;
        let item = super::row_mut(d, "items", &item_id)?;
        item["autoPreparation"]["status"] = json!("running");
        item["autoPreparation"]["jobId"] = json!(job);
        item["autoPreparation"]["attempts"] =
            json!(item["autoPreparation"]["attempts"].as_u64().unwrap_or(0) + 1);
        item["autoPreparation"]["reason"] = json!("Ассистент проверяет контекст");
        return Ok(Some((job, request)));
    }
    if review_first {Ok(None)} else {revalidation::claim(d, now)}
}

pub fn complete(d: &mut Value, job_id: &str, result: &Value, now: i64) -> super::ApiResult<Value> {
    if super::row(d, "jobs", job_id)?["purpose"] == "auto_revalidate" {
        return revalidation::complete(d, job_id, result, now);
    }
    complete_initial(d, job_id, result, now)
}
fn complete_initial(d: &mut Value, job_id: &str, result: &Value, now: i64) -> super::ApiResult<Value> {
    let job = super::row(d, "jobs", job_id)?.clone();
    let item_id = job["refId"]
        .as_str()
        .ok_or_else(|| super::bad("Auto preparation target missing"))?;
    let item = super::row(d, "items", item_id)?.clone();
    if item["autoPreparation"]["jobId"] != job_id
        || item["autoPreparation"]["status"] != "running"
        || job["status"] != "running"
    {
        return Err(super::conflict(
            "Automatic preparation no longer owns this item",
        ));
    }
    // Recheck eligibility at admission: a human draft or active action wins this race.
    if !eligible(d, &item, now)
        || super::prepare_bundle::current(d, &job["prepareBundle"]).is_err()
        || job["prepareBundle"]["request"]["items"][0]["revision"] != item["revision"]
    {
        failed(
            d,
            item_id,
            job_id,
            "Контекст изменился во время подготовки",
            false,
            now,
        );
        let outcome = json!({"status":"stale","itemId":item_id,"reason":"Context changed during automatic preparation"});
        super::row_mut(d, "jobs", job_id)?["prepareOutcome"] = outcome.clone();
        return Ok(outcome);
    }
    let assessments = result["assessments"]
        .as_array()
        .filter(|a| a.len() == 1)
        .ok_or_else(|| super::bad("Automatic preparation requires one assessment"))?;
    let assessment = &assessments[0];
    let tags = assessment_tags(assessment)?;
    let reason = assessment["reason"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 12000)
        .ok_or_else(|| super::bad("Automatic preparation reason missing"))?;
    if assessment["itemId"] != item_id {
        return Err(super::bad(
            "Automatic preparation assessment target mismatch",
        ));
    }
    let proposals = result["proposals"]
        .as_array()
        .ok_or_else(|| super::bad("Automatic preparation proposals missing"))?;
    let decision = assessment["outcome"].as_str().unwrap_or("");
    let valid = match decision {
        "needs_attention" => proposals.is_empty(),
        "reply" | "close" => {
            proposals.len() == 1
                && proposals[0]["itemId"] == item_id
                && proposals[0]["kind"]
                    == if decision == "reply" {
                        "reply_and_close"
                    } else {
                        "close"
                    }
        }
        _ => false,
    };
    if !valid {
        return Err(super::bad(
            "Automatic preparation assessment and proposal disagree",
        ));
    }
    let outcome = super::prepare_bundle::admit_to(d, job_id, None, result)?;
    let prepared = outcome["candidates"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|v| v["status"] == "review"));
    let item = super::row_mut(d, "items", item_id)?;
    item["autoPreparation"]["status"] = json!(if prepared {
        "prepared"
    } else {
        "needs_attention"
    });
    item["autoPreparation"]["reason"] = json!(reason);
    item["autoPreparation"]["updatedAt"] = json!(stamp(now));
    item["autoPreparation"]["retryAt"] = Value::Null;
    item["decision"] = json!(decision);
    // Descriptive evidence for the operator/model; never action authorization.
    item["triageTags"] = tags;
    item["reason"] = json!(reason);
    if prepared {
        item["workflow"] = json!("prepared");
    }
    let final_outcome = json!({"status":item["autoPreparation"]["status"],"itemId":item_id,"reason":reason,"admission":outcome});
    super::row_mut(d, "jobs", job_id)?["prepareOutcome"] = final_outcome.clone();
    Ok(final_outcome)
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
    super::prepare_bundle::current(d,&job["prepareBundle"]).map_err(super::conflict)?;
    let item=super::row(d,"items",job["refId"].as_str().unwrap_or(""))?;
    if waiting_for_media(d,item,chrono::Utc::now().timestamp()) {
        return Err(super::conflict("Сначала требуется расшифровка видео; модель не запускалась"));
    }
    if item["revision"]!=job["prepareBundle"]["request"]["items"][0]["revision"] {
        return Err(super::conflict("Automatic preparation changed before model call"));
    }
    Ok(())
}

pub async fn tick(app: &super::App) -> super::ApiResult<()> {
    let claimed = app
        .change_preparation_claim(|d| claim(d, chrono::Utc::now().timestamp()))
        .await?;
    let Some((job, request)) = claimed else {
        return Ok(());
    };
    let worker = app.clone();
    let run = job.clone();
    app.spawn(job, async move {
        let _assistant_guard=worker.assistant_gate.lock().await;
        let preflight=worker.read().await.and_then(|d|model_preflight(&d,&run));
        let generated=match preflight {
            Err(error)=>Err(error),
            Ok(())=>match worker.bridge("assistant", request).await {
                Err(error)=>Err(error),
                Ok(first)=>{
                    // Persist the actual first pass before spending on the stronger
                    // one. A crash cannot silently erase the first-pass evidence.
                    let review=worker.change(|d| {
                        model_preflight(d,&run)?;
                        crate::preparation_review::record_first(d,&run,&first,&super::now())
                    }).await;
                    match review {
                        Err(error)=>Err(error),
                        Ok(None)=>Ok((first,false)),
                        Ok(Some(request))=>{
                            let ready=worker.read().await.and_then(|d|model_preflight(&d,&run));
                            let reviewed=match ready {Ok(())=>worker.bridge("assistant",request).await,Err(error)=>Err(error)};
                            match reviewed {
                                Ok(result)=>Ok((result,true)),
                                Err(error)=>{
                                    worker.change(|d|crate::preparation_review::record_review(d,&run,Err(&error.1),&super::now())).await?;
                                    Err(super::conflict(&crate::preparation_review::failure_message(&error.1)))
                                }
                            }
                        }
                    }
                }
            }
        };
        let result = match generated {
            Ok((result,reviewed)) => {
                worker
                    .change(|d| {
                        let outcome=complete(d,&run,&result,chrono::Utc::now().timestamp())?;
                        if reviewed {crate::preparation_review::record_review(d,&run,Ok(&result),&super::now())?;}
                        Ok(outcome)
                    })
                    .await
            }
            Err(error) => Err(error),
        };
        if let Err(error) = &result {
            worker
                .change(|d| {
                    if super::row(d, "jobs", &run)?["purpose"] == "auto_revalidate" {
                        return revalidation::failed(d, &run, &error.1, chrono::Utc::now().timestamp());
                    }
                    let item_id = super::row(d, "jobs", &run)?["refId"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned();
                    failed(
                        d,
                        &item_id,
                        &run,
                        &error.1,
                        transient(&error.1),
                        chrono::Utc::now().timestamp(),
                    );
                    Ok(())
                })
                .await?;
        }
        result
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn without_media_schedule(mut d:Value)->Value {
        // Media reconciliation maintains its own cache; it is not preparation,
        // proposal, source, or operator state.
        d.as_object_mut().unwrap().remove("mediaQueue");
        d
    }
    #[test]
    fn transcript_is_required_even_after_timeout_or_download_failure() {
        for failed_media in [false,true] {
            let mut d=fixture();
            d["posts"][0]["postKey"]=json!("p");
            d["posts"][0]["attachments"]=json!([{"type":"video"}]);
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
        // A provider enrichment may reveal a video without changing its title.
        d["posts"][0]["attachments"]=json!([{"type":"video"}]);
        assert!(model_preflight(&d,&job).is_err());
        let outcome=complete(&mut d,&job,&response("reply"),NOW+1).unwrap();
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
        complete(&mut d, &job, &result, NOW).unwrap();
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
        complete(&mut d, &job, &reply, NOW).unwrap();
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
        older["createdAt"] = json!(stamp(NOW - 30 * 24 * 3600));
        d["items"].as_array_mut().unwrap().push(older);
        let (job, request) = claim(&mut d, NOW).unwrap().unwrap();
        assert_eq!(request["items"][0]["id"], "older");
        let result = json!({"text":"Review","assessments":[{"itemId":"older","outcome":"needs_attention","reason":"Requires human input"}],"proposals":[]});
        complete(&mut d, &job, &result, NOW).unwrap();
        super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("completed");
        let (_, next) = claim(&mut d, NOW).unwrap().unwrap();
        assert_eq!(next["items"][0]["id"], "i");
    }
    const NOW: i64 = 1_800_000_000;
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
        complete(&mut d,&job,&response("reply"),NOW).unwrap();
        let original=d["proposals"][0].clone();
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
        assert!(super::super::proposal_current(&d,&d["proposals"][1]).is_ok());
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
    fn metadata_recovery_rejects_changed_speaker_or_reply() {
        for field in ["role","text","deleted","textUnavailable","attachments"] {
            let mut d=fixture();
            d["branches"][0]["messages"]=json!([{"id":"message","role":"customer","text":"hello"}]);
            super::super::merge_snapshot(&mut d,&json!({})).unwrap();
            let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
            complete(&mut d,&job,&response("reply"),NOW).unwrap();
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
    fn fixture() -> Value {
        let mut d = super::super::empty();
        d["items"] = json!([{"id":"i","itemId":"c","objectId":"o","postKey":"p","conversationKey":"thread","branchId":"b","postId":"post","revision":1,"draft":"","workflow":"attention","providerStatus":"new","createdAt":stamp(NOW-60),"providerObservedAt":stamp(NOW)}]);
        d["branches"] = json!([{"id":"b","postId":"post","messages":[{"id":"c","text":"Hi"}],"contextComplete":false}]);
        d["posts"] = json!([{"id":"post","text":"Post"}]);
        d
    }
    fn response(outcome: &str) -> Value {
        json!({"text":"Review","sources":[],"assessments":[{"itemId":"i","outcome":outcome,"reason":"Friendly comment"}],"proposals":if outcome=="reply"{json!([{"itemId":"i","kind":"reply_and_close","text":"Спасибо!"}])}else{json!([])}})
    }
    fn prepared() -> Value {
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        complete(&mut d, &job, &response("reply"), NOW).unwrap();
        super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("completed");
        d
    }

    #[test]
    fn stale_analysis_validates_catalog_once_for_the_whole_immutable_phase(){
        let mut d=fixture();
        d["materials"]=json!([{"id":"global","text":"Retained knowledge","revision":1}]);
        crate::knowledge::sync_catalog(&mut d,"2026-01-01T00:00:00Z").unwrap();
        let (job,_)=claim(&mut d,NOW).unwrap().unwrap();
        complete(&mut d,&job,&response("reply"),NOW).unwrap();
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
        super::super::knowledge::sync_catalog(&mut d, &stamp(NOW)).unwrap();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        complete(&mut d, &job, &response("reply"), NOW).unwrap();
        super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("completed");
        let before = d.clone();
        for n in 1..=3 {
            d = serde_json::from_str(&d.to_string()).unwrap();
            super::super::knowledge::sync_catalog(&mut d, &stamp(NOW + n)).unwrap();
            super::super::recover(&mut d);
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
        complete(&mut d, &job, &response("reply"), NOW).unwrap();
        super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("completed");
        let old = d["proposals"][0].clone();
        let input_digest = d["items"][0]["autoPreparation"]["inputDigest"].clone();
        // Backdate only fixture validity; selection still uses the real clock.
        super::super::knowledge::save_instruction(&mut d, &json!({"requestId":"new-rule","title":"Style","text":"Use plain language"}), "2026-01-01T00:00:00Z").unwrap();
        super::super::recover(&mut d);
        assert_eq!(d["proposals"][0]["status"], "stale");
        assert_eq!(d["proposals"][0]["text"], old["text"]);
        assert_eq!(d["items"][0]["autoPreparation"]["inputDigest"], input_digest);
        assert_eq!(d["items"][0]["autoPreparation"]["savedProposalId"], old["id"]);
        assert!(super::super::proposal_current(&d, &d["proposals"][0]).is_err());
        let held = d.clone();
        for n in 1..=3 {
            super::super::recover(&mut d);
            assert!(claim(&mut d, NOW + n).unwrap().is_none());
            assert_eq!(without_media_schedule(d.clone()), without_media_schedule(held.clone()));
        }
        assert_eq!(d["jobs"].as_array().unwrap().len(), 1);
        assert!(super::super::list(&d, "operations").is_empty());
        // The existing explicit assistant path can generate a fresh reviewable
        // proposal; the held automatic scheduler does not own that choice.
        let manual_job = super::super::new_job(&mut d, "assistant", "operator-request").unwrap();
        let bundle = super::super::prepare_bundle::build(&d, &[json!("i")], &[]).unwrap();
        super::super::row_mut(&mut d, "jobs", &manual_job).unwrap()["prepareBundle"] = bundle;
        super::super::prepare_bundle::admit_to(&mut d, &manual_job, None, &response("reply")).unwrap();
        assert_eq!(d["proposals"].as_array().unwrap().len(), 2);
        assert!(super::super::proposal_current(&d, &d["proposals"][1]).is_ok());
        assert_eq!(d["proposals"][0]["status"], "stale");
    }

    #[test]
    fn legacy_queued_stale_result_is_held_but_fresh_comment_still_prepares() {
        let mut d = prepared();
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
        super::super::recover(&mut d);
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
        super::super::recover(&mut d);
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
        complete(&mut d,&job,&response("reply"),at).unwrap();
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
    fn stale_observation_manual_draft_and_active_action_are_excluded() {
        for field in ["draft", "providerObservedAt"] {
            let mut d = fixture();
            d["items"][0][field] = json!(if field == "draft" {
                "Human writing".to_owned()
            } else {
                stamp(NOW - 49 * 3600)
            });
            assert!(claim(&mut d, NOW).unwrap().is_none());
        }
        let mut d = fixture();
        d["operations"] = json!([{"itemId":"i","status":"unknown"}]);
        assert!(claim(&mut d, NOW).unwrap().is_none());
    }
    #[test]
    fn attention_result_survives_refresh_and_changed_context_without_automatic_reanalysis() {
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        complete(&mut d, &job, &response("needs_attention"), NOW).unwrap();
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
        let held = d.clone();
        super::super::recover(&mut d);
        assert!(claim(&mut d, NOW + 61).unwrap().is_none());
        assert_eq!(d, held);
    }
    #[test]
    fn legacy_queued_assessment_recovers_history_without_proposal_or_job_pointer() {
        for retry_state in ["queued", "running", "cancelled"] {
            let mut d = fixture();
            let (accepted_job, _) = claim(&mut d, NOW).unwrap().unwrap();
            complete(&mut d, &accepted_job, &response("needs_attention"), NOW).unwrap();
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
            super::super::recover(&mut d);
            assert!(claim(&mut d, NOW + 1).unwrap().is_none());
            assert_eq!(d["items"][0]["autoPreparation"]["requiresReview"], true);
            assert_eq!(d["items"][0]["autoPreparation"]["jobId"], accepted_job);
            assert_eq!(d["items"][0]["autoPreparation"]["inputDigest"], digest);
            assert_eq!(d["items"][0]["reason"], "Friendly comment");
            assert_eq!(super::super::list(&d, "jobs").len(), if retry_state != "queued" {2} else {1});
            assert!(super::super::list(&d, "proposals").is_empty());
            let restored = d.clone();
            super::super::recover(&mut d);
            assert!(claim(&mut d, NOW + 2).unwrap().is_none());
            assert_eq!(d, restored);
        }
    }
    #[test]
    fn prepares_proposal_without_chat_or_external_action() {
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        complete(&mut d, &job, &response("reply"), NOW).unwrap();
        assert_eq!(d["items"][0]["workflow"], "prepared");
        assert_eq!(d["proposals"][0]["status"], "draft");
        assert!(d["conversations"].as_array().unwrap().is_empty());
        assert!(d["approvals"].as_array().unwrap().is_empty());
        assert!(d["operations"].as_array().unwrap().is_empty());
        assert!(super::super::proposal_current(&d, &d["proposals"][0]).is_ok());
    }
    #[test]
    fn restart_retries_with_backoff_and_stops_after_three_attempts() {
        let mut d = fixture();
        for attempt in 1..=3 {
            let at = NOW + ((attempt - 1) * 180) as i64;
            let (job, _) = claim(&mut d, at).unwrap().unwrap();
            assert_eq!(d["items"][0]["autoPreparation"]["attempts"], attempt);
            super::super::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("interrupted");
            assert!(claim(&mut d, at + 1).unwrap().is_none());
        }
        assert!(claim(&mut d, NOW + 550).unwrap().is_none());
    }
    #[test]
    fn startup_projects_interrupted_preparation_without_scheduler() {
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        // Exercise the actual startup path, with no preparation tick afterward.
        super::super::recover(&mut d);
        assert_eq!(super::super::row(&d, "jobs", &job).unwrap()["status"], "interrupted");
        assert_eq!(d["items"][0]["autoPreparation"]["status"], "error");
        assert_eq!(d["items"][0]["autoPreparation"]["jobId"], job);
        assert_eq!(d["jobs"].as_array().unwrap().len(), 1);
        assert!(super::super::list(&d, "proposals").is_empty());
        assert!(super::super::list(&d, "operations").is_empty());
        let recovered = d.clone();
        super::super::recover(&mut d);
        assert_eq!(d, recovered);
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
        super::super::recover(&mut d);
        assert_eq!(d["items"][0]["autoRevalidation"]["status"], "held");
        assert_eq!(d["items"][0]["autoRevalidation"]["pendingDigest"], "source");
        assert_eq!(d["items"][0]["draft"], "Keep operator text");
        assert_eq!(d["jobs"].as_array().unwrap().len(), 1);
        let recovered = d.clone();
        super::super::recover(&mut d);
        assert_eq!(d, recovered);
    }
    #[test]
    fn human_edit_during_model_run_wins() {
        let mut d = fixture();
        let (job, _) = claim(&mut d, NOW).unwrap().unwrap();
        d["items"][0]["draft"] = json!("Human draft");
        assert_eq!(
            complete(&mut d, &job, &response("reply"), NOW).unwrap()["status"],
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
}
