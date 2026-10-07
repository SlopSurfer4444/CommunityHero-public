//! Persist the provider's pre-send reply baseline with the exact operation so
//! restart reconciliation cannot mistake an old equal-text reply for a new one.
use crate::*;

/// Retain bounded failure facts, never adapter output or arbitrary detail.
pub(crate) fn safe_transport_stage(stage:&str)->bool {
    if matches!(stage,"read-fetch"|"read-json"|"read-auth"|"read-http") {return true;}
    let source=stage.strip_prefix("read-auth-proactive-")
        .or_else(||stage.strip_prefix("read-auth-after-401-"));
    source.is_some_and(|source|matches!(source,"token-state"|"credential-helper-startup"|"credential-helper-timeout-before-connect"|"credential-helper-timeout-after-connect"|"credential-helper-transport"|"credential-helper-response-invalid"|"credential-helper-failed"|"credential-missing"|"credential-value-invalid"|"token-envelope-invalid"|"token-freshness-boundary"|"token-lifecycle-blocked"|"refresh-lock"|"refresh-lock-acquire"
        |"refresh-lock-timeout"|"refresh-lock-release"|"oauth-client"|"refresh-fetch"
        |"refresh-http"|"refresh-json"|"refresh-schema"|"token-rotate"))
}
pub(crate) fn safe_transport_cause(cause:&str)->bool {
    matches!(cause,"dns"|"tcp"|"tls"|"connect_timeout"|"timeout"|"abort"|"unknown")
}
fn oauth_category(code:&str)->Option<&'static str> {
    Some(match code {
        "expired"|"token_expired"|"expired_token"|"refresh_token_expired"|"session_expired"=>"expired",
        "revoked"|"token_revoked"|"refresh_token_revoked"=>"revoked",
        "reuse"|"token_reuse"|"refresh_token_reused"|"refresh_token_reuse"|"reuse_detected"=>"reuse",
        "invalid_grant"=>"invalid_grant", "invalid_client"|"unauthorized_client"=>"client",
        "captcha"|"captcha_required"|"invalid_captcha"=>"captcha", "code_required"=>"code_required",
        "invalid_request"|"unsupported_grant_type"|"invalid_scope"|"access_denied"|"server_error"|"temporarily_unavailable"=>"unrecognized",
        _=>return None,
    })
}
pub(crate) fn safe_oauth_diagnostic(value:&Value)->Option<Value> {
    if value["version"].as_u64()!=Some(1) {return None;}
    let shape=value["responseShape"].as_str().filter(|s|matches!(*s,"unavailable"|"oversized"|"non_json"|"json_object"|"json_other"))?;
    let error=value["error"].as_str().filter(|s|oauth_category(s).is_some());
    let error_type=value["errorType"].as_str().filter(|s|oauth_category(s).is_some());
    let category=error_type.or(error).and_then(oauth_category).unwrap_or("unrecognized");
    let mut result=json!({"version":1,"classification":category,"responseShape":shape});
    for (key,code) in [("error",error),("errorType",error_type)] {if let Some(code)=code {result[key]=json!(code);}}
    if let Some(trigger)=value["trigger"].as_str().filter(|s|matches!(*s,"proactive"|"after-401")) {result["trigger"]=json!(trigger);}
    if let Some(generation)=value["generation"].as_u64().filter(|n|*n<=9007199254740991) {result["generation"]=json!(generation);}
    if let Some(at)=value["observedAt"].as_str().filter(|s|s.len()==24) {
        if chrono::DateTime::parse_from_rfc3339(at).ok().is_some_and(|date|
            date.to_rfc3339_opts(chrono::SecondsFormat::Millis,true)==at) {result["observedAt"]=json!(at);}
    }
    Some(result)
}
pub(crate) fn oauth_refresh_stage(stage:&str)->bool {
    matches!(stage,"read-auth-proactive-refresh-http"|"read-auth-after-401-refresh-http")
}
// Fixed string slots retain the existing bounded ApiError transport contract.
pub(crate) const OAUTH_SLOTS:[(&str,&str);7]=[("oauthReason","classification"),("oauthBody","responseShape"),
    ("oauthError","error"),("oauthErrorType","errorType"),("oauthTrigger","trigger"),
    ("oauthGeneration","generation"),("oauthObservedAt","observedAt")];

