//! App adapter for company/file ASR ownership. All paid paths use this adapter.
//! CAS verification runs outside the writer; reservations/fences run inside it.
//! Capturing returned output deliberately does not require a current post alias.
use crate::{App, ApiResult, accounts::Profile, media_analysis as ledger,
    media_processing::analysis_output::{self as output, AudioLifecycle, LifecycleFuture},
    runtime_lifecycle::{self, AdmissionClass, OwnerToken}};
use serde_json::{json, Value};

pub(crate) struct Runtime {
    app: App,
    execution_job_id: String,
    progress: Value,
    execution_pin: Value,
    binding: Value,
    lifecycle: OwnerToken,
}
fn error(e: crate::ApiError)->String { e.1 }
fn pin(job:&Value)->Result<Value,String> {
    let progress=match job["kind"].as_str() {
        Some("media")=>&job["result"]["visualProgress"],
        Some("media_audio")=>&job["audioPin"]["progress"],
        _=>return Err("media_analysis_execution_kind_invalid".into()),
    };
    if job["status"]!="running" || progress["schemaVersion"]!=2 {
        return Err("media_analysis_execution_not_running".into());
    }
    Ok(json!({"kind":job["kind"],"account":job["account"],"connectorBinding":job["connectorBinding"],
        "progress":progress,"audioPin":job["audioPin"]}))
}
fn current_alias(d:&Value,progress:&Value)->ApiResult<()> {
    crate::media_speech_assets::require_progress(d,progress).map_err(|e|crate::conflict(&e))?;
    if progress["account"]!=d["account"] || progress["connectorBinding"]!=crate::active_binding(d)?.to_json() {
        return Err(crate::conflict("media_analysis_alias_company_or_connector_changed"));
    }
    let post=crate::row(d,"posts",progress["sourcePostId"].as_str().ok_or_else(||crate::conflict("media_analysis_alias_missing"))?)?;
    if post["postKey"]!=progress["sourcePostKey"] || progress["sourceVersion"]!=crate::media_fullframes::source_version(post,
        d["account"].as_str().ok_or_else(||crate::conflict("media_analysis_company_missing"))?) {
        return Err(crate::conflict("media_analysis_alias_source_changed"));
    }
    Ok(())
}
fn legacy_quarantine(d:&Value,id:&str,file:&Value)->Result<(),String> {
    // A V2 download/inventory owner has not dispatched ASR. Old V1 workers and
    // media_audio UNKNOWNs can have paid work absent from the new ledger.
    let state=ledger::ledger_from_workspace(d)?;
    for job in crate::list(d,"jobs") {
        if job["id"]==id || !matches!(job["kind"].as_str(),Some("media"|"media_audio"))
            || !matches!(job["status"].as_str(),Some("running"|"dispatching"|"unknown"|"interrupted")) {continue;}
        let link=&job["mediaAnalysisBinding"];
        let known=link.is_object() && state["analyses"].as_array().is_some_and(|analyses|analyses.iter().any(|a| {
            a["key"]==link["key"] && a["attempts"].as_array().is_some_and(|attempts|attempts.iter().any(|attempt| {
                attempt["attemptId"]==link["attemptId"] && attempt["owner"]==link["owner"] && attempt["epoch"]==link["epoch"]
                    && attempt["originalRequest"]["executionJobId"]==job["id"]
            }))
        }));
        if known {continue;}
        let p=if job["kind"]=="media_audio" {&job["audioPin"]["progress"]} else {&job["result"]["visualProgress"]};
        let possible_asr=job["kind"]=="media_audio" || p["schemaVersion"]!=2
            || p["phase"]=="finalize" || job["finalizeAsr"]["started"]==true;
        if !possible_asr {continue;}
        if p["source"]["sha256"].is_null() || p["source"]["sha256"]==file["sha256"] {
            return Err("media_analysis_legacy_asr_reconciliation_required".into());
        }
    }
    Ok(())
}
fn read_adopted(store:&crate::media_artifacts::ArtifactStore,result:&Value)->Result<Value,String> {
    let read=|reference:&Value|->Result<Value,String> {
        let bytes=store.read_bytes(&crate::media_fullframes::reference(reference)?,4*1024*1024)
            .map_err(|_|"media_analysis_legacy_output_unavailable")?;
        serde_json::from_slice(&bytes).map_err(|_|"media_analysis_legacy_output_invalid".into())
    };
    let manifest=read(&result["manifest"])?;let payload=read(&result["normalizedOutput"])?;
    if manifest["schemaVersion"]!=1 || payload["schemaVersion"]!=1
        || manifest["normalizedOutput"]!=result["normalizedOutput"]
        || ledger::hash(&manifest["receipt"])!=result["adoption"]["receiptSha256"]
        || ledger::hash(&json!({"receipt":manifest["receipt"],"normalizedOutput":result["normalizedOutput"],"manifest":result["manifest"]}))!=result["verificationSha256"]
        || payload["binding"]["companyId"]!=result["companyId"]
        || payload["binding"]["verifiedFile"]!=result["verifiedFile"]
        || payload["binding"]["specSha256"]!=result["specSha256"]
        || manifest["receipt"]["originalKnowledge"]!=result["adoption"]["originalKnowledge"]
        || manifest["receipt"]["donor"]!=result["adoption"]["donor"]
        || payload["audio"]["coverage"]!=result["coverage"] || payload["audio"]["outcome"]!=result["outcome"] {
        return Err("media_analysis_legacy_output_binding_changed".into());
    }
    Ok(payload["audio"].clone())
}
fn legacy_paid_exists(d:&Value,request:&Value)->bool {
    ["materials","knowledge_versions"].iter().any(|collection| {
        crate::list(d,collection).iter().any(|m|m["kind"]=="transcript" && m["mediaSha256"]==request["verifiedFile"]["sha256"]
            && (m["account"]==request["companyId"] || m["scope"]["account"]==request["companyId"]))
    })
}
impl Runtime {
    pub(crate) async fn bind(app:&App,id:&str,progress:Option<&Value>)->Result<Self,String> {
        // A pre-ledger legacy worker has no retained-file/current-alias witness.
        // It must acquire the V2 checkpoint before it can reserve paid ASR.
        let progress=progress.ok_or("media_analysis_legacy_checkpoint_required")?;
        let lifecycle=app.lifecycle_admission_token(AdmissionClass::Media).await.map_err(error)?;
        let d=app.read().await.map_err(error)?;
        if Profile::from_workspace(&d).map_err(error)?!=app.account {return Err("media_analysis_company_changed".into());}
        let execution_pin=pin(crate::row(&d,"jobs",id).map_err(error)?)?;
        if execution_pin["progress"]!=*progress {return Err("media_analysis_execution_progress_changed".into());}
        current_alias(&d,progress).map_err(error)?;
        crate::media_fullframes::reference(&progress["source"])?;
        let attempt_id=crate::id();
        let mut binding=json!({"companyId":d["account"],"executionJobId":id,"attemptId":attempt_id,
            "owner":format!("media-asr:{id}:{attempt_id}"),"epoch":progress["leaseEpoch"].as_u64().unwrap_or(0).max(1),
            "manifestKey":format!("media-asr:{attempt_id}"),"sourceVersion":progress["sourceVersion"],
            "originalAlias":{"account":progress["account"],"connectorBinding":progress["connectorBinding"],
                "sourcePostId":progress["sourcePostId"],"sourcePostKey":progress["sourcePostKey"],
                "sourceVersion":progress["sourceVersion"],"sourceProjection":progress["sourceProjection"]}});
        if let Some(pin)=progress.get("assetPin"){binding["originalAlias"]["assetPin"]=pin.clone();}
        Ok(Self {app:app.clone(),execution_job_id:id.into(),progress:progress.clone(),execution_pin,binding,lifecycle})
    }
    fn base_binding(&self,request:&Value)->Result<(),String> {
        for key in ["companyId","executionJobId","attemptId","owner","epoch","manifestKey","sourceVersion","originalAlias"] {
            if request[key]!=self.binding[key] {return Err("media_analysis_runtime_binding_changed".into());}
        }
        Ok(())
    }
    fn request_binding(&self,request:&Value)->Result<(),String> {
        self.base_binding(request)?;
        if request["verifiedFile"]["sha256"]!=self.progress["source"]["sha256"]
            || request["verifiedFile"]["bytes"]!=self.progress["source"]["bytes"] {
            return Err("media_analysis_retained_source_changed".into());
        }
        Ok(())
    }
    fn execution_current(&self,d:&Value)->ApiResult<()> {
        if Profile::from_workspace(d)?!=self.app.account || d["account"]!=self.binding["companyId"] {
            return Err(crate::conflict("media_analysis_company_changed"));
        }
        let job=crate::row(d,"jobs",&self.execution_job_id)?;
        if pin(job).map_err(|error| crate::conflict(&error))?!=self.execution_pin {return Err(crate::conflict("media_analysis_execution_fence_changed"));}
        current_alias(d,&self.progress)
    }
    /// Authorize a subsequent OCR stage using the originally captured token.
    /// This is an admission check, not a refreshed owner or a paid reservation.
    pub(crate) async fn permit_followup(&self)->Result<(),String> {
        let scope=crate::storage::MediaValidationScope{job:&self.execution_job_id,
            post:self.progress["sourcePostId"].as_str().ok_or("media_analysis_alias_missing")?,
            mode:crate::storage::MediaValidationMode::Execution};
        self.app.check_media_validation(scope,|d| {
            runtime_lifecycle::require_admission(d,&self.lifecycle,AdmissionClass::Media)?;
            self.execution_current(d)
        }).await.map_err(error)
    }
    // Retained-result validation admits no new stage: the current fixed owner
    // may read it during drain, just as before. OCR still uses the original token.
    async fn validate_reused_output(&self,request:&Value,expected:&Value,audio:&Value,at:&str)->Result<(),String> {
        let scope=crate::storage::MediaValidationScope{job:&self.execution_job_id,
            post:self.progress["sourcePostId"].as_str().ok_or("media_analysis_alias_missing")?,
            mode:crate::storage::MediaValidationMode::Reuse{legacy_catalog:expected["sourceKind"]=="legacy_adopted"}};
        self.app.check_media_validation(scope,|d| {
            self.execution_current(d)?;
            let state=ledger::ledger_from_workspace(d).map_err(|error|crate::conflict(&error))?;
            if ledger::read_result(&state,request).map_err(|error|crate::conflict(&error))?["result"]!=*expected {
                return Err(crate::conflict("media_analysis_result_changed"));
            }
            if expected["sourceKind"]=="legacy_adopted" {
                let selected=crate::media_analysis_reuse::select_catalog_adoption(d,&request["verifiedFile"],at).map_err(|error|crate::conflict(&error))?
                    .ok_or_else(||crate::conflict("media_analysis_legacy_donor_unavailable"))?;
                if selected["donor"]!=expected["adoption"]["donor"] || selected["material"]!=audio["materials"][0] {
                    return Err(crate::conflict("media_analysis_legacy_donor_changed"));
                }
            }
            Ok(())
        }).await.map_err(error)
    }
    async fn verify_input(&self,request:&Value)->Result<(),String> {
        self.request_binding(request)?;
        let reference=self.progress["source"].clone();let request=request.clone();
        tokio::task::spawn_blocking(move|| {
            let store=crate::media_fullframes::store()?;
            store.verify(&crate::media_fullframes::reference(&reference)?).map_err(|_|"media_analysis_retained_source_unavailable".to_owned())?;
            if !request["verifiedReceipt"].is_object() || !request["probeReceipt"].is_object()
                || request["verifiedReceipt"]["method"]!="local_sha256_and_size" || request["verifiedReceipt"]["source"]!=reference
                || request["probeReceipt"]["method"]!="ffprobe_selected_audio_stream_inventory"
                || ledger::hash(&request["verifiedReceipt"])!=request["verifiedFile"]["receiptSha256"]
                || ledger::hash(&request["probeReceipt"])!=request["verifiedFile"]["probeSha256"] {
                return Err("media_analysis_input_receipt_invalid".into());
            }
            let seconds=request["probeReceipt"]["mediaDurationSeconds"].as_f64().filter(|n|n.is_finite()&&*n>0.0)
                .ok_or("media_analysis_probe_duration_invalid")?;
            if request["durationMs"].as_u64()!=Some((seconds*1000.0).round() as u64)
                || request["probeReceipt"]["hasAudio"].as_bool().is_none()
                || (request["noAudio"]==true && (request["probeReceipt"]["hasAudio"]!=false
                    || request["noAudioVerificationSha256"]!=request["verifiedFile"]["probeSha256"])) {
                return Err("media_analysis_probe_binding_invalid".into());
            }
            Ok(())
        }).await.map_err(|_|"media_analysis_input_verification_stopped".to_owned())?
    }
    async fn reconcile_declared(&self,request:&Value,selected:&Value)->Result<Option<Value>,String> {
        if !matches!(selected["disposition"].as_str(),Some("owned"|"unknown")) {return Ok(None);}
        if selected["disposition"]=="owned" && selected["attempt"]["attemptId"]==request["attemptId"] {return Ok(None);}
        let original=selected["attempt"]["originalRequest"].clone();
        // Never inspect arbitrary donor files or retarget original output slots.
        if original["companyId"]!=request["companyId"]
            || original["verifiedFile"]["sha256"]!=request["verifiedFile"]["sha256"]
            || original["verifiedFile"]["bytes"]!=request["verifiedFile"]["bytes"]
            || original["verifiedFile"]["probeSha256"]!=request["verifiedFile"]["probeSha256"] {
            return Err("media_analysis_reconciliation_binding_invalid".into());
        }
        let checked=original.clone();
        let closure=tokio::task::spawn_blocking(move|| {
            let store=crate::media_fullframes::store()?;
            let mut segments=Vec::new();
            for index in 0..checked["segments"].as_array().ok_or("media_analysis_segment_plan_invalid")?.len() {
                match output::reconcile_segment(&store,&checked,index) {
                    Ok(segment)=>segments.push(segment),
                    Err(_)=>break,
                }
            }
            // Missing/torn closures preserve uncertainty. They never dispatch.
            let full=output::reconcile_full(&store,&checked).ok();
            if segments.is_empty()&&full.is_none() {return Ok::<_,String>(None);}
            Ok(Some((segments,full)))
        }).await.map_err(|_|"media_analysis_output_reconciliation_stopped".to_owned())??;
        let Some((segments,full))=closure else {return Ok(None);};
        let expected=selected["attempt"].clone();
        self.app.change(|d| {
            if d["account"]!=original["companyId"] {return Err(crate::conflict("media_analysis_company_changed"));}
            let mut state=ledger::ledger_from_workspace(d).map_err(|error| crate::conflict(&error))?;
            let current=ledger::read_result(&state,request).map_err(|error| crate::conflict(&error))?;
            if current["attempt"]!=expected {return Err(crate::conflict("media_analysis_reconciliation_fence_changed"));}
            for segment in segments {
                let mut capture=original.clone();capture["segment"]=segment;
                ledger::commit_segment(&mut state,&capture).map_err(|error| crate::conflict(&error))?;
            }
            if let Some(full)=full {
                let mut capture=original.clone();capture["result"]=full;
                ledger::commit_full_result(&mut state,&capture).map_err(|error| crate::conflict(&error))?;
            }
            ledger::put_ledger(d,&state).map_err(|error| crate::conflict(&error))?;
            ledger::read_result(&state,request).map_err(|error| crate::conflict(&error))
        }).await.map(Some).map_err(error)
    }
    async fn reserve_inner(&self,request:Value)->Result<Value,String> {
        self.verify_input(&request).await?;
        let snapshot=self.app.read().await.map_err(error)?;
        let at=crate::now();
        let state=ledger::ledger_from_workspace(&snapshot)?;
        let absent=ledger::read_result(&state,&request)?["disposition"]=="absent";
        let donor=if absent {crate::media_analysis_reuse::select_catalog_adoption(&snapshot,&request["verifiedFile"],&at)?} else {None};
        if absent && donor.is_none() && legacy_paid_exists(&snapshot,&request) {
            return Err("media_analysis_legacy_paid_output_reconciliation_required".into());
        }
        drop(snapshot);
        let adoption=if let Some(selected)=&donor {
            if selected["coverage"]["durationMs"].as_u64().zip(request["durationMs"].as_u64())
                .is_none_or(|(original,current)|original.abs_diff(current)>250)
                || (selected["outcome"]=="no_audio")!=(request["probeReceipt"]["hasAudio"]==false) {
                return Err("media_analysis_legacy_probe_coverage_conflict".into());
            }
            let selected=selected.clone();let request=request.clone();
            Some(tokio::task::spawn_blocking(move|| {
                let store=crate::media_fullframes::store()?;
                let policy="verified_legacy_normalized_full_audio";
                let authority=ledger::hash(&json!({"policy":policy,"companyId":request["companyId"],"verifiedFile":request["verifiedFile"],
                    "donor":selected["donor"],"provenance":selected["provenance"]}));
                let compatibility=json!({"specSha256":request["specSha256"],"authorityReceiptSha256":authority,"policy":policy});
                let receipt=json!({"companyId":request["companyId"],"verifiedFile":request["verifiedFile"],
                    "originalKnowledge":selected["donor"],"donor":selected["donor"],"provenance":selected["provenance"],
                    "coverage":selected["coverage"],"outcome":selected["outcome"],"compatibility":compatibility});
                let audio=json!({"materials":[selected["material"]],"reused":true,"coverage":selected["coverage"],"outcome":selected["outcome"]});
                let payload=json!({"schemaVersion":1,"binding":{"companyId":request["companyId"],"verifiedFile":request["verifiedFile"],"specSha256":request["specSha256"]},"audio":audio});
                let normalized=store.put_bytes(payload.to_string().as_bytes()).map_err(|_|"media_analysis_legacy_capture_failed")?.to_json();
                let manifest=store.put_bytes(json!({"schemaVersion":1,"receipt":receipt,"originalKnowledge":selected["donor"],"donor":selected["donor"],"normalizedOutput":normalized}).to_string().as_bytes())
                    .map_err(|_|"media_analysis_legacy_capture_failed")?.to_json();
                let verification=ledger::hash(&json!({"receipt":receipt,"normalizedOutput":normalized,"manifest":manifest}));
                let mut adoption=request.clone();adoption["normalizedOutput"]=normalized;adoption["manifest"]=manifest;
                adoption["adoption"]=json!({"kind":"verified_legacy_catalog","rawOutputsAbsent":true,"donor":selected["donor"],
                    "originalKnowledge":selected["donor"],"provenance":selected["provenance"],"coverage":selected["coverage"],"outcome":selected["outcome"],
                    "compatibility":compatibility,"receiptSha256":ledger::hash(&receipt),"verificationSha256":verification});
                Ok::<_,String>(adoption)
            }).await.map_err(|_|"media_analysis_legacy_capture_stopped".to_owned())??)
        } else {None};
        let mut view=self.app.change(|d| {
            runtime_lifecycle::require_admission(d,&self.lifecycle,AdmissionClass::Media)?;
            self.execution_current(d)?;
            legacy_quarantine(d,&self.execution_job_id,&request["verifiedFile"]).map_err(|error| crate::conflict(&error))?;
            let mut state=ledger::ledger_from_workspace(d).map_err(|error| crate::conflict(&error))?;
            if let Some(adoption)=&adoption {
                if crate::media_analysis_reuse::select_catalog_adoption(d,&request["verifiedFile"],&at).map_err(|error| crate::conflict(&error))?!=donor {
                    return Err(crate::conflict("media_analysis_legacy_donor_changed"));
                }
                ledger::adopt_completed(&mut state,adoption).map_err(|error| crate::conflict(&error))?;
            }
            if adoption.is_none() && ledger::read_result(&state,&request).map_err(|error| crate::conflict(&error))?["disposition"]=="absent"
                && legacy_paid_exists(d,&request) {
                return Err(crate::conflict("media_analysis_legacy_paid_output_reconciliation_required"));
            }
            let view=ledger::reserve(&mut state,&request).map_err(|error| crate::conflict(&error))?;
            ledger::put_ledger(d,&state).map_err(|error| crate::conflict(&error))?;
            if view["disposition"]=="reserved" {
                crate::row_mut(d,"jobs",&self.execution_job_id)?["mediaAnalysisBinding"]=json!({
                    "key":view["key"],"attemptId":request["attemptId"],"owner":request["owner"],"epoch":request["epoch"]});
            }
            Ok(view)
        }).await.map_err(error)?;
        if let Some(reconciled)=self.reconcile_declared(&request,&view).await? {view=reconciled;}
        if view["disposition"]=="owned" && view["attempt"]["attemptId"]!=request["attemptId"] {
            // A newly bound worker is never the original owner, even when its
            // alias/job/spec happens to match after a process restart.
            view["disposition"]=json!("held");view["reason"]=json!("media_analysis_existing_owner_reconciliation_required");
        }
        let original=view["result"]["originalRequest"].as_object().map(|_|view["result"]["originalRequest"].clone())
            .unwrap_or_else(||view["attempt"]["originalRequest"].clone());
        view["originalRequest"]=original.clone();
        view["completedSegments"]=view["attempt"]["segments"].clone();
        if view["disposition"]=="reuse" {
            let expected=view["result"].clone();let source=self.progress["source"].clone();let result=expected.clone();
            let audio=tokio::task::spawn_blocking(move|| {
                let store=crate::media_fullframes::store()?;
                store.verify(&crate::media_fullframes::reference(&source)?).map_err(|_|"media_analysis_retained_source_unavailable".to_owned())?;
                if result["sourceKind"]=="legacy_adopted" {read_adopted(&store,&result)}else{output::read_full(&store,&original,&result)}
            }).await.map_err(|_|"media_analysis_output_verification_stopped".to_owned())??;
            // CAS readback does not replace final database/current-alias checks.
            self.validate_reused_output(&request,&expected,&audio,&at).await?;
            view["audio"]=audio;
        }
        Ok(view)
    }
    async fn event_inner(&self,event:&'static str,request:Value)->Result<(),String> {
        if event=="permit_ocr" {
            // Exact cached catalog reuse has no new ASR plan/file request. The
            // immutable base binding and persisted retained source suffice.
            self.base_binding(&request)?;
            return self.permit_followup().await;
        }
        self.request_binding(&request)?;
        if matches!(event,"commit_segment"|"commit_full_result") {
            let captured=request.clone();
            tokio::task::spawn_blocking(move|| {
                let store=crate::media_fullframes::store()?;
                if event=="commit_segment" {output::read_segment(&store,&captured,&captured["segment"]).map(|_|())}
                else {output::read_full(&store,&captured,&captured["result"]).map(|_|())}
            }).await.map_err(|_|"media_analysis_output_verification_stopped".to_owned())??;
        }
        self.app.change(|d| {
            if Profile::from_workspace(d)?!=self.app.account || d["account"]!=request["companyId"] {
                return Err(crate::conflict("media_analysis_company_changed"));
            }
            if event=="mark_dispatched" {
                runtime_lifecycle::require_admission(d,&self.lifecycle,AdmissionClass::Media)?;
                self.execution_current(d)?;
            }
            let mut state=ledger::ledger_from_workspace(d).map_err(|error| crate::conflict(&error))?;
            let outcome=match event {
                "mark_dispatched"=>ledger::mark_dispatched(&mut state,&request),
                "commit_segment"=>ledger::commit_segment(&mut state,&request),
                "commit_full_result"=>ledger::commit_full_result(&mut state,&request),
                "fail"=>ledger::fail(&mut state,&request),
                _=>Err("media_analysis_event_invalid".into()),
            }.map_err(|error| crate::conflict(&error))?;
            if event=="mark_dispatched" && outcome["disposition"]!="dispatch_reserved" {
                return Err(crate::conflict("media_analysis_segment_already_dispatched"));
            }
            ledger::put_ledger(d,&state).map_err(|error| crate::conflict(&error))?;
            Ok(())
        }).await.map_err(error)
    }
}
impl AudioLifecycle for Runtime {
    fn binding(&self)->Value {self.binding.clone()}
    fn reserve(&self,request:Value)->LifecycleFuture<'_,Value> {Box::pin(self.reserve_inner(request))}
    fn event(&self,event:&'static str,request:Value)->LifecycleFuture<'_,()> {Box::pin(self.event_inner(event,request))}
}

#[cfg(test)]
#[path="media_analysis_runtime_tests.rs"]
mod tests;
