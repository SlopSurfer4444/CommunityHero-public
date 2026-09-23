//! Exact, durable review receipts for actions requested in a private discussion.
//! Tool arguments select work; only a subsequent persisted operator message can
//! confirm it. This module never constructs an Actor or bypasses the ordinary
//! approval, connector, credential-generation, or execution checks.
use crate::*;
use operator_auth::Actor;
use sha2::{Digest, Sha256};

const REVIEW_SECONDS: i64 = 30 * 60;

fn authority(actor: &Actor) -> String {
    format!("{:x}", Sha256::digest(dispatch_authority::approval_binding(actor).to_string().as_bytes()))
}

fn conversation<'a>(d: &'a Value, actor: &Actor, key: &str) -> ApiResult<&'a Value> {
    let chat = row(d, "conversations", key)?;
    // Legacy ownerless conversations cannot establish private review ownership.
    if chat["operatorId"] != actor.id {
        return Err(ApiError(StatusCode::NOT_FOUND, "Conversation not found".into()));
    }
    Ok(chat)
}

fn user_turn<'a>(chat: &'a Value, message_id: &str) -> ApiResult<(usize, &'a Value)> {
    let messages = chat["messages"].as_array().ok_or_else(|| conflict("Conversation messages unavailable"))?;
    let (index, message) = messages.iter().enumerate().rev().find(|(_, m)| m["role"] == "user")
        .ok_or_else(|| conflict("Operator message required"))?;
    if message["id"] != message_id {
        return Err(conflict("Operator turn changed; review again"));
    }
    Ok((index, message))
}

fn fields(value: &Value, allowed: &[&str]) -> ApiResult<()> {
    if value.as_object().is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str()))) {
        return Err(bad("Unexpected action review arguments"));
    }
    Ok(())
}

/// Called in an app.change transaction, with the authenticated actor and source
/// user message ID captured by the HTTP handler, never supplied by the model.
/// A successful call is terminal for the assistant turn: the complete receipt
/// already is its final message. Do not append a model paraphrase or question.
pub(crate) fn prepare(d: &mut Value, actor: &Actor, conversation_id: &str, user_message_id: &str, args: &Value) -> ApiResult<Value> {
    // Even direct callers receive all-or-nothing proposal and receipt creation.
    let mut next = d.clone();
    let result = prepare_inner(&mut next, actor, conversation_id, user_message_id, args)?;
    *d = next;
    Ok(result)
}

