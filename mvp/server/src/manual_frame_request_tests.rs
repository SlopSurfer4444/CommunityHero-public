use super::*;
use crate::media_frame_sample_decode::{ProcessOutput, ProcessSpec, SampleProcess, decode_sample_with};
use sha2::{Digest, Sha256};
use std::{future::Future, pin::Pin, sync::atomic::{AtomicUsize, Ordering}};
const AT: &str = "2026-10-06T06:55:00Z";

fn fixture() -> (Value, Value, OwnerToken) {
    let mut d=crate::empty(); crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
    let binding=crate::active_binding(&d).unwrap().to_json();
    let token=OwnerToken{account:"BAW Russia".into(),runtime_id:"manual-fixture-runtime".into(),release_sha256:"a".repeat(64),epoch:1};
    d["runtimeLifecycle"]=json!({"schemaVersion":1,"owner":token_value(&token),"phase":"running","history":[],"target":null,"transfer":null,"mediaAnalysisGeneration":1,"queuedBacklog":null});
    d["posts"]=json!([{"id":"manual-post","postKey":"vk:manual-post","account":"BAW Russia","connectorBinding":binding,
        "title":"Original exact source","attachments":[{"type":"video","url":"https://example.invalid/synthetic.mp4"}]}]);
    let body=json!({"requestId":crate::id(),"postId":"manual-post","attachmentIndex":0,
        "expectedSourceVersion":crate::media_fullframes::source_version(&d["posts"][0],"BAW Russia"),
        "requestedTimeOrIntent":{"kind":"known_range","timelineBasis":"relative_video_start","startMs":1000,"endMs":2000},"reason":"Read the requested scene before answering"});
    (d,body,token)
}
fn prepare(d:&mut Value,body:&mut Value,token:&OwnerToken)->Value {
    let pin=crate::media_speech_assets::capture(d,&d["posts"][0],0).unwrap();
    let request=json!({"account":d["account"],"connectorBinding":pin["connectorBinding"],"purpose":"prepare","items":[{"id":"recipient","postId":pin["postId"]}],
        "postContextBundle":{"companyId":d["account"],"members":[{"canonicalPostId":pin["postId"],"connectorBinding":pin["connectorBinding"],"postSourceVersion":pin["sourceVersion"],
            "assets":[{"modality":"video","attachmentIndex":0,"attachmentIdentity":pin["attachmentIdentity"],"speech":{"outcome":"no_audio","coverage":"full_audio"}}]}]},
        "manualFrameRequestIds":[]});
    let digest=hash(&request); let job=json!({"id":"unpaid-prepare","kind":"assistant","purpose":"engine_prepare","status":"running",
        "prepareBundle":{"version":1,"digest":digest,"request":request},"preparationStages":{"first":null,"review":null,"initialAdmission":{"version":1,"status":"scheduled","requestSha256":digest,"owner":token_value(token),"admittedAt":AT}}});
    crate::list_mut(d,"jobs").push(job); body["prepareJobId"]=json!("unpaid-prepare"); body["expectedPrepareBundleDigest"]=json!(digest); request
}
fn source(d:&mut Value,store:&ArtifactStore,bytes:&[u8]) {
    let pin=crate::media_speech_assets::capture(d,&d["posts"][0],0).unwrap();let artifact=store.put_bytes(bytes).unwrap();
    let job=json!({"id":"source-job","kind":"media","status":"completed","purpose":"manual_source_only",
        "result":{"visualProgress":{"schemaVersion":2,"account":d["account"],"sourcePostId":pin["postId"],"sourcePostKey":pin["postKey"],
            "connectorBinding":pin["connectorBinding"],"sourceVersion":pin["sourceVersion"],"assetPin":pin,"source":artifact.to_json(),
            "sourceIdentity":{"account":d["account"],"postKey":pin["postKey"],"mediaSha256":artifact.sha256,"durationMs":6000}}}});
    crate::list_mut(d,"jobs").push(job);
}
fn fake_tools(dir:&std::path::Path)->SampleTools {
    let ffmpeg=dir.join("fixture-ffmpeg");let ffprobe=dir.join("fixture-ffprobe");
    std::fs::write(&ffmpeg,b"fake decode tool").unwrap();std::fs::write(&ffprobe,b"fake probe tool").unwrap();
    SampleTools{ffmpeg,ffprobe,ffmpeg_sha256:format!("{:x}",Sha256::digest(b"fake decode tool")),ffprobe_sha256:format!("{:x}",Sha256::digest(b"fake probe tool")),
        ffmpeg_version:"fixture ffmpeg".into(),ffprobe_version:"fixture ffprobe".into(),deadline:Duration::from_secs(30)}
}
fn fresh(d:&mut Value,body:&Value,token:&OwnerToken)->Value {
    match claim(d,body,"operator-owner",token,AT).unwrap(){Admission::Fresh(job)=>job,other=>panic!("Expected fresh local frame work: {other:?}")}
}
struct Fake(AtomicUsize);
impl SampleProcess for Fake {
    fn execute<'a>(&'a self,spec:&'a ProcessSpec)->Pin<Box<dyn Future<Output=Result<ProcessOutput,String>>+Send+'a>> {
        Box::pin(async move {
            self.0.fetch_add(1,Ordering::SeqCst);let rgb=vec![1,2,3,4,5,6];
            let (stdout,stderr)=match spec.kind {
                "probe"=>(json!({"streams":[{"index":0,"width":2,"height":1,"time_base":"1/1000","start_pts":0,"duration_ts":6000}],"format":{"duration":"6.0"}}).to_string().into_bytes(),vec![]),
                "sample"=>(rgb.clone(),format!("[Parsed_showinfo_1 @ fixture] n: 0 pts: 960 pts_time: 0.960\n[Parsed_showinfo_1 @ fixture] n: 1 pts: 1040 pts_time: 1.040\n#tb 0: 1/1000\n#dimensions 0: 2x1\n0, 1040, 1040, 1, 6, {:x}\n",Sha256::digest(&rgb)).into_bytes()),
                "encode_png"=>{let mut png=b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();png.extend(2u32.to_be_bytes());png.extend(1u32.to_be_bytes());png.extend([8,2,0,0,0,0,0,0,0]);(png,vec![])},
                "verify_png"=>(rgb,vec![]),_=>return Err("unexpected_manual_fixture_process".into()),
            };Ok(ProcessOutput{stdout,stderr})
        })
    }
}

