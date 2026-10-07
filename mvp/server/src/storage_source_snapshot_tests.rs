use super::*;

fn fixture(history_bytes:usize)->Value {
    let mut d=crate::empty();normalize(&mut d);
    d["posts"]=json!([{"id":"post","text":"Before"}]);
    d["branches"]=json!([{"id":"branch","postId":"post","messages":[]}]);
    d["items"]=json!([{"id":"item","itemId":"provider-item","objectId":"11391","postId":"post","branchId":"branch","providerStatus":"new","workflow":"prepared","draft":"Preserve operator text","draftEdited":true,"revision":3}]);
    d["proposals"]=json!([{"id":"proposal","itemId":"item","status":"approved","text":"Exact approved text"}]);
    let large="x".repeat(history_bytes);
    d["approvals"]=json!([{"id":"approval","status":"approved","proposals":[{"id":"proposal","revision":1}],"context":{"privateEvidence":large}}]);
    d["operations"]=json!([{"id":"operation","itemId":"item","proposalId":"proposal","approvalId":"approval","status":"unknown","receipt":{"source":"must retain"}}]);
    d["conversations"]=json!([{"id":"conversation","itemIds":["item"],"messages":[{"text":large}]}]);
    d["audit"]=json!([{"id":"audit-old","action":"history","refId":"item","payload":large}]);
    d["feedback"]=json!([{"id":"feedback","itemId":"item","text":large}]);
    // These unrelated semantic witnesses must not disappear from the projection.
    d["jobs"]=json!([{"id":"legacy-job","kind":"assistant","purpose":"auto_prepare","status":"completed","prepareOutcome":{"status":"needs_attention"},"prepareBundle":{"itemIds":["item"]}}]);
    validate(&d).unwrap();d
}

fn snapshot()->Value {json!({"posts":[{"id":"post","text":"After"}],"branches":[{"id":"branch","postId":"post","messages":[{"id":"target","author":"Customer","role":"customer","text":"New context"}]}],"items":[{"id":"item","itemId":"provider-item","objectId":"11391","postId":"post","branchId":"branch","providerStatus":"closed","contextObservedAt":"2026-09-27T01:00:00Z","providerStatusObservedAt":"2026-09-27T01:00:00Z","contextEvidenceDigest":"changed"}]})}

fn terminal_input_job(id:&str, bytes:usize)->Value {
    json!({"id":id,"kind":"assistant","status":"completed","purpose":"auto_prepare","refId":"item",
        "prepareBundle":{"version":1,"id":"bundle","digest":"a".repeat(64),"dependencyDigest":"b".repeat(64),
            "itemIds":["item"],"request":{"items":[{"id":"item","text":"x".repeat(bytes)}]}},
        "prepareOutcome":{"itemId":"item","status":"needs_attention","reason":"Saved human-review decision"},
        "preparationStages":{"reviewChunks":{"status":"unknown","providerRetryAllowed":false}},
        "result":{"preserved":"full result"}})
}

fn approval_projection_cases()->Vec<Value> {
    let base=json!({"id":"approval","status":"approved","context":{"old":"omitted"},"proposals":[
        {"id":"proposal","revision":7,"proposal":{"text":"immutable consent"},"item":{"draft":"operator"},"future":{"retained":true}},
        {"id":"proposal","revision":8}]});
    let mut cases=vec![base.clone(),Value::Null,json!([]),json!({}),json!({"other":"field"})];
    for proposals in [Value::Null,json!({}),json!("invalid"),json!([]),json!([1]),json!([null]),
        json!([{}]),json!([{"id":""}]),json!([{"id":12}]),json!([{"id":"proposal","proposal":null}]),
        json!([{"id":"proposal","item":"malformed"}]),json!([{"id":"proposal","proposal":[]}]),
        json!([base["proposals"][0].clone(),Value::Null])] {
        let mut value=base.clone();value["proposals"]=proposals;cases.push(value);
    }
    cases
}

#[test]
fn source_approval_projection_preserves_reference_order_unknown_fields_and_malformed_fallbacks(){
    let cases=approval_projection_cases();let before=&cases[0];let projected=approval(before);
    assert_eq!(projected["proposals"],json!([{"id":"proposal","revision":7,"future":{"retained":true}},{"id":"proposal","revision":8}]));
    assert!(before["proposals"][0]["proposal"].is_object(),"projection never mutates consent");
    for value in &cases[1..] {
        let projected=approval(value);
        if value.is_object(){assert_eq!(projected.get("proposals"),value.get("proposals"),"malformed/empty fallback {value}");}
        else{assert_eq!(projected,*value);}
    }
}

#[test]
fn source_approval_snapshots_do_not_inflate_source_view_or_change_durable_consent(){
    let mut d=fixture(0);
    d["approvals"][0]["proposals"][0]["future"]=json!({"retained":true});
    let small=project(&d).unwrap();
    d["approvals"][0]["proposals"][0]["proposal"]=json!({"text":"x".repeat(512*1024)});
    d["approvals"][0]["proposals"][0]["item"]=json!({"draft":"y".repeat(512*1024)});
    let source=project(&d).unwrap();assert_eq!(source,small);
    assert!(d["approvals"].to_string().len()>source["approvals"].to_string().len()+1024*1024);
    let mut expected=d.clone();crate::merge_snapshot(&mut expected,&snapshot()).unwrap();
    let mut after=source.clone();crate::merge_snapshot(&mut after,&snapshot()).unwrap();
    let mut actual=d.clone();apply(&mut actual,&source,&after).unwrap();
    normalize_observation_clock(&mut expected);normalize_observation_clock(&mut actual);
    assert_eq!(actual,expected);assert_eq!(actual["approvals"],d["approvals"]);
    for field in ["id","revision","future","proposal","item"] {
        let mut illegal=source.clone();illegal["approvals"][0]["proposals"][0][field]=json!("forged");
        assert!(apply(&mut d.clone(),&source,&illegal).is_err(),"source must not rewrite {field} consent");
    }
}

#[test]
fn source_jobs_preserve_all_rows_and_protect_nested_and_ambiguous_lineage() {
    let mut d=fixture(0);
    d["jobs"]=json!([terminal_input_job("unreferenced",4096),terminal_input_job("nested",4096)]);
    d["proposals"][0]["history"]=json!([{"origin":{"recovery":{"prepareRunId":"nested"}}}]);
    let protected=protected_jobs(&d).unwrap();
    let compact=source_job(&d["jobs"][0],Some(&protected));
    assert!(compact["prepareBundle"].get("request").is_none());
    let mut expected=d["jobs"][0].clone();expected["prepareBundle"].as_object_mut().unwrap().remove("request");
    expected.as_object_mut().unwrap().remove("result");
    assert_eq!(compact,expected,"only the request and completed assistant result may be omitted");
    assert_eq!(source_job(&d["jobs"][1],Some(&protected)),d["jobs"][1]);
    let projected=project(&d).unwrap();
    assert_eq!(projected["jobs"].as_array().unwrap().len(),2);
    assert_eq!(projected["jobs"][0]["id"],"unreferenced");
    assert_eq!(projected["jobs"][1]["id"],"nested");
    for key in ["autoPreparation","autoRevalidation"] {
        let mut linked=d.clone();linked["items"][0][key]=json!({"jobId":"unreferenced"});
        assert_eq!(project(&linked).unwrap()["jobs"][0],d["jobs"][0]);
    }
    for ambiguous in [json!({"jobId":[]}),json!({"prepareRunId":7}),json!({"jobId":""}),json!({"history":"invalid"}),json!({"recovery":"invalid"})] {
        let mut invalid=d.clone();invalid["proposals"][0]["origin"]=ambiguous;
        assert!(protected_jobs(&invalid).is_none());
        assert_eq!(project(&invalid).unwrap()["jobs"],invalid["jobs"]);
    }
}