/// Closed facts about one auth exchange, including a later durable barrier.
/// This projection grants neither credential access nor permission to replay.
pub(crate) fn auth_exchange_stage(stage:&str)->bool {
    stage.strip_prefix("read-auth-proactive-").or_else(||stage.strip_prefix("read-auth-after-401-"))
        .is_some_and(|s|matches!(s,"refresh-fetch"|"refresh-http"|"refresh-json"|"refresh-schema"|"token-lifecycle-blocked"))
}
pub(crate) fn safe_auth_exchange_diagnostic(d:&Value)->Option<Value> {
    if d["version"].as_f64()!=Some(1.0) {return None;}
    let id=d["diagnosticId"].as_str()?;
    let bytes=id.as_bytes();
    if bytes.len()!=36 || !bytes.iter().enumerate().all(|(i,b)|if [8,13,18,23].contains(&i) {*b==b'-'} else {b.is_ascii_digit() || (b'a'..=b'f').contains(b)})
        || bytes[14]!=b'4' || ![b'8',b'9',b'a',b'b'].contains(&bytes[19]) {return None;}
    let observation=d["observation"].as_str().filter(|s|matches!(*s,"originating_exchange"|"durable_barrier"))?;
    let stage=d["stage"].as_str().filter(|s|matches!(*s,"fetch"|"http"|"json"|"schema"))?;
    let cause=d["cause"].as_str().filter(|s|matches!(*s,"dns"|"tcp"|"tls"|"connect_timeout"|"timeout"|"abort"|"unknown"|"http_rejection"|"json_invalid"|"schema_invalid"|"invalid_status"))?;
    let watchdog=d["watchdogFired"].as_bool()?;
    let elapsed=d["elapsedMs"].as_f64().filter(|n|n.is_finite()&&*n>=0.0&&*n<=9007199254740991.0)?;
    let deadline=d["deadlineMs"].as_f64().filter(|n|n.is_finite()&&n.fract()==0.0&&(1.0..=60000.0).contains(n))? as u64;
    let received=d["responseReceived"].as_bool()?;
    let valid=d["httpStatusValid"].as_bool()?;
    let disposal=d["responseDisposal"].as_str().filter(|s|matches!(*s,"not_requested"|"requested"|"unavailable"|"failed"))?;
    if !received && disposal!="not_requested" {return None;}
    let status=if valid {Some(d["httpStatus"].as_f64().filter(|n|received&&n.is_finite()&&n.fract()==0.0&&(100.0..=599.0).contains(n))? as u64)} else {
        if d.get("httpStatus").is_some() {return None;} None
    };
    let mut result=json!({"version":1,"diagnosticId":id,"observation":observation,"stage":stage,"cause":cause,
        "watchdogFired":watchdog,"elapsedMs":elapsed,"deadlineMs":deadline,"responseReceived":received,
        "httpStatusValid":valid,"responseDisposal":disposal});
    if let Some(status)=status {result["httpStatus"]=json!(status);}
    if let Some(value)=d.get("trigger") {
        result["trigger"]=json!(value.as_str().filter(|s|matches!(*s,"proactive"|"after-401"))?);
    }
    if let Some(value)=d.get("generation") {
        result["generation"]=json!(value.as_f64().filter(|n|n.is_finite()&&n.fract()==0.0&&*n>=0.0&&*n<=9007199254740991.0)? as u64);
    }
    Some(result)
}
pub(crate) const AUTH_EXCHANGE_SLOTS:[(&str,&str);13]=[
    ("authDiagId","diagnosticId"),("authObservation","observation"),("authStage","stage"),("authCause","cause"),
    ("authWatchdog","watchdogFired"),("authElapsedMs","elapsedMs"),("authDeadlineMs","deadlineMs"),
    ("authResponseReceived","responseReceived"),("authHttpStatusValid","httpStatusValid"),("authHttpStatus","httpStatus"),
    ("authDisposal","responseDisposal"),("authTrigger","trigger"),("authGeneration","generation")];
