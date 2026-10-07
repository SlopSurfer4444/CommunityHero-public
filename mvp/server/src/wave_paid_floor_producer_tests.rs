//! Explicit floor test member, native PG/CAS writes, finite pre-crash barrier.
//! Synthetic response only. Never starts provider/model/FFmpeg or admits result.
use crate::*;
use crate::runtime_lifecycle_startup::fixture_producer::{self,VerifiedFixtureProducer};

fn synthetic_content_response(input:&Value)->ApiResult<(Value,Value)> {
    fn exact(v:&Value,keys:&[&str])->bool {
        v.as_object().is_some_and(|o|o.len()==keys.len()&&keys.iter().all(|key|o.contains_key(*key)))
    }
    if !exact(input,&["kind","classification","content"])
        ||input["kind"]!="synthetic-native-paid-response-content"
        ||input["classification"]!="SYNTHETIC-NATIVE-CONTENT"
        ||!exact(&input["content"],&["text","sources","assessments","proposals"])
    {return Err(bad("Closed synthetic native paid content required"));}
    let content=&input["content"];
    if content["text"].as_str().is_none_or(|text|text.trim().is_empty())
        ||content["sources"].as_array().is_none_or(|a|a.len()>64)
        ||content["assessments"].as_array().is_none_or(|a|a.len()!=1||a.iter().any(|v|!v.is_object()))
        ||content["proposals"].as_array().is_none_or(|a|a.len()!=1||a.iter().any(|v|!v.is_object()||v["text"].as_str().is_none_or(|text|text.trim().is_empty())))
    {return Err(bad("Bounded synthetic paid answer content required"));}
    let response=engine_prepare::tests::single_pass_result(content.clone());
    let mut restored=response.clone();
    for key in ["runMetadata","editorialEvidence","factDependencies"] {restored.as_object_mut().unwrap().remove(key);}
    if restored!=*content {return Err(conflict("Native synthetic content transformation changed answer"));}
    Ok((content.clone(),response))
}

