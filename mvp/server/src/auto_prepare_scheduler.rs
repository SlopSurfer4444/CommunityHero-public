//! Bounded rolling unpaid frontier for the existing automatic producer.
//! Futures may finish out of order; durable claims remain current and serial.
//! No speculative descriptor owns a paid attempt or survives restart.
use super::*;
use futures_util::{stream::FuturesUnordered, StreamExt};
use std::{collections::{BTreeMap,BTreeSet}, future::Future, sync::Arc};

pub(super) const MAX_PREFLIGHT_BYTES:usize=8*1024*1024;
pub(super) const PREFLIGHT_DEADLINE:std::time::Duration=std::time::Duration::from_secs(30);
pub(super) struct Candidate {
    pub(super) initial:bool,
    pub(super) token:crate::runtime_lifecycle::OwnerToken,
    pub(super) request:Value,
    pub(super) item_ids:Vec<String>,
}
pub(super) struct Prepared {
    pub(super) initial:bool,
    pub(super) token:crate::runtime_lifecycle::OwnerToken,
    pub(super) expected:Option<Value>,
    pub(super) selected:Vec<String>,
    pub(super) oversized:Vec<String>,
    pub(super) held_captures:Vec<(String,String)>,
}

/// Advisory graph only: nodes are recipients present when this wake starts;
/// native dependency/reservation reducers derive its ready edges on each read.
/// No descriptor, exclusion or speculative job survives restart or owns spend.
#[derive(Default)]
struct Frontier {
    recipients:BTreeSet<String>,
    attempted:BTreeSet<String>,
    pending:BTreeMap<usize,(bool,Vec<String>)>,
    next:usize,
    review_attempted:bool,
}
impl Frontier {
    fn new(snapshot:&Value)->Self {
        Self{recipients:crate::list(snapshot,"items").iter().filter_map(|item|item["id"].as_str().map(str::to_owned)).collect(),..Self::default()}
    }
    fn exclusions(&self,snapshot:&Value)->BTreeSet<String> {
        self.attempted.iter().cloned().chain(crate::list(snapshot,"items").iter()
            .filter_map(|item|item["id"].as_str()).filter(|id|!self.recipients.contains(*id)).map(str::to_owned)).collect()
    }
    fn launch(&mut self,candidate:&Candidate)->usize {
        let sequence=self.next;self.next+=1;
        self.attempted.extend(candidate.item_ids.iter().cloned());
        self.review_attempted|=!candidate.initial;
        self.pending.insert(sequence,(candidate.initial,candidate.item_ids.clone()));sequence
    }
}