pub(crate) fn safe_connection_state(value:&Value)->Option<Value> {
    let status=value["status"].as_str().filter(|s|matches!(*s,"recoverable"|"needs_user"))?;
    let reason=value["reason"].as_str().filter(|s|matches!(*s,"auth_required"|"credential_missing"|"credential_invalid"|"challenge"|"scope_mismatch"|"recovery_uncertain"))?;
    Some(json!({"status":status,"reason":reason}))
}

pub(crate) fn adapter_failure(error:&Value)->String {
    let code=error["code"].as_str().unwrap_or("adapter_error").chars()
        .filter(|c|c.is_ascii_alphanumeric()||*c=='_').take(80).collect::<String>();
    let mut detail=String::new();
    if let Some(role)=error["processRole"].as_str().filter(|role|
        ["provider-process","provider-worker","credential-helper","legacy-process-guard"].contains(role)) {
        detail.push_str(&format!("; processRole={role}"));
        if let Some(pid)=error["processId"].as_u64().filter(|pid|*pid>0 && *pid<=u32::MAX as u64) {
            detail.push_str(&format!("; processId={pid}"));
        }
    }
    if matches!(error["code"].as_str(),Some("HTTP_ERROR"|"TRANSPORT_ERROR")) {
        if let Some(stage)=error["transportStage"].as_str().filter(|stage|safe_transport_stage(stage)) {
            detail.push_str(&format!("; transportStage={stage}"));
        }
    }
    if error["code"]=="TRANSPORT_ERROR" && error["transportStage"]=="read-fetch" {
        if let Some(cause)=error["transportCause"].as_str().filter(|cause|safe_transport_cause(cause)) {
            detail.push_str(&format!("; transportCause={cause}"));
        }
    }
    if let Some(exit)=error["processExit"]["exitCode"].as_i64()
        .filter(|exit|(-2147483648..=4294967295).contains(exit)) {
        detail.push_str(&format!("; exitCode={exit}; exitHex=0x{:08X}",exit as u32));
    }
    if let Some(signal)=error["processExit"]["signal"].as_str().filter(|signal|
        ["SIGABRT","SIGBUS","SIGFPE","SIGHUP","SIGILL","SIGINT","SIGKILL","SIGPIPE","SIGQUIT","SIGSEGV","SIGTERM"].contains(signal)) {
        detail.push_str(&format!("; signal={signal}"));
    }
    if error["code"]=="HTTP_ERROR" {
        if let Some(status)=error["httpStatus"].as_u64().filter(|status|(100..=599).contains(status)) {
            detail.push_str(&format!("; httpStatus={status}"));
        }
    }
    if let Some(operation)=error["adapterOperation"].as_str().filter(|operation|
        ["caps","scan","read","context","head","status","auth_status","execute","readback"].contains(operation)) {
        detail.push_str(&format!("; operation={operation}"));
    }
    let recovery=&error["readbackProcessRecovery"];
    let first=&recovery["firstFailure"];
    if error["adapterOperation"]=="readback" && recovery["attempts"].as_u64()==Some(2)
        && first["adapterOperation"]=="readback" && first["code"]=="ADAPTER_PROCESS_FAILED" {
        detail.push_str("; readbackProcessAttempts=2");
        if let Some(exit)=first["processExit"]["exitCode"].as_i64()
            .filter(|exit|(-2147483648..=4294967295).contains(exit)) {
            detail.push_str(&format!("; firstReadbackExitCode={exit}; firstReadbackExitHex=0x{:08X}",exit as u32));
        }
        if let Some(signal)=first["processExit"]["signal"].as_str().filter(|signal|
            ["SIGABRT","SIGBUS","SIGFPE","SIGILL","SIGSEGV"].contains(signal)) {
            detail.push_str(&format!("; firstReadbackSignal={signal}"));
        }
    }
    if error["code"]=="HTTP_ERROR" && error["transportStage"].as_str().is_some_and(oauth_refresh_stage) {
        if let Some(diagnostic)=safe_oauth_diagnostic(&error["oauthDiagnostic"]) {
            for (slot,key) in OAUTH_SLOTS {
                if let Some(value)=diagnostic[key].as_str() {detail.push_str(&format!("; {slot}={value}"));}
                else if let Some(value)=diagnostic[key].as_u64() {detail.push_str(&format!("; {slot}={value}"));}
            }
        }
    }
    if matches!(error["code"].as_str(),Some("HTTP_ERROR"|"TRANSPORT_ERROR"))
        && error["transportStage"].as_str().is_some_and(auth_exchange_stage) {
        if let Some(d)=safe_auth_exchange_diagnostic(&error["authExchangeDiagnostic"]) {
            for (slot,key) in AUTH_EXCHANGE_SLOTS {
                if let Some(value)=d[key].as_str() {detail.push_str(&format!("; {slot}={value}"));}
                else if let Some(value)=d.get(key).filter(|v|v.is_number()||v.is_boolean()) {detail.push_str(&format!("; {slot}={value}"));}
            }
        }
    }
    if matches!(error["code"].as_str(),Some("HTTP_ERROR"|"TRANSPORT_ERROR"))
        && error["transportStage"].as_str().is_some_and(|stage|safe_transport_stage(stage)&&stage.starts_with("read-auth-")) {
        if let Some(state)=safe_connection_state(&error["connectionState"]) {
            detail.push_str(&format!("; authConnectionStatus={}; authConnectionReason={}",state["status"].as_str().unwrap(),state["reason"].as_str().unwrap()));
        }
    }
    format!("Adapter failed ({code}{detail})")
}