#[test]
fn strict_manual_admission_rejects_paid_parent_urls_ranges_and_forged_scope() {
    let(d,body,token)=fixture();
    for field in ["originatingAnsweringAttemptId","requestingPaidAttemptId","repairBudget","url","sourceArtifactSha256","frameLease","origin","actor"] {
        let mut forged=body.clone();forged[field]=json!("injected");assert!(parse(&forged).is_err(),"{field}");
    }
    for intent in [json!({"kind":"known_range","timelineBasis":"relative_video_start","startMs":0,"endMs":30001}),
        json!({"kind":"known_range","timelineBasis":"relative_video_start","startMs":10,"endMs":10}),
        json!({"kind":"uniform_overview","timelineBasis":"source_pts"}),json!({"kind":"semantic_locator","timelineBasis":"relative_video_start"})] {
        let mut v=body.clone();v["requestedTimeOrIntent"]=intent;assert!(parse(&v).is_err());
    }
    for changed in ["company","binding","source","asset"] {
        let mut next=d.clone();let mut request=body.clone();
        match changed {"company"=>next["account"]=json!("LikeAvto"),"binding"=>next["posts"][0]["connectorBinding"]["accountId"]=json!("foreign"),
            "source"=>next["posts"][0]["title"]=json!("replaced"),"asset"=>request["attachmentIndex"]=json!(1),_=>unreachable!()}
        assert!(claim(&mut next,&request,"operator-owner",&token,AT).is_err(),"{changed}");
    }
}

#[test]
fn missing_source_is_durable_finite_unpaid_and_same_request_never_restarts() {
    let(mut d,body,token)=fixture();
    let job=fresh(&mut d,&body,&token);let child=crate::row(&d,"jobs",txt(&job,"sourceJobId")).unwrap();
    assert_eq!(job["status"],"running");assert_eq!(child["kind"],"manual_frame_source");
    assert_eq!(child["sourceInitialProgress"]["phase"],"download");assert_eq!(child["sourceDownloadIntent"]["repeatAuthorized"],false);
    assert!(child.get("visualContractVersion").is_none());assert!(job.get("extractionIntent").is_none());
    let restored:Value=serde_json::from_slice(&serde_json::to_vec(&d).unwrap()).unwrap();let mut next=restored.clone();
    assert!(matches!(claim(&mut next,&body,"operator-owner",&token,AT).unwrap(),Admission::Replay(_)));assert_eq!(next,restored);
    assert!(claim(&mut next,&body,"another-owner",&token,AT).is_err());
    let mut forged=body.clone();forged["reason"]=json!("changed reason");assert!(claim(&mut next,&forged,"operator-owner",&token,AT).is_err());
    assert!(rows(&d,"jobs").iter().all(|j|j.get("repairBudget").is_none()&&j.get("originatingAnsweringAttemptId").is_none()));
    crate::recover(&mut next).unwrap();assert_eq!(crate::row(&next,"jobs",txt(&job,"id")).unwrap()["status"],"interrupted");
    let stopped=next.clone();assert!(matches!(claim(&mut next,&body,"operator-owner",&token,AT).unwrap(),Admission::Replay(_)));assert_eq!(next,stopped);
    let mut second=body.clone();second["requestId"]=json!(crate::id());assert!(claim(&mut next,&second,"operator-owner",&token,AT).is_err());
}

#[test]
fn explicit_capture_blocks_reserved_completed_unknown_and_changed_digest() {
    let(mut d,mut body,token)=fixture();prepare(&mut d,&mut body,&token);
    for field in ["first_reserved","first_completed","retained_unknown","digest"] {
        let mut next=d.clone();let job=&mut next["jobs"][0];
        match field {"first_reserved"=>job["preparationStages"]["firstAdmission"]=json!({"status":"reserved"}),
            "first_completed"=>job["preparationStages"]["first"]=json!({"status":"completed"}),
            "retained_unknown"=>job["retainedEvidence"]=json!([{"status":"unknown"}]),"digest"=>job["prepareBundle"]["digest"]=json!("b".repeat(64)),_=>unreachable!()}
        assert!(claim(&mut next,&body,"operator-owner",&token,AT).is_err(),"{field}");
    }
    let mut standalone=body.clone();standalone.as_object_mut().unwrap().remove("prepareJobId");standalone.as_object_mut().unwrap().remove("expectedPrepareBundleDigest");
    d["jobs"][0]["preparationStages"]["firstAdmission"]=json!({"status":"unknown"});
    assert!(claim(&mut d,&standalone,"operator-owner",&token,AT).is_err());
}

