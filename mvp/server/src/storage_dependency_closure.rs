//! Source-maintenance dependency selection under the existing sole writer.
//! Source rows stay complete: graph enrichment and legacy holds are global.
//! Only strictly settled cold job bodies and unpinned research are projected.
use serde_json::Value;
use std::collections::{HashMap,HashSet};

#[derive(Clone,Copy)]
pub(crate) enum SourceReadIntent<'a> {
    Snapshot(&'a Value),
    // Ordered source observations share one writer transaction. Source rows
    // currently stay global, so their combined dependency closure is already
    // conservative; never flatten pages into one synthetic observation.
    Snapshots(&'a [Value]),
    Full { reason:&'static str },
}

#[derive(Default)]
pub(super) struct SourceClosure {
    pub full_jobs:HashSet<String>,
    pub archive_ids:HashSet<String>,
    pub full_research:bool,
    pub full_job_inventory:bool,
}

fn rows<'a>(value:&'a Value,key:&str)->&'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn job_ref(value:&Value,ids:&mut HashSet<String>)->bool {
    match value {
        Value::Null=>true,
        Value::String(id) if !id.is_empty()=>{ids.insert(id.to_owned());true},
        _=>false,
    }
}

impl SourceClosure {
    pub fn from_controls(source:&Value,controls:&[(Value,bool)])->Self {
        let mut selected=Self::default();
        let proposals:HashMap<_,_>=rows(source,"proposals").iter()
            .filter_map(|p|Some((p["id"].as_str()?,p))).collect();
        // reconcile_stale validates every automatic draft. Saved/recovery
        // reviews may consume the entire original bundle, not its header.
        for proposal in rows(source,"proposals").iter().filter(|p|matches!(p["status"].as_str(),Some("draft"|"stale"))) {
            selected.full_job_inventory|=!job_ref(&proposal["prepareRunId"],&mut selected.full_jobs);
            selected.full_job_inventory|=!job_ref(&proposal["recovery"]["prepareRunId"],&mut selected.full_jobs);
            selected.expand_jobs(proposal);
        }
        for item in rows(source,"items").iter().filter(|i|
            i["workflow"]=="attention"&&i["autoPreparation"]["requiresReview"]==true) {
            selected.full_job_inventory|=!job_ref(&item["autoPreparation"]["jobId"],&mut selected.full_jobs);
            if let Some(proposal)=item["autoPreparation"]["savedProposalId"].as_str().and_then(|id|proposals.get(id)) {
                selected.full_job_inventory|=!job_ref(&proposal["prepareRunId"],&mut selected.full_jobs);
                selected.full_job_inventory|=!job_ref(&proposal["recovery"]["prepareRunId"],&mut selected.full_jobs);
            }
        }
        for (control,full_required) in controls {
            if *full_required {selected.full_job_inventory|=!job_ref(&control["id"],&mut selected.full_jobs);}
        }
        if let Some(ctx)=crate::conductor_authority::current_context(){selected.full_jobs.insert(ctx.run_id);}
        selected.expand_controls(controls);
        selected
    }

    // Repeat after loading new parent jobs: a newly discovered parent's bundle
    // may collide with a previously cold job. The original lookup uses the first
    // matching bundle, so preserve all matching bodies and their original order.
    pub fn expand_controls(&mut self,controls:&[(Value,bool)])->bool {
        let initial=self.full_jobs.len();
        if self.full_job_inventory {
            for (job,_) in controls {job_ref(&job["id"],&mut self.full_jobs);}
        }
        loop {
            let bundles:HashSet<_>=controls.iter().filter(|(job,_)|job["id"].as_str().is_some_and(|id|self.full_jobs.contains(id)))
                .filter_map(|(job,_)|job["prepareBundle"]["id"].as_str().map(str::to_owned)).collect();
            let previous=self.full_jobs.len();
            for (job,_) in controls {
                if job["prepareBundle"]["id"].as_str().is_some_and(|id|bundles.contains(id)){job_ref(&job["id"],&mut self.full_jobs);}
            }
            if previous==self.full_jobs.len(){break;}
        }
        initial!=self.full_jobs.len()
    }

    // Expanding exact full jobs may discover a paid parent/fact dependency.
    // Iterate until fixed point; the caller retains the complete control IDs.
    pub fn expand_jobs(&mut self,job:&Value)->bool {
        let previous=self.full_jobs.len();
        let was_fallback=self.full_job_inventory;
        let mut pending=vec![(job,0usize)];
        while let Some((value,depth))=pending.pop() {
            if depth>64 {self.full_research=true;self.full_job_inventory=true;continue;}
            match value {
                Value::Object(fields)=>for (key,value) in fields {
                    if key=="manualFrameRequestIds" {
                        if let Some(values)=value.as_array().filter(|values|values.len()<=8) {
                            for value in values {self.full_job_inventory|=!job_ref(value,&mut self.full_jobs);}
                        }else{self.full_job_inventory=true;}
                    }
                    if matches!(key.as_str(),"jobId"|"prepareRunId"|"originatingAnsweringAttemptId"|"parentManualFrameRequestId")||key.ends_with("JobId") {
                        self.full_job_inventory|=!job_ref(value,&mut self.full_jobs);
                    }
                    pending.push((value,depth+1));
                },
                Value::Array(values)=>pending.extend(values.iter().map(|value|(value,depth+1))),
                _=>(),
            }
        }
        self.full_jobs.len()!=previous||self.full_job_inventory!=was_fallback
    }

    pub fn collect_archives(&mut self,value:&Value) {
        let mut pending=vec![(value,0usize)];
        while let Some((value,depth))=pending.pop() {
            if depth>64 {self.full_research=true;continue;}
            match value {
                Value::Object(fields)=>for (key,value) in fields {
                    if key=="archiveId" {
                        if let Some(id)=value.as_str().filter(|id|!id.is_empty()){self.archive_ids.insert(id.to_owned());}
                        else {self.full_research=true;}
                    }
                    pending.push((value,depth+1));
                },
                Value::Array(values)=>pending.extend(values.iter().map(|value|(value,depth+1))),
                _=>(),
            }
        }
    }
}

pub(super) fn research_projection(research:Option<&Value>,closure:&SourceClosure)->Option<Value> {
    research.map(|research|match research {
        Value::Array(values) if !closure.full_research=>Value::Array(values.iter()
            .filter(|archive|archive["id"].as_str().is_some_and(|id|closure.archive_ids.contains(id)))
            .cloned().collect()),
        _=>research.clone(),
    })
}

#[cfg(test)]
#[path="storage_dependency_closure_tests.rs"]
mod tests;