pub(crate) fn readback_account_matches(result:&Value,account:&str)->bool {
    result["account"].as_str()==Some(account)
}

pub(crate) fn reply_baseline(value: &Value) -> Option<Value> {
    let values=value.as_array().filter(|rows| rows.len()<=1000)?;
    let mut seen=std::collections::HashSet::new();
    for value in values {
        let id=value.as_str().filter(|s| !s.is_empty() && s.len()<=200
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b==b'_' || b==b'-'))?;
        if !seen.insert(id) { return None; }
    }
    Some(json!({"baselineReplyIds":values}))
}

pub(crate) fn from_receipt(result: &Value, action: &Value, account: &str) -> Option<Value> {
    if result["account"].as_str()!=Some(account) { return None; }
    let rows=result["results"].as_array()?;
    let matches:Vec<_>=rows.iter().filter(|row| row["actionId"]==action["actionId"]
        && row["itemId"]==action["itemId"]).collect();
    if matches.len()!=1 { return None; }
    reply_baseline(&matches[0]["readbackEvidence"]["baselineReplyIds"])
}

pub(crate) fn confirmed_failure(result:&Value, action:&Value, account:&str)->bool {
    if result["account"].as_str()!=Some(account) {return false;}
    result["results"].as_array().is_some_and(|rows| rows.len()==1
        && rows[0]["actionId"]==action["actionId"] && rows[0]["itemId"]==action["itemId"]
        && rows[0]["status"]=="failed"
        && matches!(rows[0]["mutationOutcome"].as_str(),Some("not-attempted"|"rejected")))
}

pub(crate) fn conversation_blocker(data:&Value,op:&Value)->Option<String> {
    if op["action"]["action"]!="reply_and_close" {return None;}
    list(data,"operations").iter().find(|prior| prior["id"]!=op["id"]
        && prior["status"]=="unknown" && prior["action"]["action"]=="reply_and_close"
        && prior["target"]["connectorBinding"]==op["target"]["connectorBinding"]
        && prior["action"]["conversationKey"]==op["action"]["conversationKey"])
        .and_then(|row|row["id"].as_str().map(str::to_owned))
}

pub(crate) async fn record_execute(app:&App,op:&Value,receipt:Value)->ApiResult<()> {
    app.change_operation_evidence(op,storage::OperationEvidenceUpdate::ExecuteReceipt(receipt)).await
}

