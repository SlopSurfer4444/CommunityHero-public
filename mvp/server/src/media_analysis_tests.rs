use super::*;

fn sha(c:char)->String { std::iter::repeat_n(c,64).collect() }
fn reference_fixture(c:char)->Value {json!({"sha256":sha(c),"bytes":100})}
fn request()->Value {
    json!({"companyId":"likeavto","stage":"asr","verifiedFile":{"sha256":sha('a'),"bytes":4096,"receiptSha256":sha('b'),"probeSha256":sha('c')},
        "attemptId":"attempt-1","owner":"worker-1","epoch":1,"specSha256":sha('d'),"manifestKey":"attempt-1/manifest",
        "durationMs":2000,"segments":[{"index":0,"startMs":0,"endMs":1000},{"index":1,"startMs":1000,"endMs":2000}]})
}
fn segment(r:&Value,index:u64)->Value {
    let mut q=r.clone();
    q["segment"]=json!({"index":index,"startMs":index*1000,"endMs":(index+1)*1000,"actualDurationMs":1000,
        "rawOutput":reference_fixture('e'),"normalizedOutput":reference_fixture('f'),"verificationSha256":sha('1'),"outputBinding":r});
    q
}
fn full(r:&Value)->Value {
    let mut q=r.clone();q["result"]=json!({"manifest":reference_fixture('2'),"normalizedOutput":reference_fixture('3'),
        "coverage":{"kind":"full_audio","durationMs":2000},"outcome":"transcript","verificationSha256":sha('4')});q
}
fn dispatch(l:&mut Value,r:&Value,i:u64)->Value {
    let mut q=r.clone();q["segmentIndex"]=json!(i);mark_dispatched(l,&q).unwrap()
}
fn capture(l:&mut Value,r:&Value,i:u64) {dispatch(l,r,i);commit_segment(l,&segment(r,i)).unwrap();}
fn completed()->(Value,Value) {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();capture(&mut l,&r,0);capture(&mut l,&r,1);commit_full_result(&mut l,&full(&r)).unwrap();(l,r)
}
fn failed_request(r:&Value)->Value {let mut q=r.clone();q["reason"]=json!("process_return_unknown");q}
fn reconcile_request(r:&Value)->Value {
    let mut q=r.clone();q["action"]=json!("reconcile");q["cessationSha256"]=json!(sha('5'));q["reconciliationSha256"]=json!(sha('6'));
    q["resolution"]=json!("output_missing_after_dispatch");q
}
fn recovery_request(r:&Value)->Value {
    let mut q=r.clone();q["action"]=json!("new_attempt");let mut next=r.clone();next["attemptId"]=json!("attempt-2");
    next["owner"]=json!("worker-2");next["epoch"]=json!(2);next["manifestKey"]=json!("attempt-2/manifest");q["nextAttempt"]=next;
    q["authority"]=json!({"companyId":"likeavto","fileSha256":sha('a'),"priorAttemptId":"attempt-1", "nextSpecSha256":sha('d'),"purpose":"missing_segments","actor":"owner","receiptSha256":sha('7')});q
}

