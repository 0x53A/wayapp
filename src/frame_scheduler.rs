//! Frame scheduler for repaint requests
//!
//! Retains a next frame time and signals frame updates accordingly
//!
//! This is not supposed to hold many frame times, but instead only the next
//! frame time. This is how EGUI works.

// Note: Try not to edit this with LLMs, locking logic needs to be verified
// manually.

use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

struct PendingFrame {
    fps_target: f32,
    deadline: Option<Instant>,
    closed: bool,
}

struct SchedulerState {
    // Deadline updates, claiming an elapsed frame, and closing all use this
    // lock. Condvar waits atomically release it, so an earlier request/close
    // cannot be lost between observing state and going to sleep. Spurious
    // wakeups simply recheck the state; callback code runs outside the lock.
    pending: Mutex<PendingFrame>,
    changed: Condvar,
}

/// Schedules frame updates based on egui's repaint requests.
pub(crate) struct FrameScheduler {
    _thread: std::thread::JoinHandle<()>,
    state: Arc<SchedulerState>,
}

impl FrameScheduler {
    pub fn new(emit_frame: impl Fn() + Send + Sync + 'static) -> Self {
        let state = Arc::new(SchedulerState {
            pending: Mutex::new(PendingFrame {
                fps_target: 60.0,
                deadline: None,
                closed: false,
            }),
            changed: Condvar::new(),
        });
        let worker_state = state.clone();
        FrameScheduler {
            _thread: std::thread::spawn(move || {
                let mut pending = worker_state.pending.lock().unwrap();
                while !pending.closed {
                    match pending.deadline {
                        Some(deadline) if deadline <= Instant::now() => {
                            // Claim this frame under the same lock producers use.
                            // Requests made during emit_frame retain a new deadline.
                            pending.deadline = None;
                            drop(pending);
                            emit_frame(); // Never call user code with the lock held.
                            pending = worker_state.pending.lock().unwrap();
                        }
                        Some(deadline) => {
                            (pending, _) = worker_state
                                .changed
                                .wait_timeout(
                                    pending,
                                    deadline.saturating_duration_since(Instant::now()),
                                )
                                .unwrap();
                        }
                        None => pending = worker_state.changed.wait(pending).unwrap(),
                    }
                }
            }),
            state,
        }
    }

    /// Set the target FPS for frame scheduling
    ///
    /// If set zero, the value is clamped to a very low value (0.0001) to avoid
    /// division by zero.
    pub fn set_fps_target(&mut self, fps: f32) {
        self.state.pending.lock().unwrap().fps_target = fps.abs().max(0.0001);
    }

    /// Create scheduler function
    pub fn create_scheduler(&self) -> impl Fn(Duration) + Send + Sync + 'static {
        let state = self.state.clone();
        move |delay: Duration| Self::schedule_frame_at(&state, delay)
    }

    /// Internal method to schedule a frame at a specific duration from now.
    fn schedule_frame_at(state: &SchedulerState, delay: Duration) {
        let mut pending = state.pending.lock().unwrap();
        let min_delay = Duration::from_secs_f32(1.0 / pending.fps_target);
        let delay = delay.max(min_delay);
        // egui can forward Duration::MAX minus predicted frame time. Such
        // valid repaint delays may still exceed Instant's representable range.
        let Some(new_deadline) = Instant::now().checked_add(delay) else {
            return;
        };
        if pending
            .deadline
            .is_none_or(|deadline| new_deadline < deadline)
        {
            pending.deadline = Some(new_deadline);
            state.changed.notify_one();
        }
    }

    /// Schedule a frame update at the specified duration from now.
    #[allow(dead_code)]
    pub fn schedule_frame(&mut self, at: Duration) {
        Self::schedule_frame_at(&self.state, at);
    }
}