fn job_projection_cases()->Vec<Value> {
    let original=terminal_input_job("candidate",1024);
    let mut cases=vec![original.clone(),Value::Null,json!([]),json!({})];
    for status in ["running","queued","paused","unknown","interrupted","unrecognized"] {
        let mut v=original.clone();v["status"]=json!(status);cases.push(v);
    }
    for kind in ["media","sync","unrecognized"] {let mut v=original.clone();v["kind"]=json!(kind);cases.push(v);}
    for field in ["id","prepareBundle"] {let mut v=original.clone();v[field]=json!([]);cases.push(v);}
    for (field,value) in [("version",json!("1")),("version",json!(1.0)),("version",json!(2)),("digest",json!("A".repeat(64))),
        ("dependencyDigest",json!(123)),("itemIds",json!({})),("request",json!("malformed"))] {
        let mut v=original.clone();v["prepareBundle"][field]=value;cases.push(v);
    }
    cases
}

fn media_payload_fixture(profile:crate::accounts::Profile,bytes:usize)->Value {
    let mut d=fixture(0);d["account"]=json!(profile.display());d["connectorBinding"]=profile.binding();
    d["posts"][0]["postKey"]=json!("source-key");d["posts"][0]["attachments"]=json!([{"type":"video"}]);
    d["items"][0]["postKey"]=json!("source-key");
    let post=&d["posts"][0];
    let mut p=crate::media_fullframes::initial(profile.display(),&profile.binding(),post,"2026-09-27T00:00:00Z");
    p["phase"]=json!("complete");p["source"]=json!({"sha256":"a".repeat(64),"bytes":1024});
    p["sourceIdentity"]=json!({"account":profile.display(),"postKey":"source-key","mediaSha256":"a".repeat(64),"durationMs":181000,"extraIdentity":"retained"});
    p["frames"]=json!([{"synthetic":"x".repeat(bytes)}]);
    d["jobs"]=json!([{"id":"candidate","kind":"media","purpose":"auto_media","status":"completed",
        "visualContractVersion":2,"account":profile.display(),"connectorBinding":profile.binding(),
        "refId":"post","createdAt":"2026-09-27T00:00:00Z","sourceAttempts":[{"status":"unknown","providerRetryAllowed":false}],
        "result":{"visualProgress":p,"synthetic":"x".repeat(bytes)}}]);d
}

fn media_projection_cases()->Vec<Value> {
    let base=media_payload_fixture(crate::accounts::Profile::LikeAvto,64)["jobs"][0].clone();
    let mut cases=vec![base.clone(),Value::Null,json!([]),json!({})];
    for (path,value) in [
        ("/status",json!("unknown")),("/status",json!("running")),("/status",json!("failed")),
        ("/status",json!("interrupted")),("/status",json!("paused")),("/status",json!("queued")),("/status",json!("cancelled")),
        ("/kind",json!("assistant")),("/purpose",json!("other")),("/visualContractVersion",json!(2.0)),("/visualContractVersion",json!("2")),
        ("/id",json!("")),("/account",Value::Null),("/createdAt",json!([])),("/connectorBinding",Value::Null),
        ("/result",json!([])),("/result/visualProgress",json!("malformed")),
        ("/result/visualProgress/schemaVersion",json!(2.0)),("/result/visualProgress/schemaVersion",json!("2")),
        ("/result/visualProgress/phase",json!("held")),("/result/visualProgress/phase",json!("scan")),
        ("/result/visualProgress/sourcePostKey",Value::Null),("/result/visualProgress/connectorBinding",json!([])),
        ("/result/visualProgress/sourceIdentity",json!([])),("/result/visualProgress/sourceIdentity/account",json!("")),
        ("/result/visualProgress/sourceIdentity/durationMs",json!(0)),("/result/visualProgress/sourceIdentity/durationMs",json!(1.0)),
        ("/result/visualProgress/sourceIdentity/durationMs",json!("181000")),
        ("/result/visualProgress/sourceIdentity/durationMs",json!(1e18)),
        ("/result/visualProgress/source",json!({"sha256":"a".repeat(64),"bytes":1,"extra":true})),
        ("/result/visualProgress/source/sha256",json!("A".repeat(64))),
        ("/result/visualProgress/source/bytes",json!(-1)),("/result/visualProgress/source/bytes",json!(1.0)),
        ("/result/visualProgress/source/bytes",json!("1024")),("/result/visualProgress/source/bytes",json!(18446744073709551616.0)),
        ("/result/visualProgress/source/bytes",json!(1e18)),
    ] {let mut v=base.clone();*v.pointer_mut(path).unwrap()=value;cases.push(v);}
    for field in ["account","sourcePostKey","sourceIdentity","source"] {
        let mut v=base.clone();v["result"]["visualProgress"].as_object_mut().unwrap().remove(field);cases.push(v);
    }
    cases
}

#[test]
fn source_completed_payload_projection_preserves_fallbacks_and_unknown_controls() {
    let protected=HashSet::new();
    for (index,job) in media_projection_cases().iter().enumerate() {
        assert_eq!(compactable_media_result(job,Some(&protected)),index==0,"case {index}");
        assert_eq!(source_job(job,None),*job,"ambiguous lineage keeps full media payload");
        if index>0 {assert_eq!(source_job(job,Some(&protected)),*job,"case {index}");}
    }
    let d=media_payload_fixture(crate::accounts::Profile::LikeAvto,1024);let job=&d["jobs"][0];
    let view=source_job(job,Some(&protected));
    assert_eq!(view["sourceAttempts"],job["sourceAttempts"],"root UNKNOWN/no-retry history survives");
    assert_eq!(view["result"]["visualProgress"]["sourceIdentity"],job["result"]["visualProgress"]["sourceIdentity"]);
    assert_eq!(view["result"]["visualProgress"]["source"],job["result"]["visualProgress"]["source"]);
    let mut root=view.clone();root["result"]=job["result"].clone();assert_eq!(root,*job,"all root fields retained");
    let linked=HashSet::from(["candidate".to_owned()]);assert_eq!(source_job(job,Some(&linked)),*job);
    for (field,value) in [("resumePhase",Value::Null),("resumePhase",json!({"unknown":true}))] {
        let mut optional=job.clone();optional["result"]["visualProgress"][field]=value.clone();
        assert_eq!(source_job(&optional,Some(&protected))["result"]["visualProgress"][field],value);
    }
    let mut absent=job.clone();absent["result"]["visualProgress"].as_object_mut().unwrap().remove("resumePhase");
    assert!(source_job(&absent,Some(&protected))["result"]["visualProgress"].get("resumePhase").is_none());
    for status in ["completed","failed","cancelled","unknown","interrupted"] {
        let mut assistant=terminal_input_job("assistant",64);assistant["status"]=json!(status);
        let compact=source_job(&assistant,Some(&protected));
        assert_eq!(compact.get("result").is_none(),status=="completed");
        assert_eq!(compact["prepareOutcome"],assistant["prepareOutcome"]);
        assert_eq!(compact["preparationStages"],assistant["preparationStages"]);
        for result in [Value::Null,json!([]),json!("{\"looks\":\"json\"}")] {
            assistant["result"]=result.clone();assert_eq!(source_job(&assistant,Some(&protected))["result"],result);
        }
    }
}

