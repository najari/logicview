//! An opened device kept between captures, so a second capture right after
//! the first does not open the device again. The handle is let go after
//! [`IDLE_RELEASE`] without a capture: while it is held, no other program
//! (PulseView, lvcli, another logicview or MCP server) can open the analyzer.

use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use crate::Fx2lafw;

/// How long an unused handle is kept.
pub const IDLE_RELEASE: Duration = Duration::from_secs(5);

/// The device handle kept between captures (see the module docs).
pub type DeviceSlot = Kept<Fx2lafw>;

/// A value that is dropped once it has been put back and left alone for the
/// slot's idle time. Clones share the value.
pub struct Kept<T> {
    inner: Arc<Inner<T>>,
}

struct Inner<T> {
    idle: Duration,
    state: Mutex<State<T>>,
}

struct State<T> {
    value: Option<T>,
    /// Changes on every take and put, so a timer only drops the value it
    /// was started for.
    generation: u64,
}

impl<T> Clone for Kept<T> {
    fn clone(&self) -> Self {
        Kept {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Send + 'static> Default for Kept<T> {
    fn default() -> Self {
        Kept::new(IDLE_RELEASE)
    }
}

impl<T: Send + 'static> Kept<T> {
    pub fn new(idle: Duration) -> Self {
        Kept {
            inner: Arc::new(Inner {
                idle,
                state: Mutex::new(State {
                    value: None,
                    generation: 0,
                }),
            }),
        }
    }

    /// Take the kept value, if any. It is no longer released by the timer.
    pub fn take(&self) -> Option<T> {
        let mut s = self.lock();
        s.generation += 1;
        s.value.take()
    }

    /// Keep `value` until the next [`take`](Self::take), or drop it after
    /// the idle time.
    pub fn put(&self, value: T) {
        let generation = {
            let mut s = self.lock();
            s.generation += 1;
            s.value = Some(value);
            s.generation
        };
        let weak: Weak<Inner<T>> = Arc::downgrade(&self.inner);
        let idle = self.inner.idle;
        let spawned = thread::Builder::new()
            .name("lv-device-release".into())
            .spawn(move || {
                thread::sleep(idle);
                let Some(inner) = weak.upgrade() else { return };
                let released = {
                    let mut s = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    if s.generation == generation {
                        s.value.take()
                    } else {
                        None
                    }
                };
                if released.is_some() {
                    log::info!(
                        "released the device after {} s without a capture",
                        idle.as_secs_f64()
                    );
                }
            });
        if spawned.is_err() {
            // Without a timer, holding on would block other programs.
            self.take();
        }
    }

    /// Whether a value is kept right now.
    pub fn is_kept(&self) -> bool {
        self.lock().value.is_some()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State<T>> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counted(Arc<AtomicUsize>);
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    const IDLE: Duration = Duration::from_millis(100);
    const PAST_IDLE: Duration = Duration::from_millis(400);

    #[test]
    fn kept_idle_value_is_dropped() {
        let drops = Arc::new(AtomicUsize::new(0));
        let k = Kept::new(IDLE);
        k.put(Counted(drops.clone()));
        assert!(k.is_kept());
        thread::sleep(PAST_IDLE);
        assert!(!k.is_kept());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn kept_value_taken_in_time_is_not_dropped() {
        let drops = Arc::new(AtomicUsize::new(0));
        let k = Kept::new(IDLE);
        k.put(Counted(drops.clone()));
        let v = k.take().expect("kept");
        thread::sleep(PAST_IDLE);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        // Put back again: the old timer must not drop the new value early.
        k.put(v);
        thread::sleep(IDLE / 2);
        assert!(k.is_kept());
        thread::sleep(PAST_IDLE);
        assert!(!k.is_kept());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn kept_old_timer_leaves_a_later_value() {
        let drops = Arc::new(AtomicUsize::new(0));
        let k = Kept::new(Duration::from_millis(300));
        k.put(Counted(drops.clone()));
        thread::sleep(Duration::from_millis(200));
        let v = k.take().expect("kept");
        k.put(v); // new timer; the first fires at 300 ms and must skip it
        thread::sleep(Duration::from_millis(200));
        assert!(k.is_kept(), "dropped by the first timer");
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        thread::sleep(Duration::from_millis(400));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