#[tokio::test]
async fn bounded_native_manual_frames_survive_restart_without_paid_parent_or_decode_replay() {
    let dir=tempfile::tempdir().unwrap();let store=ArtifactStore::open(&dir.path().join("cas")).unwrap();let tools=fake_tools(dir.path());
    let(mut d,mut body,token)=fixture();let mut request=prepare(&mut d,&mut body,&token);source(&mut d,&store,b"\0\0\0\x0cftypisom");
    let job=fresh(&mut d,&body,&token);let plan=make_plan(&store,&job,base_usage(&request)).unwrap();
    assert_eq!(plan["targets"].as_array().unwrap().len(),1);assert_eq!(plan["targets"][0]["requestedTimestampMs"],1000);
    start(&mut d,&job,&token,&plan,&tools,AT).unwrap();
    assert!(require_no_pending(&d,&d["jobs"][0]).is_err());
    assert!(start(&mut d,&job,&token,&plan,&tools,AT).is_err());
    let fake=Fake(AtomicUsize::new(0));let decoded=decode_sample_with(&store,&plan,&tools,&fake).await.unwrap();
    verify_sample_result(&store,&plan,&decoded,&tools).unwrap();retain_observation(&mut d,&job,&decoded).unwrap();
    let complete=settle(&mut d,&job,&token,&plan,&decoded,AT).unwrap();warm_result(&d,&complete,&store).unwrap();
    assert_eq!(require_no_pending(&d,&d["jobs"][0]),Err("manual_frame_unpaid_capture_refresh_required"));attach_request(&d,&mut request).unwrap();
    let mut refined=d["jobs"][0].clone();refined["prepareBundle"]["request"]=request.clone();require_no_pending(&d,&refined).unwrap();
    assert_eq!(request["manualFrameRequestIds"],json!([body["requestId"]]));assert_eq!(request["optionalFrameRefs"][0]["origin"],ORIGIN);
    assert_eq!(request["optionalFrameRefs"][0]["actualPts"],1040);assert_eq!(request["optionalFrameRefs"][0]["requestedTimestampMs"],1000);
    assert_eq!(request["postContextBundle"]["members"][0]["assets"][0]["speech"]["outcome"],"no_audio");
    let mut restored:Value=serde_json::from_slice(&serde_json::to_vec(&d).unwrap()).unwrap();
    assert!(matches!(claim(&mut restored,&body,"operator-owner",&token,AT).unwrap(),Admission::Replay(_)));
    validate_result(&restored,&complete,&ArtifactStore::open(store.root()).unwrap()).unwrap();assert_eq!(fake.0.load(Ordering::SeqCst),4);
    let mut repeat=body.clone();repeat["requestId"]=json!(crate::id());assert!(claim(&mut restored,&repeat,"operator-owner",&token,AT).is_err());
    // A frozen paid capture receives exactly these references in its editor.
    restored["jobs"][0]["preparationStages"]["firstAdmission"]=json!({"status":"reserved"});require_refs(&restored,&request).unwrap();
    let mut editor=request.clone();editor["purpose"]=json!("editorial_review");editor.as_object_mut().unwrap().remove("manualFrameRequestIds");
    attach_request(&restored,&mut editor).unwrap();assert_eq!(editor["optionalFrameRefs"],request["optionalFrameRefs"]);
    let captured=request.clone();attach_request(&restored,&mut request).unwrap();assert_eq!(request,captured);
}

#[tokio::test]
async fn stale_source_fence_forged_pts_and_original_observation_recovery_are_checked() {
    let dir=tempfile::tempdir().unwrap();let store=ArtifactStore::open(&dir.path().join("cas")).unwrap();let tools=fake_tools(dir.path());
    let(mut d,body,token)=fixture();source(&mut d,&store,b"\0\0\0\x0cftypisom");let job=fresh(&mut d,&body,&token);
    let plan=make_plan(&store,&job,json!({"imageCount":0,"imageBytes":0,"pixels":0})).unwrap();start(&mut d,&job,&token,&plan,&tools,AT).unwrap();
    let fake=Fake(AtomicUsize::new(0));let decoder=decode_sample_with(&store,&plan,&tools,&fake).await.unwrap();
    for change in ["title","lease","runtime","paid"] {
        let mut next=d.clone();let frame_index=1;
        match change {"title"=>next["posts"][0]["title"]=json!("drift"),"lease"=>next["jobs"][frame_index]["frameLease"]["epoch"]=json!(2),
            "runtime"=>next["runtimeLifecycle"]["owner"]["epoch"]=json!(2),
            "paid"=>crate::list_mut(&mut next,"jobs").push(json!({"id":"paid","prepareBundle":{"request":{"posts":[d["posts"][0]]}},"preparationStages":{"firstAdmission":{"status":"reserved"}}})),_=>unreachable!()}
        retain_observation(&mut next,&job,&decoder).ok();assert!(settle(&mut next,&job,&token,&plan,&decoder,AT).is_err(),"{change}");
    }
    retain_observation(&mut d,&job,&decoder).unwrap();let unknown=stop(&mut d,&job,"lost_settlement_ack",AT).unwrap();assert_eq!(unknown["status"],"unknown");
    let recovered=recovered_job(&d,&unknown,&store).unwrap();assert_eq!(recovered["frameResult"]["decoderResult"],decoder);assert_eq!(fake.0.load(Ordering::SeqCst),4);
    let mut forged=recovered.clone();forged["frameResult"]["decoderResult"]["frames"][0]["actualPts"]=json!(1999);
    forged["frameResult"]=result_value(&forged,&forged["frameResult"]["decoderResult"]);assert!(validate_result(&d,&forged,&store).is_err());
    let complete=settle(&mut {let mut r=d.clone();r["jobs"][1]["status"]=json!("running");r},&job,&token,&plan,&decoder,AT).unwrap();
    warm_result(&d,&complete,&store).unwrap();assert!(require_warmed(&d,&complete).is_ok());
    let mut new_owner=d.clone();new_owner["runtimeLifecycle"]["owner"]["epoch"]=json!(2);assert_eq!(require_warmed(&new_owner,&complete),Err("manual_frame_proof_not_warmed"));
    warm_result(&new_owner,&complete,&store).unwrap();require_warmed(&new_owner,&complete).unwrap();
    let mut drift=d.clone();drift["posts"][0]["title"]=json!("changed after cache warm");assert!(require_warmed(&drift,&complete).is_err());
    let artifact=ArtifactRef::from_json(&complete["frameResult"]["frames"][0]["pixelArtifact"]).unwrap();
    std::fs::write(store.path(&artifact).unwrap(),[9u8;6]).unwrap();
    assert!(warm_result(&d,&complete,&store).is_err());assert!(require_warmed(&d,&complete).is_err());
}

