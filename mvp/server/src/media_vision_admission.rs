//! One writer-admitted paid vision stage on the existing V2 job checkpoint.
//! Ambiguity blocks new reservations. Returned output is immutable evidence,
//! never retry authority. The worker's original lifecycle token is not renewed.
use crate::{App, ApiResult, Value, conflict, runtime_lifecycle as lifecycle,
    runtime_owned_work::{Kind, Work}};
use serde_json::json;
use sha2::{Digest, Sha256};

const FIELD: &str = "visionStage";
fn digest(value:&Value)->String { format!("{:x}",Sha256::digest(value.to_string().as_bytes())) }
fn without_stage(mut progress:Value)->Value {
    if let Some(fields)=progress.as_object_mut(){fields.remove(FIELD);if fields.is_empty(){return Value::Null;}} progress
}
fn binding(job:&Value)->Value {
    let mut fields=serde_json::Map::new();
    for key in ["id","kind","purpose","account","connectorBinding","refId","createdAt","startedAt","attemptId","sourceAttempts"] {
        fields.insert(key.into(),job[key].clone());
    }
    fields.insert("progress".into(),without_stage(job["result"]["visualProgress"].clone()));
    Value::Object(fields)
}
fn validate_output(operation:&str,request:&Value,response:&Value)->ApiResult<()> {
    match operation {
        "media_vision_chunk"=>crate::media_frame_contract::validate_response(request,response).map(|_|()).map_err(conflict),
        "media_vision"=>crate::media_visual::admit(request,response).map(|_|()).map_err(conflict),
        _=>Err(conflict("Invalid paid vision stage operation")),
    }
}
pub(crate) struct Worker {
    app:App, job_id:String, original:Option<lifecycle::OwnerToken>, binding:Value,
    progress:Value, prior_verified:Option<String>, connector:Value, legacy_source:Option<Value>,
}
pub(crate) struct Stage { worker:Worker, progress:Value, reservation:Value, work:Option<Work> }
impl Worker {
    /// Call once before preflight/extraction. A closed capture may finish local
    /// existing evidence, but can never refresh itself into new paid admission.
    pub(crate) async fn capture(app:&App,id:&str,expected:Option<&Value>)->ApiResult<Self> {
        let original=app.lifecycle_admission_token(lifecycle::AdmissionClass::Media).await.ok();
        if let Some(token)=original.as_ref(){lifecycle::require_runtime_owner(token,&app.lifecycle_owner)?;}
        let d=app.read().await?;
        let connector=crate::active_binding(&d)?.to_json();
        let job=crate::row(&d,"jobs",id)?;
        let progress=job["result"]["visualProgress"].clone();
        if job["kind"]!="media" || job["purpose"]!="auto_media" || job["status"]!="running"
            || d["account"]!=app.account.display() || job["account"]!=d["account"] || job["connectorBinding"]!=connector
            || progress["schemaVersion"]!=2 || progress["leaseId"].as_str().is_none_or(str::is_empty)
            || expected.is_some_and(|expected|progress!=*expected) {
            return Err(conflict("Vision stage requires exact current V2 media worker lease"));
        }
        let prior=&progress[FIELD];
        let prior_verified=if prior["phase"]=="completed" {
            let store=crate::media_fullframes::store().map_err(|e|conflict(&e))?;
            let reference=crate::media_fullframes::reference(&prior["outputArtifact"]).map_err(|e|conflict(&e))?;
            let bytes=store.read_bytes(&reference,40*1024*1024).map_err(|_|conflict("Vision returned artifact unavailable"))?;
            let record:Value=serde_json::from_slice(&bytes).map_err(|_|conflict("Vision returned artifact invalid"))?;
            if record["kind"]!="returned-vision-stage-output" || record["attemptId"]!=prior["attemptId"]
                || record["runtimeOwner"]!=prior["runtimeOwner"] || record["account"]!=app.account.display()
                || record["jobId"]!=id || record["workerBinding"]!=prior["workerBinding"]
                || record["requestSha256"]!=prior["requestSha256"] || record["responseSha256"]!=prior["responseSha256"]
                || digest(&record["request"])!=prior["requestSha256"].as_str().unwrap_or("")
                || digest(&record["response"])!=prior["responseSha256"].as_str().unwrap_or("") {
                return Err(conflict("Vision returned artifact binding changed"));
            }
            validate_output(prior["operation"].as_str().unwrap_or(""),&record["request"],&record["response"])?;
            if prior["operation"]=="media_vision_chunk" {
                let (_,_,covered)=crate::media_fullframes::reviewed(&store,&progress["inventoryDescriptor"],
                    &progress["selectionDescriptor"],&progress["latestReceipt"]).map_err(|e|conflict(&e))?;
                if progress["nextSelectionIndex"]!=covered {return Err(conflict("Vision completed output cursor is not committed"));}
                // Alias-only chunks may advance the cursor without a new paid
                // stage. Verify the paid output is in that same complete chain.
                let mut link=progress["latestReceipt"].clone();let mut found=false;
                while !link.is_null(){
                    let receipt=crate::media_fullframes::read(&store,&link).map_err(|e|conflict(&e))?;
                    if digest(&receipt["request"])==prior["requestSha256"].as_str().unwrap_or("")
                        && digest(&receipt["response"])==prior["responseSha256"].as_str().unwrap_or("") {found=true;break;}
                    link=receipt["previousReceipt"].clone();
                }
                if !found{return Err(conflict("Vision completed output is not in committed cursor receipts"));}
            }
            Some(digest(prior))
        } else {None};
        Ok(Self{app:app.clone(),job_id:id.into(),original,binding:binding(&job),progress,prior_verified,connector,legacy_source:None})
    }
    /// The legacy worker has an explicit durable source-attempt context. It
    /// uses the full writer because ScopedMedia accepts only auto_media rows.
    pub(crate) async fn capture_legacy(app:&App,id:&str,source:&crate::media_processing::MediaSource)->ApiResult<Self> {
        let original=app.lifecycle_admission_token(lifecycle::AdmissionClass::Media).await.ok();
        if let Some(token)=original.as_ref(){lifecycle::require_runtime_owner(token,&app.lifecycle_owner)?;}
        let d=app.read().await?;lifecycle::current_owner(&d,&app.lifecycle_owner)?;
        let connector=crate::active_binding(&d)?.to_json();let job=crate::row(&d,"jobs",id)?;
        let attempt=job["sourceAttempts"].as_array().and_then(|a|a.last()).ok_or_else(||conflict("Legacy vision source attempt missing"))?;
        let post=crate::row(&d,"posts",attempt["postId"].as_str().ok_or_else(||conflict("Legacy vision post missing"))?)?;
        if d["account"]!=app.account.display() || source.account!=app.account.display()
            || job["kind"]!="media" || job["status"]!="running" || job["visualContractVersion"]==2
            || (!job["account"].is_null()&&job["account"]!=d["account"])
            || (!job["connectorBinding"].is_null()&&job["connectorBinding"]!=connector)
            || attempt["status"]!="running" || attempt["postId"]!=job["refId"]
            || attempt["postKey"]!=source.post_key || attempt["postKey"]!=post["postKey"]
            || attempt["sourceVersion"]!=crate::media_fullframes::source_version(post,app.account.display()) {
            return Err(conflict("Legacy vision requires exact current job and source attempt"));
        }
        let legacy_source=json!({"postId":post["id"],"postKey":post["postKey"],"sourceVersion":attempt["sourceVersion"],
            "materialEpoch":crate::media_queue::material_epoch(&d,post)});
        Ok(Self{app:app.clone(),job_id:id.into(),original,binding:binding(job),progress:job["result"]["visualProgress"].clone(),
            prior_verified:None,connector,legacy_source:Some(legacy_source)})
    }
    async fn write<T>(&self,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T> {
        if self.legacy_source.is_some(){self.app.change(f).await}else{self.app.change_media(f).await}
    }
    fn current(&self,d:&Value)->ApiResult<()> {
        lifecycle::current_owner(d,&self.app.lifecycle_owner)?;
        if d["account"]!=self.app.account.display() || crate::active_binding(d)?.to_json()!=self.connector {
            return Err(conflict("Vision company or connector changed"));
        }
        let job=crate::row(d,"jobs",&self.job_id)?;
        if job["status"]!="running" || binding(job)!=self.binding {
            return Err(conflict("Vision worker binding changed"));
        }
        if let Some(source)=self.legacy_source.as_ref(){
            let post=crate::row(d,"posts",source["postId"].as_str().ok_or_else(||conflict("Legacy vision source missing"))?)?;
            if post["postKey"]!=source["postKey"] || source["sourceVersion"]!=crate::media_fullframes::source_version(post,self.app.account.display())
                || source["materialEpoch"]!=crate::media_queue::material_epoch(d,post)
                || !crate::post_media_policy::visual_required(d,post)? {return Err(conflict("Legacy vision source or policy changed"));}
            return Ok(());
        }
        let progress=&job["result"]["visualProgress"];
        let post=crate::row(d,"posts",progress["sourcePostId"].as_str().ok_or_else(||conflict("Vision source missing"))?)?;
        if progress["account"]!=d["account"] || progress["connectorBinding"]!=crate::active_binding(d)?.to_json()
            || progress["sourcePostKey"]!=post["postKey"]
            || progress["sourceVersion"]!=crate::media_fullframes::source_version(post,self.app.account.display())
            || progress["materialEpoch"]!=crate::media_queue::material_epoch(d,post)
            || !crate::post_media_policy::visual_required(d,post)? {
            return Err(conflict("Vision source, policy or material changed"));
        }
        Ok(())
    }
    pub(crate) async fn reserve(self,operation:&str,request:&Value)->ApiResult<Stage> {
        let post_key=self.legacy_source.as_ref().map(|s|&s["postKey"]).unwrap_or(&self.progress["sourcePostKey"]);
        if !matches!(operation,"media_vision_chunk"|"media_vision") || request["source"]["account"]!=self.app.account.display()
            || request["source"]["postKey"]!=*post_key
            || (self.legacy_source.is_none()&&request["source"]["mediaSha256"]!=self.progress["source"]["sha256"])
            || (self.legacy_source.is_some()&&operation!="media_vision") {
            return Err(conflict("Vision request binding changed"));
        }
        let token=self.original.as_ref().ok_or_else(||conflict("Original vision worker admission is closed"))?;
        // Native ownership precedes the locked reservation. Any rejected writer
        // drops an unstarted Work, never creates an uncounted paid dispatch.
        let work=self.app.lifecycle_work.begin(Kind::MediaTool)?;
        let reservation=json!({"version":1,"phase":"reserved","attemptId":crate::id(),
            "operation":operation,"requestSha256":digest(request),"nativeWorkId":work.id(),
            "runtimeOwner":{"account":token.account,"runtimeId":token.runtime_id,"releaseSha256":token.release_sha256,"epoch":token.epoch},
            "workerBinding":self.binding,"source":request["source"],"chunk":request["chunk"],"reservedAtUtc":crate::now()});
        let mut progress=self.progress.clone();if progress.is_null(){progress=json!({});} progress[FIELD]=reservation.clone();
        self.write(|d| {
            self.current(d)?;
            lifecycle::require_runtime_owner(token,&self.app.lifecycle_owner)?;
            lifecycle::require_admission(d,token,lifecycle::AdmissionClass::Media)?;
            let job=crate::row_mut(d,"jobs",&self.job_id)?;
            if job["result"]["visualProgress"]!=self.progress {return Err(conflict("Vision checkpoint changed"));}
            let prior=&self.progress[FIELD];
            if !prior.is_null() {
                // A different model, request, lease, cursor or post cannot erase
                // a possibly paid stage. Only exact completed output + committed
                // cursor progression permits the next chunk.
                if prior["version"]!=1 || prior["phase"]!="completed" || self.prior_verified.as_ref()!=Some(&digest(prior))
                    || operation!="media_vision_chunk" || prior["operation"]!="media_vision_chunk"
                    || prior["source"]!=request["source"]
                    || prior["chunk"]["endSelectionIndexExclusive"].as_u64().zip(request["chunk"]["firstSelectionIndex"].as_u64())
                        .is_none_or(|(end,first)|end>first)
                    || request["chunk"]["firstSelectionIndex"]!=self.progress["nextSelectionIndex"]
                    || ["account","connectorBinding","sourcePostId","sourcePostKey","sourceVersion","materialEpoch",
                        "source","sourceIdentity","inventoryDescriptor","selectionDescriptor"].iter()
                        .any(|key|prior["workerBinding"]["progress"][*key]!=self.progress[*key])
                    || prior["requestSha256"]==digest(request) {
                    return Err(conflict("Retained vision stage blocks new paid reservation"));
                }
            }
            job["result"]["visualProgress"]=progress.clone(); Ok(())
        }).await?;
        Ok(Stage{worker:self,progress,reservation,work:Some(work)})
    }
}
impl Stage {
    pub(crate) fn progress(&self)->&Value {&self.progress}
    /// Preserve the original reservation across Draining; no new token is read.
    pub(crate) async fn dispatch(mut self,request:Value,resource:Option<&mut crate::media_processing::full::gpu_outcome::Outcome>)->ApiResult<(Value,Value)> {
        if digest(&request)!=self.reservation["requestSha256"].as_str().unwrap_or("") {
            return Err(conflict("Vision reserved request changed"));
        }
        let mut dispatched=self.reservation.clone(); dispatched["phase"]=json!("dispatched");
        self.worker.write(|d|{
            self.worker.current(d)?;
            let job=crate::row_mut(d,"jobs",&self.worker.job_id)?;
            if job["result"]["visualProgress"]!=self.progress {return Err(conflict("Vision reservation changed before dispatch"));}
            job["result"]["visualProgress"][FIELD]=dispatched.clone(); Ok(())
        }).await?;
        self.progress[FIELD]=dispatched;
        let operation=self.reservation["operation"].as_str().unwrap().to_owned();
        let work=self.work.take().ok_or_else(||conflict("Vision native reservation already consumed"))?;
        let result=self.worker.app.bridge_admitted_observed(&operation,request.clone(),work,resource).await;
        let valid=result.as_ref().ok().is_some_and(|output|validate_output(&operation,&request,output).is_ok());
        // Store returned output before any owner/checkpoint writer. A rejected
        // completion still leaves this immutable record plus existing paid spool.
        let output_artifact=if let Ok(output)=result.as_ref(){
            let store=crate::media_fullframes::store().map_err(|e|conflict(&e))?;
            let record=json!({"version":1,"kind":"returned-vision-stage-output","account":self.worker.app.account.display(),
                "jobId":self.worker.job_id,"workerBinding":self.reservation["workerBinding"],
                "attemptId":self.reservation["attemptId"],"runtimeOwner":self.reservation["runtimeOwner"],
                "requestSha256":digest(&request),"responseSha256":digest(output),"request":request,"response":output,
                "retryAuthorized":false});
            let reference=store.put_bytes(record.to_string().as_bytes()).map_err(|_|conflict("Vision returned output retention failed"))?;
            store.verify(&reference).map_err(|_|conflict("Vision returned output retention verification failed"))?;
            Some(reference.to_json())
        } else {None};
        let mut settled=self.reservation.clone();
        settled["phase"]=json!(if valid{"completed"}else{"unknown"});
        settled["finishedAtUtc"]=json!(crate::now());
        if let Some(reference)=output_artifact {settled["outputArtifact"]=reference;}
        if let Ok(output)=result.as_ref(){settled["responseSha256"]=json!(digest(output));}
        self.worker.write(|d|{
            self.worker.current(d)?; // fixed owner; completion itself need not be Running
            let job=crate::row_mut(d,"jobs",&self.worker.job_id)?;
            if job["result"]["visualProgress"]!=self.progress {return Err(conflict("Vision settlement checkpoint changed; paid output retained"));}
            job["result"]["visualProgress"][FIELD]=settled.clone(); Ok(())
        }).await?;
        self.progress[FIELD]=settled;
        let output=result?;
        if !valid {return Err(conflict("Vision returned response invalid; retained stage is unknown"));}
        Ok((output,self.progress))
    }
}
// Additive child module for production-snapshot-r1 media_vision_admission.rs.
// Source only: ROOT owns compilation and execution against its repaired test_app.
#[cfg(test)]
mod connected_stage_tests {
    use super::*;

