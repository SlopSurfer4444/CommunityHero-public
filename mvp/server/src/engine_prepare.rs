//! Explicit, durable preparation for the headless engine API.
//!
//! This path creates reviewable drafts only. It never approves or dispatches
//! them, and media prerequisites hold only the affected items.
use axum::{Json, extract::State};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const MAX_ITEMS: usize = 100;
const MAX_ITEM_ID: usize = 256;
const MAX_INSTRUCTION: usize = 12_000;
const MAX_REQUEST_BYTES: usize = 550_000;

#[derive(Clone)]
struct Input {
    item_ids: Vec<String>,
    instruction: Option<String>,
}

struct Scheduled {
    job_id: String,
    request: Option<Value>,
    selected: Vec<String>,
    held: Vec<Value>,
}

fn parse(body: &Value) -> super::ApiResult<Input> {
    let object = body
        .as_object()
        .ok_or_else(|| super::bad("Engine prepare body must be an object"))?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "itemIds" | "instruction"))
    {
        return Err(super::bad(
            "Engine prepare body contains unsupported fields",
        ));
    }
    let values = object
        .get("itemIds")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty() && values.len() <= MAX_ITEMS)
        .ok_or_else(|| super::bad("Engine prepare requires 1 to 100 itemIds"))?;
    let mut seen = BTreeSet::new();
    let mut item_ids = Vec::with_capacity(values.len());
    for value in values {
        let item_id = value
            .as_str()
            .filter(|value| !value.trim().is_empty() && value.encode_utf16().count() <= MAX_ITEM_ID)
            .ok_or_else(|| super::bad("Engine prepare itemIds must be non-empty strings"))?;
        if !seen.insert(item_id.to_owned()) {
            return Err(super::bad("Engine prepare itemIds must be unique"));
        }
        item_ids.push(item_id.to_owned());
    }
    let instruction = match object.get("instruction") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|value| value.encode_utf16().count() <= MAX_INSTRUCTION)
                .ok_or_else(|| {
                    super::bad("Engine prepare instruction must be at most 12000 characters")
                })?
                .to_owned(),
        ),
    };
    Ok(Input {
        item_ids,
        instruction,
    })
}

fn rehash(bundle: &mut Value) {
    let digest = format!(
        "{:x}",
        Sha256::digest(bundle["request"].to_string().as_bytes())
    );
    bundle["digest"] = json!(digest);
}

fn schedule(d: &mut Value, input: Input) -> super::ApiResult<Scheduled> {
    let binding = super::active_binding(d)?;
    super::bridge_account(&binding)?;

    let items: Vec<Value> = input
        .item_ids
        .iter()
        .map(|item_id| super::row(d, "items", item_id).cloned())
        .collect::<super::ApiResult<_>>()?;
    let media = super::media_queue::preparation_states(d, &items, &super::now())?;
    let mut selected = Vec::new();
    let mut held = Vec::new();
    for item_id in &input.item_ids {
        match media.get(item_id).copied().flatten() {
            Some(reason) => held.push(json!({"itemId":item_id,"reason":reason})),
            None => selected.push(item_id.clone()),
        }
    }

    let selected_values: Vec<Value> = selected.iter().map(|item_id| json!(item_id)).collect();
    let mut bundle = if selected.is_empty() {
        None
    } else {
        let mut bundle =
            super::prepare_bundle::build(d, &selected_values, &[]).map_err(super::bad)?;
        bundle["request"]["purpose"] = json!("triage");
        if let Some(instruction) = input.instruction.filter(|value| !value.is_empty()) {
            let base = bundle["request"]["instruction"].as_str().unwrap_or("");
            bundle["request"]["instruction"] = json!(format!(
                "{base}\n\nAdditional operator instruction:\n{instruction}"
            ));
        }
        if bundle["request"].to_string().len() > MAX_REQUEST_BYTES {
            return Err(super::bad(
                "Selected assistant evidence exceeds the 550000-byte budget; reduce attachments or instruction",
            ));
        }
        rehash(&mut bundle);
        Some(bundle)
    };

    let job_id = super::new_job(d, "assistant", "engine_prepare")?;
    let job = super::row_mut(d, "jobs", &job_id)?;
    job["purpose"] = json!("engine_prepare");
    job["requestedItemIds"] = json!(input.item_ids);
    job["selectedItemIds"] = json!(selected);
    job["held"] = json!(held);
    job["preparationStages"] = json!({"first":null,"review":null});
    let request = bundle.as_ref().map(|bundle| bundle["request"].clone());
    if let Some(bundle) = bundle.take() {
        job["prepareBundle"] = bundle;
    }
    Ok(Scheduled {
        job_id,
        request,
        selected,
        held,
    })
}