#[test]
fn mixed_specs_and_posts_have_one_file_stage_owner() {
    let r=request();let mut l=Value::Null;assert_eq!(reserve(&mut l,&r).unwrap()["disposition"],"reserved");
    let mut competing=r.clone();competing["specSha256"]=json!(sha('8'));competing["attemptId"]=json!("other");competing["postId"]=json!("other-post");
    assert_eq!(reserve(&mut l,&competing).unwrap()["disposition"],"owned");assert_eq!(l["analyses"].as_array().unwrap().len(),1);
    assert_eq!(l["analyses"][0]["attempts"].as_array().unwrap().len(),1);
}
#[test]
fn identical_file_in_foreign_company_cannot_read_or_reserve() {
    let (mut l,r)=completed();let before=l.clone();let mut foreign=r;foreign["companyId"]=json!("baw-russia");
    assert!(read_result(&l,&foreign).is_err());assert!(reserve(&mut l,&foreign).is_err());assert_eq!(before,l);
}
#[test]
fn file_size_or_probe_conflict_is_not_a_cache_hit() {
    let (l,r)=completed();for field in ["bytes","probeSha256"] {let mut q=r.clone();q["verifiedFile"][field]=if field=="bytes"{json!(123)}else{json!(sha('9'))};assert!(read_result(&l,&q).is_err());}
}
#[test]
fn title_url_and_acquisition_receipt_drift_preserve_original_paid_result() {
    let (mut l,r)=completed();let before=l.clone();let mut q=r;q["title"]=json!("changed title");q["url"]=json!("changed locator");q["sourceVersion"]=json!("changed");q["verifiedFile"]["receiptSha256"]=json!(sha('9'));
    assert_eq!(reserve(&mut l,&q).unwrap()["disposition"],"reuse");assert_eq!(l,before);
}
#[test]
fn completed_incompatible_model_is_held_without_a_new_attempt() {
    let (mut l,mut r)=completed();let before=l.clone();r["specSha256"]=json!(sha('8'));assert_eq!(reserve(&mut l,&r).unwrap()["disposition"],"incompatible");assert_eq!(l,before);
}
#[test]
fn dispatched_without_output_persists_unknown_across_snapshot_reload() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();dispatch(&mut l,&r,0);fail(&mut l,&failed_request(&r)).unwrap();
    let mut reloaded:Value=serde_json::from_str(&l.to_string()).unwrap();let mut q=r.clone();q["owner"]=json!("new-worker");q["specSha256"]=json!(sha('8'));
    assert_eq!(reserve(&mut reloaded,&q).unwrap()["disposition"],"unknown");assert!(mark_dispatched(&mut reloaded,&q).is_err());assert_eq!(l,reloaded);
}
#[test]
fn segment_is_durable_before_next_dispatch_and_duplicate_intent_is_no_permission() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();let mut q=r.clone();q["segmentIndex"]=json!(1);
    assert!(mark_dispatched(&mut l,&q).is_err());assert_eq!(dispatch(&mut l,&r,0)["disposition"],"dispatch_reserved");
    assert_eq!(dispatch(&mut l,&r,0)["disposition"],"already_dispatched");commit_segment(&mut l,&segment(&r,0)).unwrap();
    assert_eq!(dispatch(&mut l,&r,1)["disposition"],"dispatch_reserved");
}
#[test]
fn cannot_capture_segment_without_durable_intent() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();let before=l.clone();assert!(commit_segment(&mut l,&segment(&r,0)).is_err());assert_eq!(l,before);
}
#[test]
fn original_owner_epoch_spec_and_manifest_all_fence_capture() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();dispatch(&mut l,&r,0);
    for k in ["owner","epoch","specSha256","manifestKey","attemptId"] {let mut q=segment(&r,0);q[k]=if k=="epoch"{json!(2)}else{json!("changed")};let before=l.clone();assert!(commit_segment(&mut l,&q).is_err(),"{k}");assert_eq!(before,l);}
}
#[test]
fn result_cannot_claim_full_audio_before_all_segments() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();capture(&mut l,&r,0);let before=l.clone();assert!(commit_full_result(&mut l,&full(&r)).is_err());assert_eq!(l,before);
}
#[test]
fn segment_capture_is_idempotent_but_output_overwrite_fails() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();capture(&mut l,&r,0);let before=l.clone();
    assert_eq!(commit_segment(&mut l,&segment(&r,0)).unwrap()["disposition"],"already_captured");let mut q=segment(&r,0);q["segment"]["normalizedOutput"]=reference_fixture('9');assert!(commit_segment(&mut l,&q).is_err());assert_eq!(before,l);
}
#[test]
fn ocr_failure_or_catalog_conflict_cannot_erase_full_audio() {
    let (mut l,r)=completed();let before=l.clone();let mut q=failed_request(&r);q["reason"]=json!("ocr_failed_catalog_conflict");
    assert_eq!(fail(&mut l,&q).unwrap()["disposition"],"completed_preserved");assert_eq!(read_result(&l,&r).unwrap()["disposition"],"reuse");assert_eq!(before,l);
}
#[test]
fn durable_output_can_be_adopted_after_unknown_without_inference() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();capture(&mut l,&r,0);capture(&mut l,&r,1);fail(&mut l,&failed_request(&r)).unwrap();
    let mut q=full(&r);q["action"]=json!("adopt_full_result");assert_eq!(recover(&mut l,&q).unwrap()["disposition"],"completed");assert_eq!(read_result(&l,&r).unwrap()["disposition"],"reuse");
}
#[test]
fn timeout_or_new_authority_cannot_bypass_unknown_owner() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();dispatch(&mut l,&r,0);fail(&mut l,&failed_request(&r)).unwrap();let before=l.clone();
    let mut q=recovery_request(&r);q["elapsedMs"]=json!(u64::MAX);q["processAbsent"]=json!(true);assert!(recover(&mut l,&q).is_err());assert_eq!(l,before);
}
#[test]
fn explicit_missing_segment_recovery_preserves_old_owner_outputs_and_fences_stale_worker() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();capture(&mut l,&r,0);dispatch(&mut l,&r,1);fail(&mut l,&failed_request(&r)).unwrap();
    let old=l["analyses"][0]["attempts"][0]["segments"][0].clone();recover(&mut l,&reconcile_request(&r)).unwrap();let q=recovery_request(&r);recover(&mut l,&q).unwrap();
    assert_eq!(l["analyses"][0]["attempts"][1]["segments"][0],old);assert!(commit_segment(&mut l,&segment(&r,1)).is_err());
    let next=&q["nextAttempt"];capture(&mut l,next,1);commit_full_result(&mut l,&full(next)).unwrap();assert_eq!(read_result(&l,next).unwrap()["disposition"],"reuse");
}
#[test]
fn changed_recovery_spec_or_foreign_authority_fails_atomically() {
    let (l,r)=completed();for k in ["companyId","fileSha256","priorAttemptId","nextSpecSha256"] {let mut l=l.clone();let mut q=recovery_request(&r);q["authority"][k]=json!("wrong");let before=l.clone();assert!(recover(&mut l,&q).is_err());assert_eq!(before,l);}
}
#[test]
fn storage_adapter_preserves_unrelated_jobs_and_roundtrips_unknown_segments() {
    let r=request();let mut workspace=json!({"account":"likeavto","jobs":[{"id":"social-unknown","kind":"publish","status":"UNKNOWN","paid":true}],"operations":[{"id":"protected"}]});
    let original=workspace.clone();let mut l=ledger_from_workspace(&workspace).unwrap();reserve(&mut l,&r).unwrap();capture(&mut l,&r,0);dispatch(&mut l,&r,1);fail(&mut l,&failed_request(&r)).unwrap();put_ledger(&mut workspace,&l).unwrap();
    let restored:Value=serde_json::from_str(&workspace.to_string()).unwrap();assert_eq!(ledger_from_workspace(&restored).unwrap(),l);assert_eq!(workspace["jobs"][0],original["jobs"][0]);assert_eq!(workspace["operations"],original["operations"]);
}
#[test]
fn storage_boundary_rejects_deletion_retargeting_and_output_replacement() {
    let (l,_)=completed();for mode in 0..4 {let mut bad=l.clone();match mode {0=>bad["analyses"]=json!([]),1=>bad["companyId"]=json!("baw-russia"),2=>bad["analyses"][0]["attempts"][0]["owner"]=json!("new"),_=>bad["analyses"][0]["results"]=json!([])}assert!(validate_transition(&l,&bad).is_err());}
}
#[test]
fn fabricated_result_hash_cannot_be_read_from_valid_catalog_status() {
    let (mut l,r)=completed();l["analyses"][0]["results"][0]["normalizedOutput"]=reference_fixture('9');assert!(read_result(&l,&r).is_err());assert!(validate_transition(&Value::Null,&l).is_err());
}
#[test]
fn no_audio_uses_verified_empty_plan_without_fake_asr_dispatch() {
    let mut r=request();r["noAudio"]=json!(true);r["noAudioVerificationSha256"]=json!(sha('9'));r["segments"]=json!([]);let mut l=Value::Null;reserve(&mut l,&r).unwrap();
    let mut q=full(&r);q["result"]["outcome"]=json!("no_audio");q["result"]["coverage"]["kind"]=json!("no_audio_stream");commit_full_result(&mut l,&q).unwrap();assert_eq!(read_result(&l,&r).unwrap()["result"]["outcome"],"no_audio");assert!(l["analyses"][0]["attempts"][0]["dispatched"].as_array().unwrap().is_empty());
}
#[test]
fn unsupported_storage_version_and_foreign_carrier_fail_explicitly() {
    let mut w=json!({"account":"likeavto","jobs":[]});let (mut l,_)=completed();l["schemaVersion"]=json!(999);assert!(put_ledger(&mut w,&l).is_err());
    let (l,_)=completed();put_ledger(&mut w,&l).unwrap();w["jobs"][0]["account"]=json!("baw-russia");assert!(ledger_from_workspace(&w).is_err());
}
#[test]
fn broad_writer_cannot_append_owned_attempt_behind_unknown_or_create_extra_key_lane() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();dispatch(&mut l,&r,0);fail(&mut l,&failed_request(&r)).unwrap();
    let mut bad=l.clone();let mut next=r.clone();next["attemptId"]=json!("attempt-2");next["owner"]=json!("worker-2");next["epoch"]=json!(2);next["manifestKey"]=json!("second");
    bad["analyses"][0]["attempts"].as_array_mut().unwrap().push(new_attempt(&next).unwrap());assert!(validate_transition(&l,&bad).is_err());
    let mut bad=l.clone();bad["analyses"][0]["key"]["specSha256"]=json!(sha('8'));assert!(validate_transition(&Value::Null,&bad).is_err());
}
#[test]
fn foreign_recovery_original_request_cannot_rebind_paid_outputs() {
    let (mut l,r)=completed();let before=l.clone();let mut q=recovery_request(&r);q["nextAttempt"]["companyId"]=json!("baw-russia");assert!(recover(&mut l,&q).is_err());assert_eq!(l,before);
}
#[test]
fn truncated_segment_and_fabricated_full_state_fail_broad_writer_boundary() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();dispatch(&mut l,&r,0);let before=l.clone();let mut q=segment(&r,0);q["segment"]["actualDurationMs"]=json!(1);assert!(commit_segment(&mut l,&q).is_err());assert_eq!(l,before);
    let mut bad=l.clone();bad["analyses"][0]["attempts"][0]["status"]=json!("completed");assert!(validate_transition(&l,&bad).is_err());
}
#[test]
fn already_captured_full_result_can_be_read_back_idempotently() {
    let (mut l,r)=completed();let before=l.clone();let mut q=r;q["result"]=l["analyses"][0]["results"][0].clone();assert_eq!(commit_full_result(&mut l,&q).unwrap()["disposition"],"already_completed");assert_eq!(l,before);
}
#[test]
fn legacy_bootstrap_is_honest_company_scoped_compatible_and_blocks_config_recompute() {
    let mut r=request();r["normalizedOutput"]=reference_fixture('3');r["manifest"]=reference_fixture('2');
    r["adoption"]=json!({"receiptSha256":sha('5'),"verificationSha256":sha('6'),"originalKnowledge":{"entryId":"knowledge-entry","versionId":"old-admitted-version","sha256":sha('7')},
        "coverage":{"kind":"full_audio","durationMs":2000},"compatibility":{"specSha256":sha('d'),"authorityReceiptSha256":sha('8'),"policy":"verified_legacy_normalized_full_audio"}});
    let mut l=Value::Null;assert_eq!(adopt_completed(&mut l,&r).unwrap()["disposition"],"reuse");assert!(l["analyses"][0]["attempts"].as_array().unwrap().is_empty());
    let output=read_result(&l,&r).unwrap();assert!(output["result"]["owner"].is_null());assert!(output["result"]["rawOutput"].is_null());assert_eq!(output["result"]["adoption"]["originalKnowledge"]["versionId"],"old-admitted-version");
    let before=l.clone();r["specSha256"]=json!(sha('9'));assert_eq!(reserve(&mut l,&r).unwrap()["disposition"],"incompatible");assert_eq!(before,l);
    let mut bad=l.clone();bad["analyses"][0]["attempts"].as_array_mut().unwrap().push(new_attempt(&request()).unwrap());assert!(validate_transition(&l,&bad).is_err());
}
#[test]
fn broad_save_rejects_fake_reconciliation_and_second_uncaptured_dispatch() {
    let r=request();let mut l=Value::Null;reserve(&mut l,&r).unwrap();dispatch(&mut l,&r,0);let owned=l.clone();
    let mut bad=l.clone();let mut second=bad["analyses"][0]["attempts"][0]["dispatched"][0].clone();second["index"]=json!(1);bad["analyses"][0]["attempts"][0]["dispatched"].as_array_mut().unwrap().push(second);assert!(validate_transition(&l,&bad).is_err());
    fail(&mut l,&failed_request(&r)).unwrap();let mut bad=l.clone();bad["analyses"][0]["attempts"][0]["status"]=json!("held");bad["analyses"][0]["attempts"][0]["reconciliation"]=json!({"cessationSha256":sha('5'),"reconciliationSha256":sha('6'),"resolution":"not_dispatched"});assert!(validate_transition(&l,&bad).is_err());
    let mut bad=owned;bad["analyses"][0]["attempts"][0]["status"]=json!("held");bad["analyses"][0]["attempts"][0]["reconciliation"]=json!({"resolution":"output_missing_after_dispatch"});assert!(validate_transition(&l,&bad).is_err());
}
/// Structurally valid durable rows for source-projection/storage guard tests.
/// Digests are fixture values; this does not assert real CAS availability.
pub(super) fn workspace_fixture(account:&str)->Value {
    let mut r=request();r["companyId"]=json!(account);let mut l=Value::Null;reserve(&mut l,&r).unwrap();
    capture(&mut l,&r,0);capture(&mut l,&r,1);commit_full_result(&mut l,&full(&r)).unwrap();
    let result=read_result(&l,&r).unwrap()["result"].clone();
    let mut unknown=r.clone();unknown["verifiedFile"]["sha256"]=json!(sha('8'));unknown["attemptId"]=json!("unknown-attempt");unknown["manifestKey"]=json!("unknown-manifest");
    reserve(&mut l,&unknown).unwrap();dispatch(&mut l,&unknown,0);fail(&mut l,&failed_request(&unknown)).unwrap();
    let mut workspace=json!({"account":account,"jobs":[]});put_ledger(&mut workspace,&l).unwrap();
    let proof=json!({"schemaVersion":1,"kind":"verified_exact_file_analysis_reuse","companyId":account,"account":account,"stage":"asr","specSha256":r["specSha256"],
        "verifiedFile":r["verifiedFile"],"resultSha256":result["resultSha256"],"result":result,"donor":null,"screenReuse":false,
        "target":{"connectorBinding":{"id":"connection-fixture","workspaceId":"workspace-fixture","accountId":account,"connector":"angryspace","revision":1,"providerAccountId":"provider-fixture"},
            "postId":"target-post","postKey":"target-key","sourceVersion":sha('9'),"attachmentIndex":0,"attachmentIdentity":sha('7'),"aliasRevision":1}});
    let row=json!({"id":format!("media-applicability-{}",hash(&proof)),"kind":"media_analysis_applicability","status":"completed","account":account,
        "createdAt":"2026-10-04T00:00:00Z","completedAt":"2026-10-04T00:00:00Z","result":{"schemaVersion":1,"proof":proof}});
    workspace["jobs"].as_array_mut().unwrap().push(row);
    validate_workspace_change(&workspace,&workspace).unwrap();workspace
}
#[test]
fn global_guard_accepts_valid_independent_partial_and_omitted_job_projections() {
    assert!(validate_workspace_change(&json!({}),&json!({"posts":[]})).is_ok());
    assert!(validate_workspace_change(&json!({"jobs":[]}),&json!({"jobs":[{"id":"independent","kind":"media"}]})).is_ok());
    let w=workspace_fixture("likeavto");assert!(validate_workspace_change(&w,&w).is_ok());
    let mut partial=w.clone();partial["jobs"]=json!([w["jobs"][2]]);assert!(validate_workspace_change(&partial,&partial).is_ok());
    let mut ordinary=w.clone();ordinary["jobs"].as_array_mut().unwrap().push(json!({"id":"ordinary","kind":"generation","status":"queued"}));assert!(validate_workspace_change(&w,&ordinary).is_ok());
}
#[test]
fn global_guard_rejects_one_sided_jobs_and_company_retargeting() {
    assert!(validate_workspace_change(&json!({}),&json!({"jobs":[]})).is_err());
    assert!(validate_workspace_change(&json!({"jobs":[]}),&json!({})).is_err());
    let w=workspace_fixture("likeavto");let mut changed=w.clone();changed["account"]=json!("baw-russia");assert!(validate_workspace_change(&w,&changed).is_err());
}
#[test]
fn global_broad_writer_cannot_delete_ledger_unknown_or_change_carrier_fields() {
    let w=workspace_fixture("likeavto");
    for mode in 0..4 {
        let mut changed=w.clone();match mode {
            0=>{changed["jobs"].as_array_mut().unwrap().remove(1);},
            1=>changed["jobs"][1]["analysis"]["attempts"][0]["status"]=json!("owned"),
            2=>changed["jobs"][0]["status"]=json!("queued"),
            _=>changed["jobs"][0]["extra"]=json!("mutated")
        }
        assert!(validate_workspace_change(&w,&changed).is_err(),"{mode}");
    }
}
#[test]
fn borrowed_carrier_fields_match_clone_remove_for_shapes_presence_and_order() {
    fn legacy(prior:&Value,row:&Value)->Result<bool,String> {
        let mut prior=prior.clone();let mut row=row.clone();
        prior.as_object_mut().ok_or("media_analysis_carrier_invalid")?.remove("analysis");
        row.as_object_mut().ok_or("media_analysis_carrier_invalid")?.remove("analysis");
        Ok(prior==row)
    }
    let mut reversed=serde_json::Map::new();
    reversed.insert("status".into(),json!("ledger"));
    reversed.insert("id".into(),json!("carrier"));
    let values=vec![Value::Null,json!([]),json!("carrier"),json!({}),
        json!({"id":"carrier","status":"ledger"}),Value::Object(reversed),
        json!({"id":"carrier","status":"ledger","analysis":null}),
        json!({"id":"carrier","status":"ledger","analysis":{"attempts":[{"status":"unknown"}]}}),
        json!({"id":"other","status":"ledger"}),
        json!({"id":"carrier","status":"ledger","unknownField":null}),
        json!({"id":"carrier","status":"ledger","unknownField":{"nested":[1,2]}})];
    for prior in &values {for row in &values {
        assert_eq!(same_carrier_fields(prior,row),legacy(prior,row),"prior={prior}, row={row}");
    }}
}
#[test]
fn borrowed_carrier_guard_preserves_large_ledger_and_rejects_outer_retargeting() {
    let mut workspace=workspace_fixture("likeavto");
    // Opaque evidence already permitted by the ledger shape must survive the
    // global guard. This is source fixture data, not a real CAS proof.
    workspace["jobs"][0]["analysis"]["retainedFixtureEvidence"]=json!({"nested":["x".repeat(256*1024)]});
    let frozen=workspace.clone();
    validate_workspace_change(&workspace,&workspace).unwrap();
    assert_eq!(workspace,frozen);
    for (field,value) in [("id",json!("foreign-carrier")),("account",json!("baw-russia")),
        ("createdAt",json!("changed")),("unknownField",Value::Null)] {
        let mut changed=workspace.clone();changed["jobs"][0][field]=value;
        assert!(validate_workspace_change(&workspace,&changed).is_err(),"{field}");
        assert_eq!(workspace,frozen);
    }
}
#[test]
fn global_broad_writer_cannot_delete_modify_or_reidentify_applicability() {
    let w=workspace_fixture("likeavto");
    for mode in 0..5 {
        let mut changed=w.clone();match mode {
            0=>{changed["jobs"].as_array_mut().unwrap().remove(2);},
            1=>changed["jobs"][2]["status"]=json!("superseded"),
            2=>changed["jobs"][2]["result"]["proof"]["target"]["sourceVersion"]=json!(sha('5')),
            3=>changed["jobs"][2]["result"]["proof"]["companyId"]=json!("baw-russia"),
            _=>changed["jobs"][2]["id"]=json!("reidentified")
        }
        assert!(validate_workspace_change(&w,&changed).is_err(),"{mode}");
    }
}
#[test]
fn global_guard_requires_structural_result_file_target_company_for_new_applicability() {
    let w=workspace_fixture("likeavto");let mut without=w.clone();without["jobs"].as_array_mut().unwrap().pop();
    assert!(validate_workspace_change(&without,&w).is_ok());
    for mode in 0..9 {
        let mut changed=w.clone();let pin=&mut changed["jobs"][2]["result"]["proof"];
        match mode {
            0=>pin["target"]["aliasRevision"]=json!(0),
            1=>pin["result"]["normalizedOutput"]=json!({"sha256":"invalid","bytes":100}),
            2=>pin["verifiedFile"]["bytes"]=json!(900),
            3=>pin["target"]["attachmentIdentity"]=Value::Null,
            4=>pin["account"]=json!("baw-russia"),
            5=>pin["result"]["verificationSha256"]=Value::Null,
            6=>pin["result"]["coverage"]=Value::Null,
            7=>pin["result"]["originalRequest"]=Value::Null,
            _=>pin["result"]["verificationSha256"]=json!(sha('9'))
        }
        let mut unsigned=pin["result"].clone();unsigned.as_object_mut().unwrap().remove("resultSha256");
        let result_hash=hash(&unsigned);pin["result"]["resultSha256"]=json!(result_hash);pin["resultSha256"]=pin["result"]["resultSha256"].clone();
        let id=format!("media-applicability-{}",hash(pin));changed["jobs"][2]["id"]=json!(id);
        assert!(validate_workspace_change(&without,&changed).is_err(),"{mode}");
    }
}
#[test]
fn applicability_only_projection_rejects_zero_epoch_or_same_epoch_foreign_segment_owner() {
    let w=workspace_fixture("likeavto");let empty=json!({"account":"likeavto","jobs":[]});
    for field in ["epoch","owner","attemptId","manifestKey"] {
        let mut partial=json!({"account":"likeavto","jobs":[w["jobs"][2]]});let pin=&mut partial["jobs"][0]["result"]["proof"];
        pin["result"]["segments"][0][field]=if field=="epoch" {json!(0)}else{json!("foreign")};
        let mut unsigned=pin["result"].clone();unsigned.as_object_mut().unwrap().remove("resultSha256");
        pin["result"]["resultSha256"]=json!(hash(&unsigned));pin["resultSha256"]=pin["result"]["resultSha256"].clone();
        let id=format!("media-applicability-{}",hash(pin));partial["jobs"][0]["id"]=json!(id);
        assert!(validate_workspace_change(&empty,&partial).is_err(),"{field}");
    }
}
