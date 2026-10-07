//! Authoritative adapter sizing for new captures, outside every writer transaction.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use crate::{ApiResult, bad, internal};

const MAX_BATCH:usize=16;
const MAX_WIRE_BYTES:usize=8*1024*1024;
pub(crate) const OVERSIZED:&str="Selected assistant evidence exceeds the final model-input budget; split preparation recipients";

fn digest(request:&Value)->String {format!("{:x}",Sha256::digest(request.to_string().as_bytes()))}

#[derive(Clone,Debug,PartialEq)]
pub(crate) enum Size { Fits(usize), Oversized }

fn entry(request:&Value)->Value {json!({"serializedRequest":request.to_string(),"requestSha256":digest(request)})}

fn response(value:&Value,requests:&[Value])->ApiResult<Vec<Size>> {
    let invalid=||internal("Assistant capacity receipt does not bind the requested input");
    if value.as_object().is_none_or(|v|v.len()!=2)||value["version"]!=1 {return Err(invalid());}
    let rows=value["results"].as_array().filter(|rows|rows.len()==requests.len()).ok_or_else(invalid)?;
    rows.iter().zip(requests).map(|(row,request)|{
        if row.as_object().is_none_or(|v|v.len()!=5)||row["requestSha256"]!=digest(request)
            ||row["maxModelBytes"]!=super::MAX_REQUEST_BYTES {return Err(invalid());}
        let text=row["textBytes"].as_u64();let bounded=row["boundedModelBytes"].as_u64();
        match (row["status"].as_str(),text,bounded){
            (Some("fits"),Some(text),Some(bound)) if text<=bound && bound<=super::MAX_REQUEST_BYTES as u64=>Ok(Size::Fits(bound as usize)),
            (Some("oversized"),Some(text),Some(bound)) if text<=bound && bound>super::MAX_REQUEST_BYTES as u64=>Ok(Size::Oversized),
            (Some("oversized"),None,None) if row["textBytes"].is_null()&&row["boundedModelBytes"].is_null()=>Ok(Size::Oversized),
            _=>Err(invalid()),
        }
    }).collect()
}

pub(crate) async fn check(app:&crate::App,requests:Vec<Value>)->ApiResult<Vec<Size>>{
    let mut results=Vec::new();let mut cursor=0;
    while cursor<requests.len(){
        let mut entries=Vec::new();let start=cursor;
        while cursor<requests.len()&&entries.len()<MAX_BATCH {
            let next=entry(&requests[cursor]);entries.push(next);
            let wire=json!({"account":app.account.key(),"operation":"assistant_preflight","requests":entries});
            if wire.to_string().len()>MAX_WIRE_BYTES {
                entries.pop();if entries.is_empty(){return Err(bad("Assistant capacity request exceeds the bounded transport"));}break;
            }
            cursor+=1;
        }
        let receipt=app.bridge("assistant_preflight",json!({"requests":entries})).await?;
        results.extend(response(&receipt,&requests[start..cursor])?);
    }
    Ok(results)
}

pub(crate) async fn require(app:&crate::App,request:Option<&Value>)->ApiResult<()> {
    let Some(request)=request else{return Ok(())};
    match check(app,vec![request.clone()]).await?.first(){Some(Size::Fits(_))=>Ok(()),_=>Err(bad(OVERSIZED))}
}

/// The actual scheduler re-creates the request under the writer and must match
/// the exact capacity-checked capture. A changed source is never a size permit.
pub(crate) fn same_capture(expected:Option<&Value>,actual:Option<&Value>)->ApiResult<()> {
    if expected==actual {Ok(())}else{Err(crate::conflict("Preparation source changed after capacity preflight; replan without replaying generation"))}
}

pub(crate) async fn refine(app:&crate::App,d:&Value,plan:Value,instruction:Option<&str>)->ApiResult<Value>{
    refine_with(d,plan,instruction,|requests|check(app,requests)).await
}

