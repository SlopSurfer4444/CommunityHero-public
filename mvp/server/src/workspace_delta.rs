//! Diffs are computed only after both snapshots have passed actor filtering.
//! Rows are complete replacements, including conversation history and branch
//! context; inbox filters never alter the selected thread's transport.
use serde_json::{Map,Value,json};
use std::collections::HashMap;
const COLLECTIONS:[&str;9]=["items","posts","branches","proposals","operations","materials","conversations","jobs","approvals"];
fn keyed(rows:&Value)->Option<(&Vec<Value>,HashMap<&str,&Value>)> {
    let rows=rows.as_array()?;
    let mut keys=HashMap::new();
    for row in rows {let id=row["id"].as_str()?;if id.is_empty()||keys.insert(id,row).is_some(){return None;}}
    Some((rows,keys))
}
pub fn between(base:&Value,current:&Value,actor_id:&str)->Value {
    let _span=crate::performance::Span::new("workspace_delta.compare");
    let mut collections=Map::new();let mut set=Map::new();let mut remove=Vec::new();
    let (Some(old),Some(new))=(base.as_object(),current.as_object()) else {return json!({"kind":"full","snapshot":current});};
    for (key,value) in new {
        if key=="workspaceVersion"||old.get(key)==Some(value){continue;}
        if COLLECTIONS.contains(&key.as_str()) {
            if let (Some((old_rows,old_keys)),Some((new_rows,new_keys)))=(old.get(key).and_then(keyed),keyed(value)) {
                let upsert:Vec<&Value>=new_rows.iter().filter(|row|old_keys.get(row["id"].as_str().unwrap()).copied()!=Some(*row)).collect();
                let deleted:Vec<&str>=old_rows.iter().filter_map(|row|{let id=row["id"].as_str().unwrap();(!new_keys.contains_key(id)).then_some(id)}).collect();
                let old_order:Vec<&str>=old_rows.iter().map(|row|row["id"].as_str().unwrap()).collect();
                let new_order:Vec<&str>=new_rows.iter().map(|row|row["id"].as_str().unwrap()).collect();
                let mut patch=json!({"upsert":upsert,"remove":deleted});
                if old_order!=new_order {patch["order"]=json!(new_order);}
                collections.insert(key.clone(),patch);continue;
            }
            // Legacy approvals may have malformed IDs. Preserve their prior
            // atomic transport; never discard, coerce, or grant authority.
            if key!="approvals" {return json!({"kind":"full","snapshot":current});}
        }
        // Unknown/non-keyed arrays are replaced atomically. No positional patches.
        set.insert(key.clone(),value.clone());
    }
    for key in old.keys(){if !new.contains_key(key){remove.push(key);}}
    json!({"kind":"delta","baseVersion":base["workspaceVersion"],"workspaceVersion":current["workspaceVersion"],"actorId":actor_id,"collections":collections,"set":set,"remove":remove})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn producer_matches_browser_shared_contract() {
        let fixture:Value=serde_json::from_str(include_str!("../../tests/fixtures/workspace-delta-contract.json")).unwrap();
        assert_eq!(between(&fixture["base"],&fixture["current"],fixture["actorId"].as_str().unwrap()),fixture["delta"]);
    }
    fn snapshot()->Value {json!({"workspaceVersion":"one:1","items":[{"id":"a","draft":"first","revision":1},{"id":"b","draft":"keep","revision":1}],"conversations":[{"id":"chat","messages":[{"text":"old"}]}],"settings":{"enabled":false}})}
    #[test]
    fn changes_one_row_without_transferring_unchanged_workspace() {
        let base=snapshot();let mut next=base.clone();next["workspaceVersion"]=json!("one:2");next["items"][0]["draft"]=json!("edited");next["items"][0]["revision"]=json!(2);
        let delta=between(&base,&next,"alice");
        assert_eq!(delta["collections"]["items"]["upsert"],json!([next["items"][0]]));
        assert!(delta["collections"]["items"].get("order").is_none());
        assert!(delta["collections"].get("conversations").is_none());assert_eq!(delta["set"],json!({}));assert_eq!(delta["baseVersion"],"one:1");
    }
    #[test]
    fn additions_deletions_order_and_complete_assistant_history_are_explicit() {
        let base=snapshot();let mut next=base.clone();next["workspaceVersion"]=json!("one:2");next["items"]=json!([{"id":"c","revision":1},base["items"][0]]);next["conversations"][0]["messages"].as_array_mut().unwrap().push(json!({"text":"new"}));next.as_object_mut().unwrap().remove("settings");
        let delta=between(&base,&next,"alice");assert_eq!(delta["collections"]["items"]["remove"],json!(["b"]));assert_eq!(delta["collections"]["items"]["order"],json!(["c","a"]));assert_eq!(delta["collections"]["conversations"]["upsert"][0]["messages"].as_array().unwrap().len(),2);assert_eq!(delta["remove"],json!(["settings"]));
    }
    #[test]
    fn malformed_or_duplicate_ids_use_atomic_replacement() {
        let base=snapshot();let mut next=base.clone();next["items"]=json!([{"id":"a"},{"id":"a"}]);let delta=between(&base,&next,"alice");assert_eq!(delta["kind"],"full");assert_eq!(delta["snapshot"]["items"],next["items"]);
    }
    #[test]
    fn changed_row_payload_is_bounded_by_changes_not_workspace_size() {
        let rows:Vec<Value>=(0..3000).map(|i|json!({"id":format!("i-{i}"),"text":"x".repeat(500),"revision":1})).collect();let base=json!({"workspaceVersion":"a:1","items":rows});let mut next=base.clone();next["workspaceVersion"]=json!("a:2");next["items"][42]["revision"]=json!(2);let delta=between(&base,&next,"alice");assert!(delta.to_string().len()<1500);assert!(next.to_string().len()>1_500_000);
    }
}