#[test]
fn source_completed_media_policy_and_full_merge_match_unprojected_history() {
    for profile in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia] {
        let base=media_payload_fixture(profile,4096);
        for mutation in ["none","tie","newer","foreign_account","binding","source","held","unknown"] {
            let mut d=base.clone();let mut later=d["jobs"][0].clone();later["id"]=json!("later");
            later["result"]["visualProgress"]["sourceIdentity"]["durationMs"]=json!(1000);
            match mutation {
                "none"=>{},"tie"=>d["jobs"].as_array_mut().unwrap().push(later),
                "newer"=>{later["createdAt"]=json!("2026-09-28T00:00:00Z");d["jobs"].as_array_mut().unwrap().push(later);},
                "foreign_account"=>d["jobs"][0]["account"]=json!("foreign"),
                "binding"=>d["jobs"][0]["connectorBinding"]=json!({"foreign":true}),
                "source"=>d["jobs"][0]["result"]["visualProgress"]["sourceVersion"]=json!("changed"),
                "held"=>{d["jobs"][0]["status"]=json!("failed");d["jobs"][0]["result"]["visualProgress"]["phase"]=json!("held");},
                _=>d["jobs"][0]["status"]=json!("unknown"),
            }
            let view=project(&d).unwrap();let post=&d["posts"][0];
            assert_eq!(crate::post_media_policy::probed_duration(&view,post),crate::post_media_policy::probed_duration(&d,post),"{mutation}");
            assert_eq!(crate::post_media_policy::effective(&view,post).unwrap(),crate::post_media_policy::effective(&d,post).unwrap(),"{mutation}");
            if mutation=="none" {assert_eq!(crate::post_media_policy::probed_duration(&d,post).unwrap()["durationMs"],181000);}
            let verdict=|w:&Value|crate::proposal_current(w,&w["proposals"][0]).map_err(|e|(e.0.as_u16(),e.1));
            assert_eq!(verdict(&view),verdict(&d));
            let mut incoming=snapshot();incoming["posts"]=d["posts"].clone();
            let mut expected=d.clone();crate::merge_snapshot(&mut expected,&incoming).unwrap();
            let mut after=view.clone();crate::merge_snapshot(&mut after,&incoming).unwrap();
            let mut actual=d.clone();apply(&mut actual,&view,&after).unwrap();
            normalize_observation_clock(&mut expected);normalize_observation_clock(&mut actual);
            assert_eq!(actual,expected,"{mutation}");assert_eq!(actual["jobs"],d["jobs"]);
            assert_eq!(actual["operations"][0]["status"],"unknown");
        }
    }
}

#[test]
fn source_completed_payload_bytes_are_bounded_without_rewriting_history() {
    let mut d=media_payload_fixture(crate::accounts::Profile::LikeAvto,0);
    let mut assistant=terminal_input_job("assistant",0);assistant["result"]=json!({"synthetic":""});
    d["jobs"].as_array_mut().unwrap().push(assistant);
    let small=project(&d).unwrap();
    d["jobs"][0]["result"]["synthetic"]=json!("x".repeat(256*1024));
    d["jobs"][0]["result"]["visualProgress"]["frames"]=json!([{"synthetic":"x".repeat(256*1024)}]);
    d["jobs"][1]["result"]["synthetic"]=json!("x".repeat(256*1024));
    let large=project(&d).unwrap();assert_eq!(small["jobs"],large["jobs"]);
    let bytes=d["jobs"].to_string().len();let projected=large["jobs"].to_string().len();assert!(bytes>projected+700*1024);
    let mut durable=d.clone();apply(&mut durable,&large,&large).unwrap();assert_eq!(durable,d);
    for status in ["approved","dispatching","unknown"] {
        let mut protected=d.clone();protected["proposals"][0]["status"]=json!(status);
        protected["proposals"][0]["history"]=json!([{"origin":{"recovery":{"prepareRunId":"candidate"}}}]);
        assert_eq!(project(&protected).unwrap()["jobs"][0],protected["jobs"][0]);
    }
    let mut illegal=large.clone();illegal["jobs"][0]["status"]=json!("cancelled");assert!(apply(&mut d.clone(),&large,&illegal).is_err());
    eprintln!("source_completed_payload_fixture {}",json!({"fullJobBytes":bytes,"projectedJobBytes":projected,"scope":"synthetic only; no production speedup estimate"}));
}

#[test]
fn source_completed_media_preserves_successful_bundle_fingerprint() {
    for profile in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia] {
        let mut d=media_payload_fixture(profile,8192);
        d["proposals"]=json!([]);d["approvals"]=json!([]);d["operations"]=json!([]);
        d["items"][0]["connectorBinding"]=profile.binding();d["items"][0]["platform"]=json!("vk");
        d["items"][0]["conversationKey"]=json!("conversation");d["items"][0]["workflow"]=json!("attention");
        d["items"][0]["contextEvidenceDigest"]=json!("a".repeat(64));
        d["items"][0]["branchContextDigest"]=json!("b".repeat(64));
        d["branches"][0]["messages"]=json!([{"id":"provider-item","role":"customer","text":"Спасибо за информацию"}]);
        d["branches"][0]["contextComplete"]=json!(true);
        let source=crate::media_fullframes::source_version(&d["posts"][0],profile.display());
        d["materials"]=json!([{"id":"transcript","account":profile.display(),"postKey":"source-key","kind":"transcript",
            "text":"Complete spoken source","transcription":{"partial":false,"coverage":"full_audio","sourceVersion":source,
                "mediaDurationSeconds":181.0,"audioDurationSeconds":181.0}}]);
        crate::knowledge::sync_catalog(&mut d,"2026-09-27T00:00:00Z").unwrap();
        let proposal=crate::create_proposal(&mut d,&json!({"itemId":"item","kind":"close","expectedRevision":3})).unwrap();
        let bundle=crate::prepare_bundle::build(&d,&[json!("item")],&[]).unwrap();
        d["jobs"].as_array_mut().unwrap().push(json!({"id":"origin","kind":"assistant","status":"completed","purpose":"auto_prepare","prepareBundle":bundle}));
        let p=crate::row_mut(&mut d,"proposals",proposal["id"].as_str().unwrap()).unwrap();
        p["prepareRunId"]=json!("origin");p["prepareBundleId"]=bundle["id"].clone();p["prepareBundleDigest"]=bundle["digest"].clone();
        // New ordinary video drafts need an exact dependency receipt. Use the
        // real reducer on this synthetic readable target/full transcript,
        // rather than treating media extraction alone as editorial authority.
        assert!(crate::proposal_current(&d,&d["proposals"][0]).is_err());
        let actor=crate::operator_auth::Actor::local_owner("source-media-offline-review");
        let refs=json!([{"id":proposal["id"],"revision":proposal["revision"]}]);
        let preview=crate::operator_editorial::capture(&d,&actor,&json!({"proposals":refs})).unwrap();
        let review=json!({"requestId":"source-media-fixture-review","proposals":refs,"previewDigest":preview["previewDigest"],
            "operatorReview":{"version":1,"method":crate::operator_editorial::METHOD,"entries":[{
                "candidate":preview["entries"][0]["candidate"],"mediaDependency":{"audio":"required","visual":"independent"},
                "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"},
                "reason":"Synthetic close reviewed against the addressed readable comment and complete supplied transcript; no visual claim"
            }]}});
        crate::operator_editorial::admit(&mut d,&actor,&review).unwrap();
        let projected=project(&d).unwrap();
        let verdict=|w:&Value|crate::proposal_current(w,&w["proposals"][0]).map_err(|e|(e.0.as_u16(),e.1));
        assert!(verdict(&d).is_ok(),"fixture must reach successful source/provenance/media currentness");
        assert_eq!(verdict(&projected),verdict(&d));
        assert_eq!(projected["jobs"][1],d["jobs"][1],"referenced origin bundle remains whole");
        assert_eq!(crate::prepare_bundle::EvidenceContext::new(&projected).fingerprint("item"),crate::prepare_bundle::EvidenceContext::new(&d).fingerprint("item"));
    }
}

#[test]
fn source_jobs_keep_active_media_unknown_malformed_and_unavailable_reference_sets_full() {
    let protected=HashSet::new();
    for (index,job) in job_projection_cases().iter().enumerate() {
        assert_eq!(compactable_job(job,Some(&protected)),index==0);
        assert_eq!(source_job(job,None),*job,"ambiguous references preserve every byte");
        if index>0 {assert_eq!(source_job(job,Some(&protected)),*job);}
    }
}