pub(crate) async fn refine_with<F,Fut>(d:&Value,mut plan:Value,instruction:Option<&str>,mut check:F)->ApiResult<Value>
where F:FnMut(Vec<Value>)->Fut,Fut:std::future::Future<Output=ApiResult<Vec<Size>>> {
    let mut pending:Vec<(Vec<usize>,Vec<Value>)>=plan["batches"].as_array().ok_or_else(||internal("Preparation plan missing batches"))?
        .iter().enumerate().map(|(n,b)|(vec![n],b["itemIds"].as_array().cloned().unwrap_or_default())).collect();
    let mut done=BTreeMap::new();let mut held=plan["held"].as_array().cloned().unwrap_or_default();
    while !pending.is_empty(){
        let mut requests=Vec::new();let mut positions=Vec::new();
        let mut request_bytes=0usize;let mut sizes=vec![Size::Oversized;pending.len()];
        for (position,(_,ids)) in pending.iter().enumerate(){
            match super::build_request(d,ids,instruction){
                Ok(bundle)=>{
                    let request=bundle["request"].clone();
                    let bytes=serde_json::to_vec(&request).map_err(|_|bad("Invalid capacity request"))?.len();
                    if bytes>MAX_WIRE_BYTES{return Err(bad("Assistant capacity capture exceeds the bounded transport"));}
                    // Split rounds can contain up to100 requests. Retain only
                    // one bounded check chunk, not a whole expanded round.
                    if !requests.is_empty()&&(requests.len()==MAX_BATCH||request_bytes+bytes>MAX_WIRE_BYTES){
                        let checked=check(std::mem::take(&mut requests)).await?;
                        if checked.len()!=positions.len(){return Err(internal("Preparation capacity result count changed"));}
                        for (position,size)in std::mem::take(&mut positions).into_iter().zip(checked){sizes[position]=size;}
                        request_bytes=0;
                    }
                    request_bytes+=bytes;positions.push(position);requests.push(request);
                },
                Err(error) if crate::prepare_plan::capacity_error(error)=>{},
                Err(error)=>return Err(bad(error)),
            }
        }
        if !requests.is_empty(){
            let checked=check(requests).await?;
            if checked.len()!=positions.len(){return Err(internal("Preparation capacity result count changed"));}
            for (position,size) in positions.into_iter().zip(checked){sizes[position]=size;}
        }
        let mut next=Vec::new();
        for ((order,ids),size) in pending.into_iter().zip(sizes){
            match size {
                Size::Fits(bytes)=>{
                    let unit=crate::preparation_unit::capture(d,&ids,&crate::now()).map_err(bad)?;
                    done.insert(order,json!({"itemIds":ids,"bytes":bytes,"strictGroup":unit}));
                },
                Size::Oversized if ids.len()>1=>{
                    let mut posts:Vec<(String,Vec<Value>)>=Vec::new();
                    for id in &ids {
                        let item=crate::row(d,"items",id.as_str().ok_or_else(||bad("Invalid capacity recipient"))?)?;
                        let post=item["postId"].as_str().ok_or_else(||bad("Missing capacity post"))?;
                        if let Some((_,group))=posts.iter_mut().find(|(key,_)|key==post){group.push(id.clone());}
                        else{posts.push((post.to_owned(),vec![id.clone()]));}
                    }
                    if posts.len()==1&&crate::row(d,"posts",&posts[0].0)?["attachments"].as_array().into_iter().flatten()
                        .filter(|a|matches!(a["type"].as_str(),Some("photo"|"image"))).count()>super::MAX_REQUEST_IMAGES {
                        for id in ids{held.push(json!({"itemId":id,"reason":"mandatory_post_material_capacity_exceeded"}));}
                    }else{
                        // Preserve each copy's mandatory material body first.
                        // Only a single post may split its recipient tail.
                        let parts=if posts.len()>1{posts.into_iter().map(|(_,ids)|ids).collect::<Vec<_>>()}
                            else{let middle=ids.len()/2;vec![ids[..middle].to_vec(),ids[middle..].to_vec()]};
                        for(n,part)in parts.into_iter().enumerate(){let mut child=order.clone();child.push(n);next.push((child,part));}
                    }
                },
                Size::Oversized=>{for id in ids{held.push(json!({"itemId":id,"reason":"model_context_capacity_exceeded"}));}},
            }
        }
        pending=next;
    }
    plan["batches"]=json!(done.into_values().collect::<Vec<_>>());plan["held"]=json!(held);
    plan["capacityChecked"]=json!(true);Ok(plan)
}

