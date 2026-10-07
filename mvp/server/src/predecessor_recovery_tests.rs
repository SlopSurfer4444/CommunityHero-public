//! Pure mocked evidence/reducer tests. These do not launch a helper, native
//! server, PostgreSQL, provider or credential writer and claim no actual stop.
use super::*;
fn p(name:&str,ch:char)->Value{json!({"path":format!("C:\\fixture\\{name}"),"sha256":ch.to_string().repeat(64)})}
fn limit()->Value{json!({"totalMs":120000,"verificationMs":30000,"lockMs":5000,"databaseMs":60000,"cleanupReserveMs":10000,"inputBytes":1048576,"intentBytes":65536,"rawBytes":4194304,"recordBytes":1048576,"resultBytes":1048576,"workspaceBytes":WORKSPACE_BYTES,"cohortBytes":COHORT_BYTES,"cohortRows":4,"rawProcesses":2048,"rawEvents":4096})}
fn workspace()->Value{
    let mut d=crate::connection_gate::tests::workspace();for k in ["knowledge_entries","knowledge_versions","feedback"]{d[k]=json!([]);}d["storageGeneration"]=json!("e4e1d9f2-49b8-4b48-846f-ab113f519f89");
    d["audit"]=json!([{"id":"old-audit","action":"immutable","refId":"original","evidence":{"receipt":"keep"}}]);
    d["approvals"]=json!([{"id":"approved-old","status":"unknown","immutableReceipt":"keep"}]);
    let mut op=crate::connection_gate::tests::operation(&d,1);op["approvalId"]=json!("approved-old");op["providerResult"]=json!({"status":"unknown","actionId":"original-provider-action"});
    d["operations"]=json!([op.clone()]);crate::connection_gate::prearm(&mut d,&op).unwrap();d["operations"][0]["status"]=json!("unknown");d
}
fn scoped(d:&Value)->Value{json!({"companyId":"likeavto","account":"LikeAvto","workspaceId":"local-pilot","connectorBinding":d["connectorBinding"],"storage":{"kind":"postgres","host":"127.0.0.1","port":55432,"database":"communityhero_test","user":"communityhero_test","dataDir":"C:\\fixture\\data","storageGeneration":d["storageGeneration"]}})}
fn pkg()->Value{json!({"core":p("core.json",'a'),"binary":p("server.exe",'b'),"sourceCheckpoint":p("source-manifest.json",'c')})}
fn mocked(d:&Value)->VerifiedImport{
    let mut proof=serde_json::Map::new();for k in ["launchRequest","launchIntent","preLaunchBaseline","subjectCohortCapture","lifecycleAdmission","identity","rawObservation","control"]{proof.insert(k.into(),p(k,'d'));}
    proof.insert("observer".into(),json!({"source":p("source.cs",'e'),"script":p("script.ps1",'e'),"assembly":p("assembly.dll",'e'),"assemblyBuild":p("build.json",'e'),"sourceReview":p("review.json",'e'),"preflight":p("preflight.json",'e')}));
    let m=json!({"schemaVersion":1,"kind":"root-reviewed-predecessor-transport-import","importId":"episode-1","scope":scoped(d),"owner":d["runtimeLifecycle"]["owner"],"package":pkg(),"expectedState":{"ledgerSha256":crate::runtime_lifecycle::ledger_digest(d).unwrap(),"lifecycleSha256":digest(&d["runtimeLifecycle"]),"gateSha256":digest(&d[crate::connection_gate::FIELD]),"auditCount":d["audit"].as_array().unwrap().len()},"predecessorProof":proof,"cohort":cohort(d,&d["runtimeLifecycle"]["owner"]).unwrap(),"nextStartIntent":p("next-intent.json",'f'),"validator":{"binary":pkg()["binary"],"sourceCheckpoint":pkg()["sourceCheckpoint"],"sourcePins":p("sources.json",'f'),"review":p("validator-review.json",'f')},"receiptPath":"C:\\fixture\\result.json","limits":limit()});
    validate_input(&m).unwrap();VerifiedImport{input:m,input_pin:p("input.json",'9'),_pins:io::Pins::new()}
}
fn mocked_launch(before:&Value,id:&str)->VerifiedLaunch {
    let baseline=capture(before,&scoped(before),&before["runtimeLifecycle"]["owner"],&pkg()).unwrap();
    let intent=json!({"schemaVersion":1,"kind":"root-reviewed-native-predecessor-launch-intent","importId":id,"scope":scoped(before),"owner":before["runtimeLifecycle"]["owner"],"package":pkg(),"preLaunchBaseline":p("preLaunchBaseline",'d'),"lifecycleAdmission":p("lifecycleAdmission",'d')});
    VerifiedLaunch{intent,intent_pin:p("launchIntent",'d'),baseline,_pins:io::Pins::new()}
}
fn identity(d:&Value)->crate::runtime_lifecycle::RuntimeIdentity {
    let owner=crate::runtime_lifecycle::parse_token(&d["runtimeLifecycle"]["owner"]).unwrap();
    crate::runtime_lifecycle::RuntimeIdentity{account:owner.account,runtime_id:owner.runtime_id,release_sha256:owner.release_sha256}
}
#[test]
fn contained_transition_preserves_unknown_receipts_approval_and_complete_history(){
    let before=workspace();let import=mocked(&before);let t=import.transition(&before).unwrap();let after=t.workspace();let q=t.output();
    let mut original=before["operations"][0].clone();original[crate::connection_gate::PERMIT_FIELD]=after["operations"][0][crate::connection_gate::PERMIT_FIELD].clone();assert_eq!(original,after["operations"][0]);
    assert_eq!(after["approvals"],before["approvals"]);assert_eq!(after["runtimeLifecycle"],before["runtimeLifecycle"]);assert_eq!(after["audit"][0],before["audit"][0]);
    assert_eq!(after["operations"][0]["status"],"unknown");assert_eq!(after["operations"][0][crate::connection_gate::PERMIT_FIELD]["cessation"]["kind"],"contained");
    assert_eq!(after[crate::connection_gate::FIELD]["state"],"blocked");assert_eq!(after[crate::connection_gate::FIELD]["availability"]["state"],"unverified");assert_eq!(q["dispatchAuthorized"],false);assert_eq!(q["retryAuthorized"],false);
    let r=&after["audit"][1];assert_ne!(r["evidence"]["transitionLedgerSha256"],q["postLedgerSha256"]);assert!(r["evidence"].get("postLedgerSha256").is_none());assert!(r["evidence"].get("importReceipt").is_none());
}
#[test]
fn mixed_history_keeps_operations_without_a_permit_exactly_absent(){
    let mut before=workspace();let mut untouched=crate::connection_gate::tests::operation(&before,2);
    untouched["status"]=json!("planned");assert!(untouched.get(crate::connection_gate::PERMIT_FIELD).is_none());
    before["operations"].as_array_mut().unwrap().push(untouched.clone());
    let import=mocked(&before);assert_eq!(import.input()["cohort"].as_array().unwrap().len(),1);
    let transition=import.transition(&before).unwrap();let after=transition.workspace();
    assert_eq!(after["operations"][1],untouched);assert!(after["operations"][1].get(crate::connection_gate::PERMIT_FIELD).is_none());
    assert_eq!(after["operations"][0][crate::connection_gate::PERMIT_FIELD]["phase"],"transport_settled");
    assert_eq!(import.reconcile(after).unwrap(),transition.output());
}
#[test]
fn lost_q_reconcile_is_deterministic_and_does_not_reapply(){
    let before=workspace();let import=mocked(&before);let t=import.transition(&before).unwrap();let after=t.workspace().clone();let q=t.output();
    assert_eq!(import.reconcile(&after).unwrap(),q);assert_eq!(import.reconcile(&after).unwrap(),q);assert_eq!(after,t.workspace().clone());
    assert!(import.transition(&after).is_err());assert!(import.reconcile(&before).is_err());
    let not_committed=import.observe_reconcile(&before).unwrap();assert_eq!(not_committed["kind"],"native-predecessor-import-not-committed");assert_eq!(not_committed["retryAuthorized"],false);
}
#[test]
fn matching_r_never_admits_post_commit_drift_or_lifecycle_drift(){
    let before=workspace();let import=mocked(&before);let t=import.transition(&before).unwrap();
    for fault in ["metadata","extra_audit","operation","lifecycle","ordinal","settledAt","r_extra"]{let mut d=t.workspace().clone();match fault {
        "metadata"=>d["settings"]["drift"]=json!(true),"extra_audit"=>d["audit"].as_array_mut().unwrap().push(json!({"id":"extra","action":"drift"})),
        "operation"=>d["operations"][0]["providerResult"]["status"]=json!("succeeded"),"lifecycle"=>d["runtimeLifecycle"]["history"].as_array_mut().unwrap().push(json!({"changed":true})),
        "ordinal"=>d["audit"].as_array_mut().unwrap().swap(0,1),"settledAt"=>d["operations"][0][crate::connection_gate::PERMIT_FIELD]["settledAt"]=json!("2026-01-01T00:00:00Z"),_=>d["audit"][1]["evidence"]["selfHash"]=json!("a".repeat(64))}
        assert!(import.reconcile(&d).is_err(),"{fault}");}
}
#[test]
fn private_authority_rejects_http_like_reserved_append_even_exact_copy(){
    let before=workspace();let import=mocked(&before);let t=import.transition(&before).unwrap();
    assert!(validate_reserved_change(&before,t.workspace()).is_err());t.validate(&before).unwrap();
    assert!(validate_reserved_change(&before,t.workspace()).is_err(),"authority must end with synchronous validation");
    let mut removed=t.workspace().clone();removed["audit"].as_array_mut().unwrap().pop();assert!(validate_reserved_change(t.workspace(),&removed).is_err());
}
#[test]
fn pending_import_blocks_every_generic_startup_even_fresh_digest(){
    let before=workspace();let import=mocked(&before);let t=import.transition(&before).unwrap();guard_startup(&before,false).unwrap();assert!(guard_startup(t.workspace(),false).is_err());guard_startup(t.workspace(),true).unwrap();
    let mut duplicate=t.workspace().clone();let row=duplicate["audit"][1].clone();duplicate["audit"].as_array_mut().unwrap().push(row);assert!(guard_startup(&duplicate,false).is_err());
    let mut malformed=t.workspace().clone();malformed["audit"][1]["evidence"]["auditOrdinal"]=json!(0);assert!(guard_startup(&malformed,true).is_err());
}
#[test]
fn mandatory_start_consumes_once_and_old_a_does_not_replay(){
    let before=workspace();let import=mocked(&before);let t=import.transition(&before).unwrap();let after=t.workspace().clone();let q=t.output();
    let start=VerifiedStartup{import,q,q_pin:p("q.json",'1'),a_pin:p("a.json",'2')};
    let consumed=start.transition_for_process(&after,json!({"pid":123,"birthFileTime":"133000000000000001"})).unwrap();
    assert_eq!(consumed.workspace()["audit"][2]["action"],"connection.predecessor_start_consumed");assert_eq!(consumed.workspace()["operations"],after["operations"]);assert_eq!(consumed.workspace()["runtimeLifecycle"],after["runtimeLifecycle"]);
    assert!(start.transition_for_process(consumed.workspace(),json!({"pid":124,"birthFileTime":"133000000000000002"})).is_err());guard_startup(consumed.workspace(),false).unwrap();
    assert_ne!(crate::runtime_lifecycle::ledger_digest(consumed.workspace()).unwrap(),after["audit"][1]["evidence"]["transitionLedgerSha256"].as_str().unwrap());
}
#[test]
fn exact_cohort_generation_owner_and_payload_drift_are_rejected_before_apply(){
    let before=workspace();let import=mocked(&before);
    for fault in ["generation","owner","target","payload","extra_armed","missing_permit","malformed_settled"]{let mut d=before.clone();match fault{
        "generation"=>d["storageGeneration"]=json!("7b02e571-572b-4b41-87a6-40d2c2a36bc3"),"owner"=>d["runtimeLifecycle"]["owner"]["epoch"]=json!(2),
        "target"=>d["operations"][0]["target"]["itemId"]=json!("foreign"),"payload"=>d["operations"][0]["providerResult"]["changed"]=json!(true),
        "extra_armed"=>{let mut op=d["operations"][0].clone();op["id"]=json!("extra");d["operations"].as_array_mut().unwrap().push(op);},
        "missing_permit"=>{d["operations"][0].as_object_mut().unwrap().remove(crate::connection_gate::PERMIT_FIELD);},_=>d["operations"][0][crate::connection_gate::PERMIT_FIELD]["phase"]=json!("transport_settled")}
        let saved=d.clone();assert!(import.transition(&d).is_err(),"{fault}");assert_eq!(d,saved);}
}
#[test]
fn existing_expired_closing_intent_is_finalized_without_renewal(){
    let mut d=workspace();crate::connection_gate::request_close(&mut d,"original-close","auth_unavailable").unwrap();d[crate::connection_gate::FIELD]["closingIntent"]["requestedAt"]=json!("2020-01-01T00:00:00Z");
    let intent=d[crate::connection_gate::FIELD]["closingIntent"].clone();let import=mocked(&d);let t=import.transition(&d).unwrap();
    assert_eq!(t.workspace()[crate::connection_gate::FIELD]["closingIntent"],intent);assert_eq!(t.workspace()[crate::connection_gate::FIELD]["finalReceipt"]["intentId"],"original-close");assert_eq!(t.workspace()[crate::connection_gate::FIELD]["finalReceipt"]["providerCapablePermits"],0);
}
#[test]
fn closed_contract_rejects_reset_scope_aliases_and_unbounded_limits(){
    let d=workspace();let import=mocked(&d);
    for (key,value) in [("postLedger",json!("a".repeat(64))),("kind",json!("automatic-recovery")),("importId",json!("../reset"))]{let mut m=import.input.clone();m[key]=value;assert!(validate_input(&m).is_err());}
    for (pointer,value) in [("/limits/cohortRows",json!(5)),("/scope/storage/kind",json!("sqlite")),("/scope/storage/storageGeneration",json!("legacy")),("/scope/companyId",json!("baw-russia")),("/validator/binary/sha256",json!("0".repeat(64)))]{let mut m=import.input.clone();*m.pointer_mut(pointer).unwrap()=value;assert!(validate_input(&m).is_err(),"{pointer}");}
    assert!(io::parse(br#"{"kind":"first","kind":"second"}"#).is_err());assert!(io::parse(br#"{"nested":{"same":1,"same":2}}"#).is_err());
}
fn raw()->Value{json!({"status":"passed","failureCode":null,"executable":"C:\\fixture\\server.exe","executableSha256":"b".repeat(64),"actualImage":"C:\\fixture\\server.exe","pid":10,"observerPid":9,"creationFileTime":133000000000000001u64,"observationCutoffFileTime":133000000000000099u64,"elapsedMs":100,"pipeReadersClosed":true,"assignedBeforeResume":true,"imageVerified":true,"cleanupComplete":true,"wmiComplete":true,"controlledStop":true,"controlSha256":"c".repeat(64),"exitCode":0xe103,"before":{"totalProcesses":0,"activeProcesses":0,"totalTerminatedProcesses":0},"suspended":{"totalProcesses":1,"activeProcesses":1,"totalTerminatedProcesses":0},"final":{"totalProcesses":2,"activeProcesses":0,"totalTerminatedProcesses":2},"events":[{"kind":"start","image":"server.exe","pid":10,"parentPid":9,"exitCode":0,"timeCreated":133000000000000001u64},{"kind":"start","image":"provider.exe","pid":11,"parentPid":10,"exitCode":0,"timeCreated":133000000000000005u64},{"kind":"stop","image":"provider.exe","pid":11,"parentPid":10,"exitCode":1,"timeCreated":133000000000000050u64},{"kind":"stop","image":"server.exe","pid":10,"parentPid":9,"exitCode":0xe103,"timeCreated":133000000000000090u64}],"wmiErrors":[],"stdoutBytes":100,"stderrBytes":0,"outputCapExceeded":false})}
#[test]
fn whole_inner_tree_rejects_summary_only_fast_child_omission_and_foreign_episode(){
    let good=raw();evidence::verify_tree(&good).unwrap();
    for fault in ["summary","missing_stop","reused_pid","foreign_parent","wrong_birth","live_child","wmi_gap","image","root_exit"]{let mut r=good.clone();match fault{
        "summary"=>r["events"]=json!([]),"missing_stop"=>{r["events"].as_array_mut().unwrap().remove(2);},"reused_pid"=>r["events"][1]["pid"]=json!(10),"foreign_parent"=>{r["events"][1]["parentPid"]=json!(99);r["events"][2]["parentPid"]=json!(99);},
        "wrong_birth"=>r["creationFileTime"]=json!(133000000000000006u64),"live_child"=>r["final"]["activeProcesses"]=json!(1),"wmi_gap"=>r["wmiComplete"]=json!(false),"image"=>r["events"][0]["image"]=json!("outer-node.exe"),_=>r["events"][3]["exitCode"]=json!(0)}assert!(evidence::verify_tree(&r).is_err(),"{fault}");}
}
#[test]
fn controlled_subject_requires_exact_control_birth_binary_and_real_exit(){
    let r=raw();let identity=json!({"kind":"owned-suspended-process-identity","pid":10,"birthFileTime":"133000000000000001","executable":r["executable"],"executableSha256":r["executableSha256"],"assignedBeforeResume":true,"imageVerified":true});let control=json!({"kind":"stop-owned-process","pid":10,"birthFileTime":"133000000000000001"});let control_pin=p("stop.json",'c');let request=json!({"controlPath":control_pin["path"],"timeoutMs":1000});let binary=p("server.exe",'b');
    evidence::verify_subject(&r,&identity,&control,&control_pin,&request,&binary).unwrap();
    for (field,value) in [("controlledStop",json!(false)),("controlSha256",json!("d".repeat(64))),("creationFileTime",json!(133000000000000002u64)),("actualImage",json!("C:\\fixture\\outer-node.exe")),("exitCode",json!(0))]{let mut wrong=r.clone();wrong[field]=value;assert!(evidence::verify_subject(&wrong,&identity,&control,&control_pin,&request,&binary).is_err(),"{field}");}
}
#[test]
fn native_capture_is_complete_and_filtered_ledger_cannot_pass(){
    let d=workspace();let s=scoped(&d);let owner=&d["runtimeLifecycle"]["owner"];let c=capture(&d,&s,owner,&pkg()).unwrap();validate_capture(&c,&s,owner,&pkg()).unwrap();
    assert_eq!(c["ledgerSha256"],crate::runtime_lifecycle::ledger_digest(&d).unwrap());assert_eq!(c["workspace"],d);
    let mut filtered=c.clone();filtered["workspace"]["audit"]=json!([]);assert!(validate_capture(&filtered,&s,owner,&pkg()).is_err());
    let mut filtered=c.clone();filtered["cohort"]=json!([]);assert!(validate_capture(&filtered,&s,owner,&pkg()).is_err());
}
#[test]
fn causal_baseline_rejects_same_owner_historical_armed_or_missing_complete_capture(){
    let stopped=workspace();let import=mocked(&stopped);let mut base=stopped.clone();base["operations"]=json!([]);
    let baseline=capture(&base,&scoped(&base),&base["runtimeLifecycle"]["owner"],&pkg()).unwrap();let post=capture(&stopped,&scoped(&stopped),&stopped["runtimeLifecycle"]["owner"],&pkg()).unwrap();
    evidence::verify_captures(import.input(),&baseline,&post).unwrap();assert!(evidence::verify_captures(import.input(),&post,&post).is_err(),"same owner/binary/epoch is insufficient when the old permit predates this launch");
    let mut wrong=post.clone();wrong["cohort"]=json!([]);assert!(evidence::verify_captures(import.input(),&baseline,&wrong).is_err());
    let mut wrong=post.clone();wrong["workspace"]["audit"][0]["action"]=json!("rewritten");assert!(evidence::verify_captures(import.input(),&baseline,&wrong).is_err());
}
#[test]
fn durable_latest_occurrence_rejects_old_subject_with_new_launch_and_cohort(){
    let mut base=workspace();let armed=base["operations"][0].clone();base["operations"]=json!([]);let launch=mocked_launch(&base,"episode-1");
    let actual=json!({"pid":10,"birthFileTime":"133000000000000001"});let mut stopped=base.clone();launch.append_for_process(&base,&mut stopped,actual.clone()).unwrap();stopped["operations"]=json!([armed.clone()]);
    let import=mocked(&stopped);let post=capture(&stopped,&scoped(&stopped),&stopped["runtimeLifecycle"]["owner"],&pkg()).unwrap();
    evidence::verify_occurrence(import.input(),&launch.baseline,&post,&raw()).unwrap();
    for field in ["pid","birthFileTime","launchIntent","lifecycleAdmission","preLaunchBaseline","preLedgerSha256"]{let mut wrong=post.clone();let e=&mut wrong["workspace"]["audit"][1]["evidence"];match field {
        "pid"=>e["actualProcess"][field]=json!(99),"birthFileTime"=>e["actualProcess"][field]=json!("133000000000000002"),"preLedgerSha256"=>e[field]=json!("f".repeat(64)),_=>e[field]["sha256"]=json!("f".repeat(64))}
        assert!(evidence::verify_occurrence(import.input(),&launch.baseline,&wrong,&raw()).is_err(),"{field}");
    }
    let mut next=stopped.clone();next["operations"]=json!([]);let next_launch=mocked_launch(&next,"episode-2");let mut later=next.clone();next_launch.append_for_process(&next,&mut later,json!({"pid":20,"birthFileTime":"133000000000000100"})).unwrap();later["operations"]=json!([armed]);
    let mut later_m=mocked(&later).input().clone();later_m["importId"]=json!("episode-2");let later_capture=capture(&later,&scoped(&later),&later["runtimeLifecycle"]["owner"],&pkg()).unwrap();
    assert!(evidence::verify_occurrence(import.input(),&launch.baseline,&later_capture,&raw()).is_err());
    assert!(evidence::verify_occurrence(&later_m,&next_launch.baseline,&later_capture,&raw()).is_err(),"coherent old raw/control cannot authorize the latest native launch");
}
#[test]
fn untracked_same_identity_is_blocked_while_distinct_successor_identity_is_not_exempt_from_transfer(){
    let mut base=workspace();base["operations"]=json!([]);let current=identity(&base);guard_untracked_startup(&base,&current).unwrap();
    let launch=mocked_launch(&base,"episode-1");let mut tracked=base.clone();launch.append_for_process(&base,&mut tracked,json!({"pid":10,"birthFileTime":"133000000000000001"})).unwrap();
    assert!(guard_untracked_startup(&tracked,&current).is_err());let mut epoch=tracked.clone();epoch["runtimeLifecycle"]["owner"]["epoch"]=json!(2);assert!(guard_untracked_startup(&epoch,&current).is_err());
    let successor=crate::runtime_lifecycle::RuntimeIdentity{runtime_id:"explicit-new-runtime".into(),..current};
    guard_untracked_startup(&tracked,&successor).unwrap(); // Existing Admission still requires exact explicit transfer; this guard grants none.
}
#[test]
fn launch_append_is_private_immutable_and_unique_across_all_history(){
    let mut base=workspace();base["operations"]=json!([]);let launch=mocked_launch(&base,"episode-1");let mut after=base.clone();launch.append_for_process(&base,&mut after,json!({"pid":10,"birthFileTime":"133000000000000001"})).unwrap();
    assert!(crate::db_guards::validate_change(&base,&after).is_err());let change=AuthorizedTransition::new(&base,after.clone(),json!({})).unwrap();change.validate(&base).unwrap();assert!(crate::db_guards::validate_change(&base,&after).is_err());
    let reused=mocked_launch(&after,"episode-1");let mut untouched=after.clone();assert!(reused.append_for_process(&after,&mut untouched,json!({"pid":20,"birthFileTime":"133000000000000100"})).is_err());assert_eq!(untouched,after);
    let fresh=mocked_launch(&after,"episode-2");let mut next=after.clone();fresh.append_for_process(&after,&mut next,json!({"pid":20,"birthFileTime":"133000000000000100"})).unwrap();AuthorizedTransition::new(&after,next.clone(),json!({})).unwrap();
    let historical=mocked_launch(&next,"episode-1");let mut unchanged=next.clone();assert!(historical.append_for_process(&next,&mut unchanged,json!({"pid":30,"birthFileTime":"133000000000000200"})).is_err());assert_eq!(unchanged,next);
    let mut changed=next.clone();changed["audit"][1]["evidence"]["actualProcess"]["pid"]=json!(99);assert!(validate_reserved_change(&next,&changed).is_err());
}
#[test]
fn predecessor_consume_and_fresh_launch_form_one_exact_atomic_pair(){
    let before=workspace();let import=mocked(&before);let imported=import.transition(&before).unwrap();let after=imported.workspace().clone();let q=imported.output();
    let start=VerifiedStartup{import,q,q_pin:p("q.json",'1'),a_pin:p("a.json",'2')};let actual=json!({"pid":123,"birthFileTime":"133000000000000001"});
    let consumed=start.transition_for_process(&after,actual.clone()).unwrap();let mut launch=mocked_launch(&after,"fresh-episode-2");launch.intent["lifecycleAdmission"]=start.a_pin.clone();
    let mut combined=consumed.workspace().clone();launch.append_for_process(&after,&mut combined,actual).unwrap();let pair=AuthorizedTransition::new(&after,combined.clone(),start.q["owner"].clone()).unwrap();pair.validate(&after).unwrap();
    assert_eq!(combined["audit"][2]["refId"],"episode-1");assert_eq!(combined["audit"][3]["refId"],"fresh-episode-2");assert!(guard_untracked_startup(&combined,&identity(&combined)).is_err());assert!(crate::db_guards::validate_change(&after,&combined).is_err());
    for field in ["pid","birthFileTime","startupAdmission"]{let mut corrupt=combined.clone();if field=="startupAdmission"{corrupt["audit"][2]["evidence"][field]["sha256"]=json!("f".repeat(64));}else{corrupt["audit"][2]["evidence"]["actualProcess"][field]=if field=="pid"{json!(99)}else{json!("133000000000000002")};}assert!(AuthorizedTransition::new(&after,corrupt,json!({})).is_err(),"{field}");}
}
#[test]
fn launch_outputs_must_belong_to_the_exact_selected_prefix(){
    let prefix="C:\\fixture\\subject";let request=json!({"outputPrefix":prefix,"controlPath":format!("{prefix}.stop.json")});
    let mut proof=json!({});for (key,suffix) in [("launchRequest",".input.json"),("identity",".identity.json"),("rawObservation",".observation.json"),("control",".stop.json")]{proof[key]=json!({"path":format!("{prefix}{suffix}"),"sha256":"a".repeat(64)});}
    evidence::verify_artifact_paths(&proof,&request).unwrap();
    for key in ["launchRequest","identity","rawObservation","control"]{let mut wrong=proof.clone();wrong[key]["path"]=json!("C:\\fixture\\old-subject.input.json");assert!(evidence::verify_artifact_paths(&wrong,&request).is_err(),"{key}");}
    let mut wrong=request.clone();wrong["controlPath"]=json!("C:\\fixture\\old-subject.stop.json");assert!(evidence::verify_artifact_paths(&proof,&wrong).is_err());
}
#[test]
fn derivative_observer_allows_only_reviewed_single_timeout_expression(){
    let base=b"prefix timeoutMs <= 300000 && outputCap suffix";let generated=b"prefix timeoutMs <= (allowDescendants ? 1560000 : 300000) && outputCap suffix";
    evidence::verify_deadline_delta(base,generated).unwrap();assert!(evidence::verify_deadline_delta(base,b"prefix timeoutMs <= 900000 && outputCap suffix").is_err());assert!(evidence::verify_deadline_delta(base,b"prefix timeoutMs <= (allowDescendants ? 1560000 : 300000) && outputCap changed").is_err());
    assert!(evidence::verify_deadline_delta(b"timeoutMs <= 300000 && outputCap timeoutMs <= 300000 && outputCap",generated).is_err());
}

#[test]
fn observer_generation_pins_reject_prior_console_correlation_and_mixed_generations(){
    let canonical=json!({"sha256":"4934332b205253c89bd4236fa90a26a2bfbbf59c7df57ea635c7c23102e86328"});let generated=json!({"sha256":"1ae13f1e753abf7dee01bdc3f1605720fc765e17db614a234357a334000c6d49"});
    evidence::verify_observer_generation_pins(&canonical,&canonical,&generated).unwrap();
    let projected_base=json!({"sha256":"1b9a919d30bfd40099d39b7fab08a736fd94b8981b4e09980c1743b686981ff1"});let projected_generated=json!({"sha256":"809a613b743f674b8fab1b65e8369e2a81d432ae61e6011cc1507cea751c3614"});
    let wildcard_base=json!({"sha256":"395fbfe5079512b43f98438e2b80f4105cdc96d9cbb9f64e078e79678eb1fd4b"});let wildcard_generated=json!({"sha256":"691324e55e7a9f5d8461414286c501b861551fb8654969fc45326e9010bc389c"});
    let no_window_correlation=json!({"sha256":"291698804c998392b3c849da1336c33d445c1b9b658f954168416c21711cf904"});
    for (c,b,g) in [(&projected_base,&projected_base,&projected_generated),(&wildcard_base,&wildcard_base,&wildcard_generated),(&projected_base,&canonical,&generated),(&wildcard_base,&canonical,&generated),(&no_window_correlation,&no_window_correlation,&generated),(&canonical,&projected_base,&generated),(&canonical,&wildcard_base,&generated),(&canonical,&no_window_correlation,&generated),(&canonical,&canonical,&projected_generated),(&canonical,&canonical,&wildcard_generated),(&generated,&generated,&canonical)]{
        assert!(evidence::verify_observer_generation_pins(c,b,g).is_err());
    }
}
#[test]
fn wildcard_query_generation_still_requires_complete_payload_loss_and_cleanup_evidence(){
    let good=raw();
    for field in ["pid","parentPid","image","timeCreated","exitCode"]{let mut bad=good.clone();bad["events"][3].as_object_mut().unwrap().remove(field);assert!(evidence::verify_tree(&bad).is_err(),"{field}");}
    for error in ["provider_dropped","provider_overflow","event_cap_exceeded","event_consumer_failed","watcher_cleanup_failed","watcher_dispose_failed"]{let mut bad=good.clone();bad["wmiErrors"]=json!([error]);assert!(evidence::verify_tree(&bad).is_err(),"{error}");}
    for field in ["cleanupComplete","pipeReadersClosed","wmiComplete","assignedBeforeResume","imageVerified"]{let mut bad=good.clone();bad[field]=json!(false);assert!(evidence::verify_tree(&bad).is_err(),"{field}");}
}

#[test]
fn raw_nullable_stop_parent_correlates_only_inside_the_complete_owned_tree(){
    for parent in [Value::Null,json!(0),json!(10)] {
        let mut r=raw();r["events"][2]["parentPid"]=parent;r["events"][3]["parentPid"]=Value::Null;
        let before=r.clone();evidence::verify_tree(&r).unwrap();assert_eq!(r,before,"raw null/zero must never be filled from start");
    }
}
#[test]
fn nullable_stop_parent_rejects_malformed_missing_or_conflicting_identity(){
    for parent in [json!(99),json!(-1),json!(4294967296u64),json!("0"),json!(false),json!({})]{
        let mut r=raw();r["events"][2]["parentPid"]=parent.clone();assert!(evidence::verify_tree(&r).is_err(),"stop parent {parent}");
    }
    for parent in [Value::Null,json!(0),json!(4294967296u64)]{
        let mut r=raw();r["events"][1]["parentPid"]=parent.clone();r["events"][2]["parentPid"]=Value::Null;
        assert!(evidence::verify_tree(&r).is_err(),"start parent {parent}");
    }
    let mut r=raw();r["events"][2].as_object_mut().unwrap().remove("parentPid");assert!(evidence::verify_tree(&r).is_err());
}
#[test]
fn absent_stop_parent_does_not_admit_lost_reused_disconnected_or_unceased_processes(){
    let mut good=raw();good["events"][2]["parentPid"]=Value::Null;good["events"][3]["parentPid"]=json!(0);
    for fault in ["missing_stop","reused_pid","foreign_parent","self_cycle","wrong_birth","live_child","wmi_gap","loss","cleanup","pipes","stop_image","root_image","root_exit","reversed_pair","late_child","bad_child_exit","start_exit"]{
        let mut r=good.clone();match fault{
            "missing_stop"=>{r["events"].as_array_mut().unwrap().remove(2);},"reused_pid"=>r["events"][2]["pid"]=json!(10),
            "foreign_parent"=>r["events"][1]["parentPid"]=json!(99),"self_cycle"=>r["events"][1]["parentPid"]=json!(11),
            "wrong_birth"=>r["creationFileTime"]=json!(133000000000000006u64),"live_child"=>r["final"]["activeProcesses"]=json!(1),
            "wmi_gap"=>r["wmiComplete"]=json!(false),"loss"=>r["wmiErrors"]=json!(["provider_dropped"]),"cleanup"=>r["cleanupComplete"]=json!(false),"pipes"=>r["pipeReadersClosed"]=json!(false),
            "stop_image"=>r["events"][2]["image"]=json!("foreign.exe"),"root_image"=>r["events"][0]["image"]=json!("foreign.exe"),"root_exit"=>r["events"][3]["exitCode"]=json!(0),
            "reversed_pair"=>r["events"][2]["timeCreated"]=json!(133000000000000004u64),"late_child"=>r["events"][1]["timeCreated"]=json!(133000000000000095u64),
            "bad_child_exit"=>r["events"][2]["exitCode"]=json!(-1),_=>r["events"][1]["exitCode"]=json!(1),
        }assert!(evidence::verify_tree(&r).is_err(),"{fault}");
    }
}