#[test]
fn source_jobs_referenced_automatic_draft_reconciliation_matches_full_merge() {
    let mut before=fixture(0);
    before["jobs"]=json!([terminal_input_job("referenced",4096),terminal_input_job("irrelevant",4096)]);
    before["proposals"][0]["status"]=json!("draft");
    before["proposals"][0]["prepareRunId"]=json!("referenced");
    before["items"][0]["autoPreparation"]=json!({"status":"prepared","jobId":"referenced"});
    let scoped=project(&before).unwrap();
    assert_eq!(scoped["jobs"][0],before["jobs"][0]);
    assert!(scoped["jobs"][1]["prepareBundle"].get("request").is_none());
    let mut full=before.clone();crate::merge_snapshot(&mut full,&snapshot()).unwrap();
    let mut after=scoped.clone();crate::merge_snapshot(&mut after,&snapshot()).unwrap();
    let mut actual=before.clone();apply(&mut actual,&scoped,&after).unwrap();
    normalize_observation_clock(&mut full);normalize_observation_clock(&mut actual);
    assert_eq!(actual,full);
    assert_eq!(actual["jobs"],before["jobs"]);
    assert_eq!(actual["operations"][0]["status"],"unknown");
}

#[test]
fn source_jobs_real_bundle_currentness_and_source_change_reason_match_full_context() {
    let mut d=fixture(0);
    d["items"][0]["platform"]=json!("instagram");
    d["items"][0]["postKey"]=json!("post-key");
    d["items"][0]["conversationKey"]=json!("thread");
    d["proposals"]=json!([]);d["approvals"]=json!([]);d["operations"]=json!([]);
    let expected_revision=d["items"][0]["revision"].clone();
    let proposal=crate::create_generated_proposal(&mut d,&json!({"itemId":"item","kind":"close","text":"","expectedRevision":expected_revision})).unwrap();
    let bundle=crate::prepare_bundle::build(&d,&[json!("item")],&[]).unwrap();
    let mut origin=terminal_input_job("real-origin",0);origin["prepareBundle"]=bundle.clone();
    d["jobs"]=json!([origin,terminal_input_job("unrelated",4096)]);
    let p=crate::row_mut(&mut d,"proposals",proposal["id"].as_str().unwrap()).unwrap();
    p["prepareRunId"]=json!("real-origin");p["prepareBundleId"]=bundle["id"].clone();p["prepareBundleDigest"]=bundle["digest"].clone();
    let verdict=|workspace:&Value|crate::proposal_current(workspace,&workspace["proposals"][0]).map_err(|e|(e.0.as_u16(),e.1));
    let view=project(&d).unwrap();
    assert!(verdict(&d).is_ok(),"fixture must reach successful real provenance/source/route validation");
    assert_eq!(verdict(&view),verdict(&d));
    assert_eq!(view["jobs"][0]["prepareBundle"]["request"],bundle["request"]);
    assert!(view["jobs"][1]["prepareBundle"].get("request").is_none());
    d["branches"][0]["messages"]=json!([{"id":"target","text":"Changed source","role":"customer"}]);
    let changed=project(&d).unwrap();
    assert!(verdict(&d).is_err());assert_eq!(verdict(&changed),verdict(&d));
    let reason=crate::prepare_bundle::source_change_reason(&d,&bundle,"item");
    assert!(reason.is_some(),"real saved request must explain changed branch evidence");
    assert_eq!(crate::prepare_bundle::source_change_reason(&changed,&changed["jobs"][0]["prepareBundle"],"item"),reason);
}

#[test]
fn source_jobs_legacy_assessment_recovery_and_durable_history_match_full_workspace() {
    let mut before=fixture(0);
    before["proposals"]=json!([]);before["approvals"]=json!([]);before["operations"]=json!([]);
    before["items"][0]["workflow"]=json!("attention");before["items"][0]["draft"]=json!("");
    before["items"][0]["autoPreparation"]=json!({"status":"queued"});
    let mut newer=terminal_input_job("newer-failed",4096);newer["status"]=json!("failed");
    before["jobs"]=json!([terminal_input_job("accepted",4096),newer]);
    let scoped=project(&before).unwrap();
    assert!(scoped["jobs"][0]["prepareBundle"].get("request").is_none());
    let mut full=before.clone();crate::auto_prepare::reconcile_stale(&mut full,1_790_000_000);
    let mut after=scoped.clone();crate::auto_prepare::reconcile_stale(&mut after,1_790_000_000);
    let mut actual=before.clone();apply(&mut actual,&scoped,&after).unwrap();
    assert_eq!(actual,full);
    assert_eq!(actual["items"][0]["autoPreparation"]["jobId"],"accepted");
    assert_eq!(actual["jobs"],before["jobs"],"full request/checkpoints survive temporary projection");
    assert_eq!(actual["jobs"][0]["preparationStages"]["reviewChunks"]["providerRetryAllowed"],false);
    // The first reconciliation installs a formerly missing job pointer. A second
    // merge in the SAME projected transaction must still match full history.
    crate::merge_snapshot(&mut full,&snapshot()).unwrap();crate::merge_snapshot(&mut after,&snapshot()).unwrap();
    let mut twice=before.clone();apply(&mut twice,&scoped,&after).unwrap();
    normalize_observation_clock(&mut full);normalize_observation_clock(&mut twice);assert_eq!(twice,full);
    let mut illegal=scoped.clone();illegal["jobs"][0]["status"]=json!("cancelled");
    assert!(apply(&mut before.clone(),&scoped,&illegal).is_err());
}

fn normalize_observation_clock(d:&mut Value){for branch in d["branches"].as_array_mut().unwrap(){branch["observedAt"]=json!("comparison-clock");}}

#[test]
fn source_snapshot_retains_semantic_corpus_and_matches_full_merge() {
    let before=fixture(4096);let scoped=project(&before).unwrap();
    for table in ["posts","branches","items","proposals","operations","materials","jobs","knowledge_entries","knowledge_versions"] {assert_eq!(scoped[table],before[table],"{table}");}
    for table in OMITTED {assert_eq!(scoped[*table],json!([]));}
    assert!(scoped["approvals"][0].get("context").is_none());
    let mut full=before.clone();crate::merge_snapshot(&mut full,&snapshot()).unwrap();
    let mut after=scoped.clone();crate::merge_snapshot(&mut after,&snapshot()).unwrap();
    let mut result=before.clone();apply(&mut result,&scoped,&after).unwrap();
    normalize_observation_clock(&mut full);normalize_observation_clock(&mut result);
    assert_eq!(full,result);
    assert_eq!(result["operations"][0]["status"],"unknown");
    assert_eq!(result["items"][0]["draft"],"Preserve operator text");
}

#[test]
fn source_snapshot_scope_rejects_history_authority_and_unrelated_metadata_changes(){
    let before=project(&fixture(16)).unwrap();
    let mutations:[fn(&mut Value);7]=[
        |d:&mut Value|d["operations"][0]["status"]=json!("succeeded"),
        |d:&mut Value|d["approvals"][0]["status"]=json!("consumed"),
        |d:&mut Value|d["jobs"][0]["status"]=json!("failed"),
        |d:&mut Value|d["conversations"]=json!([{"id":"injected","itemIds":[]}]),
        |d:&mut Value|d["settings"]["provider"]=json!("foreign"),
        |d:&mut Value|d["account"]=json!("BAW Russia"),
        |d:&mut Value|d["items"]=json!([]),
    ];
    for mutate in mutations {let mut after=before.clone();mutate(&mut after);assert!(validate_change(&before,&after).is_err());}
}

