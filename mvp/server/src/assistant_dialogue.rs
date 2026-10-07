//! Bounded conversational application-tool loop with durable operator receipts.
//! External requests use only the separately reviewed, operator-bound action flow.
use crate::{App,ApiResult,assistant_context,bad,conflict,json,row,row_mut,prepare_bundle};
use serde_json::Value;
use sha2::{Digest,Sha256};

fn seal(bundle:&mut Value)->ApiResult<()> {
    let input=bundle["request"].to_string();
    if input.len()>550_000{return Err(bad("Контекст найденных комментариев слишком большой. Уточните запрос"));}
    bundle["digest"]=json!(format!("{:x}",Sha256::digest(input.as_bytes())));Ok(())
}
/// Mixed-post conversation is useful for navigation and discussion, but it has
/// no public-proposal authority. A proposal-producing conversation uses the
/// exact same native post/family and mandatory-material capture as preparation.
pub(crate) fn attach_proposal_policy(d:&Value,bundle:&mut Value)->ApiResult<()> {
    bundle["request"]["purpose"]=json!("discussion");
    let ids=bundle["itemIds"].as_array().ok_or_else(||bad("Discussion recipients missing"))?;
    let strict=!ids.is_empty()&&crate::preparation_unit::capture(d,ids,&crate::now()).is_ok();
    bundle["request"]["discussionProposalMode"]=json!(if strict{"strict_group_v1"}else{"read_only_v1"});
    if strict {
        crate::preparation_unit::attach(d,bundle,&crate::now()).map_err(conflict)?;
        crate::preparation_materials::attach_request(d,&mut bundle["request"]).map_err(conflict)?;
    }else{
        bundle["request"].as_object_mut().unwrap().remove("strictGroup");
        bundle["request"].as_object_mut().unwrap().remove("strictGroupContract");
        bundle["request"]["publicProposalInstruction"]=json!("This is a read-only discussion across posts. Return no public proposals. Use the strict preparation plan to prepare public replies separately for each post or proven family.");
    }
    seal(bundle)
}
fn current_proposal_policy(d:&Value,bundle:&Value)->ApiResult<()> {
    match bundle["request"]["discussionProposalMode"].as_str(){
        Some("read_only_v1")=>Ok(()),
        Some("strict_group_v1")=>{
            crate::preparation_unit::current_bundle(d,bundle,&crate::now()).map_err(conflict)?;
            crate::preparation_materials::require_request(d,&bundle["request"]).map_err(conflict)
        },
        _=>Err(conflict("Discussion proposal policy missing or unsupported"))
    }
}
pub fn lookup_query(result:&Value)->ApiResult<Option<&str>>{
    if result["lookup"].is_null(){return Ok(None);}
    let q=result["lookup"]["query"].as_str().unwrap_or("").trim();
    if result["lookup"]["kind"]!="search_comments"||q.chars().count()<2||q.chars().count()>300
        ||result["proposals"].as_array().is_none_or(|p|!p.is_empty()){
        return Err(bad("Invalid assistant lookup request"));
    }
    Ok(Some(q))
}
fn retrieved_bundle(d:&Value,old:&Value,messages:&[Value],query:&str)->ApiResult<(Value,Value)>{
    prepare_bundle::current(d,old).map_err(conflict)?;
    let found=assistant_context::search(d,query,8).map_err(bad)?;
    let mut ids=old["itemIds"].as_array().cloned().unwrap_or_default();
    for item in found["items"].as_array().unwrap(){if !ids.contains(&item["id"]){ids.push(item["id"].clone());}}
    let mut next=prepare_bundle::build(d,&ids,messages).map_err(bad)?;
    if old["request"]["screen"].is_object(){assistant_context::attach_screen(&mut next,old["request"]["screen"].clone()).map_err(bad)?;}
    for shown in old["request"]["items"].as_array().into_iter().flatten().filter(|i|i["draftContext"].is_object()){
        assistant_context::attach_displayed_draft(d,&mut next,&json!({"itemId":shown["id"],"text":shown["draft"],
            "proposalId":shown["draftContext"]["proposalId"],"proposalRevision":shown["draftContext"]["proposalRevision"]})).map_err(bad)?;
    }
    next["request"]["lookupAllowed"]=json!(false);
    next["request"]["lookupResults"]=found.clone();attach_proposal_policy(d,&mut next)?;
    Ok((next,found))
}

