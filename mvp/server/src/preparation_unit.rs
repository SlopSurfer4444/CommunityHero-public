//! One canonical post or an exact currently proven publication family. No jobs,
//! transport, payments or new operation history are owned by this reducer.
use crate::*;
use std::collections::{BTreeMap,BTreeSet};

pub(crate) const CONTRACT:&str="strict_post_family_v1";
pub(crate) const MIXED:&str="Preparation requires one post or a proven family; use the strict preparation plan";
fn hash(value:&Value)->String{editorial_review::hash_text(&value.to_string())}

pub(crate) fn capture(d:&Value,ids:&[Value],at:&str)->Result<Value,&'static str>{
    if ids.is_empty()||ids.len()>100{return Err("Strict preparation requires 1 to 100 exact recipients");}
    let binding=active_binding(d).map_err(|_|"Strict preparation company binding unavailable")?;
    let mut selected=BTreeSet::new();let mut copies=BTreeMap::new();let mut recipients=Vec::new();
    for value in ids {
        let id=value.as_str().filter(|id|!id.is_empty()).ok_or("Strict preparation recipient invalid")?;
        if !selected.insert(id.to_owned()){return Err("Strict preparation recipient duplicated");}
        let item=row(d,"items",id).map_err(|_|"Strict preparation recipient missing")?;
        bound_item(&binding,item).map_err(|_|"Strict preparation recipient binding changed")?;
        if ["account","accountId"].iter().any(|key|item.get(*key).is_some_and(|v|!v.is_null()&&v!=&d["account"])) {
            return Err("Strict preparation recipient company changed");
        }
        let post_id=item["postId"].as_str().filter(|id|!id.is_empty()).ok_or("Strict preparation canonical post missing")?;
        let post=row(d,"posts",post_id).map_err(|_|"Strict preparation canonical post missing")?;
        if post.get("account").is_some_and(|v|!v.is_null()&&v!=&d["account"])
            ||post.get("connectorBinding").is_some_and(|v|!v.is_null()&&v!=&binding.to_json()) {
            return Err("Strict preparation post binding changed");
        }
        if let Some(branch_id)=item["branchId"].as_str().filter(|id|!id.is_empty()) {
            let branch=row(d,"branches",branch_id).map_err(|_|"Strict preparation branch missing")?;
            if branch["postId"]!=post["id"] {return Err("Strict preparation branch post differs");}
        }
        copies.entry(post_id.to_owned()).or_insert_with(||json!({"postId":post_id,
            "connectorBinding":post.get("connectorBinding").filter(|v|!v.is_null()).cloned().unwrap_or_else(||binding.to_json()),
            "sourceVersion":media_fullframes::source_version(post,d["account"].as_str().unwrap_or(""))}));
        recipients.push(json!({"itemId":id,"revision":item["revision"],"postId":post_id,"connectorBinding":item.get("connectorBinding").filter(|v|!v.is_null()).cloned().unwrap_or_else(||binding.to_json())}));
    }
    recipients.sort_by_key(|v|v["itemId"].as_str().unwrap().to_owned());
    let posts=copies.keys().cloned().collect();
    let (kind,family_key,proofs)=if copies.len()==1 {("post",Value::Null,json!([]))}else{
        knowledge::validate_catalog(d)?;
        let evidence=knowledge::preparation_family_evidence(d,&posts,at)?;
        let keys:BTreeSet<_>=copies.keys().map(|id|evidence.families.get(id).cloned().ok_or(MIXED)).collect::<Result<_,_>>()?;
        if keys.len()!=1{return Err(MIXED);}
        let key=keys.into_iter().next().unwrap();
        if !key.starts_with("speech:"){return Err(MIXED);}
        let pins=copies.keys().map(|id|json!({"postId":id,"supports":evidence.supports.get(id).cloned().unwrap_or(json!([]))})).collect::<Vec<_>>();
        if pins.iter().any(|pin|pin["supports"].as_array().is_none_or(Vec::is_empty)){return Err(MIXED);}
        ("family",json!(key),json!(pins))
    };
    let mut unit=json!({"version":1,"contract":CONTRACT,"account":d["account"],"kind":kind,
        "itemIds":selected,"recipients":recipients,"copies":copies.into_values().collect::<Vec<_>>(),
        "familyKey":family_key,"familyProof":proofs});
    unit["unitSha256"]=json!(hash(&unit));Ok(unit)
}
pub(crate) fn current(d:&Value,unit:&Value,ids:&[Value],at:&str)->Result<(),&'static str>{
    if unit["version"]!=1||unit["contract"]!=CONTRACT {return Err("Unsupported strict preparation unit contract");}
    if capture(d,ids,at)?!=*unit {return Err("Strict preparation unit or family proof changed before model call");}
    Ok(())
}
pub(crate) fn attach(d:&Value,bundle:&mut Value,at:&str)->Result<(),&'static str>{
    let ids=bundle["itemIds"].as_array().ok_or("Strict preparation recipients missing")?;
    let unit=capture(d,ids,at)?;
    bundle["request"]["strictGroupContract"]=json!(CONTRACT);
    bundle["request"]["strictGroup"]=unit;
    bundle["digest"]=json!(hash(&bundle["request"]));Ok(())
}
pub(crate) fn current_request(d:&Value,request:&Value,at:&str)->Result<(),&'static str>{
    if request["strictGroupContract"]!=CONTRACT{return Err("Strict preparation policy proof missing or unsupported");}
    let ids=request["items"].as_array().ok_or("Strict preparation request recipients missing")?.iter().map(|i|i["id"].clone()).collect::<Vec<_>>();
    current(d,&request["strictGroup"],&ids,at)
}
pub(crate) fn current_bundle(d:&Value,bundle:&Value,at:&str)->Result<(),&'static str>{
    if bundle["digest"]!=hash(&bundle["request"]){return Err("Strict preparation captured request digest changed");}
    let ids=bundle["itemIds"].as_array().ok_or("Strict preparation captured recipients missing")?;
    current(d,&bundle["request"]["strictGroup"],ids,at)?;
    current_request(d,&bundle["request"],at)
}

#[cfg(test)]
#[path="preparation_unit_tests.rs"]
mod tests;
