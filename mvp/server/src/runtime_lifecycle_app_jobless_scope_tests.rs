use super::*;
use serde_json::json;

fn fixture()->(Value,Capture) {
    let identity=RuntimeIdentity{account:"LikeAvto".into(),runtime_id:"jobless-fixture".into(),release_sha256:"a".repeat(64)};
    let owner=OwnerToken{account:identity.account.clone(),runtime_id:identity.runtime_id.clone(),release_sha256:identity.release_sha256.clone(),epoch:1};
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
    for field in ["knowledge_entries","knowledge_versions"] {d[field]=json!([]);}
    let digest=runtime_lifecycle::ledger_digest(&d).unwrap();
    runtime_lifecycle::initialize(&mut d,owner.clone(),&"b".repeat(64),&digest).unwrap();
    d.as_object_mut().unwrap().remove("jobs");
    (d,Capture{identity,token:Some(owner)})
}
fn tls()->Option<(RuntimeIdentity,Option<OwnerToken>)> {
    WRITER.with(|v|v.borrow().as_ref().map(|c|(c.identity.clone(),c.token.clone())))
}
fn shapes()->Vec<Value> {
    vec![Value::Null,json!([]),json!({}),json!("malformed"),
        json!([{"id":"queued","kind":"assistant","status":"queued"}]),
        json!([{"id":"unknown","kind":"assistant","status":"unknown","result":{"retained":true}}])]
}

#[test]
fn jobless_scope_requires_complete_omission_before_and_after_callback() {
    let (d,c)=fixture();let original_tls=tls();
    for jobs in shapes() {
        let mut present=d.clone();present["jobs"]=jobs.clone();let before=present.clone();
        let called=std::cell::Cell::new(false);
        assert!(c.with_jobless_scope(&mut present,|_|{called.set(true);Ok(())}).is_err());
        assert!(!called.get(),"any jobs key must reject before callback");assert_eq!(present,before);assert_eq!(tls(),original_tls);
        let mut absent=d.clone();
        assert!(c.with_jobless_scope(&mut absent,|d|{d["jobs"]=jobs;Ok(())}).is_err());
        assert_eq!(tls(),original_tls,"callback errors restore writer identity");
    }
    let mut absent=d.clone();c.with_jobless_scope(&mut absent,|d|{d["itemEdit"]=json!("ordinary completion");Ok(())}).unwrap();
    assert!(absent.get("jobs").is_none());assert_eq!(absent["runtimeLifecycle"],d["runtimeLifecycle"]);assert_eq!(tls(),original_tls);
}

#[test]
fn jobless_scope_current_fixed_owner_and_account_are_checked_both_sides() {
    let (mut d,c)=fixture();let before=d.clone();let original_tls=tls();
    let mut foreign=c.clone();foreign.identity.runtime_id="foreign-owner".into();
    let called=std::cell::Cell::new(false);
    assert!(foreign.with_jobless_scope(&mut d,|_|{called.set(true);Ok(())}).is_err());assert!(!called.get());assert_eq!(d,before);
    for mode in 0..3 {
        let mut d=before.clone();assert!(c.with_jobless_scope(&mut d,|d|{
            match mode {0=>d["account"]=json!("BAW Russia"),
                1=>d["runtimeLifecycle"]["owner"]["runtimeId"]=json!("successor-owner"),
                _=>{d.as_object_mut().unwrap().remove("runtimeLifecycle");}}
            Ok(())
        }).is_err());assert_eq!(tls(),original_tls);
    }
    let mut closed=c.clone();closed.token=None;
    assert!(closed.with_jobless_scope(&mut d,|d|require_new_job(d,"assistant")).is_err(),
        "job omission cannot manufacture an admission token");
    assert_eq!(d,before);assert_eq!(tls(),original_tls);
}

#[test]
fn jobless_scope_tls_restores_on_nested_error_and_panic_and_allows_drain_completion() {
    let (mut d,c)=fixture();let original_tls=tls();let mut closed=c.clone();closed.token=None;
    c.with_jobless_scope(&mut d,|d| {
        let outer=tls();assert_eq!(outer,Some((c.identity.clone(),c.token.clone())));
        assert!(closed.with_jobless_scope(d,|d|require_new_job(d,"assistant")).is_err());assert_eq!(tls(),outer);
        Ok(())
    }).unwrap();assert_eq!(tls(),original_tls);
    let panic=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
        c.with_jobless_scope::<()>(&mut d,|_|panic!("synthetic jobless callback panic"))));
    assert!(panic.is_err());assert_eq!(tls(),original_tls);
    // Construct drain in the full durable fixture through the existing reducer,
    // then project jobs out again: same fixed owner may save ordinary completion.
    d["jobs"]=json!([]);runtime_lifecycle::begin_drain(&mut d,c.token.as_ref().unwrap(),&"c".repeat(64),"jobless-drain",false).unwrap();
    d.as_object_mut().unwrap().remove("jobs");let lifecycle=d["runtimeLifecycle"].clone();
    closed.with_jobless_scope(&mut d,|d|{d["itemEdit"]=json!("saved during drain");Ok(())}).unwrap();
    assert_eq!(d["runtimeLifecycle"],lifecycle);assert!(d.get("jobs").is_none());assert_eq!(tls(),original_tls);
}