fn refreshed_bundle(d:&Value,old:&Value,messages:&[Value],results:&[Value])->ApiResult<Value>{
    let mut ids=old["itemIds"].as_array().cloned().unwrap_or_default();
    // Discovery and navigation need only bounded summaries. Loading every search
    // hit here would make one oversized branch break a valid workspace search.
    // Mutation/proposal authority still requires explicit full read admission.
    for receipt in results.iter().filter(|r|r["ok"]==true&&r["name"]=="read_comments"){
        for item in receipt["result"]["items"].as_array().into_iter().flatten(){
            if item["id"].is_string()&&!ids.contains(&item["id"]){ids.push(item["id"].clone());}
        }
    }
    let mut next=prepare_bundle::build(d,&ids,messages).map_err(bad)?;
    if old["request"]["screen"].is_object(){assistant_context::attach_screen(&mut next,old["request"]["screen"].clone()).map_err(bad)?;}
    for shown in old["request"]["items"].as_array().into_iter().flatten().filter(|i|i["draftContext"].is_object()){
        assistant_context::attach_displayed_draft(d,&mut next,&json!({"itemId":shown["id"],"text":shown["draft"],"proposalId":shown["draftContext"]["proposalId"],"proposalRevision":shown["draftContext"]["proposalRevision"]})).map_err(bad)?;
    }
    next["request"]["toolResults"]=json!(results);attach_proposal_policy(d,&mut next)?;Ok(next)
}
fn finish(d:&mut Value,job_id:&str,conversation_id:&str,result:&Value)->ApiResult<Value>{
    crate::assistant_tools::check_owner(d,job_id,conversation_id)?;
    let job=row(d,"jobs",job_id)?.clone();
    if result["proposals"].as_array().is_some_and(|rows|!rows.is_empty()) {
        if job["prepareBundle"]["request"]["discussionProposalMode"]!="strict_group_v1" {
            return Err(conflict("Read-only discussion cannot admit public proposals; use strict preparation groups"));
        }
        current_proposal_policy(d,&job["prepareBundle"])?;
    }
    let outcome=prepare_bundle::admit(d,job_id,conversation_id,result)?;
    annotate_finish(d,&job,job_id,conversation_id)?;Ok(outcome)
}
fn finish_native(d:&mut Value,job_id:&str,conversation_id:&str,text:&str,reason:&str)->ApiResult<Value>{
    crate::assistant_tools::check_owner(d,job_id,conversation_id)?;
    let job=row(d,"jobs",job_id)?.clone();
    if job["purpose"]!="discussion"{return Err(conflict("Native terminal discussion ownership changed"));}
    let outcome=prepare_bundle::admit_native_terminal(d,job_id,conversation_id,text,reason)?;
    annotate_finish(d,&job,job_id,conversation_id)?;Ok(outcome)
}
fn annotate_finish(d:&mut Value,job:&Value,job_id:&str,conversation_id:&str)->ApiResult<()> {
    let messages=row_mut(d,"conversations",conversation_id)?["messages"].as_array_mut().unwrap();
    if let Some(message)=messages.last_mut().filter(|m|m["prepareRunId"]==job_id){
        if job["toolResults"].is_array(){message["toolResults"]=job["toolResults"].clone();}
        if let Some(nav)=job["toolResults"].as_array().into_iter().flatten().rev().find(|r|r["name"]=="navigate"&&r["ok"]==true){message["navigation"]=nav["result"].clone();}
        if job["lookupResults"].is_object(){message["lookupResults"]=job["lookupResults"].clone();}
    }Ok(())
}

async fn current_authority(app:&App,actor:&crate::operator_auth::Actor)->ApiResult<()> {
    let binding=crate::dispatch_authority::approval_binding(actor);
    crate::dispatch_authority::check(app,&json!({"dispatchAuthority":{"approved":binding,"executed":binding},"approvedBy":actor.public_json(),"executedBy":actor.public_json()})).await
}

async fn execute_review_observed(app:App,actor:&crate::operator_auth::Actor,conversation_id:&str,source_message:&str,args:&Value)->ApiResult<Value>{
    let mut admitted=crate::assistant_action_review::execute_review(app.clone(),actor.clone(),conversation_id,source_message,args).await?;
    let review_id=admitted["reviewId"].as_str().ok_or_else(||bad("Admitted review missing"))?;
    match crate::assistant_action_review::await_review_result(app,actor,conversation_id,review_id,std::time::Duration::from_secs(5)).await {
        Ok(summary)=>Ok(summary),
        Err(_)=>{admitted["resultReadbackPending"]=json!(true);Ok(admitted)} // admission already happened; never report it as not dispatched
    }
}
fn execution_message(d:&mut Value,conversation_id:&str,job_id:&str,results:&[Value],outcome:&Value)->ApiResult<()> {
    let messages=row_mut(d,"conversations",conversation_id)?["messages"].as_array_mut().unwrap();
    if let Some(message)=messages.iter_mut().find(|m|outcome["receiptMessageId"].is_string()&&m["id"]==outcome["receiptMessageId"]){
        message["prepareRunId"]=json!(job_id);message["toolResults"]=json!(results);
    }else{
        messages.push(json!({"id":crate::id(),"role":"assistant","text":"Проверенный пакет принят к выполнению. Фактический результат ещё ожидается; состояние операций будет обновлено после проверки.","createdAt":crate::now(),"prepareRunId":job_id,"toolResults":results,"actionExecution":outcome}));
    }
    Ok(())
}