#[test]
fn source_snapshot_knowledge_index_keeps_foreign_head_and_duplicate_rejection(){
    let mut d=fixture(0);
    d["knowledge_entries"]=Value::Array((0..2000).map(|n|json!({"id":format!("e{n}"),"currentVersionId":format!("v{n}")})).collect());
    d["knowledge_versions"]=Value::Array((0..2000).rev().map(|n|json!({"id":format!("v{n}"),"entryId":format!("e{n}")})).collect());
    validate(&d).unwrap();
    d["knowledge_entries"][0]["currentVersionId"]=json!("v1");assert!(validate(&d).is_err());
    d["knowledge_entries"][0]["currentVersionId"]=json!("v0");
    let duplicate=d["knowledge_versions"][0].clone();d["knowledge_versions"].as_array_mut().unwrap().push(duplicate);assert!(validate(&d).is_err());
}

#[test]
fn source_snapshot_appends_audit_without_replacing_retained_history(){
    let mut full=fixture(512);let before=project(&full).unwrap();let mut after=before.clone();
    after["audit"]=json!([{"id":"new-audit","action":"post_media_policy.source_changed","refId":"post"}]);
    after["sync"]["marker"]=json!("new");
    let saved=full["audit"][0].clone();apply(&mut full,&before,&after).unwrap();
    assert_eq!(full["audit"][0],saved);assert_eq!(full["audit"].as_array().unwrap().len(),2);
    let before=project(&full).unwrap();let mut after=before.clone();after["audit"]=json!([saved]);
    assert!(apply(&mut full,&before,&after).is_err());
}

#[tokio::test]
async fn source_snapshot_sqlite_rolls_back_batch_and_preserves_noop(){
    let folder=tempfile::tempdir().unwrap();let pool=crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap();let db=Database::Sqlite(pool);
    let mut source=fixture(256);
    source["jobs"].as_array_mut().unwrap().push(terminal_input_job("sqlite-eligible",4096));
    source["jobs"].as_array_mut().unwrap().push(media_payload_fixture(crate::accounts::Profile::LikeAvto,4096)["jobs"][0].clone());
    db.change(|d|{*d=source.clone();Ok(())}).await.unwrap();
    assert!(!db.change_source_snapshot_observed(|_|Ok(())).await.unwrap().1);
    let before=db.read().await.unwrap();
    let mut foreign=snapshot();foreign["items"][0]["objectId"]=json!("another-provider-object");
    let failed:ApiResult<((),bool)>=db.change_source_snapshot_observed(|d|{crate::merge_snapshot(d,&snapshot())?;crate::merge_snapshot(d,&foreign)}).await;
    assert!(failed.is_err());assert_eq!(db.read().await.unwrap(),before);
    assert!(db.change_source_snapshot_observed(|d|crate::merge_snapshot(d,&snapshot())).await.unwrap().1);
    let after=db.read().await.unwrap();
    for table in ["conversations","approvals","audit","feedback","operations","jobs"]{assert_eq!(after[table],before[table],"{table}");}
    assert_eq!(after["items"][0]["providerStatus"],"closed");
}

#[tokio::test]
async fn paged_sync_uses_scoped_writer_without_rewinding_or_losing_history(){
    let folder=tempfile::tempdir().unwrap();
    let pool=crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap();
    let db=Database::Sqlite(pool);
    let mut initial=fixture(128);
    initial["sync"]["open"]=json!({"paginationStarted":true,"cursor":"retained-cursor","hasMore":false});
    db.change(|d|{*d=initial.clone();Ok(())}).await.unwrap();
    let mut page=snapshot();page["cursor"]=json!("first-page-cursor");page["hasMore"]=json!(true);
    db.change_source_snapshot_observed(|d|crate::merge_sync_page(d,&page,"open",false)).await.unwrap();
    let after=db.read().await.unwrap();
    assert_eq!(after["sync"]["open"]["cursor"],"retained-cursor");
    assert_eq!(after["sync"]["open"]["hasMore"],false);
    assert_eq!(after["sync"]["open"]["firstPage"]["cursor"],"first-page-cursor");
    for table in ["conversations","approvals","audit","feedback","operations","jobs"] {
        assert_eq!(after[table],initial[table],"paged sync must retain {table}");
    }
    db.close().await;
}

#[test]
fn source_change_detection_preserves_metadata_presence_and_mutation_surfaces(){
    let mut before=project(&fixture(0)).unwrap();
    before["futureMetadata"]=json!({"nested":[null,{"retained":"unchanged"}]});
    assert!(!mutable_source_changed(&before,&before));
    let mut after=before.clone();
    after.as_object_mut().unwrap().remove("futureMetadata");
    assert!(mutable_source_changed(&before,&after),"missing root field differs");
    before["futureMetadata"]=Value::Null;
    assert!(mutable_source_changed(&before,&after),"explicit null is not missing");
    let mut reordered=Value::Object(before.as_object().unwrap().iter().rev()
        .map(|(key,value)|(key.clone(),value.clone())).collect());
    assert!(!mutable_source_changed(&before,&reordered),"object order is not a change");
    reordered["sync"]["cursor"]=json!("new-cursor");
    validate_change(&before,&reordered).unwrap();
    assert!(mutable_source_changed(&before,&reordered));
    for mutation in [0,1,2] {
        let mut after=before.clone();
        match mutation {
            0=>{after.as_object_mut().unwrap().remove("sync");},
            1=>{after["sync"]=Value::Null;},
            _=>{after["settings"]["postMediaPolicies"]=json!({"post":{"status":"superseded"}});},
        }
        validate_change(&before,&after).unwrap();
        assert_eq!(mutable_source_changed(&before,&after),before!=after,
            "validated metadata missing/null/policy changes retain equality semantics");
    }
    for table in MUTABLE.iter().copied().chain(std::iter::once("audit")) {
        let mut after=before.clone();
        if table=="audit" {after[table]=json!([{"id":"new","action":"source.observed","refId":"item"}]);}
        else {after[table][0]["futureSourceField"]=json!({"observed":true});}
        validate_change(&before,&after).unwrap();
        assert!(mutable_source_changed(&before,&after),"permitted {table} change is visible");
        assert_eq!(mutable_source_changed(&before,&after),before!=after);
    }
}

#[tokio::test]
async fn source_snapshot_noop_detection_keeps_readonly_guards_and_rollback(){
    let folder=tempfile::tempdir().unwrap();
    let db=Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    let mut initial=fixture(0);
    // Referenced payload remains full; this change never removes paid evidence.
    initial["jobs"].as_array_mut().unwrap().push(terminal_input_job("retained-paid",32*1024));
    initial["proposals"][0]["history"]=json!([{"origin":{"prepareRunId":"retained-paid"}}]);
    db.change(|d|{*d=initial.clone();Ok(())}).await.unwrap();
    assert!(!db.change_source_snapshot_observed(|_|Ok(())).await.unwrap().1);
    let saved=db.read().await.unwrap();
    for table in ["jobs","operations","approvals"] {
        let failed:ApiResult<((),bool)>=db.change_source_snapshot_observed(|d|{
            d[table][0]["futureControl"]=json!("illegal");Ok(())
        }).await;
        assert!(failed.is_err(),"readonly {table} guard precedes narrowed equality");
        assert_eq!(db.read().await.unwrap(),saved,"illegal mutation rolls back {table}");
    }
    let (_,changed)=db.change_source_snapshot_observed(|d|{
        d["sync"]["newCursor"]=json!("next");
        d["audit"]=json!([{"id":"source-observed","action":"source.observed","refId":"item"}]);
        Ok(())
    }).await.unwrap();
    assert!(changed);
    let after=db.read().await.unwrap();
    assert_eq!(after["sync"]["newCursor"],"next");
    assert_eq!(after["audit"].as_array().unwrap().len(),saved["audit"].as_array().unwrap().len()+1);
    for table in ["jobs","operations","approvals","feedback","conversations"] {
        assert_eq!(after[table],saved[table],"metadata/audit commit preserves {table}");
    }
    assert!(!db.change_source_snapshot_observed(|_|Ok(())).await.unwrap().1);
    db.close().await;
}

