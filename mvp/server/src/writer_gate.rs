//! One process-local writer with bounded preference for operator writes and
//! durable operation evidence/outcomes and preparation checkpoints.
//! The current holder is never preempted.
//! The PostgreSQL writer lease and workspace transaction remain the authority.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

const INTERACTIVE_BURST: usize = 3;
// Source hydration is expensive. Let already waiting admissions/checkpoints
// progress before another source snapshot, while guaranteeing source progress.
const FOREGROUND_BURST: usize = 3;
// Reserve a bounded scheduling quantum for evidence/outcome writes from the
// four-wide dispatch wave. Four writes per slot is a quantum, not a promise
// that all external/readback phases of a wave are already waiting here.
pub(crate) const SETTLEMENT_BURST: usize = crate::dispatch_wave::MAX_IN_FLIGHT * 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Settlement,
    Interactive,
    Standard,
    SourceSnapshot,
}

struct Waiter {
    id: u64,
    ready: oneshot::Sender<()>,
}

#[derive(Default)]
struct State {
    holder: Option<u64>,
    next_id: u64,
    settlement: VecDeque<Waiter>,
    interactive: VecDeque<Waiter>,
    standard: VecDeque<Waiter>,
    source_snapshot: VecDeque<Waiter>,
    interactive_streak: usize,
    foreground_streak: usize,
    settlement_streak: usize,
}

#[derive(Default)]
pub(crate) struct WriterGate {
    state: Mutex<State>,
}

pub(crate) struct Permit {
    gate: Arc<WriterGate>,
    id: u64,
}

struct Registration {
    gate: Arc<WriterGate>,
    id: u64,
    armed: bool,
}

impl State {
    fn record_grant(&mut self, class: Class) {
        if class == Class::Settlement {
            self.settlement_streak = (self.settlement_streak + 1).min(SETTLEMENT_BURST);
            // The normal scheduler retains both debts across settlement work.
            // Charging foreground debt here could repeatedly select Source at
            // every boundary and starve Standard/Interactive indefinitely.
            return;
        }
        self.settlement_streak = 0;
        match class {
            Class::Settlement => unreachable!(),
            Class::Interactive => {
                self.interactive_streak = (self.interactive_streak + 1).min(INTERACTIVE_BURST);
            }
            Class::Standard => self.interactive_streak = 0,
            // Do not reset Interactive's debt to Standard. Otherwise repeated
            // source grants could let Interactive starve a Standard admission.
            Class::SourceSnapshot => (),
        }
        self.foreground_streak = if class == Class::SourceSnapshot || self.source_snapshot.is_empty() {
            0
        } else {
            (self.foreground_streak + 1).min(FOREGROUND_BURST)
        };
    }

    fn dispatch_next(&mut self) {
        while self.holder.is_none() {
            let normal_waiting = !self.interactive.is_empty() || !self.standard.is_empty()
                || !self.source_snapshot.is_empty();
            let class = if !self.settlement.is_empty()
                && (self.settlement_streak < SETTLEMENT_BURST || !normal_waiting)
            {
                Class::Settlement
            } else if !self.source_snapshot.is_empty()
                && ((self.interactive.is_empty() && self.standard.is_empty())
                    || self.foreground_streak >= FOREGROUND_BURST)
            {
                Class::SourceSnapshot
            } else if !self.standard.is_empty()
                && (self.interactive.is_empty() || self.interactive_streak >= INTERACTIVE_BURST)
            {
                Class::Standard
            } else if !self.interactive.is_empty() {
                Class::Interactive
            } else {
                return;
            };
            let waiter = match class {
                Class::Settlement => self.settlement.pop_front(),
                Class::Interactive => self.interactive.pop_front(),
                Class::Standard => self.standard.pop_front(),
                Class::SourceSnapshot => self.source_snapshot.pop_front(),
            }
            .expect("selected nonempty writer queue");
            // A receiver may disappear before or after this send. Before: skip it.
            // After: Registration::drop releases this exact reservation.
            if waiter.ready.send(()).is_ok() {
                self.holder = Some(waiter.id);
                self.record_grant(class);
            }
        }
    }
}