fn preflight(d: &Value, job_id: &str) -> super::ApiResult<()> {
    let binding = super::active_binding(d)?;
    super::bridge_account(&binding)?;
    let job = super::row(d, "jobs", job_id)?;
    if job["status"] != "running"
        || job["kind"] != "assistant"
        || job["purpose"] != "engine_prepare"
    {
        return Err(super::conflict(
            "Engine preparation cancelled before model call",
        ));
    }
    let bundle = job
        .get("prepareBundle")
        .filter(|bundle| bundle.is_object())
        .ok_or_else(|| super::conflict("Engine preparation bundle is missing"))?;
    super::prepare_bundle::current(d, bundle).map_err(super::conflict)?;
    let item_ids = bundle["itemIds"]
        .as_array()
        .ok_or_else(|| super::conflict("Engine preparation recipients are missing"))?;
    let items: Vec<Value> = item_ids
        .iter()
        .map(|item_id| {
            item_id
                .as_str()
                .ok_or_else(|| super::conflict("Engine preparation recipient is invalid"))
                .and_then(|item_id| super::row(d, "items", item_id).cloned())
        })
        .collect::<super::ApiResult<_>>()?;
    let media = super::media_queue::preparation_states(d, &items, &super::now())?;
    if media.values().any(Option::is_some) {
        return Err(super::conflict(
            "Preparation media evidence changed before model call",
        ));
    }
    Ok(())
}

fn all_held_result(held: Vec<Value>) -> Value {
    json!({
        "conversationId":null,
        "prepareBundleId":null,
        "status":"held",
        "reason":null,
        "candidates":[],
        "held":held,
        "selectedItemIds":[],
        "preparedItemIds":[]
    })
}