#[cfg(test)]
mod r9_approval_tests {
    use super::*;
    #[test]
    fn keyed_approvals_use_sanitized_complete_rows_and_legacy_ids_remain_atomic() {
        let mut private=crate::empty();
        private["workspaceVersion"]=json!("a:1");
        private["approvals"]=json!([{"id":"a","status":"pending","approvalAuthority":{"secret":"PRIVATE_AUTH"}},{"id":"b","status":"pending"}]);
        let base=crate::bootstrap_view(private.clone(),"csrf");
        private["workspaceVersion"]=json!("a:2");private["approvals"][0]["status"]=json!("expired");
        let current=crate::bootstrap_view(private.clone(),"csrf");let delta=between(&base,&current,"owner");
        assert_eq!(delta["collections"]["approvals"]["upsert"],json!([current["approvals"][0]]));
        assert!(delta["set"].get("approvals").is_none());assert!(!delta.to_string().contains("PRIVATE_AUTH"));
        assert!(private.to_string().contains("PRIVATE_AUTH"));
        for malformed in [json!([{"id":"a"},{"id":"a"}]),json!([{"id":""}]),json!([{"id":4}]),json!([{"status":"legacy"}])] {
            private["approvals"]=malformed.clone();let current=crate::bootstrap_view(private.clone(),"csrf");let delta=between(&base,&current,"owner");
            assert_eq!(delta["kind"],"delta");assert_eq!(delta["set"]["approvals"],current["approvals"]);assert!(delta["collections"].get("approvals").is_none());
        }
    }
    #[test]
    fn one_changed_approval_transfers_only_one_complete_receipt() {
        let rows:Vec<Value>=(0..3000).map(|i|json!({"id":format!("approval-{i}"),"status":"pending","reviewText":"x".repeat(500)})).collect();
        let base=json!({"workspaceVersion":"a:1","approvals":rows});let mut current=base.clone();
        current["workspaceVersion"]=json!("a:2");current["approvals"][42]["status"]=json!("expired");
        let delta=between(&base,&current,"owner");
        assert_eq!(delta["collections"]["approvals"]["upsert"].as_array().unwrap().len(),1);
        assert_eq!(delta["collections"]["approvals"]["upsert"][0],current["approvals"][42]);
        assert!(delta.to_string().len()<1500);assert!(current.to_string().len()>1_500_000);
    }
    #[tokio::test]
    async fn approval_delta_handler_keeps_session_identity_and_strips_execution_authority() {
        use crate::*;
        let (app,_folder)=crate::tests::test_app().await;
        let actor=operator_auth::Actor{id:"local-owner".into(),name:"Owner".into(),role:"owner".into(),csrf_token:"session-csrf".into(),authority_generation:None};
        app.change(|data|{data["approvals"]=json!([{"id":"approval-r9","status":"pending","approvalAuthority":{"secret":"PRIVATE_AUTH"}}]);Ok(())}).await.unwrap();
        let base=operator_http::bootstrap(axum::extract::State(app.clone()),axum::Extension(actor.clone())).await.unwrap().0;
        app.change(|data|{data["approvals"][0]["status"]=json!("expired");Ok(())}).await.unwrap();
        let delta=operator_http::bootstrap_delta(axum::extract::State(app.clone()),axum::Extension(actor),axum::extract::Query(HashMap::from([("since".into(),base["workspaceVersion"].as_str().unwrap().to_owned())]))).await.unwrap().0;
        assert_eq!(delta["actorId"],"local-owner");assert_eq!(delta["set"]["csrfToken"],"session-csrf");
        assert_eq!(delta["collections"]["approvals"]["upsert"][0]["status"],"expired");assert!(!delta.to_string().contains("PRIVATE_AUTH"));
        assert!(delta["collections"]["approvals"]["upsert"][0].get("approvalAuthority").is_none());app.db.close().await;
    }
}