/// Each discovery has one private aggregate preview. Simulated native
/// claims reserve complete aliases/families for subsequent discovery, never DB.
pub(super) fn discover(snapshot:&Value,at:i64,width:usize)->super::super::ApiResult<Vec<Candidate>> {
    discover_refill(snapshot,at,width,width,&BTreeSet::new(),&BTreeMap::new())
}
fn discover_refill(snapshot:&Value,at:i64,width:usize,limit:usize,excluded:&BTreeSet<String>,
    pending:&BTreeMap<usize,(bool,Vec<String>)>)->crate::ApiResult<Vec<Candidate>> {
    if !(1..=crate::preparation_workers::MAX_WORKERS).contains(&width) {
        return Err(crate::bad("Invalid automatic preflight window"));
    }
    let token=crate::runtime_lifecycle::admission_token(snapshot,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    let mut preview=snapshot.clone();let mut candidates=Vec::new();let mut bytes=0usize;
    // Recreate complete native family/alias reservations for unpaid in-flight
    // descriptors. These claims exist only in this private preview. If current
    // dependency/affinity evidence cannot reproduce one, wait for it to settle;
    // absence of an unproven pending reservation is never spare capacity.
    for (initial,ids) in pending.values() {
        if !initial {return Ok(candidates);}
        let Some((job,_))=claim_reconciled(&mut preview,at,Some((ids.as_slice(),&[])),width)? else{return Ok(candidates);};
        let stored=crate::row(&preview,"jobs",&job)?;
        if stored["purpose"]!="auto_prepare" || bundle_item_ids(&stored["prepareBundle"])?!=*ids {return Ok(candidates);}
    }
    for _ in 0..limit.min(width.saturating_sub(pending.len())) {
        let Some((job,request))=claim_reconciled_excluding(&mut preview,at,None,width,excluded)? else{break;};
        let stored=crate::row(&preview,"jobs",&job)?;
        let initial=stored["purpose"]=="auto_prepare";
        let item_ids=bundle_item_ids(&stored["prepareBundle"])?;
        let size=serde_json::to_vec(&request).map_err(|_|crate::bad("Invalid preflight capture"))?.len();
        let Some(total)=bytes.checked_add(size).filter(|total|*total<=MAX_PREFLIGHT_BYTES) else{break;};
        bytes=total;candidates.push(Candidate{initial,request,item_ids,token:token.clone()});
        // Historical revalidation is still the existing exclusive class. Its
        // original capture is not changed by the new-input capacity contract.
        if !initial {break;}
    }
    Ok(candidates)
}

pub(super) async fn preflight_with<F,Fut>(snapshot:&Value,candidate:Candidate,mut check:F)->crate::ApiResult<Prepared>
where F:FnMut(Vec<Value>)->Fut,Fut:Future<Output=crate::ApiResult<Vec<crate::engine_prepare::capacity::Size>>> {
    if !candidate.initial {
        return Ok(Prepared{initial:false,token:candidate.token,expected:Some(candidate.request),selected:candidate.item_ids,
            oversized:Vec::new(),held_captures:Vec::new()});
    }
    drop(candidate.request);
    let plan=json!({"batches":[{"itemIds":candidate.item_ids}],"held":[]});
    // Keep only compact exact-fit receipt hashes across split rounds. Building
    // again after a timed source selection changes is NOT a size permission.
    let fit_hashes=Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let saved=fit_hashes.clone();
    let plan=crate::engine_prepare::capacity::refine_with(snapshot,plan,None,|requests| {
        let hashes=requests.iter().map(|request|crate::editorial_review::hash_text(&request.to_string())).collect::<Vec<_>>();
        let future=check(requests);let saved=saved.clone();
        async move {
            let sizes=future.await?;
            if sizes.len()!=hashes.len(){return Err(crate::internal("Preparation capacity result count changed"));}
            let mut fits=saved.lock().unwrap();
            for (hash,size)in hashes.into_iter().zip(&sizes) {
                if matches!(size,crate::engine_prepare::capacity::Size::Fits(_)){fits.insert(hash);}
            }
            Ok(sizes)
        }
    }).await?;
    // A split tail is unclaimed input. Its eligibility remains in the durable
    // queue and a later wake may preflight it after this family's owner settles.
    let selected:Vec<String>=plan["batches"].as_array().and_then(|rows|rows.first())
        .and_then(|b|b["itemIds"].as_array()).into_iter().flatten()
        .filter_map(|id|id.as_str().map(str::to_owned)).collect();
    let oversized:Vec<String>=plan["held"].as_array().into_iter().flatten()
        .filter_map(|held|held["itemId"].as_str().map(str::to_owned)).collect();
    let context=crate::prepare_bundle::EvidenceContext::new(snapshot);
    let held_captures=oversized.iter().map(|id|Ok((id.clone(),context.fingerprint(id).map_err(crate::bad)?)))
        .collect::<crate::ApiResult<Vec<_>>>()?;
    let ids=selected.iter().map(|id|json!(id)).collect::<Vec<_>>();
    let expected=if selected.is_empty(){None}else{Some(crate::engine_prepare::build_request(snapshot,&ids,None)
        .map_err(crate::bad)?["request"].clone())};
    if expected.as_ref().is_some_and(|request|!fit_hashes.lock().unwrap()
        .contains(&crate::editorial_review::hash_text(&request.to_string()))) {
        return Err(crate::conflict("Preparation source changed after capacity preflight; replan without replaying generation"));
    }
    Ok(Prepared{initial:true,token:candidate.token,expected,selected,oversized,held_captures})
}

/// Same reducer in production and hostile fixtures. Caller owns rollback.
pub(super) fn commit_captured(d:&mut Value,at:i64,width:usize,prepared:&Prepared)->crate::ApiResult<Option<(String,Value)>> {
    crate::runtime_lifecycle::require_admission(d,&prepared.token,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    reconcile_claim_state(d,at)?;
    let context=crate::prepare_bundle::EvidenceContext::new(d);
    for(id,expected)in &prepared.held_captures {
        if context.fingerprint(id).map_err(crate::bad)?!=*expected {
            return Err(crate::conflict("Preparation source changed after capacity preflight; replan without replaying generation"));
        }
    }
    // Exact selected IDs prevent a slow earlier family or a newly arrived
    // family from stealing a completed sibling's capacity receipt.
    let capacity=prepared.initial.then_some((prepared.selected.as_slice(),prepared.oversized.as_slice()));
    let claimed=claim_reconciled(d,at,capacity,width)?;
    crate::engine_prepare::capacity::same_capture(prepared.expected.as_ref(),claimed.as_ref().map(|(_,request)|request))?;
    if let Some((run,_))=&claimed {
        // Native revalidation creates a fresh job without stage bookkeeping.
        // Initialize only this newly appended job, never an old paid run.
        let job=crate::row(d,"jobs",run)?;
        if job["purpose"]=="auto_revalidate"&&job["preparationStages"].is_null() {
            crate::row_mut(d,"jobs",run)?["preparationStages"]=json!({"first":null,"review":null});
        }
        let request=&crate::row(d,"jobs",run)?["prepareBundle"]["request"];
        crate::preparation_unit::current_request(d,request,&stamp(at)).map_err(crate::conflict)?;
        crate::preparation_review::record_initial_admission(d,&prepared.token,run,&stamp(at))?;
    }
    Ok(claimed)
}
pub(super) async fn commit(app:&crate::App,prepared:&Prepared)->crate::ApiResult<Option<(String,Value)>> {
    app.change_preparation_claim(|d|commit_captured(d,chrono::Utc::now().timestamp(),app.preparation_workers.width(),prepared)).await
}

/// This is the actual producer core; injection replaces only unpaid sizing and
/// postcommit spawn in offline tests, not discovery or guarded durable claims.
pub(super) async fn fill_with<F,Fut,S>(app:&crate::App,check:F,mut spawn:S)->crate::ApiResult<()>
where F:Fn(Arc<Value>,Candidate)->Fut+Clone,Fut:Future<Output=crate::ApiResult<Prepared>>,S:FnMut(String,Value) {
    fill_with_deadline(app,check,spawn,PREFLIGHT_DEADLINE).await
}
pub(super) async fn fill_with_deadline<F,Fut,S>(app:&crate::App,check:F,mut spawn:S,deadline:std::time::Duration)->crate::ApiResult<()>
where F:Fn(Arc<Value>,Candidate)->Fut+Clone,Fut:Future<Output=crate::ApiResult<Prepared>>,S:FnMut(String,Value) {

    let token=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await?;
    let discovery_read=crate::performance::Span::new("preparation.graph.discovery_read");
    let initial=app.db.read_preparation_discovery().await?;
    drop(discovery_read);
    let mut frontier=Frontier::new(&initial);
    let mut initial_snapshot=Some(initial);
    let width=app.preparation_workers.width();
    let mut pending=FuturesUnordered::new();
    let mut first_error=None;
    // Preserve completion hints for tick's fact/recovery/review tail after this
    // same producer uses them for rolling discovery; never spawn a second loop.
    struct Relay<'a>{app:&'a crate::App,consumed:bool,rounds:usize,probes:usize,recipients:usize}
    impl Drop for Relay<'_>{fn drop(&mut self){
        if self.consumed{self.app.preparation_wake.notify_one();}
        if std::env::var("COMMUNITYHERO_PERF_TRACE").as_deref()==Ok("1") {
            eprintln!("preparation_frontier rounds={} probes={} initial_recipients={}",self.rounds,self.probes,self.recipients);
        }
    }}
    let mut relay=Relay{app,consumed:false,rounds:0,probes:0,recipients:frontier.recipients.len()};
    loop {
        let at=chrono::Utc::now().timestamp();
        let snapshot=match initial_snapshot.take(){Some(snapshot)=>snapshot,None=>{
            let discovery_read=crate::performance::Span::new("preparation.graph.discovery_read");
            let snapshot=app.db.read_preparation_discovery().await?;drop(discovery_read);snapshot
        }};
        relay.rounds+=1;
        let discovery=crate::performance::Span::new("preparation.graph.discovery");
        let excluded=frontier.exclusions(&snapshot);
        let (snapshot,candidates)=crate::runtime_lifecycle_app::Capture::preview(app,&token,snapshot,|d| {
            reconcile_claim_state(d,at)?;
            if frontier.review_attempted {Ok(Vec::new())} else {
                discover_refill(d,at,width,width.saturating_sub(frontier.pending.len()),&excluded,&frontier.pending)
            }
        })?;
        drop(discovery);
        let snapshot=Arc::new(snapshot);
        for candidate in candidates {
            let sequence=frontier.launch(&candidate);
            relay.probes+=1;
            let future=check.clone()(snapshot.clone(),candidate);
            pending.push(async move {
                // Every round stays unpaid and deadline bounded. At most width
                // futures and width snapshot generations can be retained.
                let result=tokio::time::timeout(deadline,future).await.map_err(|_|crate::internal(
                    "Automatic capacity preflight deadline; no generation admitted")).and_then(|result|result);
                (sequence,result)
            });
        }
        drop(snapshot);
        if pending.is_empty() {
            if frontier.next==0 {
                // Existing idle bookkeeping, without admitting an unseen job.
                let idle=Prepared{initial:false,token:token.clone(),expected:None,selected:Vec::new(),oversized:Vec::new(),held_captures:Vec::new()};
                commit(app,&idle).await?;
            }
            break;
        }
        let (sequence,preflight)=tokio::select! {
            biased;
            result=pending.next()=>result.expect("nonempty rolling frontier"),
            _=app.preparation_wake.notified()=>{relay.consumed=true;continue;},
        };
        frontier.pending.remove(&sequence);
        match preflight {
            Ok(prepared)=>match commit(app,&prepared).await {
                Ok(Some((job,request)))=>spawn(job,request),
                Ok(None)=>{},
                Err(error)=>{if first_error.is_none(){first_error=Some(error);}},
            },
            Err(error)=>{if first_error.is_none(){first_error=Some(error);}},
        }
        // Retain original recipient exclusions after a split/error/hold. Fresh
        // reads can advance unseen independent nodes before a slow probe ends.
        // Changed dependencies still need fresh exact capture and native claim.
    }
    first_error.map_or(Ok(()),Err)
}
pub(super) async fn fill(app:&crate::App)->crate::ApiResult<()> {
    // Production scheduler has no implicit unbounded background admission.
    // Recheck current policy before unpaid discovery; commit reducers check again.
    let policy_snapshot=app.read().await?;
    if crate::continuous_preparation::admission_reason(&policy_snapshot).is_some(){return Ok(());}
    drop(policy_snapshot);
    fill_with(app,|snapshot,candidate|async move {
        preflight_with(&snapshot,candidate,|requests|crate::engine_prepare::capacity::check(app,requests)).await
    },|job,request|spawn_worker(app,job,Some(request))).await
}

#[cfg(test)]
#[path="auto_prepare_scheduler_tests.rs"]
mod tests;