#[tokio::test]
async fn actual_startup_reconciles_only_the_original_observation_and_parent_capture(){
    let dir=tempfile::tempdir().unwrap();let store=ArtifactStore::open(&dir.path().join("cas")).unwrap();let tools=fake_tools(dir.path());
    let(mut d,mut body,token)=fixture();prepare(&mut d,&mut body,&token);source(&mut d,&store,b"\0\0\0\x0cftypisom");
    let job=fresh(&mut d,&body,&token);let plan=make_plan(&store,&job,json!({"imageCount":0,"imageBytes":0,"pixels":0})).unwrap();
    start(&mut d,&job,&token,&plan,&tools,AT).unwrap();let fake=Fake(AtomicUsize::new(0));let decoded=decode_sample_with(&store,&plan,&tools,&fake).await.unwrap();
    retain_observation(&mut d,&job,&decoded).unwrap();crate::recover(&mut d).unwrap();
    let interrupted=crate::row(&d,"jobs",txt(&job,"id")).unwrap().clone();assert_eq!(interrupted["status"],"interrupted");
    assert_eq!(crate::row(&d,"jobs","unpaid-prepare").unwrap()["status"],"interrupted");
    let recovered=recovered_job(&d,&interrupted,&store).unwrap();assert_eq!(recovered["frameResult"]["decoderResult"],decoded);
    assert!(matches!(claim(&mut d,&body,"operator-owner",&token,AT).unwrap(),Admission::Replay(_)));assert_eq!(fake.0.load(Ordering::SeqCst),4);
    for fault in ["missing","corrupt","parent_owner","source","paid"]{
        let mut next=d.clone();let mut changed=interrupted.clone();
        match fault{
            "missing"=>{changed.as_object_mut().unwrap().remove("frameObservation");},
            "corrupt"=>changed["frameObservation"]["frames"][0]["actualPts"]=json!(1999),
            "parent_owner"=>crate::row_mut(&mut next,"jobs","unpaid-prepare").unwrap()["preparationStages"]["initialAdmission"]["owner"]["epoch"]=json!(9),
            "source"=>next["posts"][0]["title"]=json!("changed"),
            "paid"=>crate::row_mut(&mut next,"jobs","unpaid-prepare").unwrap()["preparationStages"]["firstAdmission"]=json!({"status":"unknown"}),_=>unreachable!(),
        }
        assert!(recovered_job(&next,&changed,&store).is_err(),"{fault}");
    }
    assert_eq!(fake.0.load(Ordering::SeqCst),4);
}