impl WriterGate {
    /// Fixture observation only: a pending outer writer future may still be in
    /// its lifecycle metadata read, rather than registered at this gate.
    #[cfg(test)]
    pub(crate) fn queued_count(&self, class: Class) -> usize {
        let state = self.state.lock().unwrap();
        match class {
            Class::Settlement => state.settlement.len(),
            Class::Interactive => state.interactive.len(),
            Class::Standard => state.standard.len(),
            Class::SourceSnapshot => state.source_snapshot.len(),
        }
    }
    pub(crate) async fn acquire(self: &Arc<Self>, class: Class) -> Permit {
        let (id, receiver) = {
            let mut state = self.state.lock().unwrap();
            let id = state.next_id;
            state.next_id = state.next_id.checked_add(1).expect("writer ticket overflow");
            if state.holder.is_none() && state.interactive.is_empty() && state.standard.is_empty()
                && state.source_snapshot.is_empty() && state.settlement.is_empty() {
                state.holder = Some(id);
                state.record_grant(class);
                return Permit { gate: self.clone(), id };
            }
            let (sender, receiver) = oneshot::channel();
            let waiter = Waiter { id, ready: sender };
            match class {
                Class::Settlement => state.settlement.push_back(waiter),
                Class::Interactive => state.interactive.push_back(waiter),
                Class::Standard => state.standard.push_back(waiter),
                Class::SourceSnapshot => state.source_snapshot.push_back(waiter),
            }
            state.dispatch_next();
            (id, receiver)
        };
        let mut registration = Registration { gate: self.clone(), id, armed: true };
        receiver.await.expect("writer gate reservation sender dropped");
        registration.armed = false;
        Permit { gate: self.clone(), id }
    }

    fn release(&self, id: u64) {
        let mut state = self.state.lock().unwrap();
        assert_eq!(state.holder, Some(id), "writer permit ownership changed");
        state.holder = None;
        state.dispatch_next();
    }

    fn cancel(&self, id: u64) {
        let mut state = self.state.lock().unwrap();
        if state.holder == Some(id) {
            state.holder = None;
            state.dispatch_next();
            return;
        }
        state.interactive.retain(|waiter| waiter.id != id);
        state.settlement.retain(|waiter| waiter.id != id);
        state.standard.retain(|waiter| waiter.id != id);
        state.source_snapshot.retain(|waiter| waiter.id != id);
        if state.source_snapshot.is_empty() { state.foreground_streak = 0; }
        state.dispatch_next();
    }
}

