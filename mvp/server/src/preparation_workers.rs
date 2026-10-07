//! Bounded model workers, independent of publication authority. Fixed slots are
//! transport identities; durable reservations still own recipients and replay.
use crate::*;
use std::{collections::BTreeSet, future::Future, sync::{Arc, Mutex as StateMutex}};
use tokio::sync::Notify;

pub(crate) const MAX_WORKERS: usize = 8;
tokio::task_local! { static SLOT: usize; }
pub(crate) fn current_slot() -> Option<usize> { SLOT.try_with(|slot| *slot).ok() }

#[derive(Default)]
struct State { active: Vec<Option<Option<BTreeSet<String>>>>, waiting: Vec<(u64, Option<BTreeSet<String>>)>, next: u64 }
pub(crate) struct Pool { state: StateMutex<State>, wake: Notify, width: usize }
impl Default for Pool { fn default() -> Self { Self::new(1).expect("default worker count") } }
impl Pool {
    pub(crate) fn new(width: usize) -> ApiResult<Self> {
        if !(1..=MAX_WORKERS).contains(&width) { return Err(bad("Preparation worker count must be 1 to 8")); }
        Ok(Self { state: StateMutex::new(State { active: vec![None; width], ..Default::default() }), wake: Notify::new(), width })
    }
    pub(crate) fn from_env() -> ApiResult<Self> {
        let width = match std::env::var("COMMUNITYHERO_PREPARE_WORKERS") {
            Err(std::env::VarError::NotPresent) => 1,
            Ok(value) => value.parse().map_err(|_| bad("Invalid preparation worker count"))?,
            Err(_) => return Err(bad("Invalid preparation worker count")),
        };
        Self::new(width)
    }
    pub(crate) fn width(&self) -> usize { self.width }
    pub(crate) async fn acquire(self: &Arc<Self>, keys: Option<BTreeSet<String>>) -> Lease {
        let ticket = { let mut state = self.state.lock().unwrap(); let ticket = state.next; state.next += 1;
            state.waiting.push((ticket, keys.clone())); ticket };
        let mut pending = Pending { pool: self.clone(), ticket, claimed: false };
        loop {
            // Register before inspecting state: a dropped lease cannot lose a wake.
            let changed = self.wake.notified(); tokio::pin!(changed); changed.as_mut().enable();
            let slot = { let mut state = self.state.lock().unwrap();
                let blocked = state.active.iter().flatten().any(|other| overlaps(&keys, other))
                    || state.waiting.iter().any(|(older, other)| *older < ticket && overlaps(&keys, other));
                if blocked { None } else {
                    let slot = if keys.is_none() { (state.active.iter().all(Option::is_none)).then_some(0) }
                        else { state.active.iter().position(Option::is_none) };
                    if let Some(slot) = slot { state.active[slot] = Some(keys.clone()); state.waiting.retain(|(id, _)| *id != ticket); }
                    slot
                }
            };
            if let Some(slot) = slot { pending.claimed = true; return Lease { pool: self.clone(), slot }; }
            changed.await;
        }
    }
}
fn overlaps(a: &Option<BTreeSet<String>>, b: &Option<BTreeSet<String>>) -> bool {
    match (a, b) { (Some(a), Some(b)) => !a.is_disjoint(b), _ => true }
}
struct Pending { pool: Arc<Pool>, ticket: u64, claimed: bool }
impl Drop for Pending { fn drop(&mut self) { if !self.claimed {
    self.pool.state.lock().unwrap().waiting.retain(|(id, _)| *id != self.ticket); self.pool.wake.notify_waiters();
} } }
pub(crate) struct Lease { pool: Arc<Pool>, slot: usize }
impl Lease {
    pub(crate) fn slot(&self) -> usize { self.slot }
    pub(crate) async fn scope<T>(&self, work: impl Future<Output=T>) -> T { SLOT.scope(self.slot, work).await }
}
impl Drop for Lease { fn drop(&mut self) { self.pool.state.lock().unwrap().active[self.slot] = None; self.pool.wake.notify_waiters(); } }