#[test]
fn source_download_checkpoint_reconciles_once_without_reinvocation_or_eager_inventory(){
    let dir=tempfile::tempdir().unwrap();let store=ArtifactStore::open(&dir.path().join("cas")).unwrap();
    let(mut d,body,token)=fixture();let job=fresh(&mut d,&body,&token);let child_id=txt(&job,"sourceJobId").to_owned();
    let reference=store.put_bytes(b"\0\0\0\x0cftypisom").unwrap();
    {let child=crate::row_mut(&mut d,"jobs",&child_id).unwrap();child["sourceDispatchedAt"]=json!(AT);
        let progress=&mut child["result"]["visualProgress"];progress["phase"]=json!("inventory");progress["source"]=reference.to_json();
        progress["sourceIdentity"]=json!({"account":progress["account"],"postKey":progress["sourcePostKey"],"mediaSha256":reference.sha256,"durationMs":6000});}
    crate::recover(&mut d).unwrap();let parent=crate::row(&d,"jobs",txt(&job,"id")).unwrap().clone();
    let(child,observed)=recoverable_source(&d,&parent,&store).unwrap();
    for fault in ["foreign_pin","lease","inventory","download","bytes"]{
        let mut changed=child.clone();match fault{
            "foreign_pin"=>changed["sourceAssetPin"]["attachmentIndex"]=json!(1),
            "lease"=>changed["result"]["visualProgress"]["leaseEpoch"]=json!(99),
            "inventory"=>changed["result"]["visualProgress"]["inventory"]=json!({"invented":true}),
            "download"=>changed["result"]["visualProgress"]["phase"]=json!("download"),
            "bytes"=>changed["result"]["visualProgress"]["source"]["bytes"]=json!(0),_=>unreachable!(),
        }assert!(source_observation(&d,&parent,&changed).is_err(),"{fault}");
    }
    let saved=bind_source_observation(&mut d,&parent,&child,&observed,&token,AT).unwrap();
    assert_eq!(saved["manualFrameRequest"],job["manualFrameRequest"]);assert!(saved.get("extractionIntent").is_none());
    let checkpoint=d.clone();let settled_child=crate::row(&d,"jobs",&child_id).unwrap().clone();
    bind_source_observation(&mut d,&saved,&settled_child,&observed,&token,"later").unwrap();assert_eq!(d,checkpoint);
    assert!(recovered_job(&d,&saved,&store).is_err());assert!(matches!(claim(&mut d,&body,"operator-owner",&token,AT).unwrap(),Admission::Replay(_)));
    let mut repeat=body.clone();repeat["requestId"]=json!(crate::id());assert!(claim(&mut d,&repeat,"operator-owner",&token,AT).is_err());
    std::fs::write(store.path(&reference).unwrap(),b"broken").unwrap();assert!(recoverable_source(&d,&saved,&store).is_err());
}

#[test]
fn mandatory_photo_capacity_is_unchanged_without_actual_optional_frames(){
    let(d,_,_)=fixture();
    for (count,bytes,width) in [(17,1,1),(1,33*1024*1024,1),(1,1,65_000_000)]{
        let assets=(0..count).map(|index|json!({"modality":"photo","attachmentIndex":index,"attachmentIdentity":format!("photo-{index}"),
            "photo":{"artifact":{"bytes":bytes},"width":width,"height":1}})).collect::<Vec<_>>();
        let mut request=json!({"manualFrameRequestIds":[],"postContextBundle":{"members":[{"assets":assets}]},
            "materialReadiness":{"status":"held","reasonCode":"mandatory_photo_capacity"}});let original=request.clone();
        attach_request(&d,&mut request).unwrap();assert_eq!(request,original);assert_eq!(rows(&request["postContextBundle"]["members"][0],"assets").len(),count);
        request["optionalFrameRefs"]=json!([{"origin":"model_requested","artifact":{"bytes":1},"width":1,"height":1}]);
        assert_eq!(attach_request(&d,&mut request),Err("manual_frame_combined_transport_exceeded"));
    }
}

pub(crate) fn native_read_fixture()->Value{
    let(mut d,mut body,token)=fixture();prepare(&mut d,&mut body,&token);fresh(&mut d,&body,&token);d
}

/// Test-only synthetic decoder boundary. Begin/decode/warm perform filesystem
/// work on an isolated fixture outside writers; observe is a pure native commit.
pub(crate) struct NativeExtractionFixture{
    pub(crate) job:Value,pub(crate) plan:Value,store:ArtifactStore,tools:SampleTools,
}
pub(crate) fn native_fixture_begin(d:&mut Value,run:&str,dir:&std::path::Path)->NativeExtractionFixture{
    let token=crate::runtime_lifecycle::admission_token(d,AdmissionClass::Preparation).unwrap();
    let parent=crate::row(d,"jobs",run).unwrap().clone();let member=&parent["prepareBundle"]["request"]["postContextBundle"]["members"][0];
    assert_eq!(d["posts"][0]["id"],member["canonicalPostId"],"fixture uses first exact post");
    let body=json!({"requestId":crate::id(),"postId":member["canonicalPostId"],"attachmentIndex":0,
        "expectedSourceVersion":member["postSourceVersion"],"requestedTimeOrIntent":{"kind":"known_range","timelineBasis":"relative_video_start","startMs":1000,"endMs":2000},
        "reason":"Exact requested fixture scene","prepareJobId":run,"expectedPrepareBundleDigest":parent["prepareBundle"]["digest"]});
    let store=crate::media_fullframes::store().unwrap();let tools=fake_tools(dir);source(d,&store,b"\0\0\0\x0cftypisom");
    let job=fresh(d,&body,&token);let plan=make_plan(&store,&job,base_usage(&parent["prepareBundle"]["request"])).unwrap();
    start(d,&job,&token,&plan,&tools,AT).unwrap();NativeExtractionFixture{job,plan,store,tools}
}
impl NativeExtractionFixture{
    pub(crate) async fn decode(&self)->Value{decode_sample_with(&self.store,&self.plan,&self.tools,&Fake(AtomicUsize::new(0))).await.unwrap()}
    pub(crate) fn observe(&self,d:&mut Value,token:&OwnerToken,decoded:&Value)->crate::ApiResult<Value>{
        retain_observation(d,&self.job,decoded)?;settle(d,&self.job,token,&self.plan,decoded,AT)
    }
    pub(crate) fn warm(&self,d:&Value,complete:&Value){warm_result(d,complete,&self.store).unwrap();}
}