fn enrich(mut admission: Value, held: Vec<Value>, selected: Vec<String>) -> Value {
    let prepared: Vec<Value> = admission["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|candidate| candidate["status"] == "review")
        .filter_map(|candidate| candidate["itemId"].as_str().map(|item_id| json!(item_id)))
        .collect();
    let object: &mut Map<String, Value> =
        admission.as_object_mut().expect("admission is an object");
    object.insert("held".into(), json!(held));
    object.insert("selectedItemIds".into(), json!(selected));
    object.insert("preparedItemIds".into(), Value::Array(prepared));
    admission
}

async fn run(app: super::App, scheduled: Scheduled) -> super::ApiResult<Value> {
    if scheduled.request.is_none() {
        return Ok(all_held_result(scheduled.held));
    }
    let _assistant_guard = app.assistant_gate.lock().await;
    let run = scheduled.job_id.clone();
    app.read().await.and_then(|d| preflight(&d, &run))?;
    let first = app
        .bridge("assistant", scheduled.request.expect("checked above"))
        .await?;
    let review = app
        .change(|d| {
            preflight(d, &run)?;
            super::preparation_review::record_first(d, &run, &first, &super::now())
        })
        .await?;
    let (result, reviewed) = match review {
        None => (first, false),
        Some(request) => {
            app.read().await.and_then(|d| preflight(&d, &run))?;
            match app.bridge("assistant", request).await {
                Ok(result) => (result, true),
                Err(error) => {
                    app.change(|d| {
                        super::preparation_review::record_review(
                            d,
                            &run,
                            Err(&error.1),
                            &super::now(),
                        )
                    })
                    .await?;
                    return Err(super::conflict(
                        &super::preparation_review::failure_message(&error.1),
                    ));
                }
            }
        }
    };
    app.change(|d| {
        preflight(d, &run)?;
        let admission = super::prepare_bundle::admit_to(d, &run, None, &result)?;
        if reviewed {
            super::preparation_review::record_review(d, &run, Ok(&result), &super::now())?;
        }
        let outcome = enrich(admission, scheduled.held, scheduled.selected);
        super::row_mut(d, "jobs", &run)?["prepareOutcome"] = outcome.clone();
        Ok(outcome)
    })
    .await
}

pub async fn prepare(
    State(app): State<super::App>,
    Json(body): Json<Value>,
) -> super::ApiResult<Json<Value>> {
    let input = parse(&body)?;
    let scheduled = app.change(|d| schedule(d, input)).await?;
    let job_id = scheduled.job_id.clone();
    let worker = app.clone();
    app.spawn(job_id.clone(), run(worker, scheduled));
    Ok(Json(json!({"jobId":job_id})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(video: bool) -> Value {
        let mut d = super::super::empty();
        super::super::accounts::initialize(&mut d, super::super::accounts::Profile::LikeAvto)
            .unwrap();
        d["items"] = json!([
            {"id":"ready","itemId":"c-ready","objectId":"o-ready","platform":"VK","postKey":"ready-post","conversationKey":"ready-thread","branchId":"ready-branch","postId":"ready-post","revision":1,"draft":"","workflow":"attention","providerStatus":"new"},
            {"id":"media","itemId":"c-media","objectId":"o-media","platform":"VK","postKey":"media-post","conversationKey":"media-thread","branchId":"media-branch","postId":"media-post","revision":1,"draft":"","workflow":"attention","providerStatus":"new"}
        ]);
        d["branches"] = json!([
            {"id":"ready-branch","postId":"ready-post","messages":[{"id":"c-ready","text":"Спасибо"}],"contextComplete":true},
            {"id":"media-branch","postId":"media-post","messages":[{"id":"c-media","text":"Что в видео?"}],"contextComplete":true}
        ]);
        d["posts"] = json!([
            {"id":"ready-post","postKey":"ready-post","objectId":"o-ready","platform":"VK","text":"Обычный пост"},
            {"id":"media-post","postKey":"media-post","objectId":"o-media","platform":"VK","text":"Видео","attachments":if video {json!([{"type":"video"}])} else {json!([])}}
        ]);
        d
    }

    fn routine_result() -> Value {
        json!({
            "text":"Draft ready",
            "sources":[],
            "assessments":[{"itemId":"ready","outcome":"reply","reason":"Friendly feedback","tags":["feedback"]}],
            "proposals":[{"itemId":"ready","kind":"reply_and_close","text":"Спасибо!"}]
        })
    }

    #[test]
    fn body_is_strict_and_bounded() {
        assert!(parse(&json!({"itemIds":["one"],"instruction":"context"})).is_ok());
        for body in [
            json!({}),
            json!({"itemIds":[]}),
            json!({"itemIds":["one","one"]}),
            json!({"itemIds":[1]}),
            json!({"itemIds":["one"],"extra":true}),
            json!({"itemIds":["one"],"instruction":"x".repeat(MAX_INSTRUCTION+1)}),
        ] {
            assert!(parse(&body).is_err());
        }
        assert!(
            parse(&json!({"itemIds":(0..MAX_ITEMS).map(|n|format!("i{n}")).collect::<Vec<_>>() }))
                .is_ok()
        );
        assert!(
            parse(&json!({"itemIds":(0..=MAX_ITEMS).map(|n|format!("i{n}")).collect::<Vec<_>>() }))
                .is_err()
        );
    }

    #[test]
    fn scheduling_holds_only_media_item_and_pins_ready_evidence() {
        let mut d = fixture(true);
        let scheduled = schedule(
            &mut d,
            Input {
                item_ids: vec!["ready".into(), "media".into()],
                instruction: Some("Keep it short".into()),
            },
        )
        .unwrap();
        assert_eq!(scheduled.selected, ["ready"]);
        assert_eq!(scheduled.held[0]["itemId"], "media");
        assert_eq!(scheduled.held[0]["reason"], "media_wait");
        assert_eq!(scheduled.request.as_ref().unwrap()["purpose"], "triage");
        assert!(
            scheduled.request.as_ref().unwrap()["instruction"]
                .as_str()
                .unwrap()
                .contains("Keep it short")
        );
        let job = super::super::row(&d, "jobs", &scheduled.job_id).unwrap();
        assert_eq!(job["kind"], "assistant");
        assert_eq!(job["purpose"], "engine_prepare");
        assert_eq!(job["prepareBundle"]["itemIds"], json!(["ready"]));
        assert!(super::super::prepare_bundle::current(&d, &job["prepareBundle"]).is_ok());
        assert!(preflight(&d, &scheduled.job_id).is_ok());
    }

    #[test]
    fn all_held_completes_without_a_model_request() {
        let mut d = fixture(true);
        d["items"] = json!([d["items"][1].clone()]);
        d["branches"] = json!([d["branches"][1].clone()]);
        d["posts"] = json!([d["posts"][1].clone()]);
        let scheduled = schedule(
            &mut d,
            Input {
                item_ids: vec!["media".into()],
                instruction: None,
            },
        )
        .unwrap();
        assert!(scheduled.request.is_none());
        assert!(
            super::super::row(&d, "jobs", &scheduled.job_id).unwrap()["prepareBundle"].is_null()
        );
        let result = all_held_result(scheduled.held);
        assert_eq!(result["status"], "held");
        assert!(result["candidates"].as_array().unwrap().is_empty());
        assert!(d["proposals"].as_array().unwrap().is_empty());
        assert!(d["approvals"].as_array().unwrap().is_empty());
        assert!(d["operations"].as_array().unwrap().is_empty());
    }

    #[test]
    fn first_pass_is_archived_before_draft_admission() {
        let mut d = fixture(false);
        d["items"] = json!([d["items"][0].clone()]);
        d["branches"] = json!([d["branches"][0].clone()]);
        d["posts"] = json!([d["posts"][0].clone()]);
        let scheduled = schedule(
            &mut d,
            Input {
                item_ids: vec!["ready".into()],
                instruction: None,
            },
        )
        .unwrap();
        let first = routine_result();
        assert!(
            super::super::preparation_review::record_first(
                &mut d,
                &scheduled.job_id,
                &first,
                "2026-09-22T12:00:00Z"
            )
            .unwrap()
            .is_none()
        );
        assert!(d["proposals"].as_array().unwrap().is_empty());
        let admission =
            super::super::prepare_bundle::admit_to(&mut d, &scheduled.job_id, None, &first)
                .unwrap();
        let result = enrich(admission, scheduled.held, scheduled.selected);
        assert_eq!(result["preparedItemIds"], json!(["ready"]));
        assert_eq!(d["proposals"][0]["prepareRunId"], scheduled.job_id);
        assert_eq!(d["proposals"][0]["status"], "draft");
        assert!(d["approvals"].as_array().unwrap().is_empty());
        assert!(d["operations"].as_array().unwrap().is_empty());
    }

    #[test]
    fn review_is_planned_but_never_admitted_from_first_pass() {
        let mut d = fixture(false);
        d["items"] = json!([d["items"][0].clone()]);
        d["branches"] = json!([d["branches"][0].clone()]);
        d["posts"] = json!([d["posts"][0].clone()]);
        let scheduled = schedule(
            &mut d,
            Input {
                item_ids: vec!["ready".into()],
                instruction: None,
            },
        )
        .unwrap();
        let first = json!({
            "text":"Needs research",
            "sources":[],
            "assessments":[{"itemId":"ready","outcome":"needs_attention","reason":"Need a fact","tags":["needs_fact"]}],
            "proposals":[]
        });
        let review = super::super::preparation_review::record_first(
            &mut d,
            &scheduled.job_id,
            &first,
            "2026-09-22T12:00:00Z",
        )
        .unwrap()
        .unwrap();
        assert_eq!(review["purpose"], "triage_review");
        assert_eq!(review["firstPass"]["trust"], "untrusted_model_output");
        assert!(d["proposals"].as_array().unwrap().is_empty());
        assert!(d["approvals"].as_array().unwrap().is_empty());
        assert!(d["operations"].as_array().unwrap().is_empty());
    }
}