fn prepare_inner(d: &mut Value, actor: &Actor, conversation_id: &str, user_message_id: &str, args: &Value) -> ApiResult<Value> {
    fields(args, &["items", "mode"])?;
    let mode = required(args, "mode")?;
    if !["execute_prepared", "close_without_reply"].contains(&mode) {
        return Err(bad("Choose execute_prepared or close_without_reply; clarify ambiguous requests first"));
    }
    let chat = conversation(d, actor, conversation_id)?;
    let (source_index, _) = user_turn(chat, user_message_id)?;
    let refs = args["items"].as_array().filter(|a| !a.is_empty() && a.len() <= 100)
        .ok_or_else(|| bad("Choose 1 to 100 exact items with revisions"))?;
    let mut seen = std::collections::HashSet::new();
    let mut entries = vec![];
    for selected in refs {
        fields(selected, &["id", "revision", "proposalId", "proposalRevision"])?;
        let key = required(selected, "id")?;
        if !seen.insert(key.to_owned()) { return Err(bad("One action per recipient required")); }
        let item = row(d, "items", key)?.clone();
        check_revision(&item, &selected["revision"])?;
        if mode == "execute_prepared" && item["workflow"] != "prepared" {
            return Err(conflict("Only currently prepared items can enter this review"));
        }
        let explicit = selected["proposalId"].as_str();
        if explicit.is_none() && !selected["proposalRevision"].is_null() {
            return Err(bad("proposalRevision requires proposalId"));
        }
        let saved = if let Some(proposal_id) = explicit {
            let p = row(d, "proposals", proposal_id)?.clone();
            check_revision(&p, &selected["proposalRevision"])?;
            if p["itemId"] != key { return Err(bad("Proposal recipient differs from selected item")); }
            Some(p)
        } else {
            list(d, "proposals").iter().rev().find(|p| p["itemId"] == key).cloned()
        };
        let draft = item["draft"].as_str().unwrap_or("");
        let proposal = if mode == "close_without_reply" {
            // A close never repurposes a reply proposal, even if the model names one.
            if explicit.is_some() && saved.as_ref().is_some_and(|p| p["kind"] != "close") {
                return Err(bad("Close review cannot select a reply proposal"));
            }
            if let Some(p) = saved.filter(|p| p["kind"] == "close" && matches!(p["status"].as_str(), Some("draft" | "approved"))) {
                p
            } else {
                create_proposal(d, &json!({"itemId":key,"expectedRevision":item["revision"],"kind":"close","text":""}))?
            }
        } else if explicit.is_none() && !draft.trim().is_empty()
            && saved.as_ref().is_none_or(|p| p["kind"] != "reply_and_close" || p["text"] != draft) {
            // Reuse the human's persisted draft exactly; never let tool args write it.
            create_proposal(d, &json!({"itemId":key,"expectedRevision":item["revision"],"kind":"reply_and_close","text":draft}))?
        } else {
            saved.ok_or_else(|| conflict("No prepared proposal or saved draft; prepare this item first"))?
        };
        if !matches!(proposal["kind"].as_str(), Some("reply_and_close" | "close"))
            || !matches!(proposal["status"].as_str(), Some("draft" | "approved")) {
            return Err(conflict("Prepared proposal is unavailable or requires a different review"));
        }
        let current = proposal_current(d, &proposal)?;
        if list(d, "operations").iter().any(|o| recipient_operation_blocks(o, &proposal, &current)) {
            return Err(conflict("Recipient has an unresolved or completed operation"));
        }
        // Store full exact proposal and route/context, so unversioned mutations
        // cannot silently retarget a review either.
        entries.push(json!({"id":proposal["id"],"revision":proposal["revision"],"proposal":proposal,"item":current}));
    }
    let review_id = id();
    let receipt_id = id();
    let has_replies = entries.iter().any(|e| e["proposal"]["kind"] == "reply_and_close");
    let mut text = format!("Проверка перед выполнением: {} комментариев. Пока ничего не отправлено и не закрыто.\n", entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let item = &entry["item"];
        let proposal = &entry["proposal"];
        let author = item["author"].as_str().unwrap_or("Автор не указан");
        text.push_str(&format!("\n{}. Получатель: {} (комментарий {}).\nКомментарий: {}\n", index + 1, author,
            item["id"].as_str().unwrap_or(""), item["text"].as_str().unwrap_or("")));
        if proposal["kind"] == "reply_and_close" {
            text.push_str("Действие: отправить ответ и закрыть. Точный текст ответа:\n");
            text.push_str(proposal["text"].as_str().unwrap_or(""));
            text.push('\n');
        } else { text.push_str("Действие: закрыть без ответа. Ответ не будет отправлен.\n"); }
    }
    text.push_str("\nПроверьте всех получателей и точные тексты. Чтобы выполнить именно этот пакет, следующим сообщением напишите «Да, выполняй». Любые изменения потребуют новой проверки.");
    if text.len() > 550_000 { return Err(bad("Review is too large to present completely; choose fewer items")); }
    let review = json!({"id":review_id,"status":"presented","operatorId":actor.id,"authorityDigest":authority(actor),
        "conversationId":conversation_id,"sourceUserMessageId":user_message_id,"sourceUserIndex":source_index,
        "receiptMessageId":receipt_id,"receiptText":text,"mode":mode,"hasReplies":has_replies,"proposals":entries,
        "createdAt":now(),"expiresAt":chrono::Utc::now().timestamp()+REVIEW_SECONDS});
    let chat = row_mut(d, "conversations", conversation_id)?;
    if !chat["actionReviews"].is_array() { chat["actionReviews"] = json!([]); }
    for prior in chat["actionReviews"].as_array_mut().unwrap() {
        if prior["status"] == "presented" { prior["status"] = json!("superseded"); }
    }
    chat["actionReviews"].as_array_mut().unwrap().push(review.clone());
    chat["messages"].as_array_mut().unwrap().push(json!({"id":receipt_id,"role":"assistant","text":text,
        "createdAt":now(),"actionReview":{"reviewId":review_id,"mode":mode,"proposals":entries},"serverActionReview":true}));
    audit(d, "assistant.review_presented", &review_id);
    Ok(json!({"reviewId":review_id,"status":"awaiting_confirmation","terminal":true,"receiptMessageId":receipt_id,"text":text,"proposals":entries}))
}

