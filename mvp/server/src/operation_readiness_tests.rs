use crate::*;

async fn setup()->(App,tempfile::TempDir,Value) {
    let (app,temp)=tests::test_app().await;
    let op=app.change(|d|{
        let p=create_proposal(d,&json!({"itemId":"item-1","kind":"close","expectedRevision":1}))?;
        let item=bound_item(&active_binding(d)?,row(d,"items","item-1")?)?;
        let op=json!({"id":"operation-ready","itemId":"item-1","proposalId":p["id"],"status":"dispatching",
            "target":item,"action":{"actionId":"action-ready","action":"close","itemId":item["itemId"]}});
        row_mut(d,"proposals",required(&p,"id")?)?["status"]=json!("dispatching");
        list_mut(d,"operations").push(op.clone());Ok(op)
    }).await.unwrap();(app,temp,op)
}

#[tokio::test]
async fn failed_stale_unknown_demote_only_readiness_without_changing_draft_or_revision() {
    for status in ["failed","stale","unknown"] {
        let (app,_temp,op)=setup().await;
        let mut d=app.db.read().await.unwrap();let before=d["items"][0].clone();
        apply_operation_outcome(&mut d,&op,status,json!({"providerRetryAllowed":false})).unwrap();
        let mut expected=before;expected["workflow"]=json!("attention");
        assert_eq!(d["items"][0],expected);assert_eq!(d["operations"][0]["evidence"]["providerRetryAllowed"],false);
        app.db.close().await;
    }
}

#[tokio::test]
async fn outcome_does_not_clobber_current_sibling_or_newer_manual_state() {
    for mode in ["close-sibling","reply-sibling","manual-revision","waiting","closed"] {
        let (app,_temp,op)=setup().await;let mut d=app.db.read().await.unwrap();
        if mode.ends_with("sibling") {
            let p=json!({"id":"new-ready","itemId":"item-1","status":"draft",
                "kind":if mode=="close-sibling"{"close"}else{"reply_and_close"},"text":"Current human answer",
                "itemRevision":d["items"][0]["revision"],"contextEvidenceDigest":d["items"][0]["contextEvidenceDigest"],
                "branchContextDigest":d["items"][0]["branchContextDigest"]});
            list_mut(&mut d,"proposals").push(p);
        } else if mode=="manual-revision" {bump(&mut d["items"][0]);d["items"][0]["draft"]=json!("New human draft");}
        else {d["items"][0]["workflow"]=json!(mode);}
        let before=d["items"][0].clone();
        apply_operation_outcome(&mut d,&op,"failed",json!({})).unwrap();assert_eq!(d["items"][0],before,"{mode}");
        app.db.close().await;
    }
}

#[tokio::test]
async fn historical_prepared_terminal_proposal_is_repaired_once_without_history_or_revision_changes() {
    let (app,_temp,op)=setup().await;let mut d=app.db.read().await.unwrap();
    d["operations"][0]["status"]=json!("failed");d["proposals"][0]["status"]=json!("failed");
    d["items"][0]["draft"]=json!("");d["items"][0]["autoPreparation"]=Value::Null;
    let item=d["items"][0].clone();let proposals=d["proposals"].clone();let operations=d["operations"].clone();
    let approvals=d["approvals"].clone();
    recover(&mut d).unwrap();let mut expected=item;expected["workflow"]=json!("attention");
    assert_eq!(d["items"][0],expected);assert_eq!(d["proposals"],proposals);
    assert_eq!(d["operations"],operations);assert_eq!(d["approvals"],approvals);
    assert_eq!(list(&d,"audit").iter().filter(|r|r["action"]=="item.readiness_repaired").count(),1);
    let after=d.clone();recover(&mut d).unwrap();assert_eq!(d,after);
    assert_eq!(d["operations"][0]["id"],op["id"]);app.db.close().await;
}

#[tokio::test]
async fn startup_preserves_current_close_reply_hide_delete_and_human_draft_but_demotes_bound_unknown() {
    for kind in ["close","reply_and_close","hide","delete","human","unknown"] {
        let (app,_temp,op)=setup().await;let mut d=app.db.read().await.unwrap();
        d["operations"][0]["status"]=json!(if kind=="unknown"{"unknown"}else{"failed"});
        d["proposals"][0]["status"]=json!(if kind=="human"||kind=="unknown"{"failed"}else{"draft"});
        d["proposals"][0]["kind"]=json!(if kind=="human"||kind=="unknown"{"close"}else{kind});
        d["proposals"][0]["text"]=json!(if kind=="reply_and_close"{"Current reply"}else{""});
        if kind=="human"||kind=="unknown" {d["items"][0]["draft"]=json!("Owner draft");d["items"][0]["draftEdited"]=json!(true);}
        let before=d["items"][0].clone();repair_prepared_readiness(&mut d);
        let mut expected=before;if kind=="unknown"{expected["workflow"]=json!("attention");}
        assert_eq!(d["items"][0],expected,"{kind}");
        assert_eq!(d["operations"][0]["id"],op["id"]);app.db.close().await;
    }
}
