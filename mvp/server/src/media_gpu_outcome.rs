//! Independent resource evidence; never a successful inference or retry permit.
use serde_json::Value;

#[derive(Default,Debug,Clone,Copy,PartialEq,Eq)]
pub(crate) enum Outcome { #[default] Unknown, UnusedAfterClosedCodex }
impl Outcome {
    /// The caller must have awaited the bridge exit AND settled its process tree.
    /// No proof is accepted on platforms without that containment guarantee.
    pub(crate) fn observe(&mut self,operation:&str,request:&Value,error:&Value,tree_settled:bool){
        *self=Self::Unknown;
        if !tree_settled||operation!="media_vision_chunk"||error["code"]!="MEDIA_VISION_CODEX_FAILED"{return;}
        let proof=&error["mediaGpuResource"];
        let Some(hash)=request["manifestSha256"].as_str().filter(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b))) else{return;};
        if proof.as_object().is_some_and(|p|p.len()==5)&&proof["version"]==1&&proof["disposition"]=="unused_local_gpu"
            &&proof["child"]=="closed_normal_exit"&&proof["requestSha256"]==hash
            &&proof["exitCode"].as_u64().is_some_and(|n|(1..=255).contains(&n)){
            *self=Self::UnusedAfterClosedCodex;
        }
    }
    pub(crate) fn permits_release(self)->bool {self==Self::UnusedAfterClosedCodex}
}

#[cfg(test)]
#[path="media_gpu_outcome_tests.rs"]
mod tests;