impl Drop for Permit {
    fn drop(&mut self) { self.gate.release(self.id); }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if self.armed { self.gate.cancel(self.id); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use tokio::sync::mpsc;

    async fn queued(gate: &WriterGate, interactive: usize, standard: usize) {
        queued_classes(gate, interactive, standard, 0).await;
    }

    async fn queued_classes(gate: &WriterGate, interactive: usize, standard: usize, source: usize) {
        queued_all(gate, 0, interactive, standard, source).await;
    }

    async fn queued_all(gate: &WriterGate, settlement: usize, interactive: usize, standard: usize, source: usize) {
        for _ in 0..1000 {
            {
                let state = gate.state.lock().unwrap();
                if state.settlement.len() == settlement && state.interactive.len() == interactive && state.standard.len() == standard
                    && state.source_snapshot.len() == source { return; }
            }
            tokio::task::yield_now().await;
        }
        panic!("writer waiters did not queue");
    }

    fn waiter(gate: &Arc<WriterGate>, class: Class, label: &'static str, out: mpsc::UnboundedSender<&'static str>) -> tokio::task::JoinHandle<()> {
        let gate = gate.clone();
        tokio::spawn(async move {
            let _permit = gate.acquire(class).await;
            out.send(label).unwrap();
        })
    }

    #[tokio::test]
    async fn ready_four_wide_settlement_pipeline_defers_sources_until_finite_work_finishes() {
        let gate = Arc::new(WriterGate::default());
        let initial = gate.acquire(Class::SourceSnapshot).await;
        let initial_id = gate.state.lock().unwrap().holder;
        let (out, mut rx) = mpsc::unbounded_channel();
        let source = waiter(&gate, Class::SourceSnapshot, "source", out);
        queued_classes(&gate, 0, 0, 1).await;
        let mut slots = Vec::new();
        for _ in 0..crate::dispatch_wave::MAX_IN_FLIGHT {
            let mut next = Box::pin(gate.acquire(Class::Settlement));
            std::future::poll_fn(|cx| {
                assert!(next.as_mut().poll(cx).is_pending()); Poll::Ready(())
            }).await;
            slots.push(Some(next));
        }
        assert_eq!(gate.state.lock().unwrap().holder, initial_id);
        assert!(rx.try_recv().is_err(), "current source holder is not preempted");
        drop(initial);
        // Synthetic ready pipeline: each slot queues its next durable phase
        // before releasing the current permit. No external-delay guarantee is
        // implied; a real provider gap can leave this queue empty.
        let mut completed = 0;
        for phase in 0..4 {
            for slot in &mut slots {
                let permit = slot.take().unwrap().await;
                assert!(rx.try_recv().is_err(), "source interrupted ready settlement pipeline");
                completed += 1;
                if phase < 3 {
                    let mut next = Box::pin(gate.acquire(Class::Settlement));
                    std::future::poll_fn(|cx| {
                        assert!(next.as_mut().poll(cx).is_pending()); Poll::Ready(())
                    }).await;
                    *slot = Some(next);
                }
                drop(permit);
            }
        }
        assert_eq!(completed, SETTLEMENT_BURST);
        assert_eq!(rx.recv().await, Some("source"));
        source.await.unwrap();
    }

    #[tokio::test]
    async fn sustained_settlement_preserves_both_normal_scheduler_debts() {
        let gate = Arc::new(WriterGate::default());
        let initial = gate.acquire(Class::SourceSnapshot).await;
        let (out, mut rx) = mpsc::unbounded_channel();
        let mut tasks = Vec::new();
        for n in 1..=6 {
            tasks.push(waiter(&gate, Class::Interactive, "interactive", out.clone()));
            queued_all(&gate, 0, n, 0, 0).await;
        }
        for n in 1..=4 {
            tasks.push(waiter(&gate, Class::Standard, "standard", out.clone()));
            queued_all(&gate, 0, 6, n, 0).await;
        }
        for n in 1..=4 {
            tasks.push(waiter(&gate, Class::SourceSnapshot, "source", out.clone()));
            queued_all(&gate, 0, 6, 4, n).await;
        }
        for n in 1..=SETTLEMENT_BURST * 6 {
            tasks.push(waiter(&gate, Class::Settlement, "settlement", out.clone()));
            queued_all(&gate, n, 6, 4, 4).await;
        }
        drop(initial);
        for normal in ["interactive", "interactive", "interactive", "source", "standard", "interactive"] {
            for _ in 0..SETTLEMENT_BURST { assert_eq!(rx.recv().await, Some("settlement")); }
            assert_eq!(rx.recv().await, Some(normal));
        }
        for task in tasks { task.await.unwrap(); }
    }

    #[tokio::test]
    async fn settlement_cancellation_releases_queued_and_delivered_reservations() {
        let gate = Arc::new(WriterGate::default());
        let initial = gate.acquire(Class::Standard).await;
        let mut pending = Box::pin(gate.acquire(Class::Settlement));
        std::future::poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending()); Poll::Ready(())
        }).await;
        queued_all(&gate, 1, 0, 0, 0).await;
        drop(pending);
        queued_all(&gate, 0, 0, 0, 0).await;
        let mut delivered = Box::pin(gate.acquire(Class::Settlement));
        std::future::poll_fn(|cx| {
            assert!(delivered.as_mut().poll(cx).is_pending()); Poll::Ready(())
        }).await;
        drop(initial);
        assert_eq!(gate.state.lock().unwrap().holder, Some(2));
        drop(delivered);
        let _next = tokio::time::timeout(std::time::Duration::from_secs(1),
            gate.acquire(Class::SourceSnapshot)).await.unwrap();
    }

    #[tokio::test]
    async fn interactive_precedes_queued_standard_and_fifo_is_preserved() {
        let gate = Arc::new(WriterGate::default());
        let held = gate.acquire(Class::Standard).await;
        let (out, mut rx) = mpsc::unbounded_channel();
        let a = waiter(&gate, Class::Standard, "s1", out.clone()); queued(&gate, 0, 1).await;
        let b = waiter(&gate, Class::Standard, "s2", out.clone()); queued(&gate, 0, 2).await;
        let c = waiter(&gate, Class::Interactive, "i1", out.clone()); queued(&gate, 1, 2).await;
        let d = waiter(&gate, Class::Interactive, "i2", out); queued(&gate, 2, 2).await;
        drop(held);
        assert_eq!(rx.recv().await, Some("i1"));
        assert_eq!(rx.recv().await, Some("i2"));
        assert_eq!(rx.recv().await, Some("s1"));
        assert_eq!(rx.recv().await, Some("s2"));
        for task in [a, b, c, d] { task.await.unwrap(); }
    }

    #[tokio::test]
    async fn standard_is_granted_after_three_interactive_writes() {
        let gate = Arc::new(WriterGate::default());
        let held = gate.acquire(Class::Standard).await;
        let (out, mut rx) = mpsc::unbounded_channel();
        let standard = waiter(&gate, Class::Standard, "standard", out.clone()); queued(&gate, 0, 1).await;
        let mut tasks = Vec::new();
        for label in ["i1", "i2", "i3", "i4"] {
            tasks.push(waiter(&gate, Class::Interactive, label, out.clone()));
            queued(&gate, tasks.len(), 1).await;
        }
        drop(held);
        for label in ["i1", "i2", "i3", "standard", "i4"] { assert_eq!(rx.recv().await, Some(label)); }
        standard.await.unwrap();
        for task in tasks { task.await.unwrap(); }
    }

    #[tokio::test]
    async fn durable_receipts_preserve_dialogue_fifo_and_background_progress_without_preemption() {
        let gate = Arc::new(WriterGate::default());
        let held = gate.acquire(Class::Standard).await;
        let holder_id = gate.state.lock().unwrap().holder;
        let (out, mut rx) = mpsc::unbounded_channel();
        let background = waiter(&gate, Class::Standard, "background", out.clone());
        queued(&gate, 0, 1).await;
        // An earlier personal dialogue write must retain its place when the
        // send lifecycle adds evidence, provisional outcome and final outcome.
        let mut tasks = Vec::new();
        for label in ["dialogue", "evidence", "provisional-outcome", "final-outcome"] {
            tasks.push(waiter(&gate, Class::Interactive, label, out.clone()));
            queued(&gate, tasks.len(), 1).await;
        }
        assert_eq!(gate.state.lock().unwrap().holder, holder_id);
        assert!(matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "priority must not preempt the existing source transaction");
        drop(held);
        for label in ["dialogue", "evidence", "provisional-outcome", "background", "final-outcome"] {
            assert_eq!(rx.recv().await, Some(label));
        }
        background.await.unwrap();
        for task in tasks { task.await.unwrap(); }
    }

    #[tokio::test]
    async fn preparation_checkpoints_share_receipt_bursts_and_preserve_background_progress() {
        let gate = Arc::new(WriterGate::default());
        let held = gate.acquire(Class::Standard).await;
        let holder_id = gate.state.lock().unwrap().holder;
        let (out, mut rx) = mpsc::unbounded_channel();
        let background_one = waiter(&gate, Class::Standard, "source-one", out.clone());
        queued(&gate, 0, 1).await;
        let background_two = waiter(&gate, Class::Standard, "source-two", out.clone());
        queued(&gate, 0, 2).await;
        let mut tasks = Vec::new();
        for label in ["dialogue", "operation-receipt", "first-pass", "review-init", "review-reserve", "review-save"] {
            tasks.push(waiter(&gate, Class::Interactive, label, out.clone()));
            queued(&gate, tasks.len(), 2).await;
        }
        assert_eq!(gate.state.lock().unwrap().holder, holder_id);
        assert!(matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "checkpoint priority must not preempt the active source transaction");
        drop(held);
        // Preparation does not get its own burst allowance or pass earlier
        // dialogue/receipt writes. Background FIFO progresses after each burst.
        for label in ["dialogue", "operation-receipt", "first-pass", "source-one",
            "review-init", "review-reserve", "review-save", "source-two"] {
            assert_eq!(rx.recv().await, Some(label));
        }
        background_one.await.unwrap();
        background_two.await.unwrap();
        for task in tasks { task.await.unwrap(); }
    }

    #[tokio::test]
    async fn source_snapshots_yield_to_waiting_admissions_without_preempting_holder() {
        let gate = Arc::new(WriterGate::default());
        let held = gate.acquire(Class::SourceSnapshot).await;
        let holder = gate.state.lock().unwrap().holder;
        let (out, mut rx) = mpsc::unbounded_channel();
        let source_one = waiter(&gate, Class::SourceSnapshot, "source-one", out.clone());
        queued_classes(&gate, 0, 0, 1).await;
        let source_two = waiter(&gate, Class::SourceSnapshot, "source-two", out.clone());
        queued_classes(&gate, 0, 0, 2).await;
        let approval = waiter(&gate, Class::Standard, "approval", out.clone());
        queued_classes(&gate, 0, 1, 2).await;
        let conductor = waiter(&gate, Class::Standard, "conductor", out.clone());
        queued_classes(&gate, 0, 2, 2).await;
        let receipt = waiter(&gate, Class::Interactive, "receipt", out);
        queued_classes(&gate, 1, 2, 2).await;
        assert_eq!(gate.state.lock().unwrap().holder, holder);
        assert!(matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        drop(held);
        for label in ["receipt", "approval", "conductor", "source-one", "source-two"] {
            assert_eq!(rx.recv().await, Some(label));
        }
        for task in [source_one, source_two, approval, conductor, receipt] { task.await.unwrap(); }
    }

    #[tokio::test]
    async fn source_and_standard_progress_under_a_waiting_interactive_burst() {
        let gate = Arc::new(WriterGate::default());
        let held = gate.acquire(Class::SourceSnapshot).await;
        let (out, mut rx) = mpsc::unbounded_channel();
        let mut tasks = Vec::new();
        for (index, label) in ["source-one", "source-two"].into_iter().enumerate() {
            tasks.push(waiter(&gate, Class::SourceSnapshot, label, out.clone()));
            queued_classes(&gate, 0, 0, index + 1).await;
        }
        for (index, label) in ["admission-one", "admission-two"].into_iter().enumerate() {
            tasks.push(waiter(&gate, Class::Standard, label, out.clone()));
            queued_classes(&gate, 0, index + 1, 2).await;
        }
        for (index, label) in ["i1", "i2", "i3", "i4", "i5", "i6", "i7"].into_iter().enumerate() {
            tasks.push(waiter(&gate, Class::Interactive, label, out.clone()));
            queued_classes(&gate, index + 1, 2, 2).await;
        }
        drop(held);
        // Source is not starved by foreground work. Its grant does not erase
        // the earlier Interactive burst's obligation to serve Standard next.
        for label in ["i1", "i2", "i3", "source-one", "admission-one",
            "i4", "i5", "source-two", "i6", "admission-two", "i7"] {
            assert_eq!(rx.recv().await, Some(label));
        }
        for task in tasks { task.await.unwrap(); }
    }

    #[tokio::test]
    async fn cancelled_source_waiters_release_both_queued_and_delivered_reservations() {
        let gate = Arc::new(WriterGate::default());
        let held = gate.acquire(Class::Standard).await;
        let cancelled = tokio::spawn({let gate=gate.clone(); async move {
            let _permit=gate.acquire(Class::SourceSnapshot).await;
        }});
        queued_classes(&gate, 0, 0, 1).await;
        cancelled.abort(); let _ = cancelled.await;
        queued_classes(&gate, 0, 0, 0).await;
        let mut delivered = Box::pin(gate.acquire(Class::SourceSnapshot));
        std::future::poll_fn(|cx| {
            assert!(matches!(delivered.as_mut().poll(cx), Poll::Pending));
            Poll::Ready(())
        }).await;
        queued_classes(&gate, 0, 0, 1).await;
        drop(held);
        assert_eq!(gate.state.lock().unwrap().holder, Some(2));
        // Cancellation after the send, before the future observes its permit.
        drop(delivered);
        let _next = tokio::time::timeout(std::time::Duration::from_secs(1),
            gate.acquire(Class::Standard)).await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_queued_and_granted_waiters_do_not_strand_gate() {
        let gate = Arc::new(WriterGate::default());
        let held = gate.acquire(Class::Standard).await;
        let queued_task = tokio::spawn({let gate=gate.clone(); async move {let _permit=gate.acquire(Class::Interactive).await;}});
        queued(&gate, 1, 0).await;
        queued_task.abort(); let _ = queued_task.await;
        queued(&gate, 0, 0).await;
        // Manually poll the waiter once. Releasing the holder sends its grant,
        // then dropping the unpolled future cancels that exact reservation.
        let mut granted = Box::pin(gate.acquire(Class::Interactive));
        std::future::poll_fn(|cx| {
            assert!(matches!(granted.as_mut().poll(cx), Poll::Pending));
            Poll::Ready(())
        }).await;
        queued(&gate, 1, 0).await;
        drop(held);
        assert_eq!(gate.state.lock().unwrap().holder, Some(2));
        drop(granted);
        let _next = tokio::time::timeout(std::time::Duration::from_secs(1), gate.acquire(Class::Standard)).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn only_one_writer_holds_the_gate_across_awaits() {
        let gate = Arc::new(WriterGate::default());
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for n in 0..32 {
            let gate = gate.clone();
            let active = active.clone();
            let maximum = maximum.clone();
            tasks.push(tokio::spawn(async move {
                let _permit = gate.acquire(match n % 4 {
                    0 => Class::Interactive, 1 => Class::Standard, 2 => Class::SourceSnapshot,
                    _ => Class::Settlement,
                }).await;
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(current, Ordering::SeqCst);
                tokio::task::yield_now().await;
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for task in tasks { task.await.unwrap(); }
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn closed_receiver_is_skipped_without_losing_the_next_grant() {
        let mut state = State::default();
        let (closed_sender, closed_receiver) = oneshot::channel();
        drop(closed_receiver);
        let (ready_sender, mut ready_receiver) = oneshot::channel();
        state.interactive.push_back(Waiter { id: 10, ready: closed_sender });
        state.interactive.push_back(Waiter { id: 11, ready: ready_sender });
        state.dispatch_next();
        assert_eq!(state.holder, Some(11));
        assert!(ready_receiver.try_recv().is_ok());
        assert_eq!(state.interactive_streak, 1);
    }

    #[test]
    fn closed_settlement_receiver_does_not_consume_burst_or_normal_debt() {
        let mut state = State::default();
        state.interactive_streak = 2;
        state.foreground_streak = 2;
        let (closed_sender, closed_receiver) = oneshot::channel();
        drop(closed_receiver);
        let (ready_sender, mut ready_receiver) = oneshot::channel();
        state.settlement.push_back(Waiter { id: 10, ready: closed_sender });
        state.settlement.push_back(Waiter { id: 11, ready: ready_sender });
        state.dispatch_next();
        assert_eq!(state.holder, Some(11));
        assert!(ready_receiver.try_recv().is_ok());
        assert_eq!(state.settlement_streak, 1);
        assert_eq!(state.interactive_streak, 2);
        assert_eq!(state.foreground_streak, 2);
    }

    #[tokio::test]
    async fn external_phase_gap_does_not_reserve_or_delay_source_admission() {
        let gate = Arc::new(WriterGate::default());
        let settled = gate.acquire(Class::Settlement).await;
        drop(settled);
        // There is no effect-wide lease: source can progress immediately when
        // the next receipt/readback is not yet waiting for the writer.
        let _source = tokio::time::timeout(std::time::Duration::from_secs(1),
            gate.acquire(Class::SourceSnapshot)).await.unwrap();
    }
}