    async fn legacy_fixture() -> (App, tempfile::TempDir, crate::media_processing::MediaSource, Value) {
        let (app, temp) = crate::tests::test_app().await;
        let account = app.account.display().to_owned();
        let source = crate::media_processing::MediaSource {
            account: account.clone(), post_key: "vision-admission-post".into(),
            title: "Synthetic vision admission video".into(),
            source_url: "https://example.invalid/video.mp4".into(),
            fallback_url: None, source_discovery: None,
        };
        app.change(|d| {
            let connector = crate::active_binding(d)?.to_json();
            let post = json!({"id":"vision-admission-post","postKey":source.post_key,
                "account":account,"connectorBinding":connector,"title":source.title,
                "attachments":[{"type":"video","url":source.source_url}]});
            let version = crate::media_fullframes::source_version(&post, &account);
            crate::list_mut(d, "posts").push(post);
            if !d["settings"]["postMediaPolicies"].is_object() {
                d["settings"]["postMediaPolicies"] = json!({});
            }
            d["settings"]["postMediaPolicies"]["vision-admission-post"] = json!({
                "version":1,"revision":1,"status":"active","postId":"vision-admission-post",
                "account":account,"connectorBinding":connector,"sourceVersion":version,
                "mode":"full_audio_visual"});
            for id in ["vision-admission-a", "vision-admission-b"] {
                crate::list_mut(d, "jobs").push(json!({"id":id,"kind":"media","status":"running",
                    "account":account,"connectorBinding":connector,"refId":"vision-admission-post",
                    "createdAt":"2026-10-04T00:00:00Z","startedAt":"2026-10-04T00:00:01Z",
                    "sourceAttempts":[{"status":"running","postId":"vision-admission-post",
                        "postKey":source.post_key,"sourceVersion":version}],"result":{}}));
            }
            Ok(())
        }).await.unwrap();
        let request = json!({"source":{"account":account,"postKey":source.post_key,
            "mediaSha256":"b".repeat(64)},"model":"synthetic-fixture"});
        (app, temp, source, request)
    }

