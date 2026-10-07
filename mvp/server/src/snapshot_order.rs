//! Keep late full-context reads from undoing a newer explicit status observation.
use super::*;
use std::collections::HashSet;

fn timestamp(value:&Value)->Option<i64>{
    value.as_str().and_then(|s|chrono::DateTime::parse_from_rfc3339(s).ok()).map(|v|v.timestamp_millis())
}
pub(super) fn ordered(d:&Value,snapshot:&Value)->ApiResult<Value>{
    let mut output=snapshot.clone();
    let mut dropped_branches=HashSet::new();let mut dropped_posts=HashSet::new();
    let mut kept_branches=HashSet::new();let mut kept_posts=HashSet::new();
    if let Some(items)=output["items"].as_array_mut(){
        let prior_index = std::cell::OnceCell::new();
        let mut accepted=Vec::with_capacity(items.len());
        for mut item in items.drain(..){
            for key in ["contextObservedAt","providerStatusObservedAt"]{
                if !item[key].is_null()&&timestamp(&item[key]).is_none(){return Err(internal("Invalid provider observation timestamp"));}
            }
            let (old_rows, old_index) = prior_index.get_or_init(|| {
                let rows = list(d,"items");
                (rows, snapshot_domain::first_rows(rows))
            });
            let old=snapshot_domain::position(old_rows,old_index,&item["id"]).map(|position|&old_rows[position]);
            let context_time=timestamp(&item["contextObservedAt"]);
            let status_time=timestamp(&item["providerStatusObservedAt"]);
            let stale=old.is_some_and(|old|{
                let prior_context=timestamp(&old["contextObservedAt"]);
                let prior_status=timestamp(&old["statusObservedAt"]).into_iter().chain(timestamp(&old["providerStatusObservedAt"])).max();
                context_time.zip(prior_context).is_some_and(|(a,b)|a<b)
                    || (item["providerStatus"]!=old["providerStatus"]&&status_time.zip(prior_status).is_some_and(|(a,b)|a<b))
            });
            if stale{
                if let Some(id)=item["branchId"].as_str(){dropped_branches.insert(id.to_owned());}
                if let Some(id)=item["postId"].as_str(){dropped_posts.insert(id.to_owned());}
                continue;
            }
            if let Some(old)=old{
                let old_time=timestamp(&old["statusObservedAt"]);
                item["statusObservedAt"]=if old_time.is_some()&&old_time>=status_time {old["statusObservedAt"].clone()} else {item["providerStatusObservedAt"].clone()};
            }else{item["statusObservedAt"]=item["providerStatusObservedAt"].clone();}
            if let Some(id)=item["branchId"].as_str(){kept_branches.insert(id.to_owned());}
            if let Some(id)=item["postId"].as_str(){kept_posts.insert(id.to_owned());}
            accepted.push(item);
        }
        *items=accepted;
    }
    for (table,dropped,kept) in [("branches",dropped_branches,kept_branches),("posts",dropped_posts,kept_posts)]{
        if let Some(rows)=output[table].as_array_mut(){rows.retain(|row|row["id"].as_str().is_none_or(|id|!dropped.contains(id)||kept.contains(id)));}
    }
    Ok(output)
}

#[cfg(test)]mod tests{
    use super::*;
    fn incoming(status:&str,at:&str)->Value{json!({"items":[{"id":"a","branchId":"b","postId":"p","providerStatus":status,"contextObservedAt":at,"providerStatusObservedAt":at}],"branches":[{"id":"b","messages":[]}],"posts":[{"id":"p","text":"old post"}]})}
    #[test]fn late_open_context_cannot_undo_newer_closed_or_replace_its_branch(){
        let d=json!({"items":[{"id":"a","providerStatus":"closed","statusObservedAt":"2026-09-22T12:01:00Z"}]});
        let result=ordered(&d,&incoming("new","2026-09-22T12:00:00Z")).unwrap();
        for key in ["items","branches","posts"]{assert!(result[key].as_array().unwrap().is_empty());}
        assert_eq!(ordered(&d,&incoming("new","2026-09-22T12:02:00Z")).unwrap()["items"][0]["providerStatus"],"new");
    }
    #[test]fn matching_status_can_supply_context_without_rewinding_fast_status_clock(){
        let d=json!({"items":[{"id":"a","providerStatus":"new","statusObservedAt":"2026-09-22T12:01:00Z"}]});
        let result=ordered(&d,&incoming("new","2026-09-22T12:00:00Z")).unwrap();
        assert_eq!(result["items"][0]["statusObservedAt"],"2026-09-22T12:01:00Z");
        assert_eq!(result["items"][0]["contextObservedAt"],"2026-09-22T12:00:00Z");
    }
    #[test]fn older_context_and_malformed_clock_are_rejected(){
        let d=json!({"items":[{"id":"a","providerStatus":"new","contextObservedAt":"2026-09-22T12:01:00Z"}]});
        assert_eq!(ordered(&d,&incoming("new","2026-09-22T12:00:00Z")).unwrap()["items"],json!([]));
        assert!(ordered(&d,&incoming("new","yesterday")).is_err());
    }
    #[test]fn first_import_of_deleted_comment_never_enters_preparation_queue(){
        let mut d=empty();
        merge_snapshot(&mut d,&incoming("deleted","2026-09-22T12:00:00Z")).unwrap();
        assert_eq!(d["items"][0]["providerStatus"],"deleted");
        assert_eq!(d["items"][0]["workflow"],"deleted");
    }
}
