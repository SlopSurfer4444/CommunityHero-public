//! Offline admission reducers. No database, bridge, approval or provider calls.
use super::*;
use super::tests::{fixture,add_recipient,NOW};

fn claim_width(d:&mut Value,at:i64,width:usize)->crate::ApiResult<Option<(String,Value)>> {
    reconcile_claim_state(d,at)?;
    claim_reconciled(d,at,None,width)
}
fn add(d:&mut Value,id:&str,post:&str) {
    add_recipient(d,id,post);
    crate::row_mut(d,"items",id).unwrap()["createdAt"]=json!(stamp(NOW));
}

#[test]
fn idle_slots_fill_with_independent_families_and_full_pool_spends_nothing() {
    let mut d=fixture();add(&mut d,"j","post-j");add(&mut d,"k","post-k");
    let (a,_)=claim_width(&mut d,NOW,2).unwrap().unwrap();
    let (b,request)=claim_width(&mut d,NOW+1,2).unwrap().expect("independent family fills idle worker");
    assert_eq!(request["items"][0]["id"],"j");assert_ne!(a,b);
    let jobs=d["jobs"].clone();let item=crate::row(&d,"items","k").unwrap().clone();
    assert!(claim_width(&mut d,NOW+2,2).unwrap().is_none());
    assert_eq!(d["jobs"],jobs);assert_eq!(crate::row(&d,"items","k").unwrap(),&item);
    assert_eq!(item["autoPreparation"]["attempts"],0);
    assert!(crate::list(&d,"operations").is_empty());assert!(crate::list(&d,"approvals").is_empty());
}

#[test]
fn occupied_first_family_and_branch_alias_do_not_hide_later_family() {
    let mut d=fixture();let (a,_)=claim_width(&mut d,NOW,2).unwrap().unwrap();
    add(&mut d,"a-tail","post");add(&mut d,"b-alias","post-alias");add(&mut d,"z-independent","post-z");
    let alias=crate::row_mut(&mut d,"items","b-alias").unwrap();
    alias["conversationKey"]=json!("thread");
    let (b,request)=claim_width(&mut d,NOW+1,2).unwrap().unwrap();
    assert_ne!(a,b);assert_eq!(request["items"][0]["id"],"z-independent");
    for id in ["a-tail","b-alias"] {
        let deferred=&crate::row(&d,"items",id).unwrap()["autoPreparation"];
        assert_eq!(deferred["status"],"queued");assert_eq!(deferred["attempts"],0);assert!(deferred["jobId"].is_null());
    }
}

#[test]
fn unknown_paid_owner_survives_restart_and_independent_family_remains_available() {
    let mut d=fixture();let (a,_)=claim_width(&mut d,NOW,2).unwrap().unwrap();
    crate::row_mut(&mut d,"jobs",&a).unwrap()["status"]=json!("interrupted");
    add(&mut d,"alias","post-alias");add(&mut d,"z-independent","post-z");
    let alias=crate::row_mut(&mut d,"items","alias").unwrap();alias["itemId"]=json!("c");
    d=serde_json::from_str(&d.to_string()).unwrap();
    let (_,request)=claim_width(&mut d,NOW+86400,2).unwrap().unwrap();
    assert_eq!(request["items"][0]["id"],"z-independent");
    assert_eq!(crate::row(&d,"items","i").unwrap()["autoPreparation"]["jobId"],a);
    assert!(crate::row(&d,"items","alias").unwrap()["autoPreparation"]["jobId"].is_null());
    assert_eq!(crate::row(&d,"items","alias").unwrap()["autoPreparation"]["attempts"],0);
}

#[test]
fn active_manual_owner_counts_and_unproven_ownership_is_exclusive() {
    let mut d=fixture();add(&mut d,"j","post-j");
    let manual=crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["i".into()],instruction:None}).unwrap();
    let before=d.clone();
    assert!(claim_width(&mut d,NOW,1).unwrap().is_none());assert_eq!(d["jobs"],before["jobs"]);
    assert!(claim_width(&mut d,NOW,2).unwrap().is_some());
    for corruption in ["scope","company","reservation","purpose"] {
        let mut d=before.clone();let job=crate::row_mut(&mut d,"jobs",&manual.job_id).unwrap();
        match corruption {
            "scope"=>{job.as_object_mut().unwrap().remove("preparationWorkerScope");},
            "company"=>job["preparationWorkerScope"]["account"]=json!("foreign"),
            "reservation"=>job["scopeReservation"]["keysDigest"]=json!("tampered"),
            _=>job["purpose"]=json!("public_fact_followup"),
        }
        let jobs=d["jobs"].clone();
        assert!(claim_width(&mut d,NOW,8).unwrap().is_none(),"{corruption}");
        assert_eq!(d["jobs"],jobs);assert!(crate::row(&d,"items","j").unwrap()["autoPreparation"]["jobId"].is_null());
    }
}

#[test]
fn concurrent_manual_claim_invalidates_preview_without_committing_a_second_attempt() {
    let mut d=fixture();add(&mut d,"j","post-j");
    let mut preview=d.clone();let (_,expected)=claim_width(&mut preview,NOW,1).unwrap().unwrap();
    crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["j".into()],instruction:None}).unwrap();
    let committed=d.clone();let mut transaction=d.clone();
    let actual=claim_width(&mut transaction,NOW,1).unwrap();
    assert!(crate::engine_prepare::capacity::same_capture(Some(&expected),actual.as_ref().map(|(_,request)|request)).is_err());
    // The caller commits only on success; the full pool creates no speculative job.
    assert_eq!(transaction["jobs"],committed["jobs"]);assert_eq!(d,committed);
    assert!(crate::row(&transaction,"items","i").unwrap()["autoPreparation"]["attempts"].is_null());
}

#[test]
fn healthy_large_queue_uses_one_reservation_scan_and_rare_conflict_splits_only_its_path() {
    let ids:Vec<String>=(0..1200).map(|n|format!("item-{n}")).collect();
    let mut calls=0;let mut kept=Vec::new();
    retain_available(&ids,&mut |_|{calls+=1;true},&mut kept);
    assert_eq!(kept,ids);assert_eq!(calls,1);
    calls=0;kept.clear();
    retain_available(&ids,&mut |selected|{calls+=1;!selected.contains(&ids[0])},&mut kept);
    assert_eq!(kept,ids[1..]);assert!(calls<=23,"{calls} guards instead of 1200 singleton scans");
}

#[test]
fn batch_availability_matches_singletons_for_paid_aliases_and_malformed_owners() {
    let mut d=fixture();let (owner,_)=claim_width(&mut d,NOW,2).unwrap().unwrap();
    add(&mut d,"alias","alias-post");add(&mut d,"free","free-post");
    crate::row_mut(&mut d,"items","alias").unwrap()["conversationKey"]=json!("thread");
    let ids=vec!["i".to_owned(),"alias".to_owned(),"free".to_owned()];
    for malformed in [false,true] {
        if malformed {crate::row_mut(&mut d,"jobs",&owner).unwrap()["scopeReservation"]["keysDigest"]=json!("bad");}
        let check=|ids:&[String]|crate::preparation_reservations::assert_available(&d,ids,None).is_ok();
        let expected:Vec<_>=ids.iter().filter(|id|check(std::slice::from_ref(id))).cloned().collect();
        let mut kept=Vec::new();retain_available(&ids,&mut |ids|check(ids),&mut kept);
        assert_eq!(kept,expected);
        if !malformed {assert_eq!(kept,vec!["free"]);}
    }
}