    async fn v2_fixture() -> (App, tempfile::TempDir, Value) {
        let (app, temp, _source, request) = legacy_fixture().await;
        app.change(|d| {
            let post = crate::row(d, "posts", "vision-admission-post")?.clone();
            let connector = crate::active_binding(d)?.to_json();
            let mut progress = crate::media_fullframes::initial(app.account.display(), &connector,
                &post, "2026-10-04T00:00:00Z");
            progress["leaseId"] = json!("vision-fixture-lease");
            progress["leaseEpoch"] = json!(1);
            progress["materialEpoch"] = json!(crate::media_queue::material_epoch(d, &post));
            progress["source"] = json!({"sha256":"b".repeat(64),"bytes":1});
            for id in ["vision-admission-a", "vision-admission-b"] {
                let job = crate::row_mut(d, "jobs", id)?;
                job["purpose"] = json!("auto_media");
                job["visualContractVersion"] = json!(2);
                job["result"]["visualProgress"] = progress.clone();
            }
            Ok(())
        }).await.unwrap();
        (app, temp, request)
    }

    #[tokio::test]
    async fn current_writer_reservation_survives_drain_and_new_reservation_is_rejected() {
        let (app, _temp, request) = v2_fixture().await;
        let first = Worker::capture(&app, "vision-admission-a", None).await.unwrap();
        let original = first.original.clone().unwrap();
        // Capture before closing: a stale preflight must fail at the actual writer.
        let second = Worker::capture(&app, "vision-admission-b", None).await.unwrap();
        let untouched = second.progress.clone();
        let stage = first.reserve("media_vision", &request).await.unwrap();
        let retained = stage.progress().clone();
        assert_eq!(retained[FIELD]["phase"], "reserved");
        assert_eq!(app.lifecycle_work.snapshot().unwrap().active, 1);
        app.change(|d| lifecycle::begin_drain(d, &original, &"d".repeat(64),
            "vision-fixture-drain", false).map(|_| ())).await.unwrap();
        assert!(second.reserve("media_vision", &request).await.is_err());
        let d = app.read().await.unwrap();
        assert_eq!(d["runtimeLifecycle"]["phase"], "draining");
        assert_eq!(crate::row(&d, "jobs", "vision-admission-a").unwrap()["result"]["visualProgress"], retained);
        assert_eq!(crate::row(&d, "jobs", "vision-admission-b").unwrap()["result"]["visualProgress"], untouched);
        let work = app.lifecycle_work.snapshot().unwrap();
        assert_eq!((work.active, work.unresolved), (1, 0));
        drop(stage);
        assert_eq!(app.lifecycle_work.snapshot().unwrap().active, 0);
    }