fn affirmative(text: &str, has_replies: bool) -> bool {
    let value = text.trim().to_lowercase();
    let normalized = value.trim_end_matches(['.', '!']).trim();
    matches!(normalized, "да, выполняй" | "да выполняй" | "подтверждаю, выполняй" | "подтверждаю выполняй" |
        "подтверждаю выполнение" | "выполняй" | "yes, proceed" | "confirm execution")
        || (has_replies && matches!(normalized, "да, отправляй" | "да отправляй" | "подтверждаю, отправляй" | "подтверждаю отправляй"))
        || (!has_replies && matches!(normalized, "да, закрывай" | "да закрывай" | "подтверждаю, закрывай"))
}

fn confirmation(d: &Value, actor: &Actor, conversation_id: &str, user_message_id: &str, review_id: &str) -> ApiResult<Value> {
    let chat = conversation(d, actor, conversation_id)?;
    let review = chat["actionReviews"].as_array().and_then(|a| a.last())
        .filter(|r| r["id"] == review_id && r["status"] == "presented")
        .ok_or_else(|| conflict("Review is missing, superseded or already consumed"))?;
    if review["operatorId"] != actor.id || review["conversationId"] != conversation_id
        || review["authorityDigest"] != authority(actor)
        || review["expiresAt"].as_i64().is_none_or(|t| t <= chrono::Utc::now().timestamp()) {
        return Err(conflict("Review ownership, authority or lifetime changed; review again"));
    }
    let (user_index, user) = user_turn(chat, user_message_id)?;
    let messages = chat["messages"].as_array().unwrap();
    let preceding = user_index.checked_sub(1).and_then(|i| messages.get(i))
        .ok_or_else(|| conflict("Confirm in a later message after reading the complete review"))?;
    if review["sourceUserMessageId"] == user_message_id || user_index + 1 != messages.len()
        || preceding["id"] != review["receiptMessageId"] || preceding["role"] != "assistant"
        || preceding["serverActionReview"] != true || preceding["text"] != review["receiptText"]
        || preceding["actionReview"]["proposals"] != review["proposals"]
        || !affirmative(user["text"].as_str().unwrap_or(""), review["hasReplies"] == true) {
        return Err(conflict("Confirmation must immediately follow the exact review; say Да, выполняй or request a new review"));
    }
    for entry in review["proposals"].as_array().ok_or_else(|| conflict("Review content missing"))? {
        let p = row(d, "proposals", required(entry, "id")?)?;
        check_revision(p, &entry["revision"])?;
        if p != &entry["proposal"] || proposal_current(d, p)? != entry["item"] {
            return Err(conflict("Reviewed proposal or recipient changed; review again"));
        }
    }
    Ok(review.clone())
}

/// Resolve a clear confirmation from actual conversation state before invoking
/// the model. Other natural-language requests continue through the model loop.
pub(crate) fn pending_confirmation(d: &Value, actor: &Actor, conversation_id: &str, user_message_id: &str) -> ApiResult<Option<Value>> {
    let chat = conversation(d, actor, conversation_id)?;
    let (_, message) = user_turn(chat, user_message_id)?;
    let Some(review) = chat["actionReviews"].as_array().and_then(|a| a.last()) else { return Ok(None); };
    if !affirmative(message["text"].as_str().unwrap_or(""), review["hasReplies"] == true) { return Ok(None); }
    let review_id = required(review, "id")?;
    confirmation(d, actor, conversation_id, user_message_id, review_id)?;
    Ok(Some(json!({"reviewId":review_id})))
}