pub(crate) async fn persist(app:&App,op:&mut Value,evidence:Value)->ApiResult<()> {
    // The post-execute receipt normally returns the same pre-dispatch baseline.
    // It is already durable; avoid another full workspace transaction.
    if op["action"]["readbackEvidence"] == evidence { return Ok(()); }
    app.change_operation_evidence(op,storage::OperationEvidenceUpdate::Readback(evidence.clone())).await?;
    op["action"]["readbackEvidence"]=evidence;
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn adapter_failure_binds_bounded_exit_to_the_actual_process_role() {
        let mut value=json!({"code":"ADAPTER_PROCESS_FAILED","processRole":"legacy-process-guard","processId":123,
            "processExit":{"exitCode":3221226505u64},"message":"secret","command":"secret","stderr":"secret"});
        assert_eq!(adapter_failure(&value),"Adapter failed (ADAPTER_PROCESS_FAILED; processRole=legacy-process-guard; processId=123; exitCode=3221226505; exitHex=0xC0000409)");
        for pid in [json!(0),json!(-1),json!(4294967296u64),json!("secret")] {
            value["processId"]=pid;assert!(!adapter_failure(&value).contains("processId="));
        }
        value["processRole"]=json!("secret");value["processId"]=json!(123);
        assert!(!adapter_failure(&value).contains("processId="));
        assert!(!adapter_failure(&value).contains("secret"));
    }
    #[test] fn adapter_failure_retains_only_bounded_os_facts() {
        assert_eq!(adapter_failure(&json!({"code":"ADAPTER_PROCESS_FAILED","processExit":{"exitCode":3221225477u64,"signal":"SIGSEGV","stderr":"secret"}})),
            "Adapter failed (ADAPTER_PROCESS_FAILED; exitCode=3221225477; exitHex=0xC0000005; signal=SIGSEGV)");
        assert_eq!(adapter_failure(&json!({"code":"ADAPTER_PROCESS_FAILED","processExit":{"exitCode":4294967296u64,"signal":"secret"}})),"Adapter failed (ADAPTER_PROCESS_FAILED)");
        assert_eq!(adapter_failure(&json!({"code":"ADAPTER_PROCESS_FAILED","adapterOperation":"readback","processExit":{"exitCode":3221226505u64}})),
            "Adapter failed (ADAPTER_PROCESS_FAILED; exitCode=3221226505; exitHex=0xC0000409; operation=readback)");
        assert_eq!(adapter_failure(&json!({"code":"ADAPTER_PROCESS_FAILED","adapterOperation":"secret"})),"Adapter failed (ADAPTER_PROCESS_FAILED)");
    }
    #[test] fn adapter_failure_retains_only_bounded_http_status_for_http_errors() {
        for status in [100,302,429,500,599] {
            let error=json!({"code":"HTTP_ERROR","httpStatus":status,"adapterOperation":"read",
                "message":"secret","body":"secret","headers":{"authorization":"secret"}});
            assert_eq!(adapter_failure(&error),format!("Adapter failed (HTTP_ERROR; httpStatus={status}; operation=read)"));
        }
        for status in [json!("429"),json!(429.0),json!(-1),json!(99),json!(600),json!(null)] {
            let error=json!({"code":"HTTP_ERROR","httpStatus":status,"adapterOperation":"read",
                "message":"secret","body":"secret","headers":{"authorization":"secret"}});
            assert_eq!(adapter_failure(&error),"Adapter failed (HTTP_ERROR; operation=read)");
        }
        let other=json!({"code":"TRANSPORT_ERROR","httpStatus":429,"adapterOperation":"read",
            "message":"secret","body":"secret","headers":{"authorization":"secret"}});
        assert_eq!(adapter_failure(&other),"Adapter failed (TRANSPORT_ERROR; operation=read)");
    }
    #[test] fn adapter_failure_retains_only_closed_fetch_cause_for_read_transport() {
        for cause in ["dns","tcp","tls","connect_timeout","timeout","abort","unknown"] {
            let error=json!({"code":"TRANSPORT_ERROR","transportStage":"read-fetch",
                "transportCause":cause,"message":"private token","url":"private URL"});
            assert_eq!(adapter_failure(&error),format!("Adapter failed (TRANSPORT_ERROR; transportStage=read-fetch; transportCause={cause})"));
        }
        for error in [json!({"code":"TRANSPORT_ERROR","transportStage":"read-fetch","transportCause":"private"}),
            json!({"code":"TRANSPORT_ERROR","transportStage":"read-json","transportCause":"dns"}),
            json!({"code":"HTTP_ERROR","transportStage":"read-http","transportCause":"dns"})] {
            assert!(!adapter_failure(&error).contains("transportCause="));
        }
    }
    #[test] fn readback_rejects_missing_or_foreign_account_even_for_verified_identity() {
        let mut result=json!({"results":[{"actionId":"a","itemId":"i","status":"verified"}]});
        assert!(!readback_account_matches(&result,"baw-russia"));
        result["account"]=json!("likeavto");assert!(!readback_account_matches(&result,"baw-russia"));
        result["account"]=json!("baw-russia");assert!(readback_account_matches(&result,"baw-russia"));
    }
    #[test] fn adapter_failure_retains_bounded_readback_recovery_without_private_content() {
        let mut error=json!({"code":"ADAPTER_PROCESS_FAILED","adapterOperation":"readback",
            "processExit":{"exitCode":17},"readbackProcessRecovery":{"attempts":2,"private":"secret",
                "firstFailure":{"code":"ADAPTER_PROCESS_FAILED","adapterOperation":"readback",
                    "message":"secret","stdout":"secret","processExit":{"exitCode":3221226505u64,"stderr":"secret"}}}});
        assert_eq!(adapter_failure(&error),"Adapter failed (ADAPTER_PROCESS_FAILED; exitCode=17; exitHex=0x00000011; operation=readback; readbackProcessAttempts=2; firstReadbackExitCode=3221226505; firstReadbackExitHex=0xC0000409)");
        error["readbackProcessRecovery"]["firstFailure"]["processExit"]=json!({"exitCode":-1073740791i64,"signal":"SIGSEGV"});
        let signed=adapter_failure(&error);
        assert!(signed.contains("firstReadbackExitCode=-1073740791; firstReadbackExitHex=0xC0000409; firstReadbackSignal=SIGSEGV"));
        assert!(!signed.contains("secret"));
        error["readbackProcessRecovery"]["firstFailure"]["processExit"]=json!({"exitCode":4294967296u64,"signal":"secret"});
        let invalid=adapter_failure(&error);
        assert!(invalid.contains("readbackProcessAttempts=2"));
        assert!(!invalid.contains("firstReadback"));
        for field in ["adapterOperation","code"] {
            let mut foreign=error.clone();foreign["readbackProcessRecovery"]["firstFailure"][field]=json!("secret");
            assert!(!adapter_failure(&foreign).contains("readbackProcessAttempts"));
        }
        for attempts in [json!(1),json!(3),json!("2"),Value::Null] {
            error["readbackProcessRecovery"]["attempts"]=attempts;
            assert!(!adapter_failure(&error).contains("readbackProcessAttempts"));
        }
        error["readbackProcessRecovery"]["attempts"]=json!(2);
        error["adapterOperation"]=json!("execute");
        assert!(!adapter_failure(&error).contains("readbackProcessAttempts"));
    }
    #[test] fn evidence_is_bounded_and_bound_to_the_exact_action_and_account() {
        let action=json!({"actionId":"a","itemId":"i"});
        let mut receipt=json!({"account":"baw-russia","results":[{"actionId":"a","itemId":"i",
            "readbackEvidence":{"baselineReplyIds":["old-1"]}}]});
        assert!(from_receipt(&receipt,&action,"baw-russia").is_some());
        assert!(from_receipt(&receipt,&action,"likeavto").is_none());
        receipt["results"][0]["itemId"]=json!("elsewhere");
        assert!(from_receipt(&receipt,&action,"baw-russia").is_none());
        for ids in [json!(["same","same"]),json!(["bad id"]),json!([1]),Value::Null] {
            assert!(reply_baseline(&ids).is_none());
        }
        assert_eq!(reply_baseline(&json!([])),Some(json!({"baselineReplyIds":[]})));
    }
    #[test] fn only_exact_proven_non_mutations_are_terminal_failures() {
        let action=json!({"actionId":"a","itemId":"i"});
        for outcome in ["not-attempted","rejected","uncertain","confirmed-2xx",""] {
            let result=json!({"account":"likeavto","results":[{"actionId":"a","itemId":"i","status":"failed","mutationOutcome":outcome}]});
            assert_eq!(confirmed_failure(&result,&action,"likeavto"),["not-attempted","rejected"].contains(&outcome));
            assert!(!confirmed_failure(&result,&action,"baw-russia"));
        }
    }
    #[tokio::test] async fn conversation_quarantine_survives_reopen_and_receipts_survive_readback() {
        let (app,temp)=crate::tests::test_app().await;
        let binding=accounts::Profile::LikeAvto.binding();
        let prior=json!({"id":"prior","status":"unknown","target":{"connectorBinding":binding},"action":{"action":"reply_and_close","conversationKey":"11391:thread"}});
        let op=json!({"id":"next","itemId":"item-1","proposalId":"p","target":{"connectorBinding":binding},"action":{"actionId":"a","action":"reply_and_close","conversationKey":"11391:thread"}});
        app.change(|data|{data["operations"]=json!([prior,op]);data["proposals"]=json!([{"id":"p"}]);Ok(())}).await.unwrap();
        let receipt=json!({"httpStatus":429,"retryAfterMs":5000,"phase":"preflight"});
        record_execute(&app,&op,receipt.clone()).await.unwrap();
        set_outcome(&app,&op,"unknown",json!({"readback":"uncertain"})).await.unwrap();
        app.db.close().await;
        let db=Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
        let data=db.read().await.unwrap();
        assert_eq!(conversation_blocker(&data,&op).as_deref(),Some("prior"));
        assert_eq!(row(&data,"operations","next").unwrap()["executeReceipt"],receipt);
        let mut other=op.clone();other["target"]["connectorBinding"]=accounts::Profile::BawRussia.binding();
        assert!(conversation_blocker(&data,&other).is_none());
        db.close().await;
    }
}