    #[tokio::test]
    async fn retained_reserved_and_unknown_stages_block_changed_request_and_retarget() {
        for phase in ["reserved", "unknown"] {
            let (app, _temp, source, request) = legacy_fixture().await;
            let stage = Worker::capture_legacy(&app, "vision-admission-a", &source).await.unwrap()
                .reserve("media_vision", &request).await.unwrap();
            drop(stage);
            app.change(|d| {
                crate::row_mut(d, "jobs", "vision-admission-a")?["result"]["visualProgress"][FIELD]["phase"] = json!(phase);
                Ok(())
            }).await.unwrap();
            let before = app.read().await.unwrap();
            let retained = crate::row(&before, "jobs", "vision-admission-a").unwrap()["result"]["visualProgress"].clone();
            let mut changed = request.clone(); changed["model"] = json!("different-model");
            assert!(Worker::capture_legacy(&app, "vision-admission-a", &source).await.unwrap()
                .reserve("media_vision", &changed).await.is_err());
            let mut retarget = request.clone(); retarget["source"]["postKey"] = json!("other-post");
            assert!(Worker::capture_legacy(&app, "vision-admission-a", &source).await.unwrap()
                .reserve("media_vision", &retarget).await.is_err());
            let d = app.read().await.unwrap();
            assert_eq!(crate::row(&d, "jobs", "vision-admission-a").unwrap()["result"]["visualProgress"], retained);
            let work = app.lifecycle_work.snapshot().unwrap();
            assert_eq!((work.active, work.unresolved), (0, 0));
        }
    }

