//! One process-local writer with bounded preference for personal dialogue writes.
//! The PostgreSQL writer lease and workspace transaction remain the authority.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

const INTERACTIVE_BURST: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Interactive,
    Standard,
}

struct Waiter {
    id: u64,
    ready: oneshot::Sender<()>,
}

#[derive(Default)]
struct State {
    holder: Option<u64>,
    next_id: u64,
    interactive: VecDeque<Waiter>,
    standard: VecDeque<Waiter>,
    interactive_streak: usize,
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
    fn dispatch_next(&mut self) {
        while self.holder.is_none() {
            let class = if !self.standard.is_empty()
                && (self.interactive.is_empty() || self.interactive_streak >= INTERACTIVE_BURST)
            {
                Class::Standard
            } else if !self.interactive.is_empty() {
                Class::Interactive
            } else {
                return;
            };
            let waiter = match class {
                Class::Interactive => self.interactive.pop_front(),
                Class::Standard => self.standard.pop_front(),
            }
            .expect("selected nonempty writer queue");
            // A receiver may disappear before or after this send. Before: skip it.
            // After: Registration::drop releases this exact reservation.
            if waiter.ready.send(()).is_ok() {
                self.holder = Some(waiter.id);
                match class {
                    Class::Interactive => {
                        self.interactive_streak = (self.interactive_streak + 1).min(INTERACTIVE_BURST)
                    }
                    Class::Standard => self.interactive_streak = 0,
                }
            }
        }
    }
}

impl WriterGate {
    pub(crate) async fn acquire(self: &Arc<Self>, class: Class) -> Permit {
        let (id, receiver) = {
            let mut state = self.state.lock().unwrap();
            let id = state.next_id;
            state.next_id = state.next_id.checked_add(1).expect("writer ticket overflow");
            if state.holder.is_none() && state.interactive.is_empty() && state.standard.is_empty() {
                state.holder = Some(id);
                if class == Class::Interactive {
                    state.interactive_streak = (state.interactive_streak + 1).min(INTERACTIVE_BURST);
                } else {
                    state.interactive_streak = 0;
                }
                return Permit { gate: self.clone(), id };
            }
            let (sender, receiver) = oneshot::channel();
            let waiter = Waiter { id, ready: sender };
            match class {
                Class::Interactive => state.interactive.push_back(waiter),
                Class::Standard => state.standard.push_back(waiter),
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
        state.standard.retain(|waiter| waiter.id != id);
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
        for _ in 0..1000 {
            {
                let state = gate.state.lock().unwrap();
                if state.interactive.len() == interactive && state.standard.len() == standard { return; }
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
                let _permit = gate.acquire(if n % 2 == 0 { Class::Interactive } else { Class::Standard }).await;
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
}