fn mark(d: &mut Value, conversation_id: &str, review_id: &str, status: &str, extra: Value) -> ApiResult<()> {
    let reviews = row_mut(d, "conversations", conversation_id)?["actionReviews"].as_array_mut()
        .ok_or_else(|| conflict("Review missing"))?;
    let review = reviews.iter_mut().find(|r| r["id"] == review_id).ok_or_else(|| conflict("Review missing"))?;
    let changed = review["status"] != status || extra.as_object().is_some_and(|extra|
        extra.iter().any(|(key, value)| review["execution"][key] != *value));
    review["status"] = json!(status);
    // Attempt identity survives every transition, including uncertain errors.
    if !review["execution"].is_object() { review["execution"] = json!({}); }
    for (key, value) in extra.as_object().ok_or_else(|| bad("Execution metadata required"))? {
        review["execution"][key] = value.clone();
    }
    if changed { review["updatedAt"] = json!(now()); }
    Ok(())
}

/// A single-use claim is committed before approval/dispatch. A crash after that
/// point requires reconciliation, never automatic replay. The existing execute
/// path still checks current revisions, actor and authority before admission.
pub(crate) async fn execute_review(app: App, actor: Actor, conversation_id: &str, user_message_id: &str, args: &Value) -> ApiResult<Value> {
    execute_review_inner(app, actor, conversation_id, user_message_id, args, |_| Ok(())).await
}

// The synchronous checkpoint seam lets tests interrupt each committed boundary
// without timing races or a provider. Production always uses the no-op above.
async fn execute_review_inner(app: App, actor: Actor, conversation_id: &str, user_message_id: &str, args: &Value,
    checkpoint: impl Fn(&str) -> ApiResult<()>) -> ApiResult<Value> {
    fields(args, &["reviewId"])?;
    let review_id = required(args, "reviewId")?.to_owned();
    app.check_execution()?;
    let approval_id = app.change(|d| {
        let review = confirmation(d, &actor, conversation_id, user_message_id, &review_id)?;
        let refs: Vec<Value> = review["proposals"].as_array().unwrap().iter()
            .map(|e| json!({"id":e["id"],"revision":e["revision"]})).collect();
        let approval = create_approval(d, &actor, &json!({"proposals":refs}))?;
        let approval_id = required(&approval, "id")?.to_owned();
        let binding = json!({"attemptId":id(),"reviewId":review_id,"conversationId":conversation_id,
            "confirmationUserMessageId":user_message_id,"operatorId":actor.id,"authorityDigest":authority(&actor)});
        row_mut(d, "approvals", &approval_id)?["assistantReview"] = binding.clone();
        let mut execution = binding;
        execution["approvalId"] = json!(approval_id);
        execution["claimedAt"] = json!(now());
        mark(d, conversation_id, &review_id, "claimed", execution)?;
        audit(d, "assistant.review_confirmed", &review_id);
        checkpoint("before_claim_commit")?;
        Ok(approval_id)
    }).await?;
    checkpoint("claimed")?;
    app.change(|d| {
        mark(d, conversation_id, &review_id, "admitting", json!({"approvalId":approval_id,"confirmationUserMessageId":user_message_id}))?;
        checkpoint("before_admitting_commit")
    }).await?;
    checkpoint("admitting")?;
    match crate::execute(State(app.clone()), axum::Extension(actor), Path(approval_id.clone())).await {
        Ok(Json(outcome)) => {
            checkpoint("execution_admitted")?;
            let mut result = json!({"reviewId":review_id,"approvalId":approval_id,"jobId":outcome["jobId"],"status":"admitted","externalOutcome":"pending"});
            if app.change(|d| {
                mark(d, conversation_id, &review_id, "admitted", result.clone())?;
                checkpoint("before_admitted_commit")
            }).await.is_err() {
                // execute already proved admission. A receipt-storage failure
                // cannot turn that evidence into "not started" for the caller.
                result["resultReadbackPending"] = json!(true);
            }
            checkpoint("admitted")?;
            Ok(result)
        }
        Err(error) => {
            // A transport/storage error is not proof that admission did not
            // commit. Reconcile durable state; never turn it into a retry.
            app.change(|d| {
                mark(d, conversation_id, &review_id, "admitting", json!({"stage":"execution_admission","reason":error.1}))?;
                reconcile_review(d, conversation_id, &review_id, false)?;
                persist_execution_summary(d, conversation_id, &review_id, false)
            }).await
        }
    }
}