pub(crate) async fn native_app()->(crate::App,tempfile::TempDir){
    use crate::*;
    let temp=tempfile::tempdir().unwrap();let db=open_db(&temp.path().join("manual-baw.sqlite")).await.unwrap();
    let(events,_)=broadcast::channel(8);let profile=accounts::Profile::BawRussia;
    let admission=Arc::new(runtime_lifecycle_startup::Admission::fixture(profile));
    let app=App{lifecycle_task_count:Default::default(),lifecycle_owner:Arc::new(admission.identity().clone()),lifecycle_admission:admission,
        lifecycle_provider_token:Default::default(),lifecycle_work:Default::default(),media_discovery:Default::default(),preparation_wake:Default::default(),provider_session:Default::default(),
        account:profile,navigation:account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),
        preparation_workers:Default::default(),editorial_gate:Default::default(),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),
        events,csrf:"manual-baw-fixture".into(),auth:None,public_origin:None,external_writes:false,port:0,data:temp.path().to_owned(),
        bridge:temp.path().join("NO_MODEL_BRIDGE.mjs"),node:temp.path().join("NO_MODEL_RUNTIME"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
    let mut initial=engine_prepare::tests::baw_fixture(false);initial["posts"][0]["attachments"]=json!([
        {"type":"video","url":"https://example.invalid/native-manual.mp4"},{"type":"photo","url":"https://example.invalid/mandatory.png"}]);
    initial["posts"][0]["sourceUrl"]=json!("https://example.invalid/native-manual.mp4");
    let version=media_fullframes::source_version(&initial["posts"][0],"BAW Russia");
    initial["materials"]=json!([{"id":"manual-full-speech","account":"BAW Russia","kind":"transcript","postKey":"ready-post","sourceUrl":initial["posts"][0]["sourceUrl"],
        "text":"All original source speech remains mandatory.","transcription":{"partial":false,"audioStatus":"transcribed","coverage":"full_audio",
            "sourceVersion":version,"mediaDurationSeconds":6.0,"audioDurationSeconds":6.0}}]);
    knowledge::sync_catalog(&mut initial,&now()).unwrap();photo_acquisition::fixture_commit_baw_photo(&mut initial,"ready-post",&now()).unwrap();
    app.db.change(|d|{for(k,v)in initial.as_object().unwrap(){d[k]=v.clone();}Ok(())}).await.unwrap();
    runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();(app,temp)
}

#[tokio::test]
async fn native_first_capture_refresh_commits_exact_manual_frames_and_preserves_all_mandatory_materials(){
    let(app,temp)=native_app().await;let token=app.lifecycle_admission_token(AdmissionClass::Preparation).await.unwrap();
    let scheduled=app.change_preparation_schedule(|d|{
        let scheduled=crate::engine_prepare::schedule(d,crate::engine_prepare::Input{item_ids:vec!["ready".into()],instruction:None})?;
        crate::preparation_review::record_initial_admission(d,&token,&scheduled.job_id,&crate::now())?;Ok(scheduled)
    }).await.unwrap();
    let run=&scheduled.job_id;let request=scheduled.request().unwrap().clone();assert_eq!(request["manualFrameRequestIds"],json!([]));assert_eq!(request["materialReadiness"]["status"],"ready");
    let before=app.db.read().await.unwrap();let mut captured=before.clone();let extraction=native_fixture_begin(&mut captured,run,temp.path());
    // Fixture extraction admission is generated outside the writer; the real
    // writer accepts its exact new native rows while retaining original scope.
    let extra=rows(&captured,"jobs").iter().filter(|j|!rows(&before,"jobs").iter().any(|old|old["id"]==j["id"])).cloned().collect::<Vec<_>>();
    app.change(|d|{crate::list_mut(d,"jobs").extend(extra);Ok(())}).await.unwrap();
    let blocked=app.change_preparation_first(run,|d|crate::preparation_review::reserve_first_admitted(d,&token,run,&request,&crate::now())).await.unwrap_err();
    assert_eq!(blocked.1,"manual_frame_requested_material_unresolved");
    assert_eq!(wait_classification(&captured,crate::row(&captured,"jobs",run).unwrap()),"active");
    let decoded=extraction.decode().await;let complete=app.change(|d|extraction.observe(d,&token,&decoded)).await.unwrap();
    let snapshot=app.db.read().await.unwrap();assert!(require_warmed(&snapshot,&complete).is_err());extraction.warm(&snapshot,&complete);
    let omitted=app.change_preparation_first(run,|d|crate::preparation_review::reserve_first_admitted(d,&token,run,&request,&crate::now())).await.unwrap_err();
    assert_eq!(omitted.1,"manual_frame_unpaid_capture_refresh_required");
    let refreshed=app.change(|d|crate::engine_prepare::refresh_unpaid_materials(d,run,&request)).await.unwrap();
    assert_eq!(refreshed["manualFrameRequestIds"],json!([extraction.job["id"]]));assert_eq!(refreshed["optionalFrameRefs"][0]["actualPts"],1040);
    assert_eq!(refreshed["postContextBundle"],request["postContextBundle"]);assert_eq!(refreshed["materials"],request["materials"]);
    let after=app.db.read().await.unwrap();let old=crate::row(&before,"jobs",run).unwrap();let new=crate::row(&after,"jobs",run).unwrap();
    assert_eq!(new["scopeReservation"]["keys"],old["scopeReservation"]["keys"]);assert_ne!(new["prepareBundle"]["digest"],old["prepareBundle"]["digest"]);
    assert_eq!(new["preparationStages"]["initialAdmission"]["owner"],old["preparationStages"]["initialAdmission"]["owner"]);
    crate::preparation_reservations::validate_change(&before,&after).unwrap();
    app.change_preparation_first(run,|d|crate::preparation_review::reserve_first_admitted(d,&token,run,&refreshed,&crate::now())).await.unwrap();
    let reserved=app.db.read().await.unwrap();assert!(app.change(|d|crate::engine_prepare::refresh_unpaid_materials(d,run,&refreshed)).await.is_err());assert_eq!(app.db.read().await.unwrap(),reserved);
    // Synthetic model receipt uses the real native material validator, never a
    // model runtime. Paid capture contains the exact frames and mandatory photo.
    let mut result=crate::engine_prepare::tests::lifecycle_first_fixture();let mut paid=reserved.clone();
    crate::model_material_receipt::fixture_result(&mut paid,run,&refreshed,&mut result).unwrap();
    assert_eq!(result["runMetadata"]["materialInvocation"]["optionalFrameRefs"],refreshed["optionalFrameRefs"]);
    assert_eq!(result["runMetadata"]["materialInvocation"]["deliveredFrames"].as_array().unwrap().len(),1);
    assert_eq!(result["runMetadata"]["materialInvocation"]["deliveredPhotos"].as_array().unwrap().len(),1);
    assert!(!app.node.exists());assert!(!app.bridge.exists());app.db.close().await;
}

