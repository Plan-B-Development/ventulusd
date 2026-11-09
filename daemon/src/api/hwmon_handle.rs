//! The API's handle on the hwmon PWM controller (FFA-h).
//!
//! The controller's mutex is the lock the engine's and the thermal force's
//! hwmon writes take, and a write that does not return keeps it held
//! (DEC-455). An async handler that took it parked a tokio worker; enough of
//! them starved the engine, and the watchdog killed the daemon. So a handler
//! never takes it on a worker:
//! - **header facts** — the descriptors, frozen at discovery — come from a
//!   snapshot taken when the handle is built, with no lock at all;
//! - **live controller state** (a lease, a revert count) is reached through
//!   [`HwmonHandle::with_controller`], on the blocking pool, waiting at most
//!   [`HWMON_CONTROLLER_WAIT`] and refused as busy after that.
//!
//! Code already on the blocking pool (a diagnostic's bounded writes, the
//! hardware diagnostics build) takes [`HwmonHandle::controller`] directly.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::constants::HWMON_CONTROLLER_WAIT;
use crate::hwmon::pwm_control::{forced_target_ids_of, HwmonPwmController};
use crate::hwmon::pwm_discovery::PwmHeaderDescriptor;

/// The hwmon controller and a lock-free copy of its headers.
#[derive(Clone)]
pub struct HwmonHandle {
    controller: Arc<Mutex<HwmonPwmController>>,
    /// In `HwmonPwmController::headers` order. Exact for the controller's
    /// whole life: nothing changes its header set or a descriptor after
    /// construction, and `/hwmon/rescan` never replaces a running controller.
    headers: Arc<[PwmHeaderDescriptor]>,
}

/// Why [`HwmonHandle::with_controller`] ran nothing within
/// [`HWMON_CONTROLLER_WAIT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerBusy {
    /// The controller's lock did not come free (a hwmon write that has not
    /// returned holds it), or the closure run under it panicked.
    Lock,
    /// No request-blocking permit came free (DEC-505): other requests' blocking
    /// work holds every one. Nothing is wrong with the hardware.
    RequestPool,
}

impl HwmonHandle {
    /// Wrap a controller nothing else holds yet.
    pub fn new(controller: HwmonPwmController) -> Self {
        let headers = controller.headers().into_iter().cloned().collect();
        Self {
            controller: Arc::new(Mutex::new(controller)),
            headers,
        }
    }

    /// The controller itself. **Never lock it on a tokio worker** — use
    /// [`Self::with_controller`] there; this is for the engine, the shutdown
    /// path and code already on the blocking pool.
    pub fn controller(&self) -> &Arc<Mutex<HwmonPwmController>> {
        &self.controller
    }

    /// Every discovered header, as `HwmonPwmController::headers` lists them.
    pub fn headers(&self) -> &[PwmHeaderDescriptor] {
        &self.headers
    }

    /// One header by id.
    pub fn header(&self, id: &str) -> Option<&PwmHeaderDescriptor> {
        self.headers.iter().find(|h| h.id == id)
    }

    /// `HwmonPwmController::forced_target_ids`, from the same definition.
    pub fn forced_target_ids(&self) -> Vec<String> {
        forced_target_ids_of(self.headers.iter())
    }

    /// Run `f` under the controller's lock on the blocking pool, waiting at
    /// most [`HWMON_CONTROLLER_WAIT`] for it. A wait that runs out takes
    /// nothing and runs nothing — `f` never lands late — and the worker that
    /// awaits this is free throughout. `f` must not block: it runs while the
    /// engine may be waiting for the same lock.
    pub async fn with_controller<T, F>(&self, f: F) -> Result<T, ControllerBusy>
    where
        F: FnOnce(&mut HwmonPwmController) -> T + Send + 'static,
        T: Send + 'static,
    {
        self.with_controller_in(crate::api::blocking::request_slots(), f)
            .await
    }

    /// [`Self::with_controller`] with the request-blocking permits given, so a
    /// test can hold its own.
    async fn with_controller_in<T, F>(
        &self,
        slots: &'static tokio::sync::Semaphore,
        f: F,
    ) -> Result<T, ControllerBusy>
    where
        F: FnOnce(&mut HwmonPwmController) -> T + Send + 'static,
        T: Send + 'static,
    {
        let controller = self.controller.clone();
        // DEC-505: ONE deadline covers waiting for a request-blocking permit and
        // then for the lock. `take_verify_lease` calls this holding the engine's
        // verify pause, so an unbounded permit wait would hold the pause for as
        // long as request work keeps every permit busy.
        let deadline = std::time::Instant::now() + HWMON_CONTROLLER_WAIT;
        let Ok(join) = tokio::time::timeout(
            HWMON_CONTROLLER_WAIT,
            crate::api::blocking::spawn_with(slots, move || {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                controller.try_lock_for(left).map(|mut guard| f(&mut guard))
            }),
        )
        .await
        else {
            // No permit in time: nothing was started, so `f` never runs.
            return Err(ControllerBusy::RequestPool);
        };
        match join.await {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(ControllerBusy::Lock),
            Err(e) => {
                log::error!("hwmon controller call failed on the blocking pool: {e}");
                Err(ControllerBusy::Lock)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::cache::StateCache;
    use crate::hwmon::lease::LeaseManager;
    use crate::hwmon::pwm_control::RealSysfsWriter;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    fn handle() -> HwmonHandle {
        HwmonHandle::new(HwmonPwmController::new(
            Vec::new(),
            LeaseManager::new(),
            Box::new(RealSysfsWriter),
            Arc::new(StateCache::new()),
        ))
    }

    /// DEC-505: with every request-blocking permit taken, the call still ends
    /// within its wait, as busy, and `f` never runs, not even later. The verify
    /// takes its lease through here while holding the engine's pause, so an
    /// unbounded permit wait would hold the pause for as long as request work
    /// kept the permits busy.
    #[tokio::test]
    async fn a_busy_request_pool_ends_the_wait_as_busy_and_runs_nothing() {
        static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
        static RAN: AtomicBool = AtomicBool::new(false);
        let held = SLOTS.acquire().await.unwrap();
        let started = Instant::now();
        let result = tokio::time::timeout(
            HWMON_CONTROLLER_WAIT * 3,
            handle().with_controller_in(&SLOTS, |_| RAN.store(true, Ordering::SeqCst)),
        )
        .await
        .expect("bounded by its own wait, not left waiting");
        assert_eq!(
            result.err(),
            Some(ControllerBusy::RequestPool),
            "busy for want of a permit"
        );
        assert!(
            started.elapsed() < HWMON_CONTROLLER_WAIT + Duration::from_millis(500),
            "ended after {:?}",
            started.elapsed()
        );
        drop(held);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!RAN.load(Ordering::SeqCst), "f never runs late");

        // With the permit free, the same call runs.
        assert!(handle()
            .with_controller_in(&SLOTS, |_| RAN.store(true, Ordering::SeqCst))
            .await
            .is_ok());
        assert!(RAN.load(Ordering::SeqCst));
    }
}