#[cfg(test)]
#[path="preparation_capacity_group_tests.rs"]
mod strict_group_tests;

#[cfg(test)]
mod tests {
    use super::*;
    async fn app_fixture()->(crate::App,tempfile::TempDir){
        let (mut app,temp)=crate::tests::test_app().await;
        let initialized=app.read().await.unwrap();
        let owner=crate::runtime_lifecycle::current_owner(&initialized,&app.lifecycle_owner).unwrap();
        let mut initial=super::super::tests::fixture(false);
        for key in ["knowledge_entries","knowledge_versions","feedback"] {initial[key]=json!([]);}
        // Install capacity evidence without deleting the actual App's fixed native owner.
        initial["runtimeLifecycle"]=initialized["runtimeLifecycle"].clone();
        app.db.change(|d|{*d=initial;Ok(())}).await.unwrap();
        let seeded=app.read().await.unwrap();
        assert_eq!(seeded["runtimeLifecycle"],initialized["runtimeLifecycle"]);
        assert_eq!(crate::runtime_lifecycle::current_owner(&seeded,&app.lifecycle_owner).unwrap(),owner);
        crate::runtime_lifecycle::require_admission(&seeded,&owner,crate::runtime_lifecycle::AdmissionClass::Preparation).unwrap();
        app.node=std::env::var_os("COMMUNITYHERO_TEST_NODE").map(std::path::PathBuf::from)
            .unwrap_or_else(||std::path::PathBuf::from("node"));
        (app,temp)
    }
    #[test]
    fn receipts_bind_exact_request_and_fail_closed(){
        let request=json!({"account":"LikeAvto","text":"é\n一","number":1.0});
        let valid=json!({"version":1,"results":[{"requestSha256":digest(&request),"status":"fits","textBytes":4,"boundedModelBytes":5,"maxModelBytes":550000}]});
        assert_eq!(response(&valid,&[request.clone()]).unwrap(),vec![Size::Fits(5)]);
        for (field,value) in [("requestSha256",json!("bad")),("status",json!("cached")),("textBytes",json!(6)),("boundedModelBytes",json!(550001)),("maxModelBytes",json!(600000))]{
            let mut invalid=valid.clone();invalid["results"][0][field]=value;assert!(response(&invalid,&[request.clone()]).is_err(),"{field}");
        }
        assert!(same_capture(Some(&request),Some(&json!({"account":"BAW Russia"}))).is_err());
        assert!(same_capture(Some(&request),None).is_err());assert!(same_capture(None,None).is_ok());
        let mut oversized=valid;oversized["results"][0]["status"]=json!("oversized");oversized["results"][0]["textBytes"]=Value::Null;oversized["results"][0]["boundedModelBytes"]=Value::Null;
        assert_eq!(response(&oversized,&[request]).unwrap(),vec![Size::Oversized]);
    }