#[tokio::test]
async fn native_standalone_interleaving_never_admits_first_with_a_frozen_empty_selection(){
    let(app,temp)=native_app().await;let token=app.lifecycle_admission_token(AdmissionClass::Preparation).await.unwrap();
    let store=ArtifactStore::open(&temp.path().join("standalone-cas")).unwrap();let tools=fake_tools(temp.path());
    let before=app.db.read().await.unwrap();let mut native=before.clone();source(&mut native,&store,b"\0\0\0\x0cftypisom");
    let body=json!({"requestId":crate::id(),"postId":"ready-post","attachmentIndex":0,
        "expectedSourceVersion":crate::media_fullframes::source_version(&native["posts"][0],"BAW Russia"),
        "requestedTimeOrIntent":{"kind":"known_range","timelineBasis":"relative_video_start","startMs":1000,"endMs":2000},"reason":"Standalone scene requested before capture"});
    let manual=fresh(&mut native,&body,&token);let plan=make_plan(&store,&manual,json!({"imageCount":0,"imageBytes":0,"pixels":0})).unwrap();
    start(&mut native,&manual,&token,&plan,&tools,AT).unwrap();
    let extra=rows(&native,"jobs").iter().filter(|j|!rows(&before,"jobs").iter().any(|old|old["id"]==j["id"])).cloned().collect::<Vec<_>>();
    app.change(|d|{crate::list_mut(d,"jobs").extend(extra);Ok(())}).await.unwrap();
    let scheduled=app.change_preparation_schedule(|d|{
        let scheduled=crate::engine_prepare::schedule(d,crate::engine_prepare::Input{item_ids:vec!["ready".into()],instruction:None})?;
        crate::preparation_review::record_initial_admission(d,&token,&scheduled.job_id,&crate::now())?;Ok(scheduled)
    }).await.unwrap();let run=&scheduled.job_id;let request=scheduled.request().unwrap().clone();assert_eq!(request["manualFrameRequestIds"],json!([]));
    let denied=app.change_preparation_first(run,|d|crate::preparation_review::reserve_first_admitted(d,&token,run,&request,&crate::now())).await.unwrap_err();
    assert_eq!(denied.1,"manual_frame_requested_material_unresolved");
    let fake=Fake(AtomicUsize::new(0));let decoded=decode_sample_with(&store,&plan,&tools,&fake).await.unwrap();
    let complete=app.change(|d|{retain_observation(d,&manual,&decoded)?;settle(d,&manual,&token,&plan,&decoded,AT)}).await.unwrap();
    let snapshot=app.db.read().await.unwrap();warm_result(&snapshot,&complete,&store).unwrap();
    let refreshed=app.change(|d|crate::engine_prepare::refresh_unpaid_materials(d,run,&request)).await.unwrap();assert_eq!(refreshed["manualFrameRequestIds"],json!([]));
    let omitted=app.change_preparation_first(run,|d|crate::preparation_review::reserve_first_admitted(d,&token,run,&refreshed,&crate::now())).await.unwrap_err();
    assert_eq!(omitted.1,"manual_frame_unpaid_capture_refresh_required");
    let frozen=app.db.read().await.unwrap();let parent=crate::row(&frozen,"jobs",run).unwrap();assert_eq!(wait_classification(&frozen,parent),"terminal");
    assert!(parent["preparationStages"].get("firstAdmission").is_none());assert!(rows(parent,"retainedEvidence").is_empty());
    let mut late=body.clone();late["requestId"]=json!(crate::id());late["requestedTimeOrIntent"]["startMs"]=json!(3000);late["requestedTimeOrIntent"]["endMs"]=json!(4000);
    assert_eq!(app.change(|d|claim(d,&late,"operator-owner",&token,AT)).await.unwrap_err().1,"manual_frame_prepare_capture_required");
    assert_eq!(fake.0.load(Ordering::SeqCst),4);assert!(!app.node.exists());assert!(!app.bridge.exists());app.db.close().await;
}