#[test]
fn source_snapshot_bounded_fixture_reports_history_cost_and_batch_clone_count(){
    // Synthetic histories, no private runtime data. Structural byte reduction is
    // deterministic; elapsed values are observations, never timing assertions.
    let full=fixture(512*1024);let view=project(&full).unwrap();
    let full_bytes=full.to_string().len();let projected_bytes=view.to_string().len();
    assert!(projected_bytes*100<full_bytes);
    let old=std::time::Instant::now();for _ in 0..4{std::hint::black_box(full.clone());}let full_clones_us=old.elapsed().as_micros();
    let new=std::time::Instant::now();for _ in 0..2{std::hint::black_box(view.clone());}let projected_clone_us=new.elapsed().as_micros();
    eprintln!("source_snapshot_fixture {}",json!({"fullBytes":full_bytes,"projectedBytes":projected_bytes,"legacyContextBatchClones":4,"coalescedContextBatchClones":2,"fullFourClonesUs":full_clones_us,"projectedTwoClonesUs":projected_clone_us,"note":"synthetic two-ready-result waves; excludes PG I/O and semantic merge; no guarantee reads complete together and not live speedup"}));
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_source_snapshot_parity_and_late_sql_rollback(){
    // Reuse the existing guard: loopback only, exact explicit test DB name,
    // pristine schema, synthetic seed. Never a company database or live clone.
    let db=crate::storage::preparation::writer_v51_fixture_db().await;
    let mut initial=fixture(512*1024);
    initial["approvals"][0]["proposals"][0]["proposal"]=json!({"text":"x".repeat(512*1024)});
    initial["approvals"][0]["proposals"][0]["item"]=json!({"draft":"y".repeat(512*1024)});
    initial["jobs"].as_array_mut().unwrap().push(terminal_input_job("unreferenced-terminal",512*1024));
    db.change(|d|{*d=initial;Ok(())}).await.unwrap();
    let baseline=db.read().await.unwrap();
    // Exercise the real table-backed PG dispatch query as well as the isolated
    // SQL-expression fixture. This authority fixture intentionally lacks a
    // current preparation proof, so both domain verdicts must reject identically.
    let dispatch=db.read_dispatch_context("proposal").await.unwrap();
    let mut expected_dispatch=baseline.clone();
    for table in ["conversations","approvals","audit","feedback","jobs"]{expected_dispatch[table]=json!([]);}
    expected_dispatch["operations"][0].as_object_mut().unwrap().retain(|key,_|["id","itemId","proposalId","approvalId","status"].contains(&key.as_str()));
    assert_eq!(dispatch,expected_dispatch,"actual PostgreSQL dispatch table projection parity");
    let verdict=|d:&Value|crate::row(d,"proposals","proposal").and_then(|p|crate::proposal_current(d,p)).map_err(|e|(e.0.as_u16(),e.1));
    assert_eq!(verdict(&dispatch),verdict(&baseline),"dispatch domain verdict must retain the same failure boundary");
    // Keep the existing dispatch assertion intact, then seed both new source-only
    // classes before the actual source loader, SQL rollback and durable readback.
    db.change(|d|{d["jobs"].as_array_mut().unwrap().push(media_payload_fixture(crate::accounts::Profile::LikeAvto,256*1024)["jobs"][0].clone());
        d["jobs"][1]["result"]=json!({"synthetic":"x".repeat(256*1024)});Ok(())}).await.unwrap();
    let baseline=db.read().await.unwrap();
    let mut incoming=snapshot();
    incoming["posts"].as_array_mut().unwrap().push(json!({"id":"new-post","text":"New source"}));
    incoming["branches"].as_array_mut().unwrap().push(json!({"id":"new-branch","postId":"new-post","messages":[]}));
    incoming["items"].as_array_mut().unwrap().push(json!({"id":"new-item","itemId":"new-provider-item","objectId":"11391","postId":"new-post","branchId":"new-branch","providerStatus":"new","contextObservedAt":"2026-09-27T01:00:00Z"}));
    let admit=|d:&mut Value|->ApiResult<()>{
        crate::merge_snapshot(d,&incoming)?;d["sync"]["snapshotFixture"]=json!("admitted");
        d["audit"].as_array_mut().unwrap().push(json!({"id":"new-source-audit","action":"source.fixture","refId":"new-item"}));Ok(())
    };
    let Database::Postgres{writer,..}=&db else{unreachable!()};
    for value in approval_projection_cases(){
        let sql=format!("SELECT payload::text AS original,({})::text AS projected FROM (SELECT $1::jsonb AS payload) input",approval_payload_sql());
        let record=sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(value.to_string()).fetch_one(writer).await.unwrap();
        let stored=parse(record.try_get::<&str,_>("original").unwrap()).unwrap();
        let projected=parse(record.try_get::<&str,_>("projected").unwrap()).unwrap();
        assert_eq!(projected,approval(&stored),"actual approval SQL/Rust parity, including malformed fallbacks");
    }
    // The same SQL expression used by the table loader must exactly match the
    // Rust projection for malformed types, states and ambiguous reference sets.
    let mut cases=job_projection_cases();cases.extend(media_projection_cases());
    for bytes in [0,u64::MAX] {
        let mut edge=media_payload_fixture(crate::accounts::Profile::LikeAvto,0)["jobs"][0].clone();
        edge["result"]["visualProgress"]["source"]["bytes"]=json!(bytes);cases.push(edge);
    }
    for job in cases {
        for (ids,valid) in [(vec![],true),(vec!["candidate".to_owned()],true),(vec![],false)] {
            // JSONB may normalize scientific notation into an integer. Compare
            // against the actual FULL stored payload that the normal loader sees,
            // not an earlier serde float's lexical representation before storage.
            let sql=format!("SELECT payload::text AS original,({})::text AS projected FROM (SELECT $1::jsonb AS payload) input",job_payload_sql());
            let record=sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(job.to_string()).bind(ids.clone()).bind(valid).fetch_one(writer).await.unwrap();
            let stored=parse(record.try_get::<&str,_>("original").unwrap()).unwrap();
            let actual=parse(record.try_get::<&str,_>("projected").unwrap()).unwrap();
            let protected:HashSet<_>=ids.into_iter().collect();
            assert_eq!(actual,source_job(&stored,valid.then_some(&protected)),"SQL/Rust stored job projection parity");
        }
    }
    let media_small=media_payload_fixture(crate::accounts::Profile::LikeAvto,0)["jobs"][0].clone();
    let media_large=media_payload_fixture(crate::accounts::Profile::LikeAvto,256*1024)["jobs"][0].clone();
    let assistant_small=terminal_input_job("candidate",0);
    let mut assistant_large=assistant_small.clone();assistant_large["result"]=json!({"synthetic":"x".repeat(256*1024)});
    for (small,large) in [(media_small,media_large),(assistant_small,assistant_large)] {
        let sql=format!("SELECT ({})::text FROM (SELECT $1::jsonb AS payload) input",job_payload_sql());
        let mut returned=Vec::new();
        for job in [&small,&large] {
            let wire:String=sqlx::query_scalar(sqlx::AssertSqlSafe(sql.as_str())).bind(job.to_string())
                .bind(Vec::<String>::new()).bind(true).fetch_one(writer).await.unwrap();
            assert_eq!(parse(&wire).unwrap(),source_job(job,Some(&HashSet::new())));returned.push(wire);
        }
        assert_eq!(returned[0],returned[1],"larger omitted fields must not increase actual PostgreSQL text transfer");
        assert!(large.to_string().len()>returned[1].len()+200*1024);
        eprintln!("source_completed_payload_pg_bytes {}",json!({"fullJobBytes":large.to_string().len(),"returnedTextBytes":returned[1].len(),"scope":"synthetic fixture only"}));
    }
    sqlx::query("CREATE FUNCTION pg_temp.source_snapshot_reject() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic final source write rejection''; END;'").execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER source_snapshot_reject BEFORE UPDATE ON communityhero.workspaces FOR EACH ROW EXECUTE FUNCTION pg_temp.source_snapshot_reject()").execute(writer).await.unwrap();
    let rejected=db.change_source_snapshot_observed(admit).await;
    sqlx::query("DROP TRIGGER source_snapshot_reject ON communityhero.workspaces").execute(writer).await.unwrap();
    assert!(rejected.is_err());assert_eq!(db.read().await.unwrap(),baseline,"all source inserts/updates/audit must roll back after a late metadata failure");
    let mut expected=baseline.clone();admit(&mut expected).unwrap();
    let started=std::time::Instant::now();
    let (_,changed)=db.change_source_snapshot_observed(|d|{assert_eq!(*d,project(&baseline).unwrap(),"SQL projection parity");admit(d)}).await.unwrap();
    let elapsed=started.elapsed().as_secs_f64()*1000.0;assert!(changed);
    let mut actual=db.read().await.unwrap();normalize_observation_clock(&mut actual);normalize_observation_clock(&mut expected);assert_eq!(actual,expected,"full persisted semantic parity");
    assert!(!db.change_source_snapshot_observed(|_|Ok(())).await.unwrap().1);
    assert_eq!(db.read().await.unwrap()["jobs"],baseline["jobs"],"actual no-op/success/late rollback preserve full terminal payload history");
    eprintln!("WRITER_V63_PG sourceSnapshot=true lateSqlRollback=true fullDomainParity=true actualDispatchReadParity=true terminalPayloadParity=true transferredByteFixture=true scopedMs={elapsed:.2}");
    db.close().await;
}

// Compatibility oracle for the replaced metadata construction. It intentionally
// keeps the previous copies in tests, never on the admitted writer path.
fn previous_source_metadata_allowed(before:&Value,after:&Value)->bool {
    let mut expected=metadata(before);
    for field in ["sync","connectorBinding"] {
        match after.get(field) {
            Some(value)=>{expected[field]=value.clone();},
            None=>{expected.as_object_mut().unwrap().remove(field);},
        }
    }
    for field in ["postMediaPolicies","mediaAudioEquivalences"] {
        match after["settings"].get(field) {
            Some(value)=>{expected["settings"][field]=value.clone();},
            None=>{if let Some(settings)=expected["settings"].as_object_mut(){settings.remove(field);}},
        }
    }
    metadata(after)==expected
}

fn patch_metadata(old:&Value,remove:&[String],set:&Value)->Value {
    let mut result=old.clone();
    for key in remove {result.as_object_mut().unwrap().remove(key);}
    for (key,value) in set.as_object().unwrap() {result[key]=value.clone();}
    result
}

#[test]
fn source_metadata_borrowed_guard_preserves_legacy_presence_and_policy_whitelist(){
    let mut before=project(&fixture(0)).unwrap();
    before["futureMetadata"]=json!({"nullWitness":null,"retainedBody":"historical"});
    let mut cases=vec![before.clone()];
    for mutation in 0..12 {
        let mut after=before.clone();
        match mutation {
            0=>{after.as_object_mut().unwrap().remove("sync");},
            1=>{after["sync"]=Value::Null;},
            2=>{after["sync"]["cursor"]=json!("next");},
            3=>{after["settings"]["postMediaPolicies"]=Value::Null;},
            4=>{after["settings"]["mediaAudioEquivalences"]=json!({"source":{"status":"superseded"}});},
            5=>{after["futureMetadata"]["nullWitness"]=json!("forged");},
            6=>{after.as_object_mut().unwrap().remove("futureMetadata");},
            7=>{after["futureMetadata"]=Value::Null;},
            8=>{after["unexpectedRoot"]=Value::Null;},
            9=>{after["settings"]["provider"]=json!("foreign");},
            10=>{after.as_object_mut().unwrap().remove("settings");},
            _=>{after["settings"]=Value::Null;},
        }
        cases.push(after);
    }
    for after in cases {
        assert_eq!(validate_source_metadata(&before,&after).is_ok(),previous_source_metadata_allowed(&before,&after),
            "borrowed guard must preserve old whitelist including absent/null");
        if validate_source_metadata(&before,&after).is_ok() {
            let (remove,set)=source_metadata_patch(&before,&after);
            assert_eq!(patch_metadata(&metadata(&before),&remove,&set),metadata(&after));
        }
    }
    for prior in [None,Some(Value::Null),Some(json!({}))] {
        let mut base=before.clone();
        match prior {Some(value)=>base["settings"]=value,None=>{base.as_object_mut().unwrap().remove("settings");}}
        for replacement in [None,Some(Value::Null),Some(json!({})),Some(json!({"postMediaPolicies":null})),
            Some(json!({"mediaAudioEquivalences":{"source":"retained"}})),Some(json!({"provider":"foreign"}))] {
            let mut after=base.clone();
            match replacement {Some(value)=>after["settings"]=value,None=>{after.as_object_mut().unwrap().remove("settings");}}
            assert_eq!(validate_source_metadata(&base,&after).is_ok(),previous_source_metadata_allowed(&base,&after));
        }
    }
}

#[test]
fn source_metadata_missing_settings_preserves_previous_index_mut_materialization_boundary(){
    let mut absent=project(&fixture(0)).unwrap();
    absent.as_object_mut().unwrap().remove("settings");
    // Independent witness for the exact legacy construction used by the
    // unchanged oracle: asking for a mutable object inserts missing null.
    let mut expected=metadata(&absent);
    assert!(expected.get("settings").is_none());
    assert!(expected["settings"].as_object_mut().is_none());
    assert_eq!(expected.get("settings"),Some(&Value::Null));
    assert!(!previous_source_metadata_allowed(&absent,&absent));
    assert!(validate_source_metadata(&absent,&absent).is_err());

    let mut explicit_null=absent.clone();explicit_null["settings"]=Value::Null;
    assert!(previous_source_metadata_allowed(&absent,&explicit_null));
    validate_source_metadata(&absent,&explicit_null).unwrap();
    let (remove,set)=source_metadata_patch(&absent,&explicit_null);
    assert!(remove.is_empty());assert_eq!(set,json!({"settings":null}));
    assert_eq!(patch_metadata(&metadata(&absent),&remove,&set),metadata(&explicit_null));
    validate_source_metadata(&explicit_null,&explicit_null).unwrap();
    assert!(validate_source_metadata(&explicit_null,&absent).is_err());
    for settings in [json!({}),json!({"provider":"foreign"})] {
        let mut after=absent.clone();after["settings"]=settings;
        assert!(!previous_source_metadata_allowed(&absent,&after));
        assert!(validate_source_metadata(&absent,&after).is_err());
    }
}

#[tokio::test]
async fn sqlite_source_missing_settings_keeps_legacy_rejection_and_explicit_null_delta(){
    let folder=tempfile::tempdir().unwrap();
    let pool=crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap();
    let db=Database::Sqlite(pool.clone());let mut initial=fixture(0);
    initial.as_object_mut().unwrap().remove("settings");
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(initial.to_string()).execute(&pool).await.unwrap();
    let before=db.read().await.unwrap();assert!(before.get("settings").is_none());
    assert!(db.change_source_snapshot_observed(|d|{d["sync"]["cursor"]=json!("not-admitted");Ok(())}).await.is_err());
    assert_eq!(db.read().await.unwrap(),before,"absence must not be silently converted by rejected admission");
    let (_,changed)=db.change_source_snapshot_observed(|d|{d["settings"]=Value::Null;Ok(())}).await.unwrap();
    assert!(changed);let mut expected=before.clone();expected["settings"]=Value::Null;
    assert_eq!(db.read().await.unwrap(),expected,"only the explicit legacy null delta persists");
    assert!(!db.change_source_snapshot_observed(|_|Ok(())).await.unwrap().1);
    assert!(db.change_source_snapshot_observed(|d|{d.as_object_mut().unwrap().remove("settings");Ok(())}).await.is_err());
    assert_eq!(db.read().await.unwrap(),expected,"reverse presence change must roll back");
    for table in ["jobs","operations","approvals"] {assert_eq!(expected[table],before[table]);}
    db.close().await;
}

#[test]
fn source_metadata_patch_excludes_cold_archive_and_preserves_exact_untouched_values(){
    let mut before=project(&fixture(0)).unwrap();
    before["preparationResearch"]=json!([{"id":"immutable-review","body":"x".repeat(256*1024),"future":{"null":null}}]);
    before["privateFutureArchive"]=json!({"raw":"y".repeat(256*1024)});
    let mut after=before.clone();after["sync"]["cursor"]=json!("next");
    validate_change(&before,&after).unwrap();
    let (remove,set)=source_metadata_patch(&before,&after);
    assert!(remove.is_empty());assert_eq!(set.as_object().unwrap().keys().map(|key|key.as_str()).collect::<Vec<_>>(),vec!["sync"]);
    assert!(set.to_string().len()<4096,"metadata wire delta must not serialize either 256 KiB archive");
    assert_eq!(patch_metadata(&metadata(&before),&remove,&set),metadata(&after));
    assert_eq!(after["preparationResearch"],before["preparationResearch"]);
    for table in ["jobs","operations","approvals"] {
        let mut illegal=after.clone();illegal[table][0]["privateWitness"]=json!("forged");
        assert!(validate_change(&before,&illegal).is_err(),"metadata delta cannot bypass readonly {table}");
    }
    let (remove,set)=source_metadata_patch(&before,&before);
    assert!(remove.is_empty());assert_eq!(set,json!({}),"source-only entity update does not rewrite metadata");
}

#[test]
fn source_metadata_delta_keeps_binding_presence_compatibility_and_company_guard(){
    let mut absent=project(&fixture(0)).unwrap();
    absent.as_object_mut().unwrap().remove("connectorBinding");
    let mut explicit_null=absent.clone();explicit_null["connectorBinding"]=Value::Null;
    // The preexisting active-binding condition uses indexed Value equality;
    // preserve its legacy absent/null compatibility while retaining DB presence.
    validate_change(&absent,&explicit_null).unwrap();
    let (remove,set)=source_metadata_patch(&absent,&explicit_null);
    assert!(remove.is_empty());assert_eq!(set,json!({"connectorBinding":null}));
    validate_change(&explicit_null,&absent).unwrap();
    let (remove,set)=source_metadata_patch(&explicit_null,&absent);
    assert_eq!(remove,vec!["connectorBinding"]);assert_eq!(set,json!({}));
    let mut bound=absent.clone();bound["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding();
    for binding in [Value::Null,crate::accounts::Profile::BawRussia.binding()] {
        let mut after=bound.clone();after["connectorBinding"]=binding;
        assert!(validate_change(&bound,&after).is_err(),"changed explicit binding still validates native company scope");
    }
    let mut foreign=bound.clone();foreign["account"]=json!("BAW Russia");
    assert!(validate_change(&bound,&foreign).is_err(),"source metadata delta never relabels the company");
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_source_metadata_delta_parity_presence_late_membership_and_rollback(){
    let db=crate::storage::preparation::writer_v51_fixture_db().await;
    let mut initial=fixture(128);
    initial["preparationResearch"]=json!([{"id":"review","body":"x".repeat(128*1024),"future":{"null":null}}]);
    initial["futureMetadata"]=json!({"preserved":null});
    initial["settings"]["postMediaPolicies"]=json!({"before":{"status":"active"}});
    db.change(|d|{*d=initial;Ok(())}).await.unwrap();
    let original=db.read().await.unwrap();
    let Database::Postgres{writer,..}=&db else{unreachable!()};
    // Execute the exact new SQL operators, comparing to the old complete
    // metadata replacement result rather than merely our Rust patch helper.
    for mutation in 0..4 {
        let mut after=project(&original).unwrap();
        match mutation {
            0=>{after["sync"]["cursor"]=json!("next");},
            1=>{after.as_object_mut().unwrap().remove("sync");},
            2=>{after["sync"]=Value::Null;},
            _=>{after["settings"]["postMediaPolicies"]=Value::Null;after["settings"]["mediaAudioEquivalences"]=json!({"post":"superseded"});},
        }
        validate_change(&project(&original).unwrap(),&after).unwrap();
        let (remove,set)=source_metadata_patch(&project(&original).unwrap(),&after);
        let wire:String=sqlx::query_scalar("SELECT (($1::jsonb-$2::text[])||$3::jsonb)::text")
            .bind(metadata(&original).to_string()).bind(remove).bind(set.to_string()).fetch_one(writer).await.unwrap();
        assert_eq!(parse(&wire).unwrap(),metadata(&after),"actual SQL field patch equals full legacy metadata");
    }
    // A preliminary read is not authority: a later native writer can add a
    // protected operation and opaque metadata before source intake gets its lock.
    db.change(|d|{
        d["futureMetadata"]["lateWriter"]=json!("keep-exactly");
        let mut late=d["operations"][0].clone();late["id"]=json!("late-unknown");
        late["receipt"]=json!({"immutable":"late external effect; no retry"});
        d["operations"].as_array_mut().unwrap().push(late);Ok(())
    }).await.unwrap();
    let baseline=db.read().await.unwrap();
    let admit=|d:&mut Value|->ApiResult<()> {
        assert_eq!(d["futureMetadata"]["lateWriter"],"keep-exactly");
        assert!(rows(d,"operations")?.iter().any(|op|op["id"]=="late-unknown"&&op["status"]=="unknown"));
        d["posts"][0]["text"]=json!("source changed");d["sync"]["cursor"]=json!("committed-next");
        d["audit"].as_array_mut().unwrap().push(json!({"id":"delta-audit","action":"source.delta","refId":"post"}));Ok(())
    };
    sqlx::query("CREATE FUNCTION pg_temp.source_metadata_delta_reject() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic final metadata delta failure''; END;'").execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER source_metadata_delta_reject BEFORE UPDATE ON communityhero.workspaces FOR EACH ROW EXECUTE FUNCTION pg_temp.source_metadata_delta_reject()").execute(writer).await.unwrap();
    assert!(db.change_source_snapshot_observed(admit).await.is_err());
    sqlx::query("DROP TRIGGER source_metadata_delta_reject ON communityhero.workspaces").execute(writer).await.unwrap();
    assert_eq!(db.read().await.unwrap(),baseline,"late metadata failure rolls back source rows and audit");
    let mut expected=baseline.clone();admit(&mut expected).unwrap();
    assert!(db.change_source_snapshot_observed(admit).await.unwrap().1);
    assert_eq!(db.read().await.unwrap(),expected,"one real writer retains complete current metadata and UNKNOWN history");
    let committed=db.read().await.unwrap();
    for mutation in 0..3 {
        let result:ApiResult<((),bool)>=db.change_source_snapshot_observed(|d|{
            match mutation {
                0=>d["futureMetadata"]=Value::Null,
                1=>d["settings"]["provider"]=json!("foreign"),
                _=>d["operations"][0]["receipt"]=json!("forged"),
            };Ok(())
        }).await;
        assert!(result.is_err());assert_eq!(db.read().await.unwrap(),committed);
    }
    assert!(!db.change_source_snapshot_observed(|_|Ok(())).await.unwrap().1);
    db.close().await;
}