    #[tokio::test]
    async fn oversized_rounds_partition_exact_recipients_and_hold_singletons(){
        let mut d=super::super::tests::baw_fixture(false);
        d["items"][1]["postId"]=json!("ready-post");d["items"][1]["postKey"]=json!("ready-post");
        d["branches"][1]["postId"]=json!("ready-post");let before=d.clone();
        let batches=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));let observed=batches.clone();
        let plan=json!({"batches":[{"itemIds":["ready","media"]}],"held":[]});
        let result=refine_with(&d,plan,None,move|requests|{
            observed.lock().unwrap().push(requests.len());
            async move {Ok(requests.into_iter().map(|request|{
                if request["items"].as_array().unwrap().len()>1||request["items"][0]["id"]=="media" {Size::Oversized}else{Size::Fits(100)}
            }).collect())}
        }).await.unwrap();
        assert_eq!(result["batches"].as_array().unwrap().len(),1);
        assert_eq!(result["batches"][0]["itemIds"],json!(["ready"]));assert_eq!(result["batches"][0]["bytes"],100);
        assert_eq!(result["batches"][0]["strictGroup"],crate::preparation_unit::capture(&d,&[json!("ready")],&crate::now()).unwrap());
        assert_eq!(result["held"],json!([{"itemId":"media","reason":"model_context_capacity_exceeded"}]));
        assert_eq!(*batches.lock().unwrap(),vec![1,2],"one adapter batch per split round, not per recipient");
        assert_eq!(d,before,"planning never creates jobs, claims or proposals");
    }

    #[tokio::test]
    async fn deterministic_oversize_creates_no_job_receipt_or_model_call(){
        let (mut app,temp)=app_fixture().await;
        app.bridge=temp.path().join("capacity-only.mjs");
        let source=r#"import fs from 'node:fs/promises';let raw='';for await(const chunk of process.stdin)raw+=chunk;
const req=JSON.parse(raw);if(req.operation!=='assistant_preflight')throw Error('paid or provider call forbidden');
process.stdout.write(JSON.stringify({ok:true,result:{version:1,results:req.requests.map(r=>({requestSha256:r.requestSha256,status:'oversized',textBytes:null,boundedModelBytes:null,maxModelBytes:550000}))}}));"#;
        std::fs::write(&app.bridge,source).unwrap();
        let before=app.read().await.unwrap();
        let result=super::super::prepare(axum::extract::State(app.clone()),axum::Extension(crate::operator_auth::Actor::local_owner("capacity-test")),
            axum::Json(json!({"requestId":"capacity-rejected","itemIds":["ready"]}))).await;
        let error=result.unwrap_err();assert!(error.1.contains("final model-input budget"),"{error:?}");
        assert_eq!(app.read().await.unwrap(),before,"no runnable invalid capture or local-admission receipt");
        app.db.close().await;
    }

    #[tokio::test]
    async fn automatic_oversized_recipient_changed_during_preflight_is_not_held_or_claimed(){
        let (mut app,temp)=app_fixture().await;
        // This isolated fixture provisions a queue epoch before enabling the
        // finite background policy. A generic product writer cannot mint it.
        let crate::storage::Database::Sqlite(pool)=&app.db else{unreachable!()};
        sqlx::query("UPDATE workspace SET payload=json_set(payload,'$.storageGeneration',?) WHERE id=1")
            .bind("e4e1d9f2-49b8-4b48-846f-ab113f519f89").execute(pool).await.unwrap();
        app.change(|d|{
            crate::connection_gate::fixture_open(d)?;
            d["items"].as_array_mut().unwrap().truncate(1);
            d["items"][0]["createdAt"]=json!((chrono::Utc::now()-chrono::Duration::seconds(60)).to_rfc3339());
            d["items"][0]["providerObservedAt"]=json!(crate::now());Ok(())
        }).await.unwrap();
        crate::continuous_preparation::configure(axum::extract::State(app.clone()),
            axum::Extension(crate::operator_auth::Actor::local_owner("capacity-test")),axum::Json(json!({
                "enabled":true,"workflowMode":"prepare_review_only","invocationLimit":2,
                "maxOriginalJobInvocations":2,"invocationsPerBridge":1,"maxActiveJobs":1,
                "maxReadyItems":10,"maxReadyBytes":65536}))).await.unwrap();
        let configured=app.read().await.unwrap();
        assert!(crate::continuous_preparation::enabled(&configured));
        assert_eq!(crate::continuous_preparation::admission_reason(&configured),None);
        app.bridge=temp.path().join("capacity-auto-wait.mjs");
        std::fs::write(&app.bridge,r#"import fs from 'node:fs/promises';import path from 'node:path';import {fileURLToPath} from 'node:url';
const root=path.dirname(fileURLToPath(import.meta.url));let raw='';for await(const chunk of process.stdin)raw+=chunk;
const req=JSON.parse(raw);if(req.operation!=='assistant_preflight')throw Error('paid call forbidden');
await fs.writeFile(path.join(root,'waiting'),'1');for(let n=0;n<1000;n++){try{await fs.access(path.join(root,'continue'));break;}catch{await new Promise(r=>setTimeout(r,5));}}
process.stdout.write(JSON.stringify({ok:true,result:{version:1,results:req.requests.map(r=>({requestSha256:r.requestSha256,status:'oversized',textBytes:null,boundedModelBytes:null,maxModelBytes:550000}))}}));"#).unwrap();
        let worker=app.clone();let pending=tokio::spawn(async move{crate::auto_prepare::tick(&worker).await});
        tokio::time::timeout(std::time::Duration::from_secs(5),async{
            while !temp.path().join("waiting").exists(){tokio::time::sleep(std::time::Duration::from_millis(5)).await;}
        }).await.unwrap();
        app.change(|d|{d["items"][0]["text"]=json!("Repaired source while projection waited");crate::bump(&mut d["items"][0]);Ok(())}).await.unwrap();
        let expected=app.read().await.unwrap();std::fs::write(temp.path().join("continue"),"1").unwrap();
        let error=pending.await.unwrap().unwrap_err();assert!(error.1.contains("source changed after capacity preflight"),"{error:?}");
        assert_eq!(app.read().await.unwrap(),expected,"stale oversize evidence cannot write a hold, attempt or job");
        assert!(app.tasks.lock().await.is_empty());app.db.close().await;
    }

    #[tokio::test]
    async fn source_changed_while_adapter_waits_rolls_back_schedule_and_writer_is_free(){
        let (mut app,temp)=app_fixture().await;
        app.bridge=temp.path().join("capacity-wait.mjs");
        std::fs::write(&app.bridge,r#"import fs from 'node:fs/promises';import path from 'node:path';import {fileURLToPath} from 'node:url';
const root=path.dirname(fileURLToPath(import.meta.url));let raw='';for await(const chunk of process.stdin)raw+=chunk;
const req=JSON.parse(raw);if(req.operation!=='assistant_preflight')throw Error('paid call forbidden');
await fs.writeFile(path.join(root,'waiting'),'1');for(let n=0;n<1000;n++){try{await fs.access(path.join(root,'continue'));break;}catch{await new Promise(r=>setTimeout(r,5));}}
process.stdout.write(JSON.stringify({ok:true,result:{version:1,results:req.requests.map(r=>({requestSha256:r.requestSha256,status:'fits',textBytes:10,boundedModelBytes:20,maxModelBytes:550000}))}}));"#).unwrap();
        let worker=app.clone();
        let pending=tokio::spawn(async move{super::super::prepare(axum::extract::State(worker),axum::Extension(crate::operator_auth::Actor::local_owner("capacity-test")),
            axum::Json(json!({"requestId":"capacity-race","itemIds":["ready"]}))).await});
        tokio::time::timeout(std::time::Duration::from_secs(5),async{
            while !temp.path().join("waiting").exists(){tokio::time::sleep(std::time::Duration::from_millis(5)).await;}
        }).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2),app.change(|d|{
            d["items"][0]["text"]=json!("Changed while pure projection was running");crate::bump(&mut d["items"][0]);Ok(())
        })).await.expect("capacity preflight must not hold writer").unwrap();
        let expected=app.read().await.unwrap();std::fs::write(temp.path().join("continue"),"1").unwrap();
        let error=pending.await.unwrap().unwrap_err();assert!(error.1.contains("source changed after capacity preflight"));
        assert_eq!(app.read().await.unwrap(),expected,"source edit survives, provisional model job rolls back");app.db.close().await;
    }
}