fn recovery_binding(d: &Value, chat: &Value, review: &Value) -> ApiResult<Value> {
    if chat["operatorId"].as_str().is_none() || chat["operatorId"] != review["operatorId"]
        || review["conversationId"] != chat["id"] {
        return Err(conflict("Private review ownership changed"));
    }
    let execution = &review["execution"];
    let confirmation_id = required(execution, "confirmationUserMessageId")?;
    let messages = chat["messages"].as_array().ok_or_else(|| conflict("Review messages missing"))?;
    let index = messages.iter().position(|m| m["id"] == confirmation_id && m["role"] == "user")
        .ok_or_else(|| conflict("Confirmation evidence missing"))?;
    let receipt = index.checked_sub(1).and_then(|i| messages.get(i))
        .ok_or_else(|| conflict("Receipt evidence missing"))?;
    if receipt["id"] != review["receiptMessageId"] || receipt["role"] != "assistant"
        || receipt["serverActionReview"] != true || receipt["text"] != review["receiptText"]
        || receipt["actionReview"]["proposals"] != review["proposals"]
        || review["sourceUserMessageId"] == confirmation_id
        || !affirmative(messages[index]["text"].as_str().unwrap_or(""), review["hasReplies"] == true) {
        return Err(conflict("Confirmation evidence changed"));
    }
    let approval = row(d, "approvals", required(execution, "approvalId")?)?;
    let digest = format!("{:x}", Sha256::digest(approval["approvalAuthority"].to_string().as_bytes()));
    if approval["approvedBy"]["id"] != review["operatorId"] || review["authorityDigest"] != digest
        || approval["proposals"] != review["proposals"] {
        return Err(conflict("Review approval authority or exact context changed"));
    }
    if execution["attemptId"].is_string() {
        let expected = json!({"attemptId":execution["attemptId"],"reviewId":review["id"],
            "conversationId":chat["id"],"confirmationUserMessageId":confirmation_id,
            "operatorId":review["operatorId"],"authorityDigest":review["authorityDigest"]});
        if approval["assistantReview"] != expected {
            return Err(conflict("Review attempt binding changed"));
        }
    } else if !approval["assistantReview"].is_null() {
        return Err(conflict("Legacy review cannot claim a different bound attempt"));
    }
    Ok(approval.clone())
}

