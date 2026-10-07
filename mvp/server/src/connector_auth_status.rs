//! Optional connection observation. No exchange, credential mutation or repair.
use crate::*;

pub(crate) async fn status(State(app): State<App>) -> ApiResult<Json<Value>> {
    let response=app.bridge("auth_status",json!({})).await?;
    Ok(Json(project(&response,app.account.key())?))
}

fn integer(value:&Value,nonnegative:bool)->Option<i64> {
    value.as_f64().filter(|n|n.is_finite()&&n.fract()==0.0&&n.abs()<=9007199254740991.0
        &&(!nonnegative||*n>=0.0)).map(|n|n as i64)
}

/// Reconstruct a closed native response; never forward a connector body.
fn project(value:&Value,account:&str)->ApiResult<Value> {
    let invalid=|| conflict("Invalid read-only connector authorization observation");
    if value["version"].as_u64()!=Some(1)||value["operation"]!="auth_status"
        ||value["account"].as_str()!=Some(account)||value["readOnly"]!=true
        ||["authRequests","credentialMutations","lockMutations"].iter()
            .any(|key|value[*key].as_u64()!=Some(0)) {return Err(invalid());}
    let status=&value["status"];
    if status["version"].as_u64()!=Some(1)||status["scopeKey"].as_str()!=Some(account) {return Err(invalid());}
    let generation=integer(&status["generation"],true).ok_or_else(invalid)?;
    let expires=integer(&status["expiresAtMs"],true).ok_or_else(invalid)?;
    let seconds=integer(&status["expiresInSeconds"],true).ok_or_else(invalid)?;
    let observed=integer(&status["observedAtMs"],true).ok_or_else(invalid)?;
    let expired=status["expired"].as_bool().ok_or_else(invalid)?;
    let refresh=status["refreshNeeded"].as_bool().ok_or_else(invalid)?;
    if expired!=(expires<=observed)||expired&&!refresh {return Err(invalid());}
    let row=&status["lifecycle"];
    let binding=row["binding"].as_str().filter(|s|matches!(*s,"missing"|"verified"|"unavailable")).ok_or_else(invalid)?;
    let phase=row["phase"].as_str().filter(|s|matches!(*s,"none"|"in-flight"|"returned"|"rejected"|"ambiguous"|"access-rejected"|"reauth-in-flight"|"reauth-returned"|"reauth-needs-user"|"unavailable")).ok_or_else(invalid)?;
    let mut lifecycle=json!({"binding":binding,"phase":phase});
    for key in ["pairPresent","pairValid","recoveryAdmitted","refreshExchangeAdmitted"] {
        lifecycle[key]=json!(row[key].as_bool().ok_or_else(invalid)?);
    }
    if lifecycle["pairValid"]==true&&lifecycle["pairPresent"]!=true
        ||binding!="verified"&&(lifecycle["recoveryAdmitted"]==true||lifecycle["refreshExchangeAdmitted"]==true) {return Err(invalid());}
    if let Some(diagnostic)=row.get("authExchangeDiagnostic") {
        let diagnostic=dispatch_evidence::safe_auth_exchange_diagnostic(diagnostic).ok_or_else(invalid)?;
        if diagnostic["observation"]!="durable_barrier"||integer(&diagnostic["generation"],true)!=Some(generation) {return Err(invalid());}
        lifecycle["authExchangeDiagnostic"]=diagnostic;
    }
    Ok(json!({"version":1,"operation":"auth_status","account":account,"readOnly":true,
        "authRequests":0,"credentialMutations":0,"lockMutations":0,"status":{"version":1,
        "scopeKey":account,"generation":generation,"expiresAtMs":expires,"expiresInSeconds":seconds,
        "observedAtMs":observed,"expired":expired,"refreshNeeded":refresh,"lifecycle":lifecycle}}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample()->Value {json!({"version":1,"operation":"auth_status","account":"baw-russia","readOnly":true,
        "authRequests":0,"credentialMutations":0,"lockMutations":0,"rawBody":"private",
        "status":{"version":1,"scopeKey":"baw-russia","generation":356,"expiresAtMs":1000,
        "expiresInSeconds":3600,"observedAtMs":2000,"expired":true,"refreshNeeded":true,"accessToken":"private",
        "lifecycle":{"binding":"verified","phase":"ambiguous","pairPresent":false,"pairValid":false,
        "recoveryAdmitted":false,"refreshExchangeAdmitted":false,"fingerprint":"private"}}})}
    #[test]
    fn closed_observation_preserves_ambiguous_generation_without_secrets() {
        let result=project(&sample(),"baw-russia").unwrap();
        assert_eq!(result["status"]["generation"],356);
        assert_eq!(result["status"]["lifecycle"]["phase"],"ambiguous");
        assert!(!result.to_string().contains("private"));
    }
    #[test]
    fn rejects_foreign_company_and_auth_mutation_claims() {
        assert!(project(&sample(),"likeavto").is_err());
        let mut value=sample();value["authRequests"]=json!(1);
        assert!(project(&value,"baw-russia").is_err());
        value=sample();value["status"]["scopeKey"]=json!("likeavto");
        assert!(project(&value,"baw-russia").is_err());
    }
    #[test]
    fn rejects_invalid_phase_expiry_and_diagnostic() {
        let mut value=sample();value["status"]["lifecycle"]["phase"]=json!("reset");
        assert!(project(&value,"baw-russia").is_err());
        value=sample();value["status"]["expired"]=json!(false);
        assert!(project(&value,"baw-russia").is_err());
        value=sample();value["status"]["lifecycle"]["authExchangeDiagnostic"]=json!({"rawBody":"private"});
        assert!(project(&value,"baw-russia").is_err());
    }
    #[test]
    fn rejects_inconsistent_pair_admission_and_expired_readiness() {
        let mut value=sample();value["status"]["lifecycle"]["pairValid"]=json!(true);
        assert!(project(&value,"baw-russia").is_err());
        value=sample();value["status"]["lifecycle"]["binding"]=json!("missing");
        value["status"]["lifecycle"]["recoveryAdmitted"]=json!(true);
        assert!(project(&value,"baw-russia").is_err());
        value=sample();value["status"]["refreshNeeded"]=json!(false);
        assert!(project(&value,"baw-russia").is_err());
    }
    #[test]
    fn observation_only_accepts_matching_historical_auth_generation() {
        let mut value=sample();
        value["status"]["lifecycle"]["authExchangeDiagnostic"]=json!({"version":1,
            "diagnosticId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","observation":"durable_barrier",
            "stage":"fetch","cause":"unknown","watchdogFired":false,"elapsedMs":0.25,"deadlineMs":15000,
            "responseReceived":false,"httpStatusValid":false,"responseDisposal":"not_requested",
            "trigger":"proactive","generation":356});
        assert!(project(&value,"baw-russia").is_ok());
        value["status"]["lifecycle"]["authExchangeDiagnostic"]["generation"]=json!(355);
        assert!(project(&value,"baw-russia").is_err());
        value["status"]["lifecycle"]["authExchangeDiagnostic"]["generation"]=json!(356);
        value["status"]["lifecycle"]["authExchangeDiagnostic"]["observation"]=json!("originating_exchange");
        assert!(project(&value,"baw-russia").is_err());
    }
}
