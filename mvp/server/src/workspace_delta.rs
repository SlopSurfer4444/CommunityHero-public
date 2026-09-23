//! Diffs are computed only after both snapshots have passed actor filtering.
//! Rows are complete replacements, including conversation history and branch
//! context; inbox filters never alter the selected thread's transport.
use serde_json::{Map,Value,json};
use std::collections::HashMap;
const COLLECTIONS:[&str;8]=["items","posts","branches","proposals","operations","materials","conversations","jobs"];
fn keyed(rows:&Value)->Option<(&Vec<Value>,HashMap<&str,&Value>)> {
    let rows=rows.as_array()?;
    let mut keys=HashMap::new();
    for row in rows {let id=row["id"].as_str()?;if id.is_empty()||keys.insert(id,row).is_some(){return None;}}
    Some((rows,keys))
}
pub fn between(base:&Value,current:&Value,actor_id:&str)->Value {
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
            return json!({"kind":"full","snapshot":current});
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