/// Bounded model-selected application tools. Every result is durable before the
/// next model pass; a later model failure cannot hide a completed local mutation.
pub async fn run(app:App,job_id:String,conversation_id:String,actor:crate::operator_auth::Actor,_request:Value)->ApiResult<Value>{
    use crate::assistant_tools;
    let worker_started=crate::now();
    let elapsed=std::time::Instant::now();
    let mut timings:Vec<Value>=Vec::new();
    current_authority(&app,&actor).await?;
    let initial=app.read_assistant_dialogue(Some(&job_id),&conversation_id).await?;
    if assistant_tools::check_owner(&initial,&job_id,&conversation_id)?!=actor.id{return Err(conflict("Assistant actor changed"));}
    let source_message=row(&initial,"jobs",&job_id)?["sourceUserMessageId"].as_str().unwrap_or("").to_owned();
    let confirmation=crate::assistant_action_review::pending_confirmation(&initial,&actor,&conversation_id,&source_message)?;
    drop(initial);
    if let Some(args)=confirmation {
        let call=json!({"id":format!("confirmed:{job_id}"),"name":"execute_action_review","arguments":args});
        let execution=execute_review_observed(app.clone(),&actor,&conversation_id,&source_message,&args).await;
        let receipt=assistant_tools::receipt(&call,execution);
        return app.change_assistant(Some(&job_id),&conversation_id,|d|{
            if assistant_tools::check_owner(d,&job_id,&conversation_id)?!=actor.id{return Err(conflict("Assistant actor changed"));}
            row_mut(d,"jobs",&job_id)?["toolResults"]=json!([receipt]);
            if receipt["ok"]==true {
                row_mut(d,"jobs",&job_id)?["prepareOutcome"]=receipt["result"].clone();
                execution_message(d,&conversation_id,&job_id,&[receipt.clone()],&receipt["result"])?;
                Ok(receipt["result"].clone())
            }else{finish_native(d,&job_id,&conversation_id,&format!("Не удалось подтвердить состояние выполнения пакета: {}",receipt["error"]["message"].as_str().unwrap_or("ошибка проверки")),"confirmed_execution_failed")}
        }).await;
    }
    let mut used=0_usize;let mut legacy_lookup=false;
    for pass in 0..assistant_tools::MAX_PASSES {
        current_authority(&app,&actor).await?;
        let photo_ids=app.change_assistant_dialogue(Some(&job_id),&conversation_id,&[],|d|{
            assistant_tools::check_owner(d,&job_id,&conversation_id)?;
            let mut bundle=row(d,"jobs",&job_id)?["prepareBundle"].clone();
            prepare_bundle::current(d,&bundle).map_err(conflict)?;
            if bundle["request"]["discussionProposalMode"].is_null(){attach_proposal_policy(d,&mut bundle)?;}
            let ids=if bundle["request"]["discussionProposalMode"]=="strict_group_v1" {
                crate::preparation_unit::current_bundle(d,&bundle,&crate::now()).map_err(conflict)?;
                if bundle["request"]["materialReadiness"]["requirements"].as_array().into_iter().flatten()
                    .any(|r|r["kind"]=="post_photo") {bundle["itemIds"].as_array().cloned().unwrap_or_default()}else{Vec::new()}
            }else{Vec::new()};
            row_mut(d,"jobs",&job_id)?["prepareBundle"]=bundle;Ok(ids)
        }).await?;
        if !photo_ids.is_empty(){crate::photo_acquisition::ensure_for_preparation(&app,&job_id,&photo_ids).await?;}
        let context_started=std::time::Instant::now();
        let request=app.change_assistant_dialogue(Some(&job_id),&conversation_id,&[],|d|{
            if assistant_tools::check_owner(d,&job_id,&conversation_id)?!=actor.id{return Err(conflict("Assistant actor changed"));}
            let mut bundle=row(d,"jobs",&job_id)?["prepareBundle"].clone();
            prepare_bundle::current(d,&bundle).map_err(conflict)?;
            if bundle["request"]["discussionProposalMode"].is_null(){attach_proposal_policy(d,&mut bundle)?;}
            if bundle["request"]["discussionProposalMode"]=="strict_group_v1" {
                // Only new acquisition observations may enter this next pass;
                // old source/recipient/family pins must still match first.
                crate::preparation_unit::current_bundle(d,&bundle,&crate::now()).map_err(conflict)?;
                crate::preparation_materials::attach_request(d,&mut bundle["request"]).map_err(conflict)?;
                seal(&mut bundle)?;
            }
            current_proposal_policy(d,&bundle)?;
            if pass==0&&row(d,"jobs",&job_id)?["toolResults"].as_array().is_some_and(|a|!a.is_empty()){
                return Err(conflict("Assistant tool run already has durable results; start a new request"));
            }
            bundle["request"]["currentTime"]=json!(crate::now());
            bundle["request"]["timezoneHint"]=json!(d["settings"]["timezone"].as_str().unwrap_or("Europe/Moscow"));
            let remaining=assistant_tools::MAX_PASSES-1-pass;
            bundle["request"]["lookupAllowed"]=json!(!legacy_lookup&&remaining>0&&used<assistant_tools::MAX_CALLS);
            bundle["request"]["assistantTools"]=json!({"version":1,"callsRemaining":assistant_tools::MAX_CALLS-used,"roundsRemaining":remaining,
                "definitions":if remaining>0&&used<assistant_tools::MAX_CALLS{assistant_tools::definitions()}else{json!([])}});
            seal(&mut bundle)?;
            let request=bundle["request"].clone();
            let job=row_mut(d,"jobs",&job_id)?;
            job["prepareBundle"]=bundle;
            job["assistantTimings"]=json!({"version":1,"workerStartedAt":worker_started,"phases":timings,"elapsedBeforeSaveMs":elapsed.elapsed().as_millis() as u64});
            Ok(request)
        }).await?;
        timings.push(json!({"phase":"context","pass":pass,"elapsedMs":context_started.elapsed().as_millis() as u64}));
        let model_started=std::time::Instant::now();
        let response=app.bridge("assistant",request).await?;
        timings.push(json!({"phase":"model","pass":pass,"elapsedMs":model_started.elapsed().as_millis() as u64}));
        let calls=assistant_tools::calls(&response)?;
        if let Some(query)=lookup_query(&response)? {
            if legacy_lookup||pass+1>=assistant_tools::MAX_PASSES||used>=assistant_tools::MAX_CALLS{return Err(bad("Assistant lookup limit reached"));}
            let query=query.to_owned();
            app.change_assistant(Some(&job_id),&conversation_id,|d|{
                if assistant_tools::check_owner(d,&job_id,&conversation_id)?!=actor.id{return Err(conflict("Assistant actor changed"));}
                let job=row(d,"jobs",&job_id)?.clone();let chat=row(d,"conversations",&conversation_id)?;
                let (mut bundle,found)=retrieved_bundle(d,&job["prepareBundle"],chat["messages"].as_array().map(Vec::as_slice).unwrap_or(&[]),&query)?;
                if job["toolResults"].is_array(){bundle["request"]["toolResults"]=job["toolResults"].clone();seal(&mut bundle)?;}
                let job=row_mut(d,"jobs",&job_id)?;job["lookupRunMetadata"]=response["runMetadata"].clone();
                job["lookupResults"]=found;job["prepareBundle"]=bundle;Ok(())
            }).await?;
            legacy_lookup=true;used+=1;continue;
        }
        if calls.is_empty(){return app.change_assistant_dialogue(Some(&job_id),&conversation_id,&[],|d|{
            row_mut(d,"jobs",&job_id)?["assistantTimings"]=json!({"version":1,"workerStartedAt":worker_started,"phases":timings,"elapsedBeforeSaveMs":elapsed.elapsed().as_millis() as u64});
            finish(d,&job_id,&conversation_id,&response)
        }).await;}
        if pass+1>=assistant_tools::MAX_PASSES||used+calls.len()>assistant_tools::MAX_CALLS {
            return app.change_assistant(Some(&job_id),&conversation_id,|d|finish_native(d,&job_id,&conversation_id,"Достигнут лимит шагов этого запроса. Выполненные действия и результаты сохранены; продолжите новым сообщением.","tool_budget_exhausted")).await;
        }
        for call in calls {
            let tool_started=std::time::Instant::now();
            current_authority(&app,&actor).await?;
            let before_tool=app.read_assistant(Some(&job_id),&conversation_id).await?;
            if assistant_tools::check_owner(&before_tool,&job_id,&conversation_id)?!=actor.id{return Err(conflict("Assistant actor changed"));}
            let before_job=row(&before_tool,"jobs",&job_id)?;
            prepare_bundle::current(&before_tool,&before_job["prepareBundle"]).map_err(conflict)?;
            if before_job["toolResults"].as_array().into_iter().flatten().any(|r|r["id"]==call["id"]){return Err(conflict("Tool call ID was already used"));}
            drop(before_tool);
            // Research alone crosses the model bridge, never a social connector.
            let external_execution=call["name"]=="execute_action_review";
            let researched=if external_execution {
                Some(execute_review_observed(app.clone(),&actor,&conversation_id,&source_message,&call["arguments"]).await)
            }else if call["name"]=="research_public" {
                let args=assistant_tools::research_arguments(&call)?;
                let snapshot=app.read_assistant(Some(&job_id),&conversation_id).await?;
                assistant_tools::check_owner(&snapshot,&job_id,&conversation_id)?;
                prepare_bundle::current(&snapshot,&row(&snapshot,"jobs",&job_id)?["prepareBundle"]).map_err(conflict)?;
                let mut request=args;request["account"]=snapshot["account"].clone();drop(snapshot);
                Some(app.bridge("assistant_research",request).await)
            }else{None};
            let terminal=app.change_assistant(Some(&job_id),&conversation_id,|d|{
                if assistant_tools::check_owner(d,&job_id,&conversation_id)?!=actor.id{return Err(conflict("Assistant actor changed"));}
                let job=row(d,"jobs",&job_id)?.clone();let old=job["prepareBundle"].clone();
                if !external_execution{prepare_bundle::current(d,&old).map_err(conflict)?;}
                let mut results=job["toolResults"].as_array().cloned().unwrap_or_default();
                if results.iter().any(|r|r["id"]==call["id"]){return Err(conflict("Tool call ID was already used"));}
                let messages=row(d,"conversations",&conversation_id)?["messages"].as_array().cloned().unwrap_or_default();
                let mut scratch=d.clone();
                let result=match researched {
                    Some(value) if external_execution=>value,
                    Some(value)=>value.and_then(assistant_tools::research_result),
                    None if call["name"]=="prepare_action_review"=>{
                        if call["arguments"]["items"].as_array().is_none_or(|a|a.iter().any(|i|!assistant_tools::observed(&old,i["id"].as_str().unwrap_or(""),&i["revision"]))) {
                            Err(conflict("Read the exact selected revisions before preparing an action review"))
                        }else{crate::assistant_action_review::prepare(&mut scratch,&actor,&conversation_id,&source_message,&call["arguments"])}
                    },
                    None=>assistant_tools::execute(&mut scratch,&old,&job_id,&conversation_id,&actor,&call)
                };
                let success=result.is_ok();let receipt=assistant_tools::receipt(&call,result);
                let terminal=success&&(receipt["result"]["terminal"]==true||external_execution);
                if terminal {
                    results.push(receipt.clone());
                    *d=scratch;
                    let job=row_mut(d,"jobs",&job_id)?;job["toolResults"]=json!(results);job["prepareOutcome"]=receipt["result"].clone();
                    if external_execution {
                        execution_message(d,&conversation_id,&job_id,&results,&receipt["result"])?;
                    }else {
                        let messages=row_mut(d,"conversations",&conversation_id)?["messages"].as_array_mut().unwrap();
                        if let Some(message)=messages.iter_mut().find(|m|m["id"]==receipt["result"]["receiptMessageId"]){message["prepareRunId"]=json!(job_id);message["toolResults"]=json!(results);}
                    }
                    return Ok(Some(receipt["result"].clone()));
                }
                results.push(receipt);
                // Admission includes result size and all source bindings. Oversized
                // evidence never commits a workflow batch without its receipt.
                let mut bundle=match refreshed_bundle(if success{&scratch}else{d},&old,&messages,&results){
                    Ok(b)=>b,
                    Err(error)=>{
                        results.pop();results.push(assistant_tools::receipt(&call,Err(error)));
                        scratch=d.clone();refreshed_bundle(d,&old,&messages,&results)?
                    }
                };
                if legacy_lookup&&job["lookupResults"].is_object(){bundle["request"]["lookupResults"]=job["lookupResults"].clone();seal(&mut bundle)?;}
                if success{*d=scratch;}
                let job=row_mut(d,"jobs",&job_id)?;job["prepareBundle"]=bundle;job["toolResults"]=json!(results);
                job["assistantProgress"]=json!({"pass":pass+1,"toolCallsCompleted":used+1,"lastTool":call["name"]});Ok(None)
            }).await?;
            timings.push(json!({"phase":"tool","pass":pass,"elapsedMs":tool_started.elapsed().as_millis() as u64}));
            if let Some(outcome)=terminal{return Ok(outcome);}
            used+=1;
        }
    }
    Err(bad("Assistant pass limit reached"))
}