#[cfg(test)] mod auth_exchange_projection_tests {
    use super::*;
    pub(super) fn fixture()->Value {json!({"version":1,"diagnosticId":"00000000-0000-4000-8000-000000000001",
        "observation":"originating_exchange","stage":"fetch","cause":"connect_timeout","watchdogFired":false,
        "elapsedMs":7.125,"deadlineMs":15000,"responseReceived":false,"httpStatusValid":false,
        "responseDisposal":"not_requested","trigger":"proactive","generation":4})}
    #[test] fn auth_exchange_sanitizer_rejects_inconsistent_facts_and_drops_content() {
        let original=fixture();let mut value=original.clone();value["body"]=json!("PRIVATE");value["fingerprint"]=json!("PRIVATE");
        assert_eq!(safe_auth_exchange_diagnostic(&value),Some(original.clone()));
        for (key,bad) in [("version",json!(2)),("diagnosticId",json!("PRIVATE")),("elapsedMs",json!(-1)),
            ("elapsedMs",json!(9007199254740992u64)),("deadlineMs",json!(60001)),("httpStatus",json!(200)),
            ("httpStatusValid",json!(true)),("responseDisposal",json!("requested")),("generation",json!(1.1)),
            ("trigger",json!("PRIVATE"))] {
            let mut value=original.clone();value[key]=bad;assert!(safe_auth_exchange_diagnostic(&value).is_none(),"{key}");
        }
        let mut response=original.clone();
        response["responseReceived"]=json!(true);response["httpStatusValid"]=json!(true);response["httpStatus"]=json!(400);
        response["responseDisposal"]=json!("requested");
        assert_eq!(safe_auth_exchange_diagnostic(&response),Some(response));
        let mut numeric=original.clone();numeric["deadlineMs"]=json!(15000.0);numeric["generation"]=json!(4.0);
        assert_eq!(safe_auth_exchange_diagnostic(&numeric),Some(original));
    }
}