async fn emit()->ApiResult<()> {
    let verified=fixture_producer::verified_fixture_producer_from_environment()?;
    verified.require_selector(fixture_producer::SELECTOR)?;
    let url=std::env::var("COMMUNITYHERO_DATABASE_URL").map_err(|_|conflict("Producer isolated database environment required"))?;
    verified.guard_existing_database(&url).await?;
    // The durable unresolved attempt comes before lease acquisition/scheduling;
    // a failed lease or timeout requires reconciliation of THIS attempt.
    verified.mark_attempt()?;
    let db=Database::postgres(&url).await.map_err(|_|conflict("Producer sole existing PG writer lease unavailable"))?;
    let before=db.read().await?;let token=match verified.require_workspace(&before){Ok(token)=>token,Err(error)=>{db.close().await;return Err(error);}};
    if list(&before,"jobs").iter().any(|j|matches!(j["status"].as_str(),Some("queued"|"running"))) {
        db.close().await;return Err(conflict("Prior fixture work is not quiescent"));
    }
    let pre_inventory=verified.data_root.join(format!("{}.native-before.json",verified.record["attemptId"].as_str().unwrap()));
    fixture_producer::write_new(&pre_inventory,&before)?;
    let VerifiedFixtureProducer{admission,record,admission_pin,response,data_root,cas_root,barrier,timeout_ms}=verified;
    let admission=Arc::new(admission);let profile=accounts::Profile::BawRussia;let(events,_)=broadcast::channel(8);
    let app=App{lifecycle_task_count:Default::default(),lifecycle_owner:Arc::new(admission.identity().clone()),lifecycle_admission:admission,
        lifecycle_provider_token:Default::default(),lifecycle_work:Default::default(),media_discovery:Default::default(),preparation_wake:Default::default(),provider_session:Default::default(),
        account:profile,navigation:account_navigation::Navigation::root(),db,gate:Arc::new(writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),
        preparation_workers:Default::default(),editorial_gate:Default::default(),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),
        events,csrf:"isolated-paid-producer".into(),auth:None,public_origin:None,external_writes:false,port:0,data:data_root.clone(),
        bridge:data_root.join("PROVIDER_MODEL_EXECUTION_FORBIDDEN.mjs"),node:data_root.join("MODEL_EXECUTION_FORBIDDEN.exe"),
        tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
    if app.node.exists()||app.bridge.exists(){return Err(conflict("Producer cannot have an executable bridge"));}
    let owned_task=runtime_lifecycle_app::TaskCount::begin(app.lifecycle_task_count.clone());
    let producer_store=media_fullframes::store().map_err(|_|conflict("Pinned producer CAS unavailable"))?;
    if producer_store.root()!=cas_root {app.db.close().await;return Err(conflict("Producer CAS root changed"));}
    let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(timeout_ms);
    let result=tokio::time::timeout_at(deadline,photo_acquisition::with_fixture_store(&producer_store,async {
        let item=record["fixture"]["itemId"].as_str().ok_or_else(||bad("Exact fixture item required"))?.to_owned();
        let scheduled=app.change_preparation_schedule(|d| {
            // Recheck pre-ledger while locked, immediately before FIRST write.
            if runtime_lifecycle::ledger_digest(d)?!=record["preLedgerSha256"].as_str().unwrap() {return Err(conflict("Producer pre-attempt ledger drift"));}
            runtime_lifecycle::require_admission(d,&token,runtime_lifecycle::AdmissionClass::Preparation)?;
            let scheduled=engine_prepare::schedule(d,engine_prepare::Input{item_ids:vec![item.clone()],instruction:None})?;
            preparation_review::record_initial_admission(d,&token,&scheduled.job_id,&now())?;Ok(scheduled)
        }).await?;
        let request=scheduled.request().ok_or_else(||conflict("Native fixture item is not ready"))?.clone();let job_id=&scheduled.job_id;
        app.change_preparation_first(job_id,|d|preparation_review::reserve_first_admitted(d,&token,job_id,&request,&now())).await?;
        let mut wire=request.clone();profile.bind_request(&mut wire)?;wire["operation"]=json!("assistant");
        let (content,mut response)=synthetic_content_response(&response)?;
        let envelope_before=response.clone();
        super::floor_characterization::material_invocation(&wire,&mut response);
        let mut restored=response.clone();
        restored["runMetadata"].as_object_mut().unwrap().remove("materialInvocation");
        if restored!=envelope_before {return Err(conflict("Derived native material metadata changed response content"));}
        model_material_receipt::validate_result(&wire,&response).map_err(bad)?;
        // This counter denotes ONE source-bound fixture payload emission, never
        // a real paid/model API invocation or cost observation.
        let emission=data_root.join(format!("{}.emission.json",record["attemptId"].as_str().unwrap()));
        fixture_producer::write_new(&emission,&json!({"kind":"synthetic-native-paid-response-emission","classification":"SYNTHETIC-PAID-RESPONSE",
            "attemptId":record["attemptId"],"count":1,"inputResponse":record["fixture"]["response"],
            "inputContent":content,"nativeSyntheticEnvelope":envelope_before,"response":response,
            "transformation":"pinned-content-to-native-synthetic-envelope-to-current-wire-material-metadata",
            "responseSha256":media_fullframes::hash(&response),"realModelEffects":0,"actualModelCliInvocation":false}))?;
        let paid=runtime_lifecycle_app::with_job(job_id.clone(),runtime_paid_result::retain(&app,"assistant",&wire,&response)).await?
            .ok_or_else(||conflict("Native paid retention required"))?;
        app.change_job(job_id,|d|runtime_paid_result::attach(d,job_id,&paid,&app.lifecycle_owner)).await?;
        let material=model_material_receipt::retain(&app,&wire,&response,&paid).await?.ok_or_else(||conflict("Native mandatory-material retention required"))?;
        app.change_job(job_id,|d|model_material_receipt::attach(d,job_id,&material)).await?;
        // Read through the real independent read pool after both commits. Root
        // performs its own external PG/CAS assertion BEFORE owned crash too.
        let persisted=app.db.read().await?;let job=row(&persisted,"jobs",job_id)?;
        if job["status"]!="running"||!job["preparationStages"]["first"].is_null()||!job["prepareOutcome"].is_null()||!job["result"].is_null()
            ||job["preparationStages"]["firstAdmission"]["status"]!="reserved"||job["preparationStages"]["firstAdmission"]["owner"]!=record["fixedOwner"]
            ||job["retainedEvidence"]!=json!([paid.clone()])||job["modelMaterialReceipts"]!=json!([material.clone()])
            ||list(&persisted,"proposals").iter().any(|p|p["prepareRunId"].as_str()==Some(job_id.as_str())){return Err(conflict("Native pre-crash boundary is not incomplete"));}
        let capture=runtime_paid_result::resolve(&app,job_id,"assistant",&paid).await?;
        if capture["request"]!=wire||capture["response"]!=response{return Err(conflict("Native paid reconstruction changed"));}
        let store=media_fullframes::store().map_err(|_|conflict("Producer pinned CAS unavailable"))?;
        if store.root()!=cas_root{return Err(conflict("Producer effective CAS root changed"));}
        let material_bytes=media_fullframes::read(&store,&material["artifact"]).map_err(|_|conflict("Native material CAS readback failed"))?;
        if material_bytes["request"]!=wire||material_bytes["body"]!=material["body"]||material_bytes["paidResultRef"]!=paid{return Err(conflict("Native material bytes changed"));}
        let readback=data_root.join(format!("{}.native-prestop.json",record["attemptId"].as_str().unwrap()));fixture_producer::write_new(&readback,&persisted)?;
        let locator=json!({"schemaVersion":1,"kind":"native-fixture-paid-precrash-barrier-locator","attemptId":record["attemptId"],"runId":record["runId"],
            "admission":admission_pin,"jobId":job_id,"pid":std::process::id(),"readbackPath":readback,"syntheticEmissions":1,
            "requestDigest":job["prepareBundle"]["digest"],"paidReference":paid,"materialPointer":material});
        fixture_producer::write_new(&barrier,&locator)?;
        println!("\nWAVE_PAID_PRECRASH_LOCATOR={}",locator);
        use std::io::Write;std::io::stdout().flush().map_err(|_|internal("Producer locator flush failed"))?;
        // No release file/timeout converts this deliberately incomplete native
        // state into a pass. Root must prove boundary, then kill ONLY this owner.
        std::future::pending::<ApiResult<()>>().await
    })).await;
    drop(owned_task);app.db.close().await;
    match result{Ok(result)=>result,Err(_)=>Err(conflict("Producer barrier deadline: failed unresolved attempt; no retry authority"))}
}