#[cfg(test)]mod tests{
    use super::*;
    #[test]fn search_request_is_read_only_and_bounded(){
        assert_eq!(lookup_query(&json!({"text":"Searching","proposals":[],"lookup":{"kind":"search_comments","query":"Олег"}})).unwrap(),Some("Олег"));
        for value in [json!({"lookup":{"kind":"delete","query":"Олег"},"proposals":[]}),json!({"lookup":{"kind":"search_comments","query":"x"},"proposals":[]}),json!({"lookup":{"kind":"search_comments","query":"Олег"},"proposals":[{"kind":"close"}]})]{assert!(lookup_query(&value).is_err());}
    }
    #[test]fn retrieval_keeps_exact_ids_and_uses_workspace_content(){
        let d=json!({"account":"LikeAvto","items":[{"id":"a","author":"Олег","text":"Нужен полный привод","branchId":"b","revision":1}],"branches":[{"id":"b","postId":"p","messages":[]}],"posts":[{"id":"p","title":"Машина"}],"materials":[]});
        let original=prepare_bundle::build(&d,&[],&[]).unwrap();
        let (next,found)=retrieved_bundle(&d,&original,&[],"Олег").unwrap();
        assert_eq!(next["itemIds"],json!(["a"]));assert_eq!(next["request"]["lookupAllowed"],false);assert_eq!(found["total"],1);
        assert!(prepare_bundle::current(&d,&next).is_ok());
        let (empty,found)=retrieved_bundle(&d,&original,&[],"Unknown").unwrap();assert_eq!(found["total"],0);assert_eq!(empty["itemIds"],json!([]));
    }
    async fn fake_run(query:&str,repeat:bool)->(ApiResult<Value>,Value,Vec<Value>){
        use crate::*;
        let temp=tempfile::tempdir().unwrap();
        let db=open_db(&temp.path().join("isolated.sqlite")).await.unwrap();
        let (events,_)=broadcast::channel(8);
        let app=App{lifecycle_task_count: Default::default(),lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::BawRussia)),lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::BawRussia).identity().clone()),lifecycle_provider_token: Default::default(),lifecycle_work: Default::default(),media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),account:crate::accounts::Profile::BawRussia,navigation:crate::account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),
            events,csrf:"test".into(),auth:None,public_origin:None,external_writes:false,port:0,data:temp.path().to_owned(),
            bridge:temp.path().join("fake-dialogue.mjs"),node:PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe"),
            tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
        app.db.change(|d|crate::accounts::initialize(d,crate::accounts::Profile::BawRussia)).await.unwrap();
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        let log=temp.path().join("requests.jsonl");
        let material_module=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../adapters/assistant-materials.mjs");
        let material_url=format!("file:///{}",material_module.to_string_lossy().replace('\\',"/"));
        let script=r#"import {appendFile} from 'node:fs/promises';