#[test]
fn multi_video_source_requires_exact_asset_pin_and_overview_is_finite() {
    let dir=tempfile::tempdir().unwrap();let store=ArtifactStore::open(&dir.path().join("cas")).unwrap();
    let(mut d,mut body,token)=fixture();d["posts"][0]["attachments"].as_array_mut().unwrap().push(json!({"type":"video","url":"https://example.invalid/other.mp4"}));
    body["expectedSourceVersion"]=json!(crate::media_fullframes::source_version(&d["posts"][0],"BAW Russia"));source(&mut d,&store,b"\0\0\0\x0cftypisom");
    let pin=crate::media_speech_assets::capture(&d,&d["posts"][0],0).unwrap();let mut legacy=d.clone();legacy["jobs"][0]["result"]["visualProgress"].as_object_mut().unwrap().remove("assetPin");
    assert!(retained_source(&legacy,&pin).is_err());let foreign=crate::media_speech_assets::capture(&d,&d["posts"][0],1).unwrap();assert!(retained_source(&d,&foreign).is_err());
    body["requestedTimeOrIntent"]=json!({"kind":"uniform_overview","timelineBasis":"relative_video_start"});let job=fresh(&mut d,&body,&token);
    let plan=make_plan(&store,&job,json!({"imageCount":0,"imageBytes":0,"pixels":0})).unwrap();assert_eq!(plan["targets"].as_array().unwrap().len(),6);assert_eq!(plan["exhaustive"],false);
    assert!(make_plan(&store,&job,json!({"imageCount":11,"imageBytes":0,"pixels":0})).is_err());
}

async fn offline_command(path:&std::path::Path,args:&[String])->Vec<u8> {
    let mut command=tokio::process::Command::new(path);command.args(args).kill_on_drop(true);
    #[cfg(windows)]command.creation_flags(0x08000000);
    let out=tokio::time::timeout(Duration::from_secs(30),command.output()).await.unwrap().unwrap();
    assert!(out.status.success(),"offline fixture command failed: {}",String::from_utf8_lossy(&out.stderr));out.stdout
}
#[tokio::test]
#[ignore="root queue only: actual synthetic file FFmpeg manual before first paid generation"]
async fn offline_actual_manual_frames_before_first_generation() {
    let dir=tempfile::tempdir().unwrap();let store=ArtifactStore::open(&dir.path().join("cas")).unwrap();
    let ffmpeg=PathBuf::from(std::env::var_os("COMMUNITYHERO_TEST_FRAME_FFMPEG").expect("explicit fixture ffmpeg"));
    let ffprobe=PathBuf::from(std::env::var_os("COMMUNITYHERO_TEST_FRAME_FFPROBE").expect("explicit fixture ffprobe"));
    assert!(ffmpeg.is_absolute()&&ffprobe.is_absolute());
    let source_path=dir.path().join("manual-synthetic.mp4");
    let args=["-nostdin","-hide_banner","-loglevel","error","-f","lavfi","-i","testsrc=size=64x48:rate=10:duration=6","-c:v","mpeg4","-threads","1","-g","5","-an","-movflags","+faststart","-y"]
        .iter().map(|s|s.to_string()).chain([source_path.display().to_string()]).collect::<Vec<_>>();offline_command(&ffmpeg,&args).await;
    let ffmpeg_version=String::from_utf8(offline_command(&ffmpeg,&["-version".into()]).await).unwrap().lines().next().unwrap().to_owned();
    let ffprobe_version=String::from_utf8(offline_command(&ffprobe,&["-version".into()]).await).unwrap().lines().next().unwrap().to_owned();
    let tools=SampleTools{ffmpeg_sha256:format!("{:x}",Sha256::digest(std::fs::read(&ffmpeg).unwrap())),ffprobe_sha256:format!("{:x}",Sha256::digest(std::fs::read(&ffprobe).unwrap())),
        ffmpeg,ffprobe,ffmpeg_version,ffprobe_version,deadline:Duration::from_secs(30)};
    let(mut d,mut body,token)=fixture();let mut request=prepare(&mut d,&mut body,&token);source(&mut d,&store,&std::fs::read(&source_path).unwrap());
    let job=fresh(&mut d,&body,&token);let plan=make_plan(&store,&job,base_usage(&request)).unwrap();start(&mut d,&job,&token,&plan,&tools,AT).unwrap();
    let decoded=decode_sample(&store,&plan,&tools,&crate::runtime_owned_work::Registry::default()).await.unwrap();verify_sample_result(&store,&plan,&decoded,&tools).unwrap();
    assert_eq!(decoded["status"],"complete");assert_eq!(decoded["frames"][0]["width"],64);assert_eq!(decoded["frames"][0]["height"],48);
    assert_eq!(decoded["frames"][0]["pixelArtifact"]["bytes"],64*48*3);assert_eq!(decoded["frames"][0]["fullFrame"],true);
    retain_observation(&mut d,&job,&decoded).unwrap();let complete=settle(&mut d,&job,&token,&plan,&decoded,AT).unwrap();warm_result(&d,&complete,&store).unwrap();
    attach_request(&d,&mut request).unwrap();assert_eq!(request["optionalFrameRefs"].as_array().unwrap().len(),1);
    assert!(d["jobs"][0]["preparationStages"]["first"].is_null());assert!(d["jobs"][0]["preparationStages"].get("firstAdmission").is_none());
    assert!(rows(&d,"jobs").iter().all(|j|j.get("originatingAnsweringAttemptId").is_none()&&j["preparationStages"].get("repairBudget").is_none()));
    validate_result(&d,&complete,&ArtifactStore::open(store.root()).unwrap()).unwrap();
}
