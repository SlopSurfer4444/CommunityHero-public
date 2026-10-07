//! A new research job may yield the preparation lane only while its exact
//! durable attempt and current company/recipient evidence remain provable.
//! The capture never grants another attempt or changes a retained paid record.
use super::*;
use std::collections::BTreeMap;

pub(super) fn capture(d:&Value,job:&Value)->Option<Value> {
    if job["kind"]!="assistant" || job["purpose"]!="public_fact_followup"
        || !matches!(job["status"].as_str(),Some("queued"|"running")) {return None;}
    let id=job["id"].as_str().filter(|s|!s.is_empty())?;
    let parent=row(d,"jobs",job["parentPrepareJobId"].as_str()?).ok()?;
    let bundle=&parent["prepareBundle"];
    if parent["kind"]!="assistant" || parent["status"]!="completed"
        || !matches!(parent["purpose"].as_str(),Some("engine_prepare"|"auto_prepare"))
        || bundle["digest"]!=hash(&bundle["request"])
        || bundle["request"]["account"]!=d["account"]
        || bundle["request"]["connectorBinding"]!=active_binding(d).ok()?.to_json()
        || parent["preparationStages"]["first"]["status"]!="completed" {return None;}
    if parent["purpose"]=="auto_prepare" {automatic_parent(d,parent).ok()?;}
    conductor_authority::fence_job_capture(d,id,"prepare").ok()?;
    let reservation=preparation_reservations::capture(d,parent["id"].as_str()?).ok()?;
    if reservation!=parent["scopeReservation"] {return None;}
    let dependencies=job["factDependencyIds"].as_array().filter(|v|!v.is_empty()&&v.len()<=100)?;
    let recipients=job["requestedItemIds"].as_array().filter(|v|v.len()==dependencies.len())?;
    let signatures=job["factSignatures"].as_array().filter(|v|v.len()==dependencies.len())?;
    let mut seen=BTreeSet::new();let mut items=BTreeSet::new();let mut posts=BTreeSet::new();
    let mut keys=BTreeSet::new();let mut attempts=Vec::new();
    for dependency in dependencies {
        let dependency=dependency.as_str().filter(|s|!s.is_empty())?;
        if !seen.insert(dependency) {return None;}
        let entries:Vec<_>=rows(parent,"factFollowups").iter().filter(|e|e["id"]==dependency).collect();
        if entries.len()!=1 {return None;} let entry=entries[0];
        let item=entry["itemId"].as_str().filter(|s|!s.is_empty())?;
        if !items.insert(item.to_owned()) || !recipients.contains(&json!(item))
            || entry["prepareJobId"]!=parent["id"] || entry["bundleId"]!=bundle["id"]
            || entry["bundleDigest"]!=bundle["digest"] || entry["kind"]!="missing_public_fact"
            || entry["status"]!="researching" || !entry["consumedByJobId"].is_null()
            || !signatures.contains(&json!({"id":dependency,"signature":entry["signature"]}))
            || job["factGroupKey"]!=group_key(entry) {return None;}
        current_dependency(d,entry).ok()?;
        let saved:Vec<_>=rows(&bundle["request"],"items").iter().filter(|v|v["id"]==item).collect();
        if saved.len()!=1 || saved[0]["postId"]!=entry["binding"]["postId"]
            || row(d,"items",item).ok()?["postId"]!=entry["binding"]["postId"]
            || !rows(&parent["preparationStages"]["first"]["result"],"factDependencies").contains(&entry["binding"]["declaration"])
            || job["researchRequest"]!=json!({"account":d["account"],"query":entry["binding"]["declaration"]["publicQuery"],"factResearchContract":SOURCE_CONTRACT}) {return None;}
        let ledger=entry["attempts"].as_array().filter(|v|!v.is_empty()&&v.len()<=MAX_ATTEMPTS)?;
        for (index,attempt) in ledger.iter().enumerate() {
            if attempt["attempt"]!=json!(index+1) || timestamp(attempt["createdAt"].as_str()?).is_err() {return None;}
            if index+1<ledger.len() && row(d,"jobs",attempt["jobId"].as_str()?).ok()?["status"]!="interrupted" {return None;}
        }
        if ledger.last()?["jobId"]!=id {return None;}
        if parent["purpose"]=="auto_prepare" && (ledger.len()!=1 || !automatic_eligible(d,parent,entry,&now())) {return None;}
        let captures:Vec<_>=rows(&reservation,"recipients").iter().filter(|v|v["itemId"]==item).collect();
        if captures.len()!=1 {return None;}
        let captured: BTreeSet<String>=captures[0]["keys"].as_array()?.iter().map(|k|k.as_str().map(str::to_owned)).collect::<Option<_>>()?;
        if captured!=preparation_reservations::recipient_keys(d,row(d,"items",item).ok()?).ok()? {return None;}
        keys.extend(captured);posts.insert(entry["binding"]["postId"].as_str()?.to_owned());
        attempts.push(json!({"dependencyId":dependency,"signature":entry["signature"],"attempts":ledger}));
    }
    // This supplies the canonical immutable-target alias and UNKNOWN guard;
    // current_dependency alone intentionally is not an alias resolver.
    preparation_reservations::assert_available(d,&items.iter().cloned().collect::<Vec<_>>(),None).ok()?;
    Some(json!({"version":1,"account":d["account"],"connectorBinding":bundle["request"]["connectorBinding"],
        "jobId":id,"parentJobId":parent["id"],"bundleDigest":bundle["digest"],"researchRequest":job["researchRequest"],
        "groupKey":job["factGroupKey"],"attempts":attempts,"itemIds":items,"postIds":posts,"recipientKeys":keys}))
}

pub(super) fn keys(d:&Value,job:&Value,families:&BTreeMap<String,String>)->Option<BTreeSet<String>> {
    let saved=job.get("factWorkerScope")?;
    if *saved!=capture(d,job)? {return None;}
    let recipients:BTreeSet<String>=saved["recipientKeys"].as_array()?.iter().map(|k|k.as_str().map(str::to_owned)).collect::<Option<_>>()?;
    let mut posts:BTreeSet<String>=saved["postIds"].as_array()?.iter().map(|p|p.as_str().map(str::to_owned)).collect::<Option<_>>()?;
    for item in list(d,"items") {
        let keys=preparation_reservations::recipient_keys(d,item).ok()?;
        if !keys.is_disjoint(&recipients) {posts.insert(item["postId"].as_str()?.to_owned());}
    }
    let selected:BTreeSet<String>=posts.iter().map(|post|families.get(post).cloned()).collect::<Option<_>>()?;
    Some(families.iter().filter(|(_,family)|selected.contains(*family))
        .map(|(post,_)|json!([d["account"],"post",post]).to_string()).collect())
}
