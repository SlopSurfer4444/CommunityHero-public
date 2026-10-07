//! Read only the existing queue/current/pending/ready journal tree. A saved
//! child summary never replaces its actual referenced file or native receipt.
use super::*;
use std::collections::{HashSet,VecDeque};

fn unknown(value:&Value)->bool {value["phase"]=="unknown"||value["error"]["code"]=="UNKNOWN_MUTATION_OUTCOME"}
fn negative_match(value:&Value,binding:&Value,ancestors:Option<&Value>,skip_child:bool)->ApiResult<bool> {
    if unknown(value){return Err(invalid());}
    let original=if !value["admission"].is_null() {
        value["admission"]["id"]==binding["approvalId"]&&value["executeAttemptId"]==binding["requestId"]
            &&value["executeAdmissionProtocol"]=="local-admission-v1"&&value["executePayloadHash"]==binding["payloadHash"]
    }else{value["approvalId"]==binding["approvalId"]&&value["executeRequestId"]==binding["requestId"]
        &&value["pendingLocalAdmission"]["kind"]=="execute"&&value["pendingLocalAdmission"]["requestId"]==binding["requestId"]
        &&value["pendingLocalAdmission"]["payloadHash"]==binding["payloadHash"]};
    let saved=&value["error"]["rejection"];
    let keys=["requestId","approvalId","payloadHash","evaluationId","receiptSha256"];
    let current=keys.iter().all(|key|saved[*key]==binding[*key]);
    // An explicit native reevaluation can append a new negative while Node is
    // stopped. Its journal retains the original intent and a proven ancestor;
    // native latest-parent identity, never that ancestor, controls the action.
    let ancestor=if let Some(canonical)=ancestors {
        list(canonical,"audit").iter().any(|receipt|receipt["action"]==local_admission::REJECTED_ACTION
            &&receipt["kind"]=="execute"&&keys.iter().all(|key|receipt[*key]==saved[*key])
            &&["requestId","approvalId","payloadHash"].iter().all(|key|receipt[*key]==binding[*key]))
    }else{false};
    let mut matched=original&&value["executeJobId"].is_null()&&value["error"]["code"]=="REJECTED_LOCAL_ADMISSION"&&(current||ancestor);
    for key in ["slices","pendingSlices","readyBatches"] {
        if let Some(rows)=value.get(key) {
            for row in rows.as_array().ok_or_else(invalid)? {
                // Referenced journals are inspected below, not stale summaries.
                matched|=negative_match(row,binding,ancestors,!row["childPath"].is_null()||key=="readyBatches")?;
            }
        }
    }
    if !skip_child&&value["childPath"].is_null()&&!value["child"].is_null(){matched|=negative_match(&value["child"],binding,ancestors,false)?;}
    Ok(matched)
}

pub(super) async fn verify(app:&App,job:&Value,root:&std::path::Path,dependency:Option<&Value>,ancestors:Option<&Value>)->ApiResult<()> {
    validate_checkpoint(root,job,app.port).await?;
    let projected=if let Some(canonical)=ancestors {
        let binding=&dependency.ok_or_else(invalid)?["executeRejection"];
        local_admission::find_rejection(canonical,"execute",id(&binding["requestId"] )?)?;
        Some(json!({"audit":list(canonical,"audit").iter().filter(|receipt|receipt["action"]==local_admission::REJECTED_ACTION
            &&receipt["kind"]=="execute"&&receipt["requestId"]==binding["requestId"]).collect::<Vec<_>>()}))
    }else{None};
    let slices=PathBuf::from(format!("{}.slices",root.display()));let ready=PathBuf::from(format!("{}.ready",root.display()));
    let mut queue=VecDeque::from([(root.to_path_buf(),0usize,false)]);let mut visited=HashSet::new();let mut bytes_total=0u64;let mut matched=false;
    while let Some((path,depth,pending))=queue.pop_front() {
        if depth>5||visited.len()>=10_000||!path.is_absolute()
            ||path.components().any(|part|matches!(part,std::path::Component::ParentDir|std::path::Component::CurDir))
            ||path!=root&&!path.starts_with(&slices)&&!path.starts_with(&ready){return Err(invalid());}
        if !visited.insert(path.clone()){continue;}
        if pending&&!tokio::fs::try_exists(&path).await.map_err(|_|invalid())? {continue;}
        let metadata=tokio::fs::symlink_metadata(&path).await.map_err(|_|invalid())?;
        if !metadata.is_file()||metadata.file_type().is_symlink()||metadata.len()>32*1024*1024{return Err(invalid());}
        let canonical=tokio::fs::canonicalize(&path).await.map_err(|_|invalid())?;
        if canonical!=root&&!canonical.starts_with(&slices)&&!canonical.starts_with(&ready){return Err(invalid());}
        let file=tokio::fs::File::open(&canonical).await.map_err(|_|invalid())?;
        let mut bytes=Vec::new();file.take(32*1024*1024+1).read_to_end(&mut bytes).await.map_err(|_|invalid())?;
        bytes_total=bytes_total.checked_add(bytes.len() as u64).ok_or_else(invalid)?;
        if bytes.len()>32*1024*1024||bytes_total>64*1024*1024{return Err(invalid());}
        let value:Value=serde_json::from_slice(&bytes).map_err(|_|invalid())?;
        if value["schemaVersion"]!=1{return Err(invalid());}
        if path==root&&ancestors.is_none() {
            if let Some(dependency)=dependency {if value["connectionDependency"]!=*dependency{return Err(invalid());}}
        }
        let binding=dependency.map(|value|&value["executeRejection"]);
        if let Some(binding)=binding.filter(|value|!value.is_null()){matched|=negative_match(&value,binding,projected.as_ref(),false)?;}
        else {negative_match(&value,&Value::Null,None,false)?;}
        if !value["currentSlice"].is_null() {
            let child=required(&value["currentSlice"],"childPath")?;queue.push_back((PathBuf::from(child),depth+1,false));
        }
        for key in ["slices","pendingSlices"] {
            if let Some(rows)=value.get(key) {
                for row in rows.as_array().ok_or_else(invalid)? {
                    if let Some(path)=row["childPath"].as_str() {queue.push_back((PathBuf::from(path),depth+1,key=="pendingSlices"&&row["preparationReady"]!=true));}
                }
            }
        }
        if let Some(rows)=value.get("readyBatches") {
            for row in rows.as_array().ok_or_else(invalid)? {
                if !hash(&row["id"]){return Err(invalid());}
                queue.push_back((PathBuf::from(format!("{}.ready",path.display())).join(format!("{}.json",required(row,"id")?)),depth+1,row["mode"]=="pending"));
            }
        }
    }
    if dependency.is_some_and(|value|!value["executeRejection"].is_null())&&!matched{return Err(invalid());}Ok(())
}