#[tokio::test]
#[ignore="ROOT original floor libtest role, SAME isolated populated BAW PG/CAS, expected controlled crash only"]
async fn emit_owner_bound_paid_precrash(){
    if let Err(error)=emit().await {panic!("owner-bound paid producer failed: {}",error.1);}
    panic!("Producer must not return a fabricated ordinary libtest pass");
}


#[test]
fn paid_synthetic_content_is_closed_and_native_enrichment_preserves_static_answer() {
    let content=json!({"text":"Synthetic local floor reply","sources":[],
        "assessments":[{"itemId":"paid-item","intent":"reply"}],
        "proposals":[{"itemId":"paid-item","kind":"reply_and_close","text":"Synthetic local floor reply"}]});
    let input=json!({"kind":"synthetic-native-paid-response-content","classification":"SYNTHETIC-NATIVE-CONTENT","content":content});
    let (original,response)=synthetic_content_response(&input).unwrap();
    assert_eq!(original,content);
    assert!(response["runMetadata"].is_object());assert!(response["editorialEvidence"].is_object());
    assert_eq!(input["content"],content);
    for case in ["extra","metadata","receipt","kind","text","proposals"] {
        let mut changed=input.clone();
        match case {
            "extra"=>changed["sourceReceipt"]=json!({}),
            "metadata"=>changed["content"]["runMetadata"]=json!({}),
            "receipt"=>changed["content"]["modelMaterialReceipt"]=json!({}),
            "kind"=>changed["classification"]=json!("ACTUAL-MODEL-RESULT"),
            "text"=>changed["content"]["text"]=json!(""),
            _=>changed["content"]["proposals"][0]["text"]=Value::Null,
        }
        assert!(synthetic_content_response(&changed).is_err(),"{case}");
    }
}
