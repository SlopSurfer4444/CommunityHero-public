//! Authored source-only fixtures. Native execution belongs to ROOT.
use super::*;
fn reference(account:&str,company:&str,hash:&str)->Value {
    json!({"version":1,"kind":"native-paid-capture-ref","company":company,"account":account,
        "binding":{"nativeJobId":"paid-job","operation":"assistant"},
        "runtimeOwner":{"account":account,"runtimeId":"old-owner","releaseSha256":"a".repeat(64)},
        "requestSha256":"b".repeat(64),"responseSha256":"c".repeat(64),
        "artifact":{"sha256":hash,"bytes":100},"retryAuthorized":false,"dispatchAuthorized":false})
}
#[test]
fn paid_history_prefix_is_immutable_but_old_runtime_owner_survives_restart() {
    let old_ref=reference("LikeAvto","likeavto",&"d".repeat(64));
    let before=json!({"account":"LikeAvto","jobs":[{"id":"paid-job","retainedEvidence":[old_ref]}]});
    let mut after=before.clone();after["jobs"][0]["status"]=json!("completed");
    validate_change(&before,&after).unwrap();
    after["jobs"][0]["retainedEvidence"].as_array_mut().unwrap().push(reference("LikeAvto","likeavto",&"e".repeat(64)));
    validate_change(&before,&after).unwrap();
    for variant in 0..6 {
        let mut bad=after.clone();
        match variant {
            0=>{bad["jobs"][0].as_object_mut().unwrap().remove("retainedEvidence");},
            1=>{bad["jobs"][0]["retainedEvidence"]=json!([]);},
            2=>{bad["jobs"][0]["retainedEvidence"].as_array_mut().unwrap().reverse();},
            3=>{bad["jobs"][0]["retainedEvidence"][0]["binding"]["operation"]=json!("media");},
            4=>{bad["jobs"]=json!([]);},
            _=>{bad["jobs"].as_array_mut().unwrap().push(before["jobs"][0].clone());},
        }
        assert!(validate_change(&before,&bad).is_err(),"variant {variant}");
    }
}
#[test]
fn newly_attached_history_rejects_foreign_malformed_duplicate_and_authorizing_refs() {
    let before=json!({"account":"LikeAvto","jobs":[{"id":"paid-job"}]});
    let valid=reference("LikeAvto","likeavto",&"d".repeat(64));
    for path in ["/company","/account","/binding/nativeJobId","/requestSha256","/dispatchAuthorized"] {
        let mut forged=valid.clone();*forged.pointer_mut(path).unwrap()=json!("foreign");
        let mut after=before.clone();after["jobs"][0]["retainedEvidence"]=json!([forged]);
        assert!(validate_change(&before,&after).is_err(),"{path}");
    }
    let mut after=before.clone();after["jobs"][0]["retainedEvidence"]=json!([valid.clone(),valid]);
    assert!(validate_change(&before,&after).is_err(),"duplicate CAS object is not a second capture");
    after["jobs"][0]["retainedEvidence"]=json!({"legacy":"retain"});
    assert!(validate_change(&before,&after).is_err());
}