/// Use the existing confirmed-equivalence resolver. Every member key is retained,
/// so a later family merge intersects an earlier lease's original post identity.
/// Incomplete affinity evidence never establishes independence.
pub(crate) fn capture(d: &Value, job: &Value) -> Option<Value> {
    capture_with_families(d,job,&family_index(d)?)
}
fn family_index(d:&Value)->Option<std::collections::BTreeMap<String,String>> {
    let all: BTreeSet<String> = list(d,"posts").iter().filter_map(|p|p["id"].as_str().map(str::to_owned)).collect();
    knowledge::validate_catalog(d).ok()?;
    knowledge::preparation_families(d,&all,&now()).ok()
}
fn capture_with_families(d:&Value,job:&Value,families:&std::collections::BTreeMap<String,String>)->Option<Value> {
    let reservation = job.get("scopeReservation")?;
    if reservation["version"] != 1 { return None; }
    let request = &job["prepareBundle"]["request"];
    let selected: BTreeSet<String> = request["items"].as_array()?.iter()
        .map(|item| item["postId"].as_str().filter(|id| !id.is_empty()).map(str::to_owned)).collect::<Option<_>>()?;
    if selected.is_empty() { return None; }
    let selected_families: BTreeSet<_> = selected.iter().map(|id|families.get(id).cloned()).collect::<Option<_>>()?;
    let keys: BTreeSet<String> = families.iter().filter(|(_,family)|selected_families.contains(*family))
        .map(|(post,_)|json!([d["account"],"post",post]).to_string()).collect();
    Some(json!({"version":1,"account":d["account"],"prepareBundleDigest":job["prepareBundle"]["digest"],
        "reservationKeysDigest":reservation["keysDigest"],"keys":keys}))
}
pub(crate) fn keys(d: &Value, job: &Value) -> Option<BTreeSet<String>> {
    keys_with_families(d,job,&family_index(d)?)
}
fn keys_with_families(d:&Value,job:&Value,families:&std::collections::BTreeMap<String,String>)->Option<BTreeSet<String>> {
    let saved = job.get("preparationWorkerScope")?;
    if *saved != capture_with_families(d,job,families)? { return None; }
    saved["keys"].as_array()?.iter().map(|key|key.as_str().map(str::to_owned)).collect()
}
pub(crate) fn pending_conflict(d: &Value, job: &Value, legacy_discussion_blocks: bool) -> bool {
    // One catalog/resolver pass for the whole guard, rather than one per job.
    let families=family_index(d);
    let scope=|job:&Value|families.as_ref().and_then(|families| {
        if job["purpose"]=="public_fact_followup" {fact_followup::worker_scope_keys(d,job,families)}
        else {keys_with_families(d,job,families)}
    });
    let own = scope(job);
    list(d,"jobs").iter().any(|other|other["id"]!=job["id"] && other["kind"]=="assistant"
        && !(own.is_some() && other["purpose"]=="discussion")
        && (legacy_discussion_blocks || other["purpose"]!="discussion")
        && matches!(other["status"].as_str(),Some("running"|"queued")) && overlaps(&own,&scope(other)))
}