fn reconcile_review(d: &mut Value, conversation_id: &str, review_id: &str, abandoned: bool) -> ApiResult<()> {
    let chat = row(d, "conversations", conversation_id)?;
    let review = chat["actionReviews"].as_array().into_iter().flatten().find(|r| r["id"] == review_id)
        .ok_or_else(|| conflict("Review missing"))?.clone();
    // Historical admitted receipts already have their durable job identity.
    if review["status"] == "admitted" && review["execution"]["attemptId"].is_null() {
        persist_execution_summary(d, conversation_id, review_id, false)?;
        return Ok(());
    }
    let proof = recovery_binding(d, chat, &review);
    let (status, extra) = match proof {
        Err(error) => ("recovery_required", json!({"recoveryReason":error.1,"externalOutcome":"unknown"})),
        Ok(approval) => {
            let jobs: Vec<&Value> = list(d, "jobs").iter().filter(|j| j["kind"] == "execute" && j["refId"] == approval["id"]).collect();
            let ops: Vec<&Value> = list(d, "operations").iter().filter(|o| o["approvalId"] == approval["id"]).collect();
            let entries = review["proposals"].as_array().ok_or_else(|| conflict("Review proposals missing"))?;
            let exact_ops = ops.len() == entries.len() && entries.iter().all(|entry|
                ops.iter().filter(|op| op["proposalId"] == entry["id"] && op["itemId"] == entry["proposal"]["itemId"]
                    && op["target"] == entry["item"] && op["approvedBy"] == approval["approvedBy"]
                    && op["dispatchAuthority"]["approved"] == approval["approvalAuthority"]
                    && op["dispatchAuthority"]["executed"] == approval["approvalAuthority"]
                    && op["id"].as_str().is_some_and(|id| action_for(&entry["proposal"], &entry["item"], id)
                        .is_ok_and(|action| action == op["action"])) ).count() == 1);
            if jobs.len() == 1 && exact_ops && approval["status"] == "consumed"
                && (review["execution"]["jobId"].is_null() || review["execution"]["jobId"] == jobs[0]["id"]) {
                ("admitted", json!({"jobId":jobs[0]["id"],"externalOutcome":"pending","recoveryReason":null}))
            } else if jobs.is_empty() && ops.is_empty() && approval["status"] == "approved" && abandoned {
                ("not_started", json!({"externalOutcome":"not_started","recoveryReason":"Approval exists; no execution admission committed"}))
            } else {
                ("recovery_required", json!({"externalOutcome":"unknown","recoveryReason":"Execution admission is absent or inconsistent; no replay performed"}))
            }
        }
    };
    mark(d, conversation_id, review_id, status, extra)?;
    persist_execution_summary(d, conversation_id, review_id, false)?;
    Ok(())
}

/// Startup only: caller has excluded active admission/dispatch and recovered
/// interrupted jobs. No approval, job, operation or provider work is created.
pub(crate) fn recover_execution_receipts(d: &mut Value) -> ApiResult<()> {
    let mut reviews = vec![];
    for chat in list(d, "conversations") {
        for review in chat["actionReviews"].as_array().into_iter().flatten().filter(|r|
            matches!(r["status"].as_str(), Some("claimed" | "admitting" | "admitted" | "failed" | "not_started" | "recovery_required"))) {
            reviews.push((required(chat, "id")?.to_owned(), required(review, "id")?.to_owned()));
        }
    }
    for (chat, review) in reviews { reconcile_review(d, &chat, &review, true)?; }
    Ok(())
}