import {materialInvocation} from __MATERIAL_MODULE__;
let input='';for await(const chunk of process.stdin)input+=chunk;const r=JSON.parse(input);
if(r.operation!=='assistant')throw Error('unexpected action');
await appendFile(__LOG__,JSON.stringify(r)+'\n');
// Offline text-only BAW fixture: normal invocation projection and native
// paid-capture retention are exercised without images or model execution.
function respond(result){
 if(r.mandatoryMaterialContract==='mandatory_post_materials_v1'){
  if(r.postContextBundle.members.some(m=>m.assets.some(a=>a.modality==='photo')))throw Error('unexpected fixture photo');
  const invocation=materialInvocation({payload:r,input:JSON.stringify(r)},{manifest:[]},{instructions:'Synthetic BAW fixture',schema:'{}',cliSha256:__CLI_SHA__,stdin:JSON.stringify(r)});
  result.runMetadata={schemaVersion:1,model:__MODEL__,modelProfile:__PROFILE__,reasoningEffort:'high',promptVersion:'communityhero-discussion-fixture-v1',instructionSha256:invocation.instructionSha256,inputSha256:invocation.actualTextInputSha256,cliSha256:__CLI_SHA__,elapsedMs:1,completedAt:'2026-10-06T00:00:00Z',materialInvocation:invocation,visualNeedContract:r.visualNeedContract,visualSelection:r.visualSelection};
 }
 process.stdout.write(JSON.stringify({ok:true,result}));
}
const mode=__QUERY__;
if(mode==='LANE'){respond({text:'Личный ассистент ответил',sources:[],proposals:[],toolCalls:[]});process.exit(0);}
if(mode.startsWith('TOOLS')){
 const step=(r.toolResults||[]).length;
 if(mode==='TOOLS_FAIL'&&step===3)throw Error('simulated late model failure');
 let tool=null;
 if(step===0)tool={name:'search_comments',arguments:{query:'Олег',limit:1}};
 if(step===1)tool={name:'read_comments',arguments:{itemIds:['a']}};
 if(step===2)tool={name:'set_workflow',arguments:{items:[{itemId:'a',expectedRevision:1}],workflow:'waiting',waitingReason:'Operator requested'}};
 if(mode==='TOOLS_ATOMIC'&&step===2)tool.arguments.items.push({itemId:'c',expectedRevision:99});
 if(step===3)tool={name:'workspace_stats',arguments:{workflow:'waiting'}};
 if(step===4)tool={name:'navigate',arguments:{kind:'comment',itemId:'a'}};
 if(mode==='TOOLS_HUGE'){
   if(step===1)tool={name:'navigate',arguments:{kind:'comment',itemId:'a'}};
   if(step===2)tool={name:'read_comments',arguments:{itemIds:['a']}};
   if(step>=3)tool=null;
 }
 if(mode==='TOOLS_LIMIT')tool={name:'workspace_stats',arguments:{}};
 if(mode==='TOOLS_PROPOSAL'&&step>=2){
   respond({text:'Подготовлено',sources:[],proposals:[{itemId:'a',kind:'reply_and_close',text:'Точный подготовленный ответ.'}],toolCalls:[]});process.exit(0);
 }
 if(mode==='TOOLS_REVIEW'&&step===2)tool={name:'prepare_action_review',arguments:{mode:'close_without_reply',items:[{id:'a',revision:1}]}};
 respond({text:tool?'Working':'Done',sources:[],proposals:[],toolCalls:tool?[{id:'call'+step,...tool}]:[]});process.exit(0);
}
const lookup=r.lookupAllowed||__REPEAT__?{kind:'search_comments',query:__QUERY__}:null;
respond({text:r.lookupAllowed?'Ищу':r.lookupResults.total?'Найдены совпадения':'Совпадений нет',sources:[],proposals:[],lookup});
"#.replace("__MATERIAL_MODULE__",&json!(material_url).to_string()).replace("__CLI_SHA__",&json!(crate::codex_model_policy::CLI_SHA256).to_string())
            .replace("__MODEL__",&json!(crate::codex_model_policy::MODEL).to_string()).replace("__PROFILE__",&json!(crate::codex_model_policy::PROFILE).to_string())
            .replace("__LOG__",&json!(log.to_string_lossy()).to_string()).replace("__QUERY__",&json!(query).to_string()).replace("__REPEAT__",if repeat{"true"}else{"false"});
        std::fs::write(&app.bridge,script).unwrap();
        app.change(|d|{
            d["items"]=json!([{"id":"a","author":"Олег","text":"Первый комментарий","branchId":"b","revision":1,"workflow":"attention"},{"id":"c","author":"Олег","text":"Второй комментарий","branchId":"b","revision":1,"workflow":"attention"}]);
            d["branches"]=json!([{"id":"b","postId":"p","messages":[]}]);d["posts"]=json!([{"id":"p","title":"Публикация"}]);
            if matches!(query,"TOOLS_PROPOSAL"|"TOOLS_REVIEW") {
                d["items"][0]["postId"]=json!("p");d["posts"][0]["attachments"]=json!([]);
                d["items"][0]["itemId"]=json!("source-a");d["items"][0]["objectId"]=json!("12182");d["items"][0]["platform"]=json!("VK");d["items"][0]["postKey"]=json!("12182:p");d["items"][0]["conversationKey"]=json!("12182:a");d["items"][0]["contextEvidenceDigest"]=json!("a".repeat(64));
            }
            let messages=json!([{"id":"user1","role":"user","text":"Найди комментарий Олега"}]);
            d["conversations"]=json!([{"id":"chat","operatorId":"local-owner","messages":messages}]);
            if query=="TOOLS_HUGE" {d["branches"][0]["messages"]=json!((0..301).map(|i|json!({"id":format!("m{i}"),"text":"Large branch"})).collect::<Vec<_>>());}
            let source=if query=="CONFIRM" {
                d["items"][0]["itemId"]=json!("source-a");d["items"][0]["objectId"]=json!("11391");d["items"][0]["postKey"]=json!("11391:p");d["items"][0]["conversationKey"]=json!("11391:a");d["items"][0]["contextEvidenceDigest"]=json!("a".repeat(64));
                crate::assistant_action_review::prepare(d,&operator_auth::Actor::local_owner("test"),"chat","user1",&json!({"mode":"close_without_reply","items":[{"id":"a","revision":1}]}))?;
                d["conversations"][0]["messages"].as_array_mut().unwrap().push(json!({"id":"user2","role":"user","text":"Да, закрывай"}));"user2"
            }else{"user1"};
            let mut bundle=prepare_bundle::build(d,&[],d["conversations"][0]["messages"].as_array().unwrap()).map_err(bad)?;
            attach_proposal_policy(d,&mut bundle)?;
            d["jobs"]=json!([{"id":"job","kind":"assistant","purpose":"discussion","status":"running","operatorId":"local-owner","refId":"chat","sourceUserMessageId":source,"prepareBundle":bundle}]);Ok(())
        }).await.unwrap();
        let result=if query=="LANE" {
            let _background_guard=app.assistant_gate.lock().await;
            let actor=operator_auth::Actor::local_owner("test");
            let Json(chat)=crate::conversation_new(State(app.clone()),axum::Extension(actor.clone()),Json(json!({"title":"New personal chat"}))).await.unwrap();
            let key=chat["id"].as_str().unwrap().to_owned();
            let Json(admission)=crate::conversation_message(State(app.clone()),axum::Extension(actor),Path(key.clone()),Json(json!({"text":"Ответь сейчас"}))).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10),async{
                loop {
                    let job=app.db.read_job(admission["jobId"].as_str().unwrap()).await.unwrap().unwrap();
                    if job["status"]!="running" {
                        assert_eq!(job["status"],"completed","{job:?}");
                        assert_eq!(job["purpose"],"discussion");
                        let snapshot=app.read_assistant(Some(admission["jobId"].as_str().unwrap()),&key).await.unwrap();
                        assert_eq!(snapshot["conversations"][0]["messages"][1]["text"],"Личный ассистент ответил");
                        break Ok(admission);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }).await.expect("personal discussion was blocked by background preparation gate")
        }else{crate::runtime_lifecycle_app::with_job("job".into(),run(app.clone(),"job".into(),"chat".into(),operator_auth::Actor::local_owner("test"),Value::Null)).await};
        let state=app.read().await.unwrap();
        let calls=std::fs::read_to_string(log).unwrap_or_default().lines().map(|l|serde_json::from_str(l).unwrap()).collect();
        (result,state,calls)
    }
    #[tokio::test]async fn real_bridge_roundtrip_searches_before_final_answer_without_actions(){
        let (result,state,calls)=fake_run("Олег",false).await;
        assert!(result.is_ok());assert_eq!(calls.len(),2);assert_eq!(calls[0]["lookupAllowed"],true);assert_eq!(calls[1]["lookupAllowed"],false);
        assert_eq!(calls[1]["lookupResults"]["total"],2);
        let ids:Vec<_>=calls[1]["items"].as_array().unwrap().iter().map(|i|i["id"].as_str().unwrap()).collect();assert_eq!(ids,vec!["a","c"]);
        assert_eq!(state["conversations"][0]["messages"][1]["lookupResults"]["items"].as_array().unwrap().len(),2);
        let metrics=&state["jobs"][0]["assistantTimings"];
        assert_eq!(metrics["version"],1);
        assert!(metrics["elapsedBeforeSaveMs"].is_u64());
        let phases=metrics["phases"].as_array().unwrap();
        assert_eq!(phases.iter().filter(|p|p["phase"]=="model").count(),2);
        assert!(phases.iter().all(|p|p["elapsedMs"].is_u64()&&p["pass"].is_u64()));
        assert!(!metrics.to_string().contains("Олег"));
        assert!(crate::list(&state,"operations").is_empty());assert!(crate::list(&state,"approvals").is_empty());assert!(crate::list(&state,"proposals").is_empty());
    }
    #[tokio::test]async fn empty_search_is_reported_and_repeated_lookup_never_gets_a_third_pass(){
        let (result,state,calls)=fake_run("Nobody",false).await;assert!(result.is_ok());assert_eq!(calls.len(),2);assert_eq!(calls[1]["lookupResults"]["total"],0);
        assert_eq!(state["conversations"][0]["messages"][1]["text"],"Совпадений нет");
        let (result,state,calls)=fake_run("Олег",true).await;assert!(result.is_err());assert_eq!(calls.len(),2);assert_eq!(state["conversations"][0]["messages"].as_array().unwrap().len(),1);
    }
    #[tokio::test]async fn conversational_tools_find_change_count_and_navigate_with_durable_receipts(){
        let (result,state,calls)=fake_run("TOOLS",false).await;
        assert!(result.is_ok(),"{result:?}");assert_eq!(calls.len(),6);
        assert_eq!(state["items"][0]["workflow"],"waiting");assert_eq!(state["items"][0]["revision"],2);
        assert_eq!(state["jobs"][0]["toolResults"][3]["result"]["total"],1);
        assert_eq!(state["conversations"][0]["messages"][1]["navigation"],json!({"kind":"comment","itemId":"a"}));
        assert_eq!(state["feedback"][0]["actor"]["id"],"local-owner");
        assert!(crate::list(&state,"operations").is_empty());assert!(crate::list(&state,"approvals").is_empty());
        assert!(prepare_bundle::current(&state,&state["jobs"][0]["prepareBundle"]).is_ok());
    }
    #[tokio::test]async fn completed_internal_change_survives_later_model_failure(){
        let (result,state,calls)=fake_run("TOOLS_FAIL",false).await;
        assert!(result.is_err());assert_eq!(calls.len(),4);
        assert_eq!(state["items"][0]["workflow"],"waiting");assert_eq!(state["jobs"][0]["toolResults"].as_array().unwrap().len(),3);
        assert_eq!(state["jobs"][0]["toolResults"][2]["ok"],true);
    }
    #[tokio::test]async fn endless_model_tools_are_bounded_and_saved_as_final_answer(){
        let (result,state,calls)=fake_run("TOOLS_LIMIT",false).await;
        assert!(result.is_ok());assert_eq!(calls.len(),6);assert_eq!(calls[5]["assistantTools"]["roundsRemaining"],0);
        assert_eq!(state["jobs"][0]["toolResults"].as_array().unwrap().len(),5);
        assert!(state["conversations"][0]["messages"][1]["text"].as_str().unwrap().contains("лимит"));
    }

    #[tokio::test]async fn failed_workflow_batch_commits_receipt_without_partial_changes(){
        let (result,state,_)=fake_run("TOOLS_ATOMIC",false).await;assert!(result.is_ok());
        assert_eq!(state["items"][0]["workflow"],"attention");assert_eq!(state["items"][0]["revision"],1);
        assert_eq!(state["jobs"][0]["toolResults"][2]["ok"],false);assert_eq!(state["jobs"][0]["toolResults"][3]["result"]["total"],0);
        assert!(crate::list(&state,"feedback").is_empty());
    }
    #[tokio::test]async fn reviewed_confirmation_does_not_need_a_model_and_respects_disabled_execution(){
        let (result,state,calls)=fake_run("CONFIRM",false).await;assert!(result.is_ok(),"{result:?}");assert!(calls.is_empty());
        assert_eq!(state["jobs"][0]["toolResults"][0]["name"],"execute_action_review");assert_eq!(state["jobs"][0]["toolResults"][0]["ok"],false);
        assert!(crate::list(&state,"operations").is_empty());assert!(crate::list(&state,"approvals").is_empty());
        assert_eq!(state["conversations"][0]["actionReviews"][0]["status"],"presented");
    }

    #[tokio::test]async fn oversized_branch_does_not_break_search_or_navigation_and_read_fails_safely(){
        let (result,state,calls)=fake_run("TOOLS_HUGE",false).await;assert!(result.is_ok(),"{result:?}");assert_eq!(calls.len(),4);
        let results=&state["jobs"][0]["toolResults"];
        assert_eq!(results[0]["name"],"search_comments");assert_eq!(results[0]["ok"],true);assert_eq!(results[0]["result"]["total"],2);
        assert_eq!(results[1]["name"],"navigate");assert_eq!(results[1]["ok"],true);
        assert_eq!(results[2]["name"],"read_comments");assert_eq!(results[2]["ok"],false);
        assert!(results[2]["error"]["message"].as_str().unwrap().contains("300 messages"));
        assert_eq!(state["jobs"][0]["prepareBundle"]["itemIds"],json!([]));
        assert_eq!(state["conversations"][0]["messages"][1]["navigation"],json!({"kind":"comment","itemId":"a"}));
        assert_eq!(state["items"][0]["revision"],1);assert!(crate::list(&state,"operations").is_empty());
    }

    #[tokio::test]async fn scoped_dialogue_admits_generated_proposal_and_server_review(){
        for mode in ["TOOLS_PROPOSAL","TOOLS_REVIEW"] {
            let (result,state,calls)=fake_run(mode,false).await;assert!(result.is_ok(),"{mode}: {result:?}");assert_eq!(calls.len(),3);
            let proposal=crate::list(&state,"proposals").last().unwrap();assert!(crate::proposal_current(&state,proposal).is_ok());
            assert_eq!(state["items"][0]["workflow"],"prepared");assert_eq!(state["items"][0]["revision"],2);
            if mode=="TOOLS_PROPOSAL"{assert_eq!(proposal["text"],"Точный подготовленный ответ.");}
            else{assert_eq!(state["conversations"][0]["actionReviews"][0]["status"],"presented");assert_eq!(state["jobs"][0]["toolResults"][2]["ok"],true);}
            assert!(crate::list(&state,"operations").is_empty());assert!(crate::list(&state,"approvals").is_empty());
        }
    }
    #[tokio::test]async fn personal_message_completes_while_background_preparation_gate_is_held(){
        let (result,_,calls)=fake_run("LANE",false).await;assert!(result.is_ok());assert_eq!(calls.len(),1);
    }

    fn policy_fixture()->Value {
        let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
        d["items"]=json!([{"id":"a","itemId":"a","objectId":"12182","platform":"VK","postId":"p","postKey":"p","conversationKey":"12182:a","branchId":"b","revision":1},
            {"id":"z","itemId":"z","objectId":"12182","platform":"VK","postId":"q","postKey":"q","conversationKey":"12182:z","branchId":"c","revision":1}]);
        d["posts"]=json!([{"id":"p","postKey":"p","text":"Post one","attachments":[]},{"id":"q","postKey":"q","text":"Post two","attachments":[]}]);
        d["branches"]=json!([{"id":"b","postId":"p","messages":[]},{"id":"c","postId":"q","messages":[]}]);
        d["conversations"]=json!([{"id":"chat","operatorId":"local-owner","messages":[{"id":"user","role":"user","text":"Discuss sources"}]}]);d
    }
    #[test]fn mixed_discussion_rejects_public_proposals_before_any_admission(){
        let mut d=policy_fixture();let mut bundle=prepare_bundle::build(&d,&[json!("a"),json!("z")],&[]).unwrap();
        attach_proposal_policy(&d,&mut bundle).unwrap();assert_eq!(bundle["request"]["discussionProposalMode"],"read_only_v1");
        assert!(bundle["request"]["strictGroup"].is_null());
        d["jobs"]=json!([{"id":"job","kind":"assistant","status":"running","operatorId":"local-owner","refId":"chat","sourceUserMessageId":"user","prepareBundle":bundle}]);
        let before=d.clone();assert!(finish(&mut d,"job","chat",&json!({"text":"Claimed proposal","sources":[],"proposals":[{"itemId":"a","kind":"reply_and_close","text":"Forged mixed reply"}]})).is_err());
        assert_eq!(d,before);assert!(crate::list(&d,"proposals").is_empty());assert!(crate::list(&d,"operations").is_empty());
    }
    #[test]fn tool_read_of_another_post_removes_previous_proposal_authority(){
        let d=policy_fixture();let mut old=prepare_bundle::build(&d,&[json!("a")],&[]).unwrap();attach_proposal_policy(&d,&mut old).unwrap();
        assert_eq!(old["request"]["discussionProposalMode"],"strict_group_v1");assert!(current_proposal_policy(&d,&old).is_ok());
        let next=refreshed_bundle(&d,&old,&[],&[json!({"ok":true,"name":"read_comments","result":{"items":[{"id":"z"}]}})]).unwrap();
        assert_eq!(next["request"]["discussionProposalMode"],"read_only_v1");assert!(next["request"]["strictGroup"].is_null());
        let mut stale=d.clone();stale["posts"][0]["text"]=json!("Changed while waiting");assert!(current_proposal_policy(&stale,&old).is_err());
    }
    #[test]fn native_terminal_keeps_local_receipts_without_inventing_paid_model_evidence(){
        for reason in ["tool_budget_exhausted","confirmed_execution_failed"] {
            let mut d=policy_fixture();let mut bundle=prepare_bundle::build(&d,&[],&[]).unwrap();attach_proposal_policy(&d,&mut bundle).unwrap();
            let results=json!([{"id":"call","name":"execute_action_review","ok":false,"error":{"message":"Confirmed local failure"}}]);
            d["jobs"]=json!([{"id":"job","kind":"assistant","purpose":"discussion","status":"running","operatorId":"local-owner","refId":"chat","sourceUserMessageId":"user","prepareBundle":bundle,"toolResults":results}]);
            let outcome=finish_native(&mut d,"job","chat","Local terminal observation",reason).unwrap();
            assert_eq!(outcome["decisionSource"],"native_tool_terminal");assert_eq!(outcome["candidates"],json!([]));
            assert_eq!(d["jobs"][0]["toolResults"],results);assert_eq!(d["conversations"][0]["messages"][1]["toolResults"],results);
            for key in ["proposals","approvals","operations"]{assert!(crate::list(&d,key).is_empty());}
            assert!(d["jobs"][0]["modelMaterialReceipts"].is_null());
        }
    }
    #[test]fn native_terminal_requires_both_saved_discussion_job_and_exact_discussion_request(){
        for fault in ["job_purpose","request_purpose","conversation","operator","source_turn"] {
            let mut d=policy_fixture();let mut bundle=prepare_bundle::build(&d,&[],&[]).unwrap();attach_proposal_policy(&d,&mut bundle).unwrap();
            assert_eq!(bundle["request"]["purpose"],"discussion");
            d["jobs"]=json!([{"id":"job","kind":"assistant","purpose":"discussion","status":"running","operatorId":"local-owner","refId":"chat","sourceUserMessageId":"user","prepareBundle":bundle}]);
            match fault {
                "job_purpose"=>d["jobs"][0]["purpose"]=json!("engine_prepare"),
                "request_purpose"=>{d["jobs"][0]["prepareBundle"]["request"]["purpose"]=json!("triage");seal(&mut d["jobs"][0]["prepareBundle"]).unwrap();},
                "conversation"=>d["jobs"][0]["refId"]=json!("other-chat"),
                "operator"=>d["conversations"][0]["operatorId"]=json!("other-operator"),
                _=>d["conversations"][0]["messages"].as_array_mut().unwrap().push(json!({"id":"later","role":"user","text":"New operator turn"})),
            }
            let before=d.clone();assert!(finish_native(&mut d,"job","chat","Claimed terminal observation","tool_budget_exhausted").is_err(),"{fault}");
            assert_eq!(d,before,"{fault}: rejection must preserve local receipts and authority");
        }
    }

}