    #[tokio::test]
    async fn failed_legacy_unknown_stage_survives_late_sibling_coverage_reconcile() {
        let (app, _temp) = crate::tests::test_app().await;
        const AT: &str = "2026-10-04T00:00:00Z";
        app.change(|d| {
            // Match the queue's established exact-identity sibling fixture.
            d["posts"] = json!([]); d["items"] = json!([]); d["jobs"] = json!([]);
            d["materials"] = json!([]); d["knowledge_entries"] = json!([]);
            d["knowledge_versions"] = json!([]);
            d["settings"]["postMediaPolicies"] = json!({});
            let connector = crate::active_binding(d)?.to_json();
            for (id, key, channel, object) in [
                ("post-11391:one", "11391:one", "VK", "11391"),
                ("post-11390:two", "11390:two", "YouTube", "11390")
            ] {
                let post = json!({"id":id,"postKey":key,"objectId":object,
                    "title":"Synthetic exact video siblings","canonicalMediaId":"fixture-exact-video",
                    "channel":channel,"attachments":[{"type":"video"}]});
                let version = crate::media_fullframes::source_version(&post, app.account.display());
                crate::list_mut(d,"posts").push(post);
                crate::list_mut(d,"items").push(json!({"id":format!("item-{object}-one"),
                    "itemId":"one","objectId":object,"postId":id,"postKey":key,
                    "conversationKey":format!("{object}:thread"),"providerStatus":"new","workflow":"attention"}));
                d["settings"]["postMediaPolicies"][id] = json!({"version":1,"revision":1,
                    "status":"active","postId":id,"account":app.account.display(),
                    "connectorBinding":connector,"sourceVersion":version,"mode":"full_audio_visual"});
            }
            crate::media_queue::reconcile(d, AT)
        }).await.unwrap();
        let d = app.read().await.unwrap();
        let id = crate::list(&d,"jobs").iter().find(|job|job["kind"]=="media"
            && job["purpose"]=="auto_media").unwrap()["id"].as_str().unwrap().to_owned();
        let peer_id = if crate::row(&d,"jobs",&id).unwrap()["refId"] == "post-11390:two" {
            "post-11391:one"
        } else { "post-11390:two" };
        // This synthetic returned record is actual immutable CAS evidence. No
        // provider or model is called, and the opaque reference is never retry authority.
        let store = crate::media_fullframes::store().unwrap();
        let artifact = store.put_bytes(b"synthetic returned paid-stage evidence").unwrap().to_json();
        let raw_progress = json!({"legacyCursor":7,"visionStage":{"version":1,"phase":"unknown",
            "attemptId":"retained-paid-attempt","operation":"media_vision","requestSha256":"a".repeat(64),
            "outputArtifact":artifact,"retryAuthorized":false}});
        app.change(|d| {
            let job = crate::row_mut(d,"jobs",&id)?;
            job["status"] = json!("failed"); job["fallbackAllowed"] = json!(false);
            job["result"] = json!({"visualProgress":raw_progress});
            Ok(())
        }).await.unwrap();
        app.change(|d| {
            let peer = crate::row(d,"posts",peer_id)?.clone();
            assert_ne!(crate::row(d,"jobs",&id)?["refId"],peer["id"],"coverage must be from a sibling");
            let evidence = crate::media_fullframes::fixture_for_post(app.account.display(),&peer);
            crate::list_mut(d,"materials").push(json!({"id":"late-sibling-audio","kind":"transcript",
                "account":app.account.display(),"postKey":peer["postKey"],"text":"Synthetic complete audio"}));
            crate::list_mut(d,"materials").push(json!({"id":"late-sibling-visual","kind":"visual_context",
                "account":app.account.display(),"postKey":peer["postKey"],"text":"Synthetic complete visual",
                "mediaSha256":evidence["source"]["mediaSha256"],"visualEvidence":evidence}));
            // Independently observed target bytes keep the sibling in the same
            // queue group after the donor's visual head adds its observed SHA.
            // A canonical identifier alone cannot transfer visual evidence.
            let target_id = crate::row(d,"jobs",&id)?["refId"].as_str().unwrap().to_owned();
            let target = crate::row_mut(d,"posts",&target_id)?;
            target["mediaSha256"] = evidence["source"]["mediaSha256"].clone();
            let target_version = crate::media_fullframes::source_version(target,app.account.display());
            d["settings"]["postMediaPolicies"][&target_id]["sourceVersion"] = json!(target_version);
            crate::knowledge::sync_catalog(d,AT).map_err(|e|crate::bad(&e))?;
            let target = crate::row(d,"posts",&target_id)?;
            let groups = crate::knowledge::visual_groups(d,app.account.display());
            assert_eq!(groups.get(&target_id),groups.get(peer_id),"recipient and donor must remain exact byte siblings");
            assert!(groups.contains_key(&target_id));
            // Prove the setup reaches recipient coverage, rather than a donor-only/no-op path.
            let lookup = crate::knowledge::TranscriptLookup::new(d,AT).unwrap();
            assert!(lookup.ready(&peer).unwrap());
            assert!(lookup.ready(target).unwrap());
            crate::media_queue::reconcile(d,"2026-10-04T00:01:00Z")
        }).await.unwrap();
        let after = app.read().await.unwrap();
        let job = crate::row(&after,"jobs",&id).unwrap();
        assert_eq!(job["status"],"completed");
        assert_eq!(job["result"]["reused"],true);
        assert_eq!(job["result"]["visualProgress"],raw_progress);
        assert_eq!(job["result"]["visualProgress"][FIELD]["outputArtifact"],artifact);
        store.verify(&crate::media_fullframes::reference(&artifact).unwrap()).unwrap();
        assert_eq!(app.lifecycle_work.snapshot().unwrap().active,0);
    }
}