fn execution_summary(d: &Value, review: &Value, timed_out: bool) -> ApiResult<Value> {
    if matches!(review["status"].as_str(), Some("not_started" | "recovery_required")) {
        let not_started = review["status"] == "not_started";
        return Ok(json!({"reviewId":review["id"],"attemptId":review["execution"]["attemptId"],
            "status":"settled","externalOutcome":if not_started {"not_started"} else {"unknown"},
            "terminal":true,"timedOut":false,"requiresReadback":!not_started,"operations":[],
            "text":if not_started {"Выполнение не началось: подтверждение сохранено, но операции не были приняты. Повторный запуск не выполнялся; для нового выполнения нужна новая проверка."}
                else {"Исход приёма пакета неизвестен. Требуется сверка сохранённых подтверждений и операций. Повторная отправка не запускалась."}}));
    }
    let execution = &review["execution"];
    let approval_id = required(execution, "approvalId")?;
    let job_id = required(execution, "jobId")?;
    let job = row(d, "jobs", job_id)?;
    if job["kind"] != "execute" || job["refId"] != approval_id {
        return Err(conflict("Review execution job binding changed"));
    }
    let expected = review["proposals"].as_array().ok_or_else(|| conflict("Review proposals unavailable"))?.len();
    let mut counts = json!({"succeeded":0,"failed":0,"stale":0,"unknown":0,"pending":0});
    let mut operations = vec![];
    for op in list(d, "operations").iter().filter(|op| op["approvalId"] == approval_id) {
        let status = op["status"].as_str().unwrap_or("unknown");
        let bucket = match status { "succeeded"|"failed"|"stale"|"unknown" => status, _ => "pending" };
        counts[bucket] = json!(counts[bucket].as_u64().unwrap() + 1);
        operations.push(json!({"id":op["id"],"itemId":op["itemId"],"proposalId":op["proposalId"],"status":status,
            "navigation":{"kind":"comment","itemId":op["itemId"]}}));
    }
    let missing = expected.saturating_sub(operations.len());
    counts["pending"] = json!(counts["pending"].as_u64().unwrap() + missing as u64);
    let job_terminal = matches!(job["status"].as_str(), Some("completed"|"failed"|"cancelled"|"interrupted"));
    let pending = counts["pending"].as_u64().unwrap();
    let succeeded = counts["succeeded"].as_u64().unwrap();
    let unknown = counts["unknown"].as_u64().unwrap();
    let terminal = job_terminal || (pending == 0 && operations.len() == expected);
    let external_outcome = if !terminal { "pending" }
        else if succeeded == expected as u64 && expected > 0 { "succeeded" }
        else if succeeded > 0 { "partial" }
        else if unknown > 0 || pending > 0 { "unknown" }
        else { "not_completed" };
    let heading = match external_outcome {
        "succeeded" => "Пакет выполнен: результат всех операций подтверждён.",
        "partial" => "Пакет выполнен частично.",
        "unknown" => "Выполнение завершилось без подтверждения всех результатов. Неизвестный исход требует сверки.",
        "not_completed" => "Выполнение завершено; успешных операций нет.",
        _ if timed_out => "Пакет ещё выполняется. Время ожидания ответа истекло; операции продолжаются, повторная отправка не запускалась.",
        _ => "Пакет принят к выполнению. Ожидаю фактические результаты операций.",
    };
    let mut text = format!("{}\nПодтверждено: {}. Ошибки: {}. Устарело или отклонено: {}. Неизвестный исход: {}. Ожидают результата: {}.",
        heading, succeeded, counts["failed"], counts["stale"], unknown, pending);
    if unknown > 0 || (job_terminal && pending > 0) { text.push_str("\nНе повторяйте отправку до сверки неизвестных исходов."); }
    text.push_str("\nПодробности доступны в истории операций.");
    for op in &operations {
        text.push_str(&format!("\nКомментарий {} · операция {} · {}", op["itemId"].as_str().unwrap_or(""),
            op["id"].as_str().unwrap_or(""), match op["status"].as_str().unwrap_or("") {
                "succeeded"=>"результат подтверждён", "failed"=>"ошибка", "stale"=>"устарело или отклонено",
                "unknown"=>"неизвестный исход", _=>"ожидает результата" }));
    }
    Ok(json!({"reviewId":review["id"],"approvalId":approval_id,"jobId":job_id,"jobStatus":job["status"],
        "status":if terminal{"settled"}else{"admitted"},"externalOutcome":external_outcome,"terminal":terminal,
        "timedOut":timed_out&&!terminal,"counts":counts,"expectedCount":expected,"operations":operations,"text":text,
        "requiresReadback":unknown>0||(job_terminal&&pending>0)}))
}

