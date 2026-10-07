//! Read-only, byte- and image-aware preparation planning. No model, job or approval writes.
use crate::*;
use std::collections::{BTreeMap,BTreeSet};

pub(crate) fn capacity_error(error: &str) -> bool {
    error.starts_with("Selected assistant evidence exceeds")
        || error == engine_prepare::IMAGE_CAPACITY_ERROR
        || error == "Selected material evidence exceeds 300 records"
        || error == "Branch evidence exceeds 300 messages"
        || error == "Evidence contains a string exceeding 24000 characters; shorten the attachment explicitly"
}

// Preserve post adjacency as the packing unit, separately from the branch
// dependency groups used at admission. Bound a large post before measuring its
// exact request; byte/image limits may split it further without trimming it.
pub(crate) const AUTO_GROUP_MAX_ITEMS: usize = 100;
/// Discover affinity over the remaining finite campaign before selecting a
/// recipient window. This reads source evidence only, never captures a request.
pub(crate) fn family_windows(d:&Value,ids:&[String],batch_size:usize,max_batches:usize,at:&str)->ApiResult<Value>{
    if ids.is_empty() || ids.len()>5000 || !(1..=100).contains(&batch_size) || !(1..=preparation_workers::MAX_WORKERS).contains(&max_batches) {
        return Err(bad("Invalid family selection bounds"));
    }
    let binding=active_binding(d)?;
    bridge_account(&binding)?;
    let mut index=BTreeMap::new();
    for item in list(d,"items") {
        if let Some(id)=item["id"].as_str(){
            if index.insert(id,item).is_some(){return Err(conflict("Duplicate family recipient identity"));}
        }
    }
    let mut seen=std::collections::BTreeSet::new();
    let mut selected=Vec::with_capacity(ids.len());
    for id in ids {
        if id.is_empty()||id.len()>128||id.trim()!=id||id.contains(',')||id.chars().any(char::is_control)||!seen.insert(id) {
            return Err(bad("Invalid family selection recipient"));
        }
        let item=index.get(id.as_str()).ok_or_else(||bad("Family selection recipient missing"))?;
        bound_item(&binding,item)?;
        if ["account","accountId"].iter().any(|key|item.get(*key).is_some_and(|value|value!=&d["account"])) {
            return Err(conflict("Family selection company differs"));
        }
        selected.push(*item);
    }
    selected.sort_by_key(|item|(item["createdAt"].as_str().unwrap_or(""),item["id"].as_str().unwrap_or("")));
    let posts=selected.iter().filter_map(|item|item["postId"].as_str().filter(|id|!id.is_empty()).map(str::to_owned)).collect();
    // Invalid affinity evidence falls back to ordinary post identity. Capture
    // still checks all evidence and can hold the affected recipient explicitly.
    let families=knowledge::validate_catalog(d).and_then(|_|knowledge::preparation_families(d,&posts,at)).unwrap_or_default();
    let mut positions=BTreeMap::new();let mut windows:Vec<Vec<String>>=Vec::new();
    for item in selected {
        let id=item["id"].as_str().unwrap();
        let post=item["postId"].as_str().filter(|id|!id.is_empty());
        let key=(post.is_some(),post.map(|post|families.get(post).cloned().unwrap_or_else(||post.to_owned())).unwrap_or_else(||id.to_owned()));
        let position=if let Some(position)=positions.get(&key){*position}else{
            if windows.len()==max_batches {continue;}
            let position=windows.len();positions.insert(key,position);windows.push(Vec::new());position
        };
        if windows[position].len()<batch_size {windows[position].push(id.to_owned());}
    }
    Ok(json!({"account":d["account"],"selectedItemIds":ids,"windows":windows,"advisory":true}))
}

pub(crate) async fn conductor_family_windows(State(app):State<App>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    let ids=body["itemIds"].as_array().ok_or_else(||bad("Family recipients required"))?.iter()
        .map(|id|id.as_str().map(str::to_owned).ok_or_else(||bad("Invalid family recipient"))).collect::<ApiResult<Vec<_>>>()?;
    let size=body["batchSize"].as_u64().and_then(|v|usize::try_from(v).ok()).ok_or_else(||bad("Family batch size required"))?;
    let count=body["maxBatches"].as_u64().and_then(|v|usize::try_from(v).ok()).ok_or_else(||bad("Family window count required"))?;
    // Preserve the two-window legacy pipeline, while permitting one independent
    // family per configured preparation worker in the parallel pipeline.
    let window_limit=app.preparation_workers.width().max(2);
    if ids.is_empty()||ids.len()>5000||!(1..=100).contains(&size)||!(1..=window_limit).contains(&count){return Err(bad("Invalid family selection bounds"));}
    let d=app.db.read_preparation_families(&ids).await?;
    Ok(Json(family_windows(&d,&ids,size,count,&now())?))
}

pub(crate) fn automatic_groups(d: &Value, ids: &[String]) -> Vec<Vec<String>> {
    automatic_family_groups_at(d,ids,&now()).into_iter().map(|(_,ids)|ids).collect()
}
// Retain the affinity key alongside each bounded post group. Every new
// answering producer must
// stop at this boundary rather than fill a family with unrelated evidence.
fn automatic_family_groups_at(d:&Value,ids:&[String],at:&str)->Vec<((bool,String),Vec<String>)> {
    let mut groups: Vec<((bool, String), Vec<String>)> = Vec::new();
    for id in ids {
        let Ok(item) = row(d, "items", id) else { continue; };
        let post = item["postId"].as_str().filter(|post| !post.is_empty());
        // A missing post is an individual source, never an alias of a real post.
        let key = (post.is_some(), post.unwrap_or(id).to_owned());
        if let Some((_, members)) = groups.iter_mut().find(|(candidate, _)| candidate == &key) {
            members.push(id.clone());
        } else {
            groups.push((key, vec![id.clone()]));
        }
    }
    let posts=groups.iter().filter(|(key,_)|key.0).map(|(key,_)|key.1.clone()).collect();
    // A broken affinity catalog cannot widen admission: ordinary independent
    // post ordering remains, and build_request still performs all source gates.
    let families=knowledge::preparation_families(d,&posts,at).unwrap_or_default();
    let mut order=BTreeMap::new();
    for (index,(key,_)) in groups.iter().enumerate(){
        let family=(key.0,families.get(&key.1).cloned().unwrap_or_else(||key.1.clone()));
        order.entry(family).or_insert(index);
    }
    groups.sort_by_key(|(key,_)|order[&(key.0,families.get(&key.1).cloned().unwrap_or_else(||key.1.clone()))]);
    groups.into_iter().flat_map(|(key, ids)| {
        let family=(key.0,families.get(&key.1).cloned().unwrap_or_else(||key.1.clone()));
        ids.chunks(AUTO_GROUP_MAX_ITEMS).map(|ids|(family.clone(),ids.to_vec())).collect::<Vec<_>>()
    }).collect()
}

