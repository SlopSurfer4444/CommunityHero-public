//! Persist the provider's pre-send reply baseline with the exact operation so
//! restart reconciliation cannot mistake an old equal-text reply for a new one.
use crate::*;

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
    app.change(|data| {
        row_mut(data,"operations",required(op,"id")?)?["executeReceipt"]=receipt;
        Ok(())
    }).await
}

pub(crate) async fn persist(app:&App,op:&mut Value,evidence:Value)->ApiResult<()> {
    // The post-execute receipt normally returns the same pre-dispatch baseline.
    // It is already durable; avoid another full workspace transaction.
    if op["action"]["readbackEvidence"] == evidence { return Ok(()); }
    let key=required(op,"id")?.to_string();
    let action=op["action"].clone();
    app.change(|data| {
        let stored=row_mut(data,"operations",&key)?;
        if stored["action"]["actionId"]!=action["actionId"]
            || stored["action"]["contextEvidenceDigest"]!=action["contextEvidenceDigest"] {
            return Err(conflict("Reply evidence belongs to a different operation"));
        }
        stored["action"]["readbackEvidence"]=evidence.clone();
        Ok(())
    }).await?;
    op["action"]["readbackEvidence"]=evidence;
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
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