fn persist_execution_summary(d: &mut Value, conversation_id: &str, review_id: &str, timed_out: bool) -> ApiResult<Value> {
    let chat = row(d, "conversations", conversation_id)?;
    let review = chat["actionReviews"].as_array().and_then(|a|a.iter().find(|r|r["id"]==review_id))
        .filter(|r|matches!(r["status"].as_str(),Some("admitted"|"not_started"|"recovery_required")))
        .ok_or_else(||conflict("Consumed review not found"))?.clone();
    let mut summary = execution_summary(d,&review,timed_out)?;
    // Reuse a receipt written by an earlier engine version, or a deterministic
    // ID, so waiting, timeout, completion and reconciliation never duplicate it.
    let message_id = chat["messages"].as_array().into_iter().flatten()
        .find(|m|m["actionExecution"]["reviewId"]==review_id)
        .and_then(|m|m["id"].as_str()).map(str::to_owned).unwrap_or_else(||format!("execution:{review_id}"));
    summary["receiptMessageId"] = json!(message_id);
    let chat = row_mut(d,"conversations",conversation_id)?;
    let messages = chat["messages"].as_array_mut().ok_or_else(||conflict("Messages unavailable"))?;
    if let Some(message) = messages.iter_mut().find(|m|m["id"]==message_id) {
        if message["actionExecution"] != summary {
            message["text"] = summary["text"].clone();
            message["actionExecution"] = summary.clone();
            message["updatedAt"] = json!(now());
        }
    } else {
        messages.push(json!({"id":message_id,"role":"assistant","text":summary["text"],"createdAt":now(),
            "serverActionExecution":true,"actionExecution":summary}));
    }
    let review = chat["actionReviews"].as_array_mut().unwrap().iter_mut().find(|r|r["id"]==review_id).unwrap();
    review["outcome"] = summary.clone();
    review["resultMessageId"] = json!(message_id);
    Ok(summary)
}

/// Wait only for the already-admitted durable job and operation records. No
/// provider read, model call, approval or resend occurs here. A pending receipt
/// is persisted immediately, then updated on completion or bounded timeout.
pub(crate) async fn await_review_result(app: App, actor: &Actor, conversation_id: &str, review_id: &str, timeout: std::time::Duration) -> ApiResult<Value> {
    let timeout = timeout.min(std::time::Duration::from_secs(45));
    let deadline = tokio::time::Instant::now() + timeout;
    let initial = app.change(|d| {
        conversation(d,actor,conversation_id)?;
        persist_execution_summary(d,conversation_id,review_id,false)
    }).await?;
    if initial["terminal"] == true { return Ok(initial); }
    loop {
        if tokio::time::Instant::now() >= deadline {
            return app.change(|d|persist_execution_summary(d,conversation_id,review_id,true)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let snapshot = app.read().await?;
        let chat = conversation(&snapshot,actor,conversation_id)?;
        let review = chat["actionReviews"].as_array().and_then(|a|a.iter().find(|r|r["id"]==review_id))
            .ok_or_else(||conflict("Review missing"))?;
        let settled = execution_summary(&snapshot,review,false)?["terminal"] == true;
        drop(snapshot);
        if settled { return app.change(|d|persist_execution_summary(d,conversation_id,review_id,false)).await; }
    }
}

/// Called after durable execute/reconcile job finalization, in a full workspace
/// transaction (not the jobs-only transaction). Keeps timeout receipts current
/// after browser refresh; never creates or repeats provider work.
pub(crate) fn refresh_execution_receipts(d: &mut Value, job_id: &str) -> ApiResult<()> {
    let job = row(d,"jobs",job_id)?;
    let approval_id = match job["kind"].as_str() {
        Some("execute") => job["refId"].as_str().map(str::to_owned),
        Some("reconcile") => list(d,"operations").iter().find(|o|o["id"]==job["refId"])
            .and_then(|o|o["approvalId"].as_str()).map(str::to_owned),
        _ => return Ok(()),
    };
    let Some(approval_id) = approval_id else { return Ok(()); };
    let mut affected = vec![];
    for chat in list(d,"conversations") {
        for review in chat["actionReviews"].as_array().into_iter().flatten()
            .filter(|r|matches!(r["status"].as_str(),Some("claimed"|"admitting"|"admitted"|"failed"|"recovery_required"))
                &&r["execution"]["approvalId"]==approval_id) {
            affected.push((required(chat,"id")?.to_owned(),required(review,"id")?.to_owned()));
        }
    }
    for (conversation_id,review_id) in affected {
        reconcile_review(d,&conversation_id,&review_id,false)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "assistant_action_review_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "assistant_action_review_recovery_tests.rs"]
mod recovery_tests;
