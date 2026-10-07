// R6 source oracle: exact function apart from its name.

use std::collections::HashSet;

fn timestamp(value:&Value)->Option<i64>{
    value.as_str().and_then(|s|chrono::DateTime::parse_from_rfc3339(s).ok()).map(|v|v.timestamp_millis())
}
fn legacy_ordered(d:&Value,snapshot:&Value)->ApiResult<Value>{
    let mut output=snapshot.clone();
    let mut dropped_branches=HashSet::new();let mut dropped_posts=HashSet::new();
    let mut kept_branches=HashSet::new();let mut kept_posts=HashSet::new();
    if let Some(items)=output["items"].as_array_mut(){
        let mut accepted=Vec::with_capacity(items.len());
        for mut item in items.drain(..){
            for key in ["contextObservedAt","providerStatusObservedAt"]{
                if !item[key].is_null()&&timestamp(&item[key]).is_none(){return Err(internal("Invalid provider observation timestamp"));}
            }
            let old=list(d,"items").iter().find(|old|old["id"]==item["id"]);
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