/// Capture the exact requests that may be claimed in this transaction. Unlike
/// the advisory planner this keeps the captured bundle, and a malformed group
/// cannot prevent later independent groups from being considered.
pub(crate) fn automatic_bundles(d: &Value, ids: &[String]) -> (Vec<Value>, Vec<(String, &'static str)>) {
    captured_bundles(d,ids,None)
}

fn captured_bundles(d:&Value,ids:&[String],instruction:Option<&str>)->(Vec<Value>,Vec<(String,&'static str)>){
    fn capture(d: &Value, ids: &[String], instruction:Option<&str>, bundles: &mut Vec<Value>, held: &mut Vec<(String, &'static str)>) {
        if ids.is_empty() { return; }
        // Every recipient of one exact post needs the same complete carousel.
        // Splitting recipients cannot make this shared mandatory base fit.
        let posts:BTreeSet<_>=ids.iter().filter_map(|id|row(d,"items",id).ok().and_then(|i|i["postId"].as_str())).collect();
        if posts.len()==1 && posts.iter().next().and_then(|id|row(d,"posts",id).ok()).is_some_and(|post|
            post["attachments"].as_array().into_iter().flatten().filter(|a|matches!(a["type"].as_str(),Some("photo"|"image"))).count()>engine_prepare::MAX_REQUEST_IMAGES) {
            held.extend(ids.iter().cloned().map(|id|(id,engine_prepare::IMAGE_CAPACITY_ERROR)));return;
        }
        let values: Vec<Value> = ids.iter().map(|id| json!(id)).collect();
        match engine_prepare::build_request(d, &values, instruction) {
            Ok(bundle) => bundles.push(bundle),
            Err(error) if capacity_error(error) && ids.len() > 1 => {
                let mid = ids.len() / 2;
                capture(d, &ids[..mid], instruction, bundles, held);
                capture(d, &ids[mid..], instruction, bundles, held);
            }
            Err(error) => held.extend(ids.iter().cloned().map(|id| (id, error))),
        }
    }
    let (mut bundles, mut held) = (Vec::new(), Vec::new());
    capture(d, ids, instruction, &mut bundles, &mut held);
    (bundles, held)
}

/// Capture the next automatic model call from one admitted post family.
/// Return at most one real scheduler bundle plus encountered holds. Stop at
/// the first family boundary or valid group that does not fit; the unclaimed tail stays queued for
/// later passes instead of repeatedly capturing the entire backlog. A malformed
/// post is held on its own. Admission still tracks branch dependencies.
pub(crate) fn automatic_packed_bundles(d: &Value, ids: &[String]) -> (Vec<Value>, Vec<(String, &'static str)>) {
    family_packed_bundles(d,ids,None,Some(1))
}

// The producer takes only its next capture; a conductor needs the complete
// advisory plan. Both retain the same verified family boundary, source guards
// and splitting behavior. This helper never claims or creates a durable job.
fn family_packed_bundles(d:&Value,ids:&[String],instruction:Option<&str>,limit:Option<usize>)->(Vec<Value>,Vec<(String,&'static str)>){
    let mut captured=Vec::new();
    let mut held = Vec::new();
    let mut pending: Option<Value> = None;
    let mut pending_family = None;
    for (family,group) in automatic_family_groups_at(d, ids, &now()) {
        if pending.is_some() && pending_family.as_ref()!=Some(&family) {
            captured.extend(pending.take());
            if limit.is_some_and(|limit|captured.len()>=limit){return(captured,held);}
        }
        if pending.as_ref().is_some_and(|bundle|bundle["itemIds"].as_array().is_some_and(|items|items.len()==AUTO_GROUP_MAX_ITEMS)) {
            captured.extend(pending.take());
            if limit.is_some_and(|limit|captured.len()>=limit){return(captured,held);}
        }
        let (bundles, group_held) = captured_bundles(d, &group, instruction);
        held.extend(group_held);
        for bundle in bundles {
            let Some(previous) = pending.take() else {
                pending_family=Some(family.clone());pending=Some(bundle);continue;
            };
            // Both arrays come from build_request. A failed merge never turns
            // either independently valid capture into an invalid-source hold.
            let mut selected = previous["itemIds"].as_array().expect("captured item IDs").clone();
            selected.extend(bundle["itemIds"].as_array().expect("captured item IDs").iter().cloned());
            if selected.len() <= AUTO_GROUP_MAX_ITEMS {
                if let Ok(merged) = engine_prepare::build_request(d, &selected, instruction) {
                    pending = Some(merged);
                    continue;
                }
            }
            captured.push(previous);
            if limit.is_some_and(|limit|captured.len()>=limit){return(captured,held);}
            pending_family=Some(family.clone());pending=Some(bundle);
        }
    }
    captured.extend(pending);(captured, held)
}

fn partition<F>(ids: &[String], measure: &mut F, batches: &mut Vec<Value>, held: &mut Vec<Value>) -> Result<(), &'static str>
where F: FnMut(&[String]) -> Result<usize, &'static str> {
    if ids.is_empty() { return Ok(()); }
    match measure(ids) {
        Ok(bytes) => batches.push(json!({"itemIds":ids,"bytes":bytes})),
        Err(error) if capacity_error(error) => split_oversized(ids,error,measure,batches,held)?,
        Err(error) => return Err(error),
    }
    Ok(())
}

fn split_oversized<F>(ids: &[String], error: &'static str, measure: &mut F, batches: &mut Vec<Value>, held: &mut Vec<Value>) -> Result<(), &'static str>
where F: FnMut(&[String]) -> Result<usize, &'static str> {
    if ids.len() > 1 {
        let mid = ids.len()/2;
        partition(&ids[..mid],measure,batches,held)?;
        partition(&ids[mid..],measure,batches,held)?;
    } else {
        held.push(json!({"itemId":ids[0],"reason":
            if error == engine_prepare::IMAGE_CAPACITY_ERROR { "image_budget_exceeded" } else { "evidence_too_large" }}));
    }
    Ok(())
}

fn pack_groups<F>(groups: &[(String,Vec<String>)], measure: &mut F, batches: &mut Vec<Value>, held: &mut Vec<Value>) -> Result<(), &'static str>
where F: FnMut(&[String]) -> Result<usize, &'static str> {
    let mut current = Vec::new();
    let mut current_bytes = 0;
    for (_,ids) in groups {
        if ids.is_empty() { continue; }
        let mut candidate = current.clone();candidate.extend(ids.iter().cloned());
        let mut measured = measure(&candidate);
        if matches!(measured,Err(error) if capacity_error(error)) && !current.is_empty() {
            batches.push(json!({"itemIds":current,"bytes":current_bytes}));
            current.clear();
            candidate = ids.clone();measured = measure(&candidate);
        }
        match measured {
            Ok(bytes) => { current = candidate;current_bytes = bytes; }
            // Split only a source group that cannot fit by itself. Never trim
            // its evidence or repeatedly remeasure the same failed candidate.
            Err(error) if capacity_error(error) => split_oversized(&candidate,error,measure,batches,held)?,
            Err(error) => return Err(error),
        }
    }
    if !current.is_empty() { batches.push(json!({"itemIds":current,"bytes":current_bytes})); }
    Ok(())
}

pub(crate) fn build(d: &Value, body: &Value) -> ApiResult<Value> {
    build_strict(d,body)
}

/// A conductor may share captured evidence only within a verified publication
/// family; ordinary and conductor selections share this same native policy.
pub(crate) fn conductor_build(d:&Value,body:&Value)->ApiResult<Value>{
    build_strict(d,body)
}

fn build_strict(d:&Value,body:&Value)->ApiResult<Value>{
    let input = engine_prepare::parse(body)?;
    let binding = active_binding(d)?;
    bridge_account(&binding)?;
    let mut items = Vec::new();
    for id in &input.item_ids {
        let item = row(d,"items",id)?;
        bound_item(&binding,item)?;
        items.push(item.clone());
    }
    let media = media_queue::preparation_states(d,&items,&now())?;
    let operation_holds = engine_prepare::operation_holds(d,&input.item_ids);
    let mut held = Vec::new();
    // Keep whole related posts together when they fit; account and connector are
    // already bound. A post key is never used as a cross-company identity.
    let mut eligible = Vec::new();
    for item in &items {
        let id = required(item,"id")?;
        if let Some(reason) = operation_holds.get(id) {
            held.push(json!({"itemId":id,"reason":reason}));
            continue;
        }
        if let Some(reason) = media.get(id).copied().flatten() {
            // Match actual scheduling: incomplete default media still permits
            // exact semantic assessment. Only current owner prerequisites hold
            // preparation; final proposals retain their exact dependency gate.
            if !decision_media::may_assess(d,item).map_err(conflict)? {
                held.push(json!({"itemId":id,"reason":reason}));
                continue;
            }
        }
        if let Err(error) = preparation_reservations::assert_available(d, &[id.to_owned()], None) {
            if ["Preparation scope is reserved by prior paid or unfinished work",
                "Preparation scope conflicts with an unresolved operation",
                "Preparation scope conflicts with an existing proposal"].contains(&error.1.as_str()) {
                held.push(json!({"itemId":id,"reason":"preparation_scope_reserved","detail":error.1}));
                continue;
            }
            return Err(error);
        }
        eligible.push(id.to_owned());
    }
    let mut batches = Vec::new();
    let(bundles,source_held)=family_packed_bundles(d,&eligible,input.instruction.as_deref(),None);
    batches.extend(bundles.into_iter().map(|bundle|json!({"itemIds":bundle["itemIds"],"bytes":engine_prepare::model_request_bytes(&bundle["request"]),"strictGroup":bundle["request"]["strictGroup"]})));
    held.extend(source_held.into_iter().map(|(id,error)|json!({"itemId":id,"reason":
        if error==engine_prepare::IMAGE_CAPACITY_ERROR{"image_budget_exceeded"}
        else if capacity_error(error){"evidence_too_large"}else{error}})));
    Ok(json!({"account":d["account"],"strictGroupContract":preparation_unit::CONTRACT,"policyVersion":1,"byteLimit":engine_prepare::MAX_REQUEST_BYTES,
        "batches":batches,"held":held,"selectedItemIds":input.item_ids,"advisory":true}))
}

pub(crate) async fn plan(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    plan_strict(app,body).await
}

pub(crate) async fn conductor_plan(State(app):State<App>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    plan_strict(app,body).await
}

async fn plan_strict(app:App,body:Value)->ApiResult<Json<Value>>{
    let input=engine_prepare::parse(&body)?;
    // One consistent evidence snapshot without unrelated paid requests/chat
    // history. Actual preparation repeats the checks in its writer transaction.
    let d = app.db.read_preparation_plan(&input.item_ids).await?;
    let building = performance::Span::new("preparation.plan.build");
    let plan=build(&d,&body)?;
    drop(building);
    let _refining = performance::Span::new("preparation.plan.refine");
    Ok(Json(engine_prepare::capacity::refine(&app,&d,plan,input.instruction.as_deref()).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting_has_exact_coverage_and_does_not_truncate_an_oversized_recipient() {
        let ids: Vec<String> = ["a","huge","b","c"].into_iter().map(str::to_owned).collect();
        let (mut batches,mut held)=(Vec::new(),Vec::new());
        partition(&ids,&mut |ids| {
            if ids.iter().any(|s|s=="huge")||ids.len()>2 {Err("Selected assistant evidence exceeds the 550000-byte budget; reduce attachments")}
            else {Ok(ids.len()*100)}
        },&mut batches,&mut held).unwrap();
        assert_eq!(batches,json!([{"itemIds":["a"],"bytes":100},{"itemIds":["b","c"],"bytes":200}]).as_array().unwrap().clone());
        assert_eq!(held,json!([{"itemId":"huge","reason":"evidence_too_large"}]).as_array().unwrap().clone());
    }

    #[test]
    fn malformed_or_missing_evidence_is_not_mislabeled_as_capacity() {
        let mut calls=0;
        let result=partition(&["a".into(),"b".into()],&mut |_| {calls+=1;Err("Attached branch is missing")},&mut Vec::new(),&mut Vec::new());
        assert_eq!(result,Err("Attached branch is missing"));
        assert_eq!(calls,1);
        let mut calls=0;
        let result=pack_groups(&[("p".into(),vec!["a".into(),"b".into()])],&mut |_| {calls+=1;Err("Attached branch is missing")},&mut Vec::new(),&mut Vec::new());
        assert_eq!(result,Err("Attached branch is missing"));assert_eq!(calls,1);
    }

    #[test]
    fn oversized_group_holds_only_oversized_recipient_and_keeps_later_groups() {
        let groups=vec![("first".into(),vec!["a".into()]),("large".into(),vec!["huge".into(),"b".into(),"c".into()]),("last".into(),vec!["d".into()])];
        let (mut batches,mut held)=(Vec::new(),Vec::new());
        pack_groups(&groups,&mut |ids| {
            if ids.iter().any(|id|id=="huge")||ids.len()>2 {Err(engine_prepare::IMAGE_CAPACITY_ERROR)}
            else {Ok(ids.len()*100)}
        },&mut batches,&mut held).unwrap();
        assert_eq!(batches,json!([{"itemIds":["a"],"bytes":100},{"itemIds":["b","c"],"bytes":200},{"itemIds":["d"],"bytes":100}]).as_array().unwrap().clone());
        assert_eq!(held,json!([{"itemId":"huge","reason":"image_budget_exceeded"}]).as_array().unwrap().clone());
    }

    fn fixture() -> Value {
        let mut d=empty(); accounts::initialize(&mut d,accounts::Profile::BawRussia).unwrap();
        d["items"]=json!([{"id":"a","itemId":"a","objectId":"o","postId":"p","postKey":"p","branchId":"b","conversationKey":"b","workflow":"attention","providerStatus":"new","revision":1}]);
        d["posts"]=json!([{"id":"p","postKey":"p","objectId":"o","text":"Пост","attachments":[]}]);
        d["branches"]=json!([{"id":"b","postId":"p","messages":[{"id":"a","text":"Спасибо"}],"contextComplete":true}]);
        d
    }

    fn default_video_fixture() -> Value {
        let mut d=fixture();
        d["posts"][0]["attachments"]=json!([{"type":"video","url":"https://example.invalid/complete-source.mp4"}]);
        d
    }

    #[test]
    fn plan_and_scheduler_allow_default_missing_video_for_semantic_assessment() {
        let mut d=default_video_fixture();let body=json!({"itemIds":["a"]});
        assert!(media_queue::preparation_states(&d,&[d["items"][0].clone()],&now()).unwrap()["a"].is_some(),
            "exercise actual missing-media hold, not a text-only source");
        assert!(decision_media::may_assess(&d,&d["items"][0]).unwrap());
        let before=d.clone();
        for plan in [build(&d,&body).unwrap(),conductor_build(&d,&body).unwrap()] {
            assert_eq!(plan["held"],json!([]));
            assert_eq!(plan["batches"][0]["itemIds"],json!(["a"]));
        }
        assert_eq!(d,before,"planning cannot create paid work or media evidence");
        let bundle=engine_prepare::build_request(&d,&[json!("a")],None).unwrap();
        assert_eq!(bundle["request"]["decisionMediaContract"],decision_media::CONTRACT);
        assert_eq!(bundle["request"]["posts"][0]["decisionMediaEvidence"]["audioReady"],false);
        assert_eq!(bundle["request"]["posts"][0]["decisionMediaEvidence"]["visualProvided"],false);
        let scheduled=engine_prepare::schedule(&mut d,engine_prepare::parse(&body).unwrap()).unwrap();
        let job=row(&d,"jobs",&scheduled.job_id).unwrap();
        assert_eq!(job["selectedItemIds"],json!(["a"]));assert_eq!(job["held"],json!([]));
    }

    #[test]
    fn plan_and_scheduler_preserve_exact_owner_video_floors() {
        for mode in ["full_audio_only","full_audio_visual"] {
            let mut d=default_video_fixture();let body=json!({"itemIds":["a"]});
            let source=media_fullframes::source_version(&d["posts"][0],d["account"].as_str().unwrap());
            d["settings"]["postMediaPolicies"]=json!({"p":{
                "version":1,"revision":1,"status":"active","postId":"p","mode":mode,
                "account":d["account"],"connectorBinding":d["connectorBinding"],"sourceVersion":source
            }});
            assert!(!decision_media::may_assess(&d,&d["items"][0]).unwrap(),"{mode}");
            let before=d.clone();let plan=build(&d,&body).unwrap();
            assert_eq!(plan["batches"],json!([]));assert_eq!(plan["held"][0]["itemId"],"a");
            assert_eq!(plan["held"],conductor_build(&d,&body).unwrap()["held"]);
            assert_eq!(d,before);
            let scheduled=engine_prepare::schedule(&mut d,engine_prepare::parse(&body).unwrap()).unwrap();
            let job=row(&d,"jobs",&scheduled.job_id).unwrap();
            assert_eq!(job["selectedItemIds"],json!([]));assert_eq!(job["held"],plan["held"]);
            assert_eq!(d["materials"],before["materials"]);
        }
    }

    #[test]
    fn semantic_media_assessment_never_bypasses_existing_paid_or_unknown_ownership() {
        for guard in ["paid","unknown","dispatching","succeeded"] {
            let mut d=default_video_fixture();let body=json!({"itemIds":["a"]});
            assert!(decision_media::may_assess(&d,&d["items"][0]).unwrap());
            if guard=="paid" {
                engine_prepare::schedule(&mut d,engine_prepare::parse(&body).unwrap()).unwrap();
            } else {
                d["operations"]=json!([{"itemId":"a","status":guard}]);
            }
            let before=d.clone();
            for plan in [build(&d,&body).unwrap(),conductor_build(&d,&body).unwrap()] {
                assert_eq!(plan["batches"],json!([]),"{guard}");
                assert_eq!(plan["held"][0]["itemId"],"a");
                assert_eq!(plan["held"][0]["reason"],match guard {
                    "paid"=>"preparation_scope_reserved","unknown"=>"operation_outcome_unknown",
                    "dispatching"=>"operation_dispatch_in_progress",_=>"operation_already_succeeded"
                });
            }
            assert_eq!(d,before,"{guard}: advisory plan cannot replace the owner");
        }
    }


    fn family_fixture()->(Value,Vec<String>) {
        let mut d=fixture();let item=d["items"][0].clone();let branch=d["branches"][0].clone();
        d["posts"]=json!([
            {"id":"source","postKey":"youtube:source","platform":"YouTube","text":"Original caption","sourceUrl":"https://www.youtube.com/watch?v=AbCdEf123_-","attachments":[{"type":"video"}]},
            {"id":"unrelated","postKey":"vk:other","platform":"VK","text":"Original caption","attachments":[]},
            {"id":"target","postKey":"vk:target","platform":"VK","text":"Different platform caption","sourceUrl":"https://vk.com/video-1_1","attachments":[{"type":"video"},{"type":"photo","url":"https://cdn.example/target.jpg"}]}
        ]);
        d["items"]=json!([]);d["branches"]=json!([]);let mut ids=Vec::new();
        for (post,count) in [("source",40),("unrelated",70),("target",40)] {
            let mut b=branch.clone();b["id"]=json!(format!("branch-{post}"));b["postId"]=json!(post);d["branches"].as_array_mut().unwrap().push(b);
            let key=row(&d,"posts",post).unwrap()["postKey"].clone();
            for n in 0..count {let id=format!("{post}-{n}");let mut i=item.clone();i["id"]=json!(id);i["itemId"]=json!(id);
                i["postId"]=json!(post);i["postKey"]=key.clone();i["branchId"]=json!(format!("branch-{post}"));i["conversationKey"]=i["branchId"].clone();
                d["items"].as_array_mut().unwrap().push(i);ids.push(id);}
        }
        let source_version=media_fullframes::source_version(&d["posts"][0],"BAW Russia");
        let target_version=media_fullframes::source_version(&d["posts"][2],"BAW Russia");
        let at=now();
        d["materials"]=json!([{"id":"source-speech","account":"BAW Russia","kind":"transcript","postKey":"youtube:source","sourceUrl":d["posts"][0]["sourceUrl"],"text":"Admitted speech. ".repeat(500),
            "transcription":{"partial":false,"audioStatus":"transcribed","coverage":"full_audio","sourceVersion":source_version,"mediaDurationSeconds":120.0,"audioDurationSeconds":120.0}}]);
        knowledge::sync_catalog(&mut d,&at).unwrap();
        let head=d["knowledge_versions"][0].clone();
        d["settings"]["mediaAudioEquivalences"]=json!({"target":{"schemaVersion":1,"status":"active","revision":1,"account":"BAW Russia","connectorBinding":active_binding(&d).unwrap().to_json(),
            "targetPostId":"target","targetPostKey":"vk:target","targetSourceVersion":target_version,"sourcePostId":"source","sourcePostKey":"youtube:source","sourceVersion":source_version,
            "transcript":{"entryId":head["entryId"],"versionId":head["id"],"hash":head["hash"]}}});
        knowledge::save_instruction(&mut d,&json!({"requestId":"family-rule","title":"Rule","text":"Use the current company rule. ".repeat(100)}),&at).unwrap();
        (d,ids)
    }

    #[test]
    fn configured_four_windows_preserve_family_grouping_and_bounded_scope(){
        let(mut d,mut ids)=family_fixture();
        for index in 0..2 {
            let mut item=d["items"][0].clone();
            let item_id=format!("extra-item-{index}");let post_id=format!("extra-post-{index}");
            item["id"]=json!(item_id);item["postId"]=json!(post_id);
            item["createdAt"]=json!(format!("2026-10-01T12:00:0{index}Z"));
            list_mut(&mut d,"items").push(item);list_mut(&mut d,"posts").push(json!({"id":post_id}));
            ids.push(item_id);
        }
        let before=d.clone();let plan=family_windows(&d,&ids,100,4,&now()).unwrap();
        let windows=plan["windows"].as_array().unwrap();assert_eq!(windows.len(),4);
        let flattened:Vec<_>=windows.iter().flat_map(|w|w.as_array().unwrap()).collect();
        assert_eq!(flattened.len(),flattened.iter().map(|v|v.as_str().unwrap()).collect::<std::collections::BTreeSet<_>>().len());
        assert!(windows.iter().all(|w|w.as_array().unwrap().len()<=100));
        assert!(flattened.iter().all(|id|ids.iter().any(|source|id.as_str()==Some(source.as_str()))));
        assert_eq!(d,before,"read-only planning does not reserve or mutate candidates");
        assert!(family_windows(&d,&ids,100,preparation_workers::MAX_WORKERS+1,&now()).is_err());
    }
    #[test]
    fn family_selection_precedes_recipient_window_and_preserves_three_platform_sources(){
        let(mut d,mut ids)=family_fixture();
        for (n,id) in ids.iter().enumerate(){row_mut(&mut d,"items",id).unwrap()["createdAt"]=json!(format!("2020-01-01T00:{:02}:{:02}Z",n/60,n%60));}
        let mut post=row(&d,"posts","target").unwrap().clone();
        post["id"]=json!("third");post["postKey"]=json!("instagram:third");post["platform"]=json!("Instagram");
        post["text"]=json!("Third platform archive caption, no current offer");post["sourceUrl"]=json!("https://instagram.com/p/third");
        post["attachments"]=json!([{"type":"video"}]);
        let version=media_fullframes::source_version(&post,"BAW Russia");list_mut(&mut d,"posts").push(post);
        let mut edge=d["settings"]["mediaAudioEquivalences"]["target"].clone();
        edge["targetPostId"]=json!("third");edge["targetPostKey"]=json!("instagram:third");edge["targetSourceVersion"]=json!(version);
        d["settings"]["mediaAudioEquivalences"]["third"]=edge;
        let mut branch=row(&d,"branches","branch-target").unwrap().clone();branch["id"]=json!("branch-third");branch["postId"]=json!("third");
        list_mut(&mut d,"branches").push(branch);
        for n in 0..20 {
            let mut item=row(&d,"items","target-0").unwrap().clone();let id=format!("third-{n}");
            item["id"]=json!(id);item["itemId"]=json!(id);item["postId"]=json!("third");item["postKey"]=json!("instagram:third");
            item["branchId"]=json!("branch-third");item["conversationKey"]=json!("branch-third");item["createdAt"]=json!("2020-01-02T00:00:00Z");
            list_mut(&mut d,"items").push(item);ids.push(id);
        }
        let before=d.clone();let selected=family_windows(&d,&ids,100,2,&now()).unwrap();
        assert_eq!(selected["selectedItemIds"],json!(ids));
        let first=selected["windows"][0].as_array().unwrap();assert_eq!(first.len(),100);
        assert!(first.iter().all(|id|!id.as_str().unwrap().starts_with("unrelated")));
        assert_eq!(selected["windows"][1].as_array().unwrap().len(),70);
        let captured=engine_prepare::build_request(&d,first,None).unwrap();
        assert_eq!(captured["request"]["posts"].as_array().unwrap().len(),3);
        assert_eq!(captured["request"]["materials"].as_array().unwrap().iter().filter(|m|m["kind"]=="transcript").count(),1);
        for post in captured["request"]["posts"].as_array().unwrap(){
            let original=row(&d,"posts",post["id"].as_str().unwrap()).unwrap();
            for field in ["text","platform","sourceUrl"]{assert_eq!(post[field],original[field]);}
        }
        assert_eq!(d,before);
    }

    #[test]
    fn oversized_family_window_continuation_revisits_oldest_unrelated_work(){
        let(mut d,mut ids)=family_fixture();
        for (n,id) in ids.iter().enumerate(){row_mut(&mut d,"items",id).unwrap()["createdAt"]=json!(format!("2020-01-01T00:{:02}:{:02}Z",n/60,n%60));}
        for n in 0..130 {
            let mut item=row(&d,"items","source-0").unwrap().clone();let id=format!("large-{n}");
            item["id"]=json!(id);item["itemId"]=json!(id);item["createdAt"]=json!("2020-01-02T00:00:00Z");
            list_mut(&mut d,"items").push(item);ids.push(id);
        }
        let selected=family_windows(&d,&ids,100,1,&now()).unwrap();let claimed=selected["windows"][0].as_array().unwrap();
        assert_eq!(claimed.len(),100);assert!(claimed.iter().all(|id|!id.as_str().unwrap().starts_with("unrelated")));
        let remaining:Vec<_>=ids.iter().filter(|id|!claimed.contains(&json!(id))).cloned().collect();
        let resumed=family_windows(&d,&remaining,100,1,&now()).unwrap();
        assert!(resumed["windows"][0].as_array().unwrap().iter().all(|id|id.as_str().unwrap().starts_with("unrelated")));
        assert_eq!(resumed,family_windows(&d,&remaining,100,1,&now()).unwrap());
        let mut foreign=d.clone();row_mut(&mut foreign,"items",&remaining[0]).unwrap()["account"]=json!("Other company");
        assert!(family_windows(&foreign,&remaining,100,1,&now()).is_err());
    }

    #[test]
    fn admitted_family_affinity_reuses_context_across_call_boundaries_without_retargeting(){
        let (d,ids)=family_fixture();let before=d.clone();
        let groups=automatic_groups(&d,&ids);
        assert_eq!(groups.iter().map(|g|g[0].as_str()).collect::<Vec<_>>(),vec!["source-0","target-0","unrelated-0"]);
        let (first,held)=automatic_packed_bundles(&d,&ids);assert!(held.is_empty());
        assert_eq!(first[0]["itemIds"].as_array().unwrap().len(),80);
        assert_eq!(first[0]["request"]["posts"].as_array().unwrap().len(),2);
        assert_eq!(first[0]["request"]["branches"].as_array().unwrap().len(),2);
        assert_eq!(engine_prepare::image_count(&first[0]["request"]),1,"the target copy's mandatory photo remains in the answering context");
        assert_eq!(first[0]["request"]["materials"].as_array().unwrap().iter().filter(|m|m["kind"]=="transcript").count(),1);
        assert_eq!(first[0]["request"]["materials"].as_array().unwrap().iter().filter(|m|m["kind"]=="rule").count(),1);
        assert!(prepare_bundle::current(&d,&first[0]).is_ok());
        let (tail,held)=automatic_packed_bundles(&d,&ids[40..110]);assert!(held.is_empty());
        assert_eq!(tail[0]["itemIds"].as_array().unwrap().len(),70);
        let old:Vec<_>=[&ids[..40],&ids[40..110],&ids[110..]].into_iter().map(|g|engine_prepare::build_request(&d,&g.iter().map(|id|json!(id)).collect::<Vec<_>>(),None).unwrap()).collect();
        let old_bytes:usize=old.iter().map(|b|engine_prepare::model_request_bytes(&b["request"])).sum();
        let new_bytes:usize=first.iter().chain(tail.iter()).map(|b|engine_prepare::model_request_bytes(&b["request"])).sum();
        assert!(new_bytes<old_bytes);
        println!("family fixture: calls 3 -> 2; model bytes {old_bytes} -> {new_bytes}; transcript copies 2 -> 1; rule copies 3 -> 2");
        assert_eq!(d,before);
    }

    #[test]
    fn automatic_family_boundary_preserves_spare_capacity_continuation_and_explicit_selection(){
        let (mut d,ids)=family_fixture();
        let unselected=d["items"][110].clone();
        d["items"].as_array_mut().unwrap().push({let mut item=unselected;item["id"]=json!("unselected-copy");item});
        // Source and target share admitted speech, while the unrelated family
        // fits the remaining count/byte/image capacity. It must still wait.
        let selected:Vec<_>=[&ids[..10],&ids[40..45],&ids[110..120]].concat();
        let before=d.clone();
        let (first,held)=automatic_packed_bundles(&d,&selected);assert!(held.is_empty());
        let expected:Vec<_>=[&ids[..10],&ids[110..120]].concat();
        assert_eq!(first[0]["itemIds"],json!(expected));
        assert_eq!(first[0]["request"]["posts"].as_array().unwrap().len(),2);
        for post in first[0]["request"]["posts"].as_array().unwrap(){
            let source=row(&d,"posts",post["id"].as_str().unwrap()).unwrap();
            for key in ["text","platform","sourceUrl"]{assert_eq!(post[key],source[key]);}
        }
        let remaining:Vec<_>=selected.iter().filter(|id|!expected.contains(id)).cloned().collect();
        let (next,held)=automatic_packed_bundles(&d,&remaining);assert!(held.is_empty());
        assert_eq!(next[0]["itemIds"],json!(&ids[40..45]));
        let plan=build(&d,&json!({"itemIds":selected})).unwrap();
        assert_eq!(plan["batches"].as_array().unwrap().len(),2,"every public producer separates unrelated posts");
        assert_eq!(plan["batches"][0]["itemIds"].as_array().unwrap().len(),20);
        assert_eq!(d,before,"capturing does not claim or remove queued recipients");
        // First-seen eligible family is chosen, rather than a favored platform
        // or a large shared-media family overtaking an earlier independent one.
        let reversed:Vec<_>=[&ids[40..45],&ids[..10],&ids[110..120]].concat();
        let (earlier,held)=automatic_packed_bundles(&d,&reversed);assert!(held.is_empty());
        assert_eq!(earlier[0]["itemIds"],json!(&ids[40..45]));
    }

    #[test]
    fn conductor_plan_separates_unrelated_families_even_when_they_both_fit(){
        let(d,ids)=family_fixture();let selected=vec![ids[0].clone(),ids[40].clone()];let body=json!({"itemIds":selected});
        let before=d.clone();let mixed=build(&d,&body).unwrap();let strict=conductor_build(&d,&body).unwrap();
        assert_eq!(mixed["batches"].as_array().unwrap().len(),2,"ordinary explicit selection uses the same strict policy");
        assert_eq!(mixed,strict);
        assert!(mixed["batches"][0]["bytes"].as_u64().unwrap()<=engine_prepare::MAX_REQUEST_BYTES as u64);
        assert_eq!(strict["batches"].as_array().unwrap().len(),2);
        assert_eq!(strict["batches"][0]["itemIds"],json!([ids[0]]));
        assert_eq!(strict["batches"][1]["itemIds"],json!([ids[40]]));
        assert_eq!(strict["held"],json!([]));assert_eq!(strict["selectedItemIds"],body["itemIds"]);
        assert_eq!(d,before,"conductor planning stays read-only");
    }

    #[test]
    fn conductor_plan_keeps_admitted_copies_together_and_retains_post_specific_evidence(){
        let(d,ids)=family_fixture();let selected:Vec<_>=[&ids[..2],&ids[40..42],&ids[110..112]].concat();
        let before=d.clone();let plan=conductor_build(&d,&json!({"itemIds":selected,"instruction":"Keep publication facts separate"})).unwrap();
        let family:Vec<_>=[&ids[..2],&ids[110..112]].concat();
        assert_eq!(plan["batches"].as_array().unwrap().len(),2);assert_eq!(plan["held"],json!([]));
        assert_eq!(plan["batches"][0]["itemIds"],json!(family));
        assert_eq!(plan["batches"][1]["itemIds"],json!(&ids[40..42]));
        let bundle=engine_prepare::build_request(&d,plan["batches"][0]["itemIds"].as_array().unwrap(),Some("Keep publication facts separate")).unwrap();
        assert_eq!(plan["batches"][0]["bytes"],engine_prepare::model_request_bytes(&bundle["request"]));
        assert_eq!(bundle["request"]["posts"].as_array().unwrap().len(),2);
        for post in bundle["request"]["posts"].as_array().unwrap(){
            let original=row(&d,"posts",post["id"].as_str().unwrap()).unwrap();
            for field in ["id","postKey","text","platform","sourceUrl"]{assert_eq!(post[field],original[field],"{field}");}
        }
        assert_eq!(engine_prepare::image_count(&bundle["request"]),1,"every photo of each selected copy is mandatory");
        for kind in ["transcript","rule"]{
            assert_eq!(bundle["request"]["materials"].as_array().unwrap().iter().filter(|material|material["kind"]==kind).count(),1);
        }
        assert!(prepare_bundle::current(&d,&bundle).is_ok(),"source and rule proof stays current");assert_eq!(d,before);
    }

    #[test]
    fn conductor_plan_holds_malformed_family_without_blocking_independent_source(){
        let(mut d,ids)=family_fixture();
        let selected=vec![ids[0].clone(),ids[40].clone()];
        row_mut(&mut d,"items",&ids[0]).unwrap()["branchId"]=json!("missing-source-branch");
        let before=d.clone();let plan=conductor_build(&d,&json!({"itemIds":selected})).unwrap();
        assert_eq!(plan["batches"].as_array().unwrap().len(),1);
        assert_eq!(plan["batches"][0]["itemIds"],json!([ids[40]]));
        assert_eq!(plan["held"].as_array().unwrap().len(),1);assert_eq!(plan["held"][0]["itemId"],ids[0]);
        assert_eq!(d,before);
    }

    #[test]
    fn family_affinity_rejects_stale_revoked_foreign_and_title_only_edges(){
        let (d,ids)=family_fixture();
        for variant in ["stale","revoked","foreign","title"] {
            let mut bad=d.clone();
            match variant {
                "stale"=>bad["posts"][2]["text"]=json!("Changed target source"),
                "revoked"=>bad["settings"]["mediaAudioEquivalences"]["target"]["status"]=json!("revoked"),
                "foreign"=>bad["posts"][2]["account"]=json!("Other company"),
                _=>{bad["settings"]["mediaAudioEquivalences"]=json!({});bad["posts"][0]["title"]=json!("Same title");bad["posts"][2]["title"]=json!("Same title");}
            }
            assert_eq!(automatic_groups(&bad,&ids).iter().map(|g|g[0].as_str()).collect::<Vec<_>>(),vec!["source-0","unrelated-0","target-0"],"{variant}");
        }
    }

    #[test]
    fn plan_is_read_only_and_measures_the_real_scheduler_request() {
        let d=fixture();let before=d.clone();
        let p=build(&d,&json!({"itemIds":["a"],"instruction":"Коротко"})).unwrap();
        let request=engine_prepare::build_request(&d,&[json!("a")],Some("Коротко")).unwrap();
        assert_eq!(p["batches"][0]["bytes"],engine_prepare::model_request_bytes(&request["request"]));
        assert_eq!(p["selectedItemIds"],json!(["a"]));assert_eq!(p["held"],json!([]));
        assert_eq!(d,before);
        let mut foreign=d.clone();foreign["items"][0]["connectorBinding"]=json!({"accountId":"BAW Russia"});
        assert!(build(&foreign,&json!({"itemIds":["a"]})).is_err());
    }

    #[test]
    fn plan_and_scheduler_hold_the_same_protected_operations_without_losing_other_work() {
        let mut d=fixture();
        let seed=d["items"][0].clone();
        for id in ["b","c","d"] {
            let mut item=seed.clone();item["id"]=json!(id);item["itemId"]=json!(id);
            // These are independent operation recipients, so their branch and
            // conversation identities must also be independent. Cloning A's
            // unresolved branch would correctly fence a new preparation of D.
            item["branchId"]=json!(format!("branch-{id}"));
            item["conversationKey"]=json!(format!("thread-{id}"));
            d["branches"].as_array_mut().unwrap().push(json!({"id":item["branchId"],"postId":item["postId"],"messages":[]}));
            d["items"].as_array_mut().unwrap().push(item);
        }
        d["operations"]=json!([
            {"itemId":"a","status":"unknown"},
            {"itemId":"b","status":"dispatching"},
            {"itemId":"c","status":"succeeded"},
            {"itemId":"d","status":"failed"}
        ]);
        let body=json!({"itemIds":["a","b","c","d"]});
        let before=d.clone();
        let plan=build(&d,&body).unwrap();
        assert_eq!(d,before,"advisory planning must remain read-only");
        let scheduled=engine_prepare::schedule(&mut d,engine_prepare::parse(&body).unwrap()).unwrap();
        let job=row(&d,"jobs",&scheduled.job_id).unwrap();
        assert_eq!(plan["held"],job["held"]);
        assert_eq!(plan["held"],json!([
            {"itemId":"a","reason":"operation_outcome_unknown"},
            {"itemId":"b","reason":"operation_dispatch_in_progress"},
            {"itemId":"c","reason":"operation_already_succeeded"}
        ]));
        assert_eq!(plan["batches"][0]["itemIds"],json!(["d"]));
        assert_eq!(job["selectedItemIds"],json!(["d"]));
    }

    #[test]
    fn advisory_bytes_use_the_same_model_projection_as_single_pass_admission() {
        let mut d=fixture();
        let seed=d["items"][0].clone();
        let repeated="а".repeat(1_700);
        d["items"]=json!([]);
        let mut ids=Vec::new();
        for n in 0..100 {
            let id=format!("item-{n:03}");let mut item=seed.clone();
            item["id"]=json!(id);item["itemId"]=json!(id);
            item["text"]=json!(repeated);item["preview"]=json!(repeated);
            d["items"].as_array_mut().unwrap().push(item);ids.push(id);
        }
        let body=json!({"itemIds":ids});
        let before=d.clone();
        let plan=build(&d,&body).unwrap();
        assert_eq!(d,before,"advisory planning is read-only");
        assert_eq!(plan["held"],json!([]));
        assert_eq!(plan["batches"].as_array().unwrap().len(),1);
        assert_eq!(plan["batches"][0]["itemIds"],body["itemIds"]);
        let request=engine_prepare::build_request(&d,body["itemIds"].as_array().unwrap(),None).unwrap();
        assert_eq!(request["request"]["preparationMode"],"single_pass_v1");
        assert!(request["request"].to_string().len()>engine_prepare::MAX_REQUEST_BYTES,
            "the complete immutable capture deliberately exceeds the model budget");
        let model_bytes=engine_prepare::model_request_bytes(&request["request"]);
        assert!(model_bytes<=engine_prepare::MAX_REQUEST_BYTES);
        assert_eq!(plan["batches"][0]["bytes"],model_bytes);
        let scheduled=engine_prepare::schedule(&mut d,engine_prepare::parse(&body).unwrap()).unwrap();
        assert_eq!(row(&d,"jobs",&scheduled.job_id).unwrap()["selectedItemIds"],body["itemIds"]);
    }

    #[test]
    fn automatic_groups_keep_posts_separate_and_bound_recipient_count() {
        let mut d=fixture(); let seed=d["items"][0].clone();
        let mut ids=Vec::new(); d["items"]=json!([]);
        for n in 0..205 {
            let id=format!("recipient-{n:03}"); let mut item=seed.clone(); item["id"]=json!(id);
            ids.push(id); d["items"].as_array_mut().unwrap().push(item);
        }
        let groups=automatic_groups(&d,&ids);
        assert_eq!(groups.iter().map(Vec::len).collect::<Vec<_>>(),vec![100,100,5]);
        assert_eq!(groups.concat(),ids);
        assert!(engine_prepare::parse(&json!({"itemIds":groups[0]})).is_ok(),"automatic bound matches explicit selection contract");
        assert!(engine_prepare::parse(&json!({"itemIds":(0..=AUTO_GROUP_MAX_ITEMS).map(|n|format!("id-{n}")).collect::<Vec<_>>()})).is_err());
        let (d,ids)=photo_fixture(); let ids:Vec<String>=ids.iter().map(|id|id.as_str().unwrap().to_owned()).collect();
        let groups=automatic_groups(&d,&ids);
        assert_eq!(groups.iter().map(Vec::len).collect::<Vec<_>>(),vec![1,1,10,1]);
        for group in groups {
            let (bundles,held)=automatic_bundles(&d,&group); assert!(held.is_empty()); assert_eq!(bundles.len(),1);
            let request=&bundles[0]["request"];
            assert_eq!(request["posts"].as_array().unwrap().len(),1);
            assert!(request.to_string().len()<=engine_prepare::MAX_REQUEST_BYTES);
            assert!(engine_prepare::image_count(request)<=engine_prepare::MAX_REQUEST_IMAGES);
        }
    }

    #[test]
    fn automatic_group_byte_partition_preserves_complete_branch_evidence() {
        let mut d=fixture(); let mut second=d["items"][0].clone();
        second["id"]=json!("z"); second["itemId"]=json!("z"); second["branchId"]=json!("z-branch");
        d["items"].as_array_mut().unwrap().push(second);
        let messages:Vec<Value>=(0..15).map(|n|json!({"id":format!("m{n}"),"text":"я".repeat(10000)})).collect();
        d["branches"][0]["messages"]=json!(messages);
        let mut branch=d["branches"][0].clone(); branch["id"]=json!("z-branch"); d["branches"].as_array_mut().unwrap().push(branch);
        let before=d.clone();
        assert!(engine_prepare::build_request(&d,&[json!("a"),json!("z")],None).is_err());
        let (bundles,held)=automatic_bundles(&d,&["a".into(),"z".into()]);
        assert_eq!(bundles.len(),2); assert!(held.is_empty()); assert_eq!(d,before);
        for bundle in bundles {
            assert_eq!(bundle["request"]["branches"][0]["messages"],d["branches"][0]["messages"]);
            assert!(bundle["request"].to_string().len()<=engine_prepare::MAX_REQUEST_BYTES);
            assert!(prepare_bundle::current(&d,&bundle).is_ok());
        }
    }

    #[test]
    fn automatic_group_image_partition_and_oversized_singleton_keep_exact_coverage() {
        let mut d=fixture(); let mut second=d["items"][0].clone();
        second["id"]=json!("z"); second["itemId"]=json!("z"); d["items"].as_array_mut().unwrap().push(second);
        for item in d["items"].as_array_mut().unwrap() {
            item["attachments"]=json!((0..9).map(|n|json!({"type":"photo","url":format!("https://cdn.example/{}/{n}.png",item["id"].as_str().unwrap())})).collect::<Vec<_>>());
        }
        let (bundles,held)=automatic_bundles(&d,&["a".into(),"z".into()]);
        assert_eq!(bundles.len(),2); assert!(held.is_empty());
        assert_eq!(bundles.iter().flat_map(|b|b["itemIds"].as_array().unwrap().clone()).collect::<Vec<_>>(),vec![json!("a"),json!("z")]);
        for bundle in bundles { assert_eq!(engine_prepare::image_count(&bundle["request"]),9); }
        d["items"][0]["attachments"]=json!((0..17).map(|n|json!({"type":"photo","url":format!("https://cdn.example/huge/{n}.png")})).collect::<Vec<_>>());
        let (bundles,held)=automatic_bundles(&d,&["a".into(),"z".into()]);
        assert_eq!(bundles.len(),1); assert_eq!(bundles[0]["itemIds"],json!(["z"]));
        assert_eq!(held,vec![("a".to_owned(),engine_prepare::IMAGE_CAPACITY_ERROR)]);
    }

    #[test]
    fn automatic_packing_reuses_real_capture_and_keeps_post_adjacency() {
        let (d,_) = photo_fixture(); let before = d.clone();
        let ids: Vec<String> = ["c-9","a-0","c-0","d-0","c-4","b-0"].into_iter().map(str::to_owned).collect();
        let (bundles,held) = automatic_packed_bundles(&d,&ids);
        assert!(held.is_empty()); assert_eq!(d,before); assert_eq!(bundles.len(),1);
        assert_eq!(bundles[0]["itemIds"],json!(["c-9","c-0","c-4"]));
        let (tail,held)=automatic_packed_bundles(&d,&["b-0".into()]);
        assert!(held.is_empty()); assert_eq!(tail[0]["itemIds"],json!(["b-0"]));
        for bundle in bundles {
            let rebuilt=engine_prepare::build_request(&d,bundle["itemIds"].as_array().unwrap(),None).unwrap();
            for key in ["request","itemIds","digest","dependencyDigest","researchManifest"] {assert_eq!(bundle[key],rebuilt[key]);}
            assert!(bundle["request"].to_string().len()<=engine_prepare::MAX_REQUEST_BYTES);
            assert!(engine_prepare::image_count(&bundle["request"])<=engine_prepare::MAX_REQUEST_IMAGES);
            assert!(prepare_bundle::current(&d,&bundle).is_ok());
        }
    }

    #[test]
    fn automatic_packing_bounds_recipients_without_splitting_fitting_posts() {
        let mut d=fixture();
        let item=d["items"][0].clone(); let post=d["posts"][0].clone(); let branch=d["branches"][0].clone();
        d["items"]=json!([]); d["posts"]=json!([]); d["branches"]=json!([]);
        let mut ids=Vec::new();
        for (group,count) in [("a",60),("b",50),("c",40)] {
            let mut p=post.clone(); p["id"]=json!(group); p["postKey"]=json!(group); d["posts"].as_array_mut().unwrap().push(p);
            let mut b=branch.clone(); b["id"]=json!(format!("branch-{group}")); b["postId"]=json!(group); d["branches"].as_array_mut().unwrap().push(b);
            for n in 0..count {
                let id=format!("{group}-{n}"); let mut i=item.clone();
                i["id"]=json!(id); i["itemId"]=json!(id); i["postId"]=json!(group); i["postKey"]=json!(group);
                i["branchId"]=json!(format!("branch-{group}")); i["conversationKey"]=i["branchId"].clone();
                d["items"].as_array_mut().unwrap().push(i); ids.push(id);
            }
        }
        let (mut bundles,held)=automatic_packed_bundles(&d,&ids);
        assert!(held.is_empty());
        assert_eq!(bundles.len(),1);
        let (tail,held)=automatic_packed_bundles(&d,&ids[60..]);
        assert!(held.is_empty()); bundles.extend(tail);
        let (tail,held)=automatic_packed_bundles(&d,&ids[110..]);
        assert!(held.is_empty()); bundles.extend(tail);
        assert_eq!(bundles.iter().map(|b|b["itemIds"].as_array().unwrap().len()).collect::<Vec<_>>(),vec![60,50,40]);
        assert_eq!(bundles.iter().flat_map(|b|b["itemIds"].as_array().unwrap().iter().map(|id|id.as_str().unwrap().to_owned())).collect::<Vec<_>>(),ids);
        for bundle in bundles { assert!(engine_prepare::parse(&json!({"itemIds":bundle["itemIds"]})).is_ok()); }
    }

    #[test]
    fn automatic_packing_holds_only_bad_post_and_preserves_later_valid_captures() {
        let (mut d,selected)=photo_fixture();
        d["branches"].as_array_mut().unwrap().retain(|b|b["id"]!="branch-b-0");
        d["items"][0]["attachments"]=json!((0..17).map(|n|json!({"type":"photo","url":format!("https://cdn.example/oversized/{n}.png")})).collect::<Vec<_>>());
        let before=d.clone(); let ids:Vec<String>=selected.iter().map(|id|id.as_str().unwrap().to_owned()).collect();
        let (bundles,held)=automatic_packed_bundles(&d,&ids);
        assert_eq!(d,before); assert_eq!(bundles.len(),1); assert_eq!(held.len(),2);
        assert_eq!(held[0],("a-0".to_owned(),engine_prepare::IMAGE_CAPACITY_ERROR));
        assert_eq!(held[1].0,"b-0"); assert!(!capacity_error(held[1].1));
        assert_eq!(bundles[0]["itemIds"],json!(["c-0","c-1","c-2","c-3","c-4","c-5","c-6","c-7","c-8","c-9"]));
        let (tail,tail_held)=automatic_packed_bundles(&d,&["d-0".into()]);assert!(tail_held.is_empty());
        assert_eq!(tail[0]["itemIds"],json!(["d-0"]));
        let covered=bundles[0]["itemIds"].as_array().unwrap().len()+tail[0]["itemIds"].as_array().unwrap().len()+held.len(); assert_eq!(covered,ids.len());
        assert!(prepare_bundle::current(&d,&bundles[0]).is_ok());
    }

    #[test]
    fn automatic_packing_byte_limit_retains_whole_branch_messages() {
        let mut d=fixture(); let mut item=d["items"][0].clone(); let mut branch=d["branches"][0].clone(); let mut post=d["posts"][0].clone();
        let messages=json!((0..15).map(|n|json!({"id":format!("m{n}"),"text":"я".repeat(10000)})).collect::<Vec<_>>());
        d["branches"][0]["messages"]=messages.clone();
        item["id"]=json!("z"); item["itemId"]=json!("z"); item["postId"]=json!("q"); item["postKey"]=json!("q"); item["branchId"]=json!("z-branch");
        branch["id"]=json!("z-branch"); branch["postId"]=json!("q"); branch["messages"]=messages.clone(); post["id"]=json!("q"); post["postKey"]=json!("q");
        d["items"].as_array_mut().unwrap().push(item); d["branches"].as_array_mut().unwrap().push(branch); d["posts"].as_array_mut().unwrap().push(post);
        let before=d.clone(); let (mut bundles,held)=automatic_packed_bundles(&d,&["a".into(),"z".into()]);
        assert!(held.is_empty()); assert_eq!(d,before); assert_eq!(bundles.len(),1);
        let (tail,held)=automatic_packed_bundles(&d,&["z".into()]); assert!(held.is_empty()); bundles.extend(tail);
        for bundle in bundles {
            assert_eq!(bundle["request"]["branches"][0]["messages"],messages);
            assert!(bundle["request"].to_string().len()<=engine_prepare::MAX_REQUEST_BYTES);
        }
    }

    #[test]
    fn automatic_group_identity_does_not_alias_missing_post_to_real_post_id() {
        let mut d=fixture(); d["items"][0]["postId"]=Value::Null;
        let mut item=d["items"][0].clone(); item["id"]=json!("z"); item["postId"]=json!("a"); d["items"].as_array_mut().unwrap().push(item);
        assert_eq!(automatic_groups(&d,&["a".into(),"z".into()]),vec![vec!["a".to_owned()],vec!["z".to_owned()]]);
    }

    #[test]
    fn automatic_packing_leaves_backlog_beyond_next_nonfitting_post_unexamined() {
        let (mut d,selected)=photo_fixture();
        d["branches"].as_array_mut().unwrap().retain(|b|b["id"]!="branch-d-0");
        let ids:Vec<String>=selected.iter().map(|id|id.as_str().unwrap().to_owned()).collect();
        let (bundles,held)=automatic_packed_bundles(&d,&ids);
        assert_eq!(bundles.len(),1); assert_eq!(bundles[0]["itemIds"],json!(["a-0"]));
        assert!(held.is_empty(),"the malformed tail is not captured ahead of the next claim");
        let (tail,held)=automatic_packed_bundles(&d,&ids[2..]);
        assert_eq!(tail[0]["itemIds"].as_array().unwrap().len(),10);
        assert!(held.is_empty(),"a different family's malformed tail remains unexamined");
        let (tail,held)=automatic_packed_bundles(&d,&ids[12..]);
        assert!(tail.is_empty());assert_eq!(held.len(),1);assert_eq!(held[0].0,"d-0");
    }

    #[test]
    fn missing_video_url_is_assessable_without_inventing_media_readiness() {
        let mut d=fixture();d["posts"][0]["attachments"]=json!([{"type":"video"}]);
        assert!(d["posts"][0]["attachments"][0]["url"].is_null());
        assert!(d["posts"][0]["sourceUrl"].is_null(),"keep the unavailable source fixture");
        assert!(media_queue::preparation_states(&d,&[d["items"][0].clone()],&now()).unwrap()["a"].is_some());
        assert!(decision_media::may_assess(&d,&d["items"][0]).unwrap());
        let before=d.clone();let body=json!({"itemIds":["a"]});
        for plan in [build(&d,&body).unwrap(),conductor_build(&d,&body).unwrap()] {
            assert_eq!(plan["held"],json!([]));
            assert_eq!(plan["batches"].as_array().unwrap().len(),1);
            assert_eq!(plan["batches"][0]["itemIds"],json!(["a"]));
        }
        let bundle=engine_prepare::build_request(&d,&[json!("a")],None).unwrap();
        assert_eq!(bundle["request"]["decisionMediaContract"],decision_media::CONTRACT);
        let state=&bundle["request"]["posts"][0]["decisionMediaEvidence"];
        for field in ["audioReady","audioProvided","visualReady","visualProvided"] {
            assert_eq!(state[field],false,"{field}: missing URL cannot invent available media");
        }
        let candidate=json!({"decisionMediaContract":decision_media::CONTRACT,"decisionMediaEvidence":[state.clone()]});
        for dependency in [json!({"audio":"required","visual":"independent"}),
            json!({"audio":"independent","visual":"required"}),json!({"audio":"unknown","visual":"independent"})] {
            assert!(decision_media::validate_judgment(&candidate,&json!({"decision":"accept","mediaDependency":dependency})).is_err(),
                "assessment eligibility never supplies required or unresolved media");
        }
        assert_eq!(d,before,"planning cannot create jobs, paid work or media evidence");
    }

    fn photo_fixture() -> (Value,Vec<Value>) {
        let mut d=fixture();
        let item=d["items"][0].clone();let branch=d["branches"][0].clone();let post=d["posts"][0].clone();
        d["items"]=json!([]);d["branches"]=json!([]);d["posts"]=json!([]);
        let mut selected=Vec::new();
        for (group,count,recipients) in [("a",5,1),("b",4,1),("c",8,10),("d",3,1)] {
            let mut p=post.clone();p["id"]=json!(group);p["postKey"]=json!(group);
            p["attachments"]=json!((0..count).map(|n|json!({"type":"photo","url":format!("https://cdn.example/{group}/{n}.png")})).collect::<Vec<_>>());
            d["posts"].as_array_mut().unwrap().push(p);
            for n in 0..recipients {
                let id=format!("{group}-{n}");let branch_id=format!("branch-{id}");selected.push(json!(id));
                let mut i=item.clone();i["id"]=json!(id);i["itemId"]=json!(id);i["postId"]=json!(group);i["postKey"]=json!(group);
                i["branchId"]=json!(branch_id);i["conversationKey"]=json!(branch_id);i["attachments"]=json!([]);i["attachmentsState"]=json!("none");
                let mut b=branch.clone();b["id"]=json!(branch_id);b["postId"]=json!(group);b["messages"][0]["id"]=json!(id);
                d["items"].as_array_mut().unwrap().push(i);d["branches"].as_array_mut().unwrap().push(b);
            }
        }
        (d,selected)
    }

    #[test]
    fn mandatory_post_carousels_keep_every_copy_and_photo_in_its_strict_group() {
        let (d,selected)=photo_fixture();let before=d.clone();
        assert_eq!(engine_prepare::build_request(&d,&selected,None).unwrap_err(),preparation_unit::MIXED);
        let p=build(&d,&json!({"itemIds":selected})).unwrap();
        assert_eq!(d,before);assert_eq!(p["held"],json!([]));assert_eq!(p["account"],d["account"]);
        assert_eq!(p["batches"].as_array().unwrap().len(),4);
        let mut covered=Vec::new();
        let mut packed_sources=0;let mut packed_c_requests=0;
        for batch in p["batches"].as_array().unwrap() {
            let ids=batch["itemIds"].as_array().unwrap();covered.extend(ids.iter().cloned());
            let bundle=engine_prepare::build_request(&d,ids,None).unwrap();
            assert_eq!(batch["bytes"],bundle["request"].to_string().len());
            assert!(engine_prepare::image_count(&bundle["request"])<=16);
            assert_eq!(bundle["request"]["strictGroup"]["kind"],"post");
            assert_eq!(bundle["request"]["materialReadiness"]["status"],"pending","photos must be acquired before paid dispatch");
            let photos=bundle["request"]["posts"][0]["attachments"].as_array().unwrap().len();
            assert_eq!(engine_prepare::image_count(&bundle["request"]),photos,"the mandatory transport counts all source photos");
            packed_sources+=bundle["request"]["posts"].as_array().unwrap().len();
            if ids.iter().any(|id|id.as_str().unwrap().starts_with("c-")) {
                packed_c_requests+=1;
                let source=bundle["request"]["posts"].as_array().unwrap().iter().find(|post|post["id"]=="c").unwrap();
                assert_eq!(source["attachments"].as_array().unwrap().len(),8,"the source post is never trimmed to fit");
            }
        }
        assert_eq!(covered,selected);
        assert_eq!(packed_sources,4);assert_eq!(packed_c_requests,1);
        assert_eq!(p["strictGroupContract"],preparation_unit::CONTRACT);
        assert!(p["batches"].as_array().unwrap().iter().all(|b|b["strictGroup"]["copies"].as_array().unwrap().len()==1));
    }

    #[test]
    fn interleaved_selection_preserves_first_seen_posts_and_within_post_order() {
        let (d,_)=photo_fixture();let before=d.clone();
        let selected=json!(["c-9","a-0","c-0","d-0","c-4","b-0"]);
        let body=json!({"itemIds":selected});let p=build(&d,&body).unwrap();
        assert_eq!(p,build(&d,&body).unwrap());assert_eq!(d,before);
        assert_eq!(p["selectedItemIds"],selected);assert_eq!(p["held"],json!([]));
        assert_eq!(p["batches"].as_array().unwrap().len(),4);
        assert_eq!(p["batches"][0]["itemIds"],json!(["c-9","c-0","c-4"]));
        assert_eq!(p["batches"].as_array().unwrap().iter().flat_map(|b|b["itemIds"].as_array().unwrap().iter()).cloned().collect::<Vec<_>>(),
            vec![json!("c-9"),json!("c-0"),json!("c-4"),json!("a-0"),json!("d-0"),json!("b-0")]);
        let mut foreign=d.clone();
        foreign["items"].as_array_mut().unwrap().iter_mut().find(|item|item["id"]=="c-4").unwrap()["connectorBinding"]=json!({"accountId":"BAW Russia"});
        assert!(build(&foreign,&body).is_err());
    }

    #[test]
    fn single_recipient_exceeding_image_capacity_is_explicitly_held_without_a_job() {
        let mut d=fixture();
        d["items"][0]["attachments"]=json!((0..17).map(|n|json!({"type":"photo","url":format!("https://cdn.example/{n}.png")})).collect::<Vec<_>>());
        let before=d.clone();let plan=build(&d,&json!({"itemIds":["a"]})).unwrap();
        assert_eq!(plan["batches"],json!([]));
        assert_eq!(plan["held"],json!([{"itemId":"a","reason":"image_budget_exceeded"}]));
        assert_eq!(d,before);
    }
    #[test]fn mandatory_shared_photo_base_cannot_be_bypassed_by_splitting_recipients(){
        let mut d=fixture();let mut second=d["items"][0].clone();second["id"]=json!("z");second["itemId"]=json!("z");
        d["items"].as_array_mut().unwrap().push(second);
        d["posts"][0]["attachments"]=json!((0..17).map(|i|json!({"type":"photo","url":format!("https://fixture.invalid/{i}.png")})).collect::<Vec<_>>());
        let before=d.clone();let plan=build(&d,&json!({"itemIds":["a","z"]})).unwrap();
        assert_eq!(plan["batches"],json!([]));assert_eq!(plan["held"].as_array().unwrap().len(),2);
        assert!(plan["held"].as_array().unwrap().iter().all(|h|h["reason"]=="image_budget_exceeded"));
        assert_eq!(d,before);assert!(list(&d,"jobs").is_empty());
        assert_eq!(d["posts"][0]["attachments"].as_array().unwrap().len(),17);
    }

    #[test]
    fn real_large_branches_split_without_raising_budget_or_dropping_messages() {
        let mut d=fixture();
        let mut second=d["items"][0].clone();second["id"]=json!("z");second["itemId"]=json!("z");second["branchId"]=json!("z-branch");
        d["items"].as_array_mut().unwrap().push(second);
        let messages:Vec<Value>=(0..15).map(|n|json!({"id":format!("m{n}"),"text":"x".repeat(20000)})).collect();
        d["branches"][0]["messages"]=json!(messages);
        let mut branch=d["branches"][0].clone();branch["id"]=json!("z-branch");d["branches"].as_array_mut().unwrap().push(branch);
        assert!(engine_prepare::build_request(&d,&[json!("a"),json!("z")],None).is_err());
        let before=d.clone();let p=build(&d,&json!({"itemIds":["a","z"]})).unwrap();
        assert_eq!(d,before);
        assert_eq!(p["batches"].as_array().unwrap().len(),2);assert_eq!(p["held"],json!([]));
        for batch in p["batches"].as_array().unwrap() {
            assert!(batch["bytes"].as_u64().unwrap()<=550000);
            let bundle=engine_prepare::build_request(&d,batch["itemIds"].as_array().unwrap(),None).unwrap();
            assert_eq!(batch["bytes"],bundle["request"].to_string().len());
            assert_eq!(bundle["request"]["branches"][0]["messages"].as_array().unwrap().len(),15);
        }
    }

    #[test]
    fn byte_capacity_flush_keeps_a_fitting_shared_branch_group_whole() {
        let mut d=fixture();
        d["branches"][0]["messages"]=json!((0..15).map(|n|json!({"id":format!("m{n}"),"text":"я".repeat(12000)})).collect::<Vec<_>>());
        let mut same=d["items"][0].clone();same["id"]=json!("z");same["itemId"]=json!("z");
        let mut other=same.clone();other["id"]=json!("w");other["itemId"]=json!("w");other["branchId"]=json!("w-branch");other["postId"]=json!("q");other["postKey"]=json!("q");
        d["items"].as_array_mut().unwrap().extend([same,other]);
        let mut branch=d["branches"][0].clone();branch["id"]=json!("w-branch");branch["postId"]=json!("q");d["branches"].as_array_mut().unwrap().push(branch);
        let mut post=d["posts"][0].clone();post["id"]=json!("q");post["postKey"]=json!("q");d["posts"].as_array_mut().unwrap().push(post);
        let before=d.clone();
        let error=engine_prepare::build_request(&d,&[json!("a"),json!("z"),json!("w")],None).unwrap_err();
        assert_eq!(error,preparation_unit::MIXED,"mixed direct capture is rejected before capacity measurement");
        let p=build(&d,&json!({"itemIds":["a","w","z"]})).unwrap();
        assert_eq!(d,before);assert_eq!(p["held"],json!([]));assert_eq!(p["batches"].as_array().unwrap().len(),2);
        assert_eq!(p["batches"][0]["itemIds"],json!(["a","z"]));assert_eq!(p["batches"][1]["itemIds"],json!(["w"]));
        for batch in p["batches"].as_array().unwrap() {
            let bundle=engine_prepare::build_request(&d,batch["itemIds"].as_array().unwrap(),None).unwrap();
            assert_eq!(batch["bytes"],bundle["request"].to_string().len());
            assert!(batch["bytes"].as_u64().unwrap()<=engine_prepare::MAX_REQUEST_BYTES as u64);
            assert_eq!(bundle["request"]["branches"][0]["messages"],d["branches"][0]["messages"]);
        }
    }

    #[test]
    fn single_recipient_exceeding_byte_capacity_is_held_without_truncation() {
        let mut d=fixture();
        d["branches"][0]["messages"]=json!((0..25).map(|n|json!({"id":format!("m{n}"),"text":"x".repeat(23000)})).collect::<Vec<_>>());
        let before=d.clone();let p=build(&d,&json!({"itemIds":["a"]})).unwrap();
        assert_eq!(p["batches"],json!([]));assert_eq!(p["held"],json!([{"itemId":"a","reason":"evidence_too_large"}]));assert_eq!(d,before);
    }

    #[test]
    #[ignore = "Explicit private read-only snapshot/selection replay; outputs counts only"]
    fn private_snapshot_plan_replay() {
        let evidence=std::env::var("COMMUNITYHERO_MEDIA_EVIDENCE_DIR").expect("explicit evidence directory required for a meaningful replay");
        assert!(std::path::Path::new(&evidence).is_absolute() && std::path::Path::new(&evidence).is_dir());
        let fixture=std::env::var("CH_PREPARE_PLAN_FIXTURE").expect("snapshot path");
        let selection=std::env::var("CH_PREPARE_PLAN_SELECTION").expect("selection path");
        let d:Value=serde_json::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
        let body:Value=serde_json::from_slice(&std::fs::read(selection).unwrap()).unwrap();
        // Mirror the live runtime's read-only proof refresh before pure planning.
        let mut warm=std::collections::BTreeMap::<String,usize>::new();
        for version in d["knowledge_versions"].as_array().unwrap().iter().filter(|v|v["kind"]=="visual_context") {
            let result=media_fullframes::verify_and_cache(&version["visualEvidence"]).err().unwrap_or_else(||"valid".into());
            *warm.entry(result).or_default()+=1;
        }
        println!("visualProofRefresh={}",json!(warm));
        let plan=build(&d,&body).unwrap();
        let lookup=knowledge::TranscriptLookup::new(&d,&now()).unwrap();
        let mut audio=0;let mut visual=0;
        for id in body["itemIds"].as_array().unwrap(){
            let item=row(&d,"items",id.as_str().unwrap()).unwrap();
            if let Some(post)=d["posts"].as_array().unwrap().iter().find(|p|p["id"]==item["postId"]){
                audio+=usize::from(lookup.has(post).unwrap());visual+=usize::from(lookup.has_visual(post).unwrap());
            }
        }
        println!("audioCovered={audio} visualCovered={visual}");
        let mut checks=std::collections::BTreeMap::<String,usize>::new();
        for version in d["knowledge_versions"].as_array().unwrap().iter().filter(|v|v["kind"]=="visual_context") {
            let result=media_fullframes::validate_evidence(&version["visualEvidence"]).err().unwrap_or_else(||"valid".into());
            *checks.entry(result).or_default()+=1;
        }
        println!("visualValidation={}",json!(checks));
        assert_eq!(plan["selectedItemIds"],body["itemIds"]);
        let mut ids=std::collections::BTreeSet::new();
        for batch in plan["batches"].as_array().unwrap() {
            assert!(batch["bytes"].as_u64().unwrap()<=550000);
            for id in batch["itemIds"].as_array().unwrap(){assert!(ids.insert(id.as_str().unwrap()));}
        }
        for hold in plan["held"].as_array().unwrap(){assert!(ids.insert(hold["itemId"].as_str().unwrap()));}
        assert_eq!(ids.len(),body["itemIds"].as_array().unwrap().len());
        println!("selected={} batches={} held={} batchBytes={}",ids.len(),plan["batches"].as_array().unwrap().len(),plan["held"].as_array().unwrap().len(),json!(plan["batches"].as_array().unwrap().iter().map(|b|b["bytes"].clone()).collect::<Vec<_>>()));
    }
}
