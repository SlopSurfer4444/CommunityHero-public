//! Dispatch outcomes and content-free explanations. None grants retry authority.
use crate::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome { Succeeded, Failed, Stale, Unknown }
impl Outcome {
    pub(crate) fn status(self)->&'static str {match self {
        Self::Succeeded=>"succeeded",Self::Failed=>"failed",Self::Stale=>"stale",Self::Unknown=>"unknown"
    }}
}

pub(crate) struct Stop {pub outcome:Outcome,pub evidence:Value}
pub(crate) fn stop(outcome:Outcome,code:&'static str)->Stop {
    Stop {outcome,evidence:json!({"phase":"predispatch","code":code,
        "providerCallAttempted":false,"mutationOutcome":"not-attempted","providerRetryAllowed":false})}
}
pub(crate) fn read_failure(code:&'static str,error:&ApiError)->Stop {
    let mut failure=stop(Outcome::Failed,code);
    // httpStatus is the engine API status. The bridge's normalized provider
    // status is a different observation and must never be inferred from it.
    failure.evidence["diagnostic"]=json!({"httpStatus":error.0.as_u16(),"nativeHttpStatus":error.0.as_u16()});
    // Errors may contain private transport text. Only the closed adapter-code
    // slot emitted by our bridge formatter is eligible for retention.
    if let Some(code)=error.1.strip_prefix("Adapter failed (").and_then(|s|s.split([';',')']).next())
        .filter(|s|!s.is_empty()&&s.len()<=80&&s.bytes().all(|b|b.is_ascii_uppercase()||b.is_ascii_digit()||b==b'_')) {
        failure.evidence["diagnostic"]["adapterCode"]=json!(code);
    }
    if matches!(failure.evidence["diagnostic"]["adapterCode"].as_str(),Some("HTTP_ERROR"|"TRANSPORT_ERROR")) {
        if let Some(fields)=error.1.strip_prefix("Adapter failed (")
            .and_then(|s|s.strip_suffix(')')).filter(|s|s.len()<=2048) {
            let stages:Vec<_>=fields.split("; ").skip(1)
                .filter_map(|field|field.strip_prefix("transportStage=")).collect();
            if let [stage]=stages.as_slice() {
                if dispatch_evidence::safe_transport_stage(stage) {
                    failure.evidence["diagnostic"]["transportStage"]=json!(stage);
                }
            }
        }
    }
    if failure.evidence["diagnostic"]["adapterCode"]=="TRANSPORT_ERROR"
        && failure.evidence["diagnostic"]["transportStage"]=="read-fetch" {
        if let Some(fields)=error.1.strip_prefix("Adapter failed (")
            .and_then(|s|s.strip_suffix(')')).filter(|s|s.len()<=2048) {
            let causes:Vec<_>=fields.split("; ").skip(1)
                .filter_map(|field|field.strip_prefix("transportCause=")).collect();
            if let [cause]=causes.as_slice() {
                if dispatch_evidence::safe_transport_cause(cause) {
                    failure.evidence["diagnostic"]["transportCause"]=json!(cause);
                }
            }
        }
    }
    if failure.evidence["diagnostic"]["adapterCode"]=="HTTP_ERROR" {
        // Only consume the fixed formatter's bounded slot, never transport
        // text, response bodies, URLs or arbitrary exception messages.
        if let Some(fields)=error.1.strip_prefix("Adapter failed (")
            .and_then(|s|s.strip_suffix(')')).filter(|s|s.len()<=2048) {
            let statuses:Vec<_>=fields.split("; ").skip(1)
                .filter_map(|field|field.strip_prefix("httpStatus=")).collect();
            if let [status]=statuses.as_slice() {
                if status.len()==3 && status.bytes().all(|b|b.is_ascii_digit()) {
                    if let Ok(status)=status.parse::<u16>() {
                        if (100..=599).contains(&status) {
                            failure.evidence["diagnostic"]["upstreamHttpStatus"]=json!(status);
                        }
                    }
                }
            }
        }
    }
    if failure.evidence["diagnostic"]["adapterCode"]=="HTTP_ERROR"
        && failure.evidence["diagnostic"]["transportStage"].as_str().is_some_and(dispatch_evidence::oauth_refresh_stage) {
        if let Some(fields)=error.1.strip_prefix("Adapter failed (").and_then(|s|s.strip_suffix(')')).filter(|s|s.len()<=2048) {
            let mut candidate=json!({"version":1});let mut duplicate=false;
            for (slot,key) in dispatch_evidence::OAUTH_SLOTS {
                let prefix=format!("{slot}=");
                let values:Vec<_>=fields.split("; ").skip(1).filter_map(|field|field.strip_prefix(&prefix)).collect();
                if values.len()>1 {duplicate=true;break;}
                if let [value]=values.as_slice() {
                    if key=="generation" {
                        if let Ok(n)=value.parse::<u64>() {if n.to_string()==*value {candidate[key]=json!(n);}}
                    } else {candidate[key]=json!(value);}
                }
            }
            if !duplicate {
                if let Some(safe)=dispatch_evidence::safe_oauth_diagnostic(&candidate) {
                    if safe["classification"]==candidate["classification"] {failure.evidence["diagnostic"]["oauthDiagnostic"]=safe;}
                }
            }
        }
    }

    if matches!(failure.evidence["diagnostic"]["adapterCode"].as_str(),Some("HTTP_ERROR"|"TRANSPORT_ERROR")) {
        if let Some(fields)=error.1.strip_prefix("Adapter failed (").and_then(|s|s.strip_suffix(')')).filter(|s|s.len()<=2048) {
            if failure.evidence["diagnostic"]["transportStage"].as_str().is_some_and(dispatch_evidence::auth_exchange_stage) {
                let mut candidate=json!({"version":1});let mut invalid=false;
                for (slot,key) in dispatch_evidence::AUTH_EXCHANGE_SLOTS {
                    let prefix=format!("{slot}=");
                    let values:Vec<_>=fields.split("; ").skip(1).filter_map(|field|field.strip_prefix(&prefix)).collect();
                    if values.len()>1 {invalid=true;break;}
                    if let [value]=values.as_slice() {
                        if matches!(key,"watchdogFired"|"elapsedMs"|"deadlineMs"|"responseReceived"|"httpStatusValid"|"httpStatus"|"generation") {
                            match serde_json::from_str::<Value>(value) {
                                Ok(n) if n.is_number()||n.is_boolean()=>candidate[key]=n,
                                _=>{invalid=true;break;}
                            }
                        } else {candidate[key]=json!(value);}
                    }
                }
                if !invalid {
                    if let Some(d)=dispatch_evidence::safe_auth_exchange_diagnostic(&candidate) {
                        failure.evidence["diagnostic"]["authRequestAttempted"]=json!(d["observation"]=="originating_exchange");
                        failure.evidence["diagnostic"]["authExchangeDiagnostic"]=d;
                    }
                }
            }
            if failure.evidence["diagnostic"]["transportStage"].as_str().is_some_and(|s|s.starts_with("read-auth-")) {
                let mut state=json!({});let mut duplicate=false;
                for (slot,key) in [("authConnectionStatus","status"),("authConnectionReason","reason")] {
                    let prefix=format!("{slot}=");
                    let values:Vec<_>=fields.split("; ").skip(1).filter_map(|field|field.strip_prefix(&prefix)).collect();
                    if values.len()>1 {duplicate=true;break;}
                    if let [value]=values.as_slice() {state[key]=json!(value);}
                }
                if !duplicate {
                    if let Some(state)=dispatch_evidence::safe_connection_state(&state) {failure.evidence["diagnostic"]["connectionState"]=state;}
                }
            }
        }
    }
    failure
}
fn changes(failure:&mut Stop,expected:&Value,observed:&Value,keys:&[&str]) {
    // Hash only the named, bounded structural fields; never comments/replies.
    let fingerprint=|value:&Value| {
        let fields:Vec<_>=keys.iter().map(|key|(*key,match &value[*key] {
            Value::String(s) if s.len()<=2048=>json!(s),Value::Number(n)=>json!(n),_=>Value::Null
        })).collect();
        format!("{:x}",Sha256::digest(serde_json::to_vec(&fields).unwrap()))
    };
    failure.evidence["mismatchedFields"]=json!(keys);
    failure.evidence["expectedFingerprint"]=json!(fingerprint(expected));
    failure.evidence["observedFingerprint"]=json!(fingerprint(observed));
}
pub(crate) fn context_check(context:&Value,target:&Value)->Result<(),Stop> {
    const IDENTITY:[&str;4]=["itemId","objectId","postKey","conversationKey"];
    let invalid:Vec<_>=IDENTITY.iter().copied().chain(["contextEvidenceDigest"])
        .filter(|key|!context[*key].is_string()||!target[*key].is_string()).collect();
    if !invalid.is_empty() {
        let mut failure=stop(Outcome::Failed,"fresh_context_schema_invalid");
        changes(&mut failure,target,context,&invalid);return Err(failure);
    }
    let identity:Vec<_>=IDENTITY.iter().copied().filter(|key|context[*key]!=target[*key]).collect();
    if !identity.is_empty() {
        let mut failure=stop(Outcome::Stale,"fresh_context_identity_changed");
        changes(&mut failure,target,context,&identity);return Err(failure);
    }
    if context["contextEvidenceDigest"]!=target["contextEvidenceDigest"] {
        let mut failure=stop(Outcome::Stale,"fresh_context_digest_changed");
        changes(&mut failure,target,context,&["contextEvidenceDigest"]);return Err(failure);
    }
    Ok(())
}
// These labels identify the first failed validator boundary, not a guessed
// historical cause. Never parse or persist an arbitrary ApiError message.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) enum LocalPredicate {
    RecoveredPreparation, ReviewSourceUnavailable, ReviewSourceChanged,
    PreparationProvenance, PreparationSources, ConnectorBinding, TargetBinding,
    ReplyConstraints, Route, VideoEvidence, ProposalItemContext, ItemAlreadyClosed,
}
impl LocalPredicate {
    fn code(self)->&'static str {match self {
        Self::RecoveredPreparation=>"recovered_preparation",
        Self::ReviewSourceUnavailable=>"review_source_unavailable",
        Self::ReviewSourceChanged=>"review_source_changed",
        Self::PreparationProvenance=>"preparation_provenance",
        Self::PreparationSources=>"preparation_sources",
        Self::ConnectorBinding=>"connector_binding",
        Self::TargetBinding=>"target_binding",
        Self::ReplyConstraints=>"reply_constraints",
        Self::Route=>"route",
        Self::VideoEvidence=>"video_evidence",
        Self::ProposalItemContext=>"proposal_item_context",
        Self::ItemAlreadyClosed=>"item_already_closed",
    }}
}
pub(crate) struct LocalPreconditionFailure {
    pub(crate) error:ApiError,
    pub(crate) predicate:LocalPredicate,
    pub(crate) review_fingerprints:Option<(String,String)>,
}
pub(crate) fn safe_review_fingerprints(expected:&str,observed:&str)->Option<(String,String)> {
    let valid=|value:&str|value.len()==64 && value.bytes().all(|c|c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
    (valid(expected)&&valid(observed)).then(||(expected.to_owned(),observed.to_owned()))
}
impl LocalPreconditionFailure {
    fn into_stop(self)->Stop {
        if self.error.0.is_server_error() {
            return read_failure("local_validation_unavailable",&self.error);
        }
        let mut failure=stop(Outcome::Stale,"local_precondition_changed");
        failure.evidence["diagnostic"]=json!({"localPredicate":self.predicate.code()});
        if self.predicate==LocalPredicate::ReviewSourceChanged {
            if let Some((expected,observed))=self.review_fingerprints
                .and_then(|(expected,observed)|safe_review_fingerprints(&expected,&observed)) {
                failure.evidence["diagnostic"]["expectedFingerprint"]=json!(expected);
                failure.evidence["diagnostic"]["observedFingerprint"]=json!(observed);
            }
        }
        failure
    }
}

pub(crate) fn local_check(data:&Value,op:&Value)->Result<(),Stop> {
    let proposal=op["proposalId"].as_str().and_then(|id|row(data,"proposals",id).ok())
        .ok_or_else(||stop(Outcome::Stale,"local_proposal_missing"))?;
    let item=proposal["itemId"].as_str().and_then(|id|row(data,"items",id).ok())
        .ok_or_else(||stop(Outcome::Stale,"local_target_missing"))?;
    for (key,code) in [("revision","local_target_revision_changed"),("contextEvidenceDigest","local_target_digest_changed")] {
        if item[key]!=op["target"][key] {
            let mut failure=stop(Outcome::Stale,code);changes(&mut failure,&op["target"],item,&[key]);return Err(failure);
        }
    }
    // Dispatch reads a bounded projection without approval rows. Only an
    // operation admitted with this exact receipt may use newer editorial
    // source evidence; historical operations retain their original source gate.
    let context=prepare_bundle::EvidenceContext::new(data);
    let allow_current_review=match op["approvedEditorialReceiptSha256"].as_str() {
        Some(marker) if proposal["editorialReview"]["receiptSha256"]==marker =>
            editorial_review::dedicated_current(&context,proposal).is_ok(),
        Some(_)=>return Err(stop(Outcome::Stale,"approved_editorial_receipt_changed")),
        None=>false,
    };
    proposal_current_checked_mode(proposal,&context,allow_current_review,Some(op))
        .map_err(LocalPreconditionFailure::into_stop)?;
    if op["editorialPolicyVersion"]==1 && proposal["kind"]=="reply_and_close" {
        editorial_review::require_current(&prepare_bundle::EvidenceContext::new(data),proposal)
            .map_err(|_|stop(Outcome::Stale,"editorial_review_changed"))?;
    }
    Ok(())
}
pub(crate) async fn persist_stop(app:&App,op:&Value,failure:Stop)->ApiResult<Outcome> {
    set_outcome(app,op,failure.outcome.status(),failure.evidence).await?;
    Ok(failure.outcome)
}

/// This is a connector-reported observation, not the engine's final success.
/// Use the same generic exact identity contract as independent readback.
pub(crate) fn retain_execute_observation(op:&Value,evidence:&mut Value) {
    // The diagnostic is server-owned, never accepted from a provider envelope.
    if let Some(fields)=evidence.as_object_mut() {fields.remove("executeObservation");}
    let receipt=&op["executeReceipt"];
    if operation_account(op).ok().is_some_and(|account|
        dispatch_evidence::readback_account_matches(receipt,account))
        && readback_confirmed(receipt,&op["action"]) {
        if !evidence.is_object() {*evidence=json!({"observation":evidence.take()});}
        evidence["executeObservation"]=json!({"status":"verified","source":"executeReceipt",
            "finalSuccessRequiresIndependentReadback":true});
    }
}

pub(crate) fn known_failure_evidence(receipt:Value,op:&Value,account:&str)->Value {
    let catalogue=dispatch_evidence::confirmed_failure(&receipt,&op["action"],account)
        && receipt["results"][0]["mutationOutcome"]=="not-attempted"
        && receipt["results"][0]["phase"]=="catalogue"
        && receipt["results"][0]["operation"]=="adapter-catalogue";
    let mut evidence=json!({"receipt":receipt,"providerRetryAllowed":false});
    if catalogue {
        evidence["recovery"]=json!({"classification":"fresh_review_required",
            "requiresFreshContextAndApproval":true,"newOperationOnly":true,"sameOperationRetryAllowed":false});
    }
    evidence
}

#[cfg(test)]
#[path="dispatch_diagnostics_tests.rs"]
mod tests;

#[cfg(test)] mod auth_exchange_roundtrip_tests {
    use super::*;
    fn fixture(observation:&str)->Value {json!({"version":1,"diagnosticId":"00000000-0000-4000-8000-000000000001",
        "observation":observation,"stage":"fetch","cause":"connect_timeout","watchdogFired":false,
        "elapsedMs":7.125,"deadlineMs":15000,"responseReceived":false,"httpStatusValid":false,
        "responseDisposal":"not_requested","trigger":"proactive","generation":4})}
    fn error(diagnostic:Value)->Value {json!({"code":"TRANSPORT_ERROR","transportStage":"read-auth-proactive-refresh-fetch",
        "adapterOperation":"context","authExchangeDiagnostic":diagnostic,
        "connectionState":{"status":"needs_user","reason":"recovery_uncertain","secret":"PRIVATE"},"message":"PRIVATE"})}
    #[test] fn originating_and_barrier_roundtrip_keep_request_facts_distinct_from_native_status() {
        for observation in ["originating_exchange","durable_barrier"] {
            let original=fixture(observation);
            let message=dispatch_evidence::adapter_failure(&error(original.clone()));
            assert!(message.len()<=2048);assert!(!message.contains("PRIVATE"));
            let evidence=read_failure("context",&internal(&message)).evidence;
            assert_eq!(evidence["diagnostic"]["authExchangeDiagnostic"],original);
            assert_eq!(evidence["diagnostic"]["nativeHttpStatus"],500);
            assert!(evidence["diagnostic"].get("upstreamHttpStatus").is_none());
            assert_eq!(evidence["diagnostic"]["authRequestAttempted"],observation=="originating_exchange");
            assert_eq!(evidence["diagnostic"]["connectionState"],json!({"status":"needs_user","reason":"recovery_uncertain"}));
            assert_eq!(evidence["providerCallAttempted"],false);
            assert_eq!(evidence["providerRetryAllowed"],false);
        }
        let message=dispatch_evidence::adapter_failure(&error(fixture("originating_exchange")));
        for bad in [
            message.replace("; authCause=connect_timeout","; authCause=PRIVATE"),
            message.replace("; authElapsedMs=7.125","; authElapsedMs=7.125; authElapsedMs=7.125"),
            message.replace("; authDiagId=","; authDiagId=PRIVATE"),
            message.replace("read-auth-proactive-refresh-fetch","read-fetch"),
            format!("{}{}",message,"x".repeat(2049))
        ] {
            assert!(read_failure("context",&internal(&bad)).evidence["diagnostic"].get("authExchangeDiagnostic").is_none());
        }
    }
    #[test] fn actual_auth_http_status_and_existing_oauth_slots_survive_combined_projection() {
        let mut d=fixture("originating_exchange");d["stage"]=json!("http");d["cause"]=json!("http_rejection");
        d["responseReceived"]=json!(true);d["httpStatusValid"]=json!(true);d["httpStatus"]=json!(400);d["responseDisposal"]=json!("requested");
        let mut value=error(d.clone());value["code"]=json!("HTTP_ERROR");value["httpStatus"]=json!(400);
        value["transportStage"]=json!("read-auth-proactive-refresh-http");
        value["oauthDiagnostic"]=json!({"version":1,"error":"invalid_grant","responseShape":"json_object","trigger":"proactive","generation":4});
        let message=dispatch_evidence::adapter_failure(&value);
        let evidence=read_failure("context",&internal(&message)).evidence;
        assert_eq!(evidence["diagnostic"]["authExchangeDiagnostic"],d);
        assert_eq!(evidence["diagnostic"]["upstreamHttpStatus"],400);
        assert_eq!(evidence["diagnostic"]["nativeHttpStatus"],500);
        assert_eq!(evidence["diagnostic"]["oauthDiagnostic"]["classification"],"invalid_grant");
    }
    #[tokio::test] async fn auth_exchange_evidence_is_persisted_without_social_attempt_or_retry() {
        let (app,temp)=crate::tests::test_app().await;
        let op=json!({"id":"auth-projection-op","itemId":"item-1","proposalId":"p","status":"pending",
            "target":{"connectorBinding":accounts::Profile::LikeAvto.binding()},"action":{"actionId":"a","action":"close","itemId":"i"}});
        app.change(|data|{data["operations"]=json!([op.clone()]);data["proposals"]=json!([{"id":"p"}]);Ok(())}).await.unwrap();
        let message=dispatch_evidence::adapter_failure(&error(fixture("durable_barrier")));
        let failure=read_failure("fresh_context_read_failed",&internal(&message));
        assert_eq!(persist_stop(&app,&op,failure).await.unwrap(),Outcome::Failed);
        app.db.close().await;
        let db=Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
        let data=db.read().await.unwrap();let saved=&row(&data,"operations","auth-projection-op").unwrap()["evidence"];
        assert_eq!(saved["diagnostic"]["authExchangeDiagnostic"],fixture("durable_barrier"));
        assert_eq!(saved["diagnostic"]["authRequestAttempted"],false);
        assert_eq!(saved["providerCallAttempted"],false);
        assert_eq!(saved["providerRetryAllowed"],false);
        assert_eq!(saved["mutationOutcome"],"not-attempted");db.close().await;
    }
}