impl Drop for FrameScheduler {
    fn drop(&mut self) {
        self.state.pending.lock().unwrap().closed = true;
        self.state.changed.notify_one();
        // The worker owns its state/callback until it exits. Detach rather
        // than joining user code which may be waiting for this owner to drop.
        // An in-flight callback may finish, but no later frame is claimed.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn owner_drop_does_not_wait_for_a_wakeup_callback_waiting_on_its_owner() {
        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        let (finished_tx, finished_rx) = channel();
        let release_rx = Mutex::new(release_rx);
        let callback_lifetime = Arc::new(());
        let lifetime = Arc::downgrade(&callback_lifetime);
        let scheduler = FrameScheduler::new(move || {
            let _callback_lifetime = &callback_lifetime;
            let _ = entered_tx.send(());
            // Rescue a broken synchronous Drop rather than hanging the suite.
            let released = release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .is_ok();
            let _ = finished_tx.send(released);
        });
        let request = scheduler.create_scheduler();
        request(Duration::ZERO);
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        request(Duration::ZERO); // A pending frame must not survive shutdown.
        drop(request);
        drop(scheduler);
        let finished_before_release = finished_rx.try_recv().ok();
        let callback_retained = lifetime.strong_count() != 0;
        let _ = release_tx.send(());
        assert!(
            finished_before_release.is_none(),
            "owner Drop waited for its blocked callback"
        );
        assert!(
            callback_retained,
            "worker lost its callback before it returned"
        );
        assert!(finished_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        let deadline = Instant::now() + Duration::from_secs(2);
        while lifetime.strong_count() != 0 {
            assert!(
                Instant::now() < deadline,
                "closed worker retained its callback"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            entered_rx.try_recv().is_err(),
            "closed worker emitted another callback"
        );
    }

    #[test]
    fn wakeup_callback_can_drop_its_own_scheduler() {
        let slot = Arc::new(Mutex::new(None::<FrameScheduler>));
        let callback_slot = slot.clone();
        let (tx, rx) = channel();
        let scheduler = FrameScheduler::new(move || {
            let scheduler = callback_slot.lock().unwrap().take().unwrap();
            let dropped =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(scheduler)));
            let _ = tx.send(dropped.is_ok());
        });
        let state = Arc::downgrade(&scheduler.state);
        let request = scheduler.create_scheduler();
        *slot.lock().unwrap() = Some(scheduler);
        request(Duration::ZERO);
        drop(request);
        assert!(
            rx.recv_timeout(Duration::from_secs(2))
                .expect("self-drop blocked its own callback"),
            "self-drop tried to join the currently running worker"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while state.strong_count() != 0 {
            assert!(
                Instant::now() < deadline,
                "closed worker retained scheduler state"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn repaint_callback_after_scheduler_drop_is_safe() {
        let (tx, rx) = channel();
        let scheduler = FrameScheduler::new(move || {
            let _ = tx.send(());
        });
        let request = scheduler.create_scheduler();
        drop(scheduler);
        request(Duration::ZERO);
        assert!(
            rx.recv_timeout(Duration::from_secs(1)).is_err(),
            "closed worker emitted a late callback"
        );
    }

    #[test]
    fn earlier_request_interrupts_a_later_wait() {
        let (tx, rx) = channel();
        let mut scheduler = FrameScheduler::new(move || {
            let _ = tx.send(());
        });
        scheduler.set_fps_target(1000.0);
        let request = scheduler.create_scheduler();
        request(Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
        request(Duration::ZERO);
        rx.recv_timeout(Duration::from_millis(500))
            .expect("earlier request did not wake the sleeping scheduler");
    }

    #[test]
    fn wakeup_callback_does_not_hold_pending_frame_lock() {
        let slot = Arc::new(std::sync::OnceLock::<Arc<SchedulerState>>::new());
        let callback_slot = slot.clone();
        let (tx, rx) = channel();
        let mut scheduler = FrameScheduler::new(move || {
            let unlocked = callback_slot.get().unwrap().pending.try_lock().is_ok();
            let _ = tx.send(unlocked);
        });
        assert!(slot.set(scheduler.state.clone()).is_ok());
        scheduler.schedule_frame(Duration::ZERO);
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).unwrap(),
            "callback cannot safely request its next repaint"
        );
    }

    #[test]
    fn wakeup_callback_can_schedule_its_next_frame_without_losing_it() {
        const CHILD: &str = "WAYAPP_REENTRANT_REPAINT_PROBE";
        if std::env::var_os(CHILD).is_none() {
            // A future regression that calls user code under the pending lock
            // would deadlock request(), including scheduler Drop during unwind.
            // Bound the entire probe rather than just its channel observations.
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    std::thread::current().name().unwrap(),
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        assert!(status.success(), "reentrant repaint probe failed: {status}");
                        return;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    state => {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("reentrant repaint probe timed out: {state:?}");
                    }
                }
            }
        }
        use std::sync::OnceLock;
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;
        type Request = Box<dyn Fn(Duration) + Send + Sync>;
        let slot = Arc::new(OnceLock::<Request>::new());
        let callback_slot = slot.clone();
        let frames = AtomicUsize::new(0);
        let (tx, rx) = channel();
        let mut scheduler = FrameScheduler::new(move || {
            let frame = frames.fetch_add(1, Ordering::Relaxed) + 1;
            if frame == 1 {
                let request = callback_slot.get().unwrap();
                request(Duration::ZERO);
                // Later requests must neither overwrite the newly queued
                // immediate frame nor turn into extra wakeups afterward.
                request(Duration::from_secs(20));
            }
            let _ = tx.send(frame);
        });
        scheduler.set_fps_target(1000.0);
        assert!(slot.set(Box::new(scheduler.create_scheduler())).is_ok());
        slot.get().unwrap()(Duration::ZERO);
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), 2);
        assert!(rx.recv_timeout(Duration::from_millis(80)).is_err());
    }

    #[test]
    fn concurrent_repaint_callbacks_are_safe_during_shutdown() {
        let scheduler = FrameScheduler::new(|| {});
        let request = Arc::new(scheduler.create_scheduler());
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let request = request.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                request(Duration::ZERO);
            }));
        }
        barrier.wait();
        drop(scheduler);
        for thread in threads {
            thread
                .join()
                .expect("repaint callback panicked during shutdown");
        }
    }

    #[test]
    fn requests_during_a_blocked_wakeup_coalesce_and_resume() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;
        let (frame_tx, frame_rx) = channel();
        let (release_tx, release_rx) = channel();
        let release_rx = Mutex::new(release_rx);
        let count = AtomicUsize::new(0);
        let mut scheduler = FrameScheduler::new(move || {
            let first = count.fetch_add(1, Ordering::Relaxed) == 0;
            let _ = frame_tx.send(());
            if first {
                // Bound the gate even if a producer/test assertion panics.
                let _ = release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2));
            }
        });
        scheduler.set_fps_target(1000.0);
        let request = scheduler.create_scheduler();
        request(Duration::ZERO);
        frame_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // The callback cannot consume any of these yet. On upstream, each
        // request queues a deadline; its worker then overwrites the early one
        // with the later one. Four requests reproduce this without a flood.
        request(Duration::ZERO);
        for _ in 0..3 {
            request(Duration::from_secs(20));
        }
        release_tx.send(()).unwrap();
        frame_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("pending repaint was lost while callback was blocked");
        assert!(
            frame_rx.recv_timeout(Duration::from_millis(80)).is_err(),
            "requests queued during a callback did not coalesce"
        );
    }
}