/// Durable admission budget, before a model job is created. Execution leases
/// remain authoritative too; completed paid reservations are checked separately.
pub(crate) struct AutomaticAdmission {
    available: bool,
    active: usize,
    families: Option<std::collections::BTreeMap<String,String>>,
    occupied: BTreeSet<String>,
}
impl AutomaticAdmission {
    pub(crate) fn capture(d: &Value, width: usize) -> Self {
        let active:Vec<_> = list(d,"jobs").iter().filter(|job| job["kind"]=="assistant" && job["purpose"]!="discussion"
            && matches!(job["status"].as_str(),Some("queued"|"running"))).collect();
        // Research executes under assistant_chat_gate rather than a preparation
        // lease. One proven research attempt may coexist even at pool width 1.
        let research=active.iter().filter(|job|job["purpose"]=="public_fact_followup").count();
        let prepare=active.len()-research;
        let mut view = Self { available: (1..=MAX_WORKERS).contains(&width) && prepare<width && research<=1,
            active: active.len(), families: None, occupied: BTreeSet::new() };
        // A full pool needs no affinity/catalog reconstruction. An empty pool
        // retains the ordinary exclusive fallback for incomplete affinity.
        if !view.available || active.is_empty() {return view;}
        view.families = family_index(d);
        for job in active {
            // Historical/unverifiable research and reviews remain exclusive.
            // A missing/tampered ownership record is never spare capacity.
            if job["purpose"]=="public_fact_followup" {
                if let Some(keys)=view.families.as_ref().and_then(|families|fact_followup::worker_scope_keys(d,job,families)) {
                    view.occupied.extend(keys);
                } else {view.available=false;}
                continue;
            }
            let valid = matches!(job["purpose"].as_str(),Some("engine_prepare"|"auto_prepare"))
                && job["id"].as_str().is_some_and(|id| preparation_reservations::capture(d,id)
                    .is_ok_and(|reservation| reservation==job["scopeReservation"]));
            let keys = valid.then(|| view.families.as_ref().and_then(|families| keys_with_families(d,job,families))).flatten();
            if let Some(keys) = keys { view.occupied.extend(keys); } else { view.available = false; }
        }
        view
    }
    pub(crate) fn available(&self) -> bool { self.available }
    pub(crate) fn permits(&self, d: &Value, item: &Value) -> bool {
        if !self.available { return false; }
        // Preserve the existing exclusive fallback when there is no active job.
        if self.active == 0 { return true; }
        let Some(post) = item["postId"].as_str().filter(|post| !post.is_empty()) else { return false; };
        self.families.as_ref().is_some_and(|families| families.contains_key(post))
            && !self.occupied.contains(&json!([d["account"],"post",post]).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn family(key:&str)->Option<BTreeSet<String>> { Some(BTreeSet::from([key.into()])) }
    #[tokio::test]
    async fn distinct_families_overlap_and_same_family_waits_without_blocking_other_work() {
        let pool=Arc::new(Pool::new(2).unwrap()); let first=pool.acquire(family("a")).await;
        let waiting=tokio::spawn({let pool=pool.clone();async move{pool.acquire(family("a")).await}});
        tokio::task::yield_now().await;
        let second=tokio::time::timeout(std::time::Duration::from_secs(1),pool.acquire(family("b"))).await.unwrap();
        assert_ne!(first.slot(),second.slot()); assert!(!waiting.is_finished());
        drop(first);let resumed=waiting.await.unwrap();assert_eq!(resumed.slot(),0);
        drop(second);drop(resumed);
    }
    #[tokio::test]
    async fn capacity_cancelled_waiter_and_legacy_exclusive_preserve_slots() {
        let pool=Arc::new(Pool::new(2).unwrap());let first=pool.acquire(family("a")).await;let second=pool.acquire(family("b")).await;
        let waiter=tokio::spawn({let pool=pool.clone();async move{pool.acquire(None).await}});tokio::task::yield_now().await;
        assert!(!waiter.is_finished());waiter.abort();let _=waiter.await;drop(first);drop(second);
        let exclusive=pool.acquire(None).await;
        assert!(tokio::time::timeout(std::time::Duration::from_millis(30),pool.acquire(family("c"))).await.is_err());
        drop(exclusive);let next=pool.acquire(family("c")).await;
        assert_eq!(next.scope(async{current_slot()}).await,Some(next.slot()));assert_eq!(current_slot(),None);
    }
    #[test]
    fn configured_count_is_bounded_and_default_compatible() {
        assert_eq!(Pool::default().width(),1);assert!(Pool::new(0).is_err());assert!(Pool::new(MAX_WORKERS+1).is_err());
    }
    #[test]
    fn family_scope_is_bound_to_capture_and_unconfirmed_copies_do_not_merge() {
        let mut d=crate::engine_prepare::tests::fixture(false);
        let first=crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["ready".into()],instruction:None}).unwrap();
        let second=crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["media".into()],instruction:None}).unwrap();
        let a=keys(&d,row(&d,"jobs",&first.job_id).unwrap()).expect("captured family");
        let b=keys(&d,row(&d,"jobs",&second.job_id).unwrap()).expect("independent family");assert!(a.is_disjoint(&b));
        assert!(!pending_conflict(&d,row(&d,"jobs",&first.job_id).unwrap(),true));
        let mut changed=d.clone();row_mut(&mut changed,"jobs",&first.job_id).unwrap()["preparationWorkerScope"]["prepareBundleDigest"]=json!("changed");
        assert!(keys(&changed,row(&changed,"jobs",&first.job_id).unwrap()).is_none());
        assert!(pending_conflict(&changed,row(&changed,"jobs",&first.job_id).unwrap(),true),"unproven ownership stays exclusive");
        let mut legacy=d;row_mut(&mut legacy,"jobs",&second.job_id).unwrap().as_object_mut().unwrap().remove("preparationWorkerScope");
        assert!(pending_conflict(&legacy,row(&legacy,"jobs",&first.job_id).unwrap(),true),"old paid work retains its conservative barrier");
    }
}
