//! [SAFETY] Request-path blocking work, capped (DEC-505, `IPC-b`).
//!
//! Every request handler in `api/` that needs the blocking pool goes through
//! [`run`] or [`spawn`] rather than `tokio::task::spawn_blocking`. The pool is
//! shared with the daemon's own control work: the engine's writes
//! (`BoundedWrite::run`, the thermal force's included) and the sensor and
//! OpenFan polls. hyper drops a handler when its client hangs up, but blocking
//! work the handler started cannot be cancelled and runs on. Uncapped, a local
//! client looping connect, request, hang-up queued work faster than the pool
//! drained it, and the engine's next write waited behind the queue without
//! limit (`constants::REQUEST_BLOCKING_MAX_OUTSTANDING` has the measurement).
//!
//! Here a request first takes one of
//! [`REQUEST_BLOCKING_MAX_OUTSTANDING`](crate::constants::REQUEST_BLOCKING_MAX_OUTSTANDING)
//! permits, waiting asynchronously (no thread) when none is free, and the permit
//! moves into the closure, so it is released only when the work itself ends,
//! not when the handler is dropped. Request work can therefore never hold more
//! than that many pool threads, and the rest stay free for the control path.
//!
//! Exempt, each marked `Not capped (DEC-505)` at its call and counted in
//! `every_api_blocking_call_goes_through_the_cap`: work that is already
//! single-flight and already holds hardware when it reaches the pool. That is
//! a diagnostic's steps under the single diagnostic slot (the verify holds the
//! hwmon lease and the engine's pause), the GPU reset and verify under the GPU
//! write lock, and the daemon's own OpenFAN watch loops. Making those wait for a
//! request permit would hold the lease, pause or lock for as long as a flood
//! lasts. Also exempt: `handlers::bounded_profile_io`, which has its own static
//! slots (FFA-b), and the hwmon lease guard's `Drop`, which cannot await.

use tokio::sync::Semaphore;
use tokio::task::{JoinError, JoinHandle};

/// The permits every request-path blocking task takes.
static REQUEST_BLOCKING_SLOTS: Semaphore =
    Semaphore::const_new(crate::constants::REQUEST_BLOCKING_MAX_OUTSTANDING);

/// Run `f` on the blocking pool under the request cap and wait for it: the
/// capped form of `tokio::task::spawn_blocking(f).await`.
pub(crate) async fn run<F, T>(f: F) -> Result<T, JoinError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    spawn(f).await.await
}

/// Start `f` on the blocking pool under the request cap and return its handle,
/// for a caller that awaits it later, bounds the wait, or detaches it. The
/// returned future resolves once a permit is free and the work has started.
pub(crate) async fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    spawn_with(&REQUEST_BLOCKING_SLOTS, f).await
}

/// A failed task in words fit for an error envelope: what failed, and whether
/// it panicked or was cancelled — never the panic's message, which `JoinError`'s
/// `Display` carries and which may hold paths or internal state (`CLAUDE.md
/// § Boundaries`). The whole error, message included, goes to the log.
pub(crate) fn join_failure(what: &str, e: &JoinError) -> String {
    log::error!("{what} task failed: {e}");
    let how = if e.is_panic() {
        "panicked"
    } else {
        "was cancelled"
    };
    format!("{what} task failed ({how})")
}

/// Request-blocking permits free now. For tests that must show a flood
/// reached the cap.
#[doc(hidden)]
pub fn request_permits_available() -> usize {
    REQUEST_BLOCKING_SLOTS.available_permits()
}

/// The request-path permits, for a caller that must pass them to
/// [`spawn_with`] (one that bounds its wait and is tested with its own).
pub(crate) fn request_slots() -> &'static Semaphore {
    &REQUEST_BLOCKING_SLOTS
}

/// [`spawn`] with the permits given, so a test can use its own.
pub(crate) async fn spawn_with<F, T>(slots: &'static Semaphore, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // The semaphore is never closed, so `acquire` cannot fail.
    let permit = slots
        .acquire()
        .await
        .expect("request blocking slots are never closed");
    tokio::task::spawn_blocking(move || {
        // Held for as long as the work runs, whatever happened to the caller.
        let _permit = permit;
        f()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{
        PATH_RESOLVE_MAX_OUTSTANDING, PROFILE_IO_MAX_OUTSTANDING, REQUEST_BLOCKING_MAX_OUTSTANDING,
        TOKIO_MAX_BLOCKING_THREADS,
    };
    use std::time::{Duration, Instant};

    /// A panicking task's message stays in the log: the envelope text names
    /// the task and how it ended, and nothing of the payload.
    #[tokio::test]
    async fn join_failure_keeps_the_panic_message_out() {
        const PAYLOAD: &str = "/var/lib/ventulus/secret-path";
        let err = run(|| -> () { panic!("{PAYLOAD}") })
            .await
            .expect_err("the task panicked");
        // Precondition: the raw error does carry the payload.
        assert!(err.to_string().contains(PAYLOAD), "{err}");

        let text = join_failure("hardware diagnostics", &err);
        assert_eq!(text, "hardware diagnostics task failed (panicked)");

        let cancelled = tokio::spawn(std::future::pending::<()>());
        cancelled.abort();
        let err = cancelled.await.expect_err("aborted");
        assert_eq!(
            join_failure("store", &err),
            "store task failed (was cancelled)"
        );
    }

    /// The budget: request work, with the profile-read slots, can hold at most a
    /// quarter of the pool, so the control path's handful of tasks always finds
    /// a free thread.
    #[test]
    fn request_work_can_hold_at_most_a_quarter_of_the_pool() {
        let request_threads = REQUEST_BLOCKING_MAX_OUTSTANDING
            + PROFILE_IO_MAX_OUTSTANDING
            + PATH_RESOLVE_MAX_OUTSTANDING;
        assert!(
            request_threads * 4 <= TOKIO_MAX_BLOCKING_THREADS,
            "{request_threads} request threads of {TOKIO_MAX_BLOCKING_THREADS}"
        );
    }

    /// The mechanism, on a small pool: many callers start long blocking work
    /// under a cap of 4 and are then dropped, as hyper drops a handler whose
    /// client hung up. The work runs on, still holding its permits, so only 4
    /// threads are ever busy, and blocking work outside the cap (the engine's
    /// write) starts at once. Uncapped, the 64 tasks queue on the 16 threads and
    /// the outside task waits behind them.
    #[test]
    fn work_left_by_dropped_callers_cannot_fill_the_pool() {
        static SLOTS: Semaphore = Semaphore::const_new(4);
        const WORK: Duration = Duration::from_millis(600);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(16)
            .enable_all()
            .build()
            .unwrap();
        static STARTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        runtime.block_on(async {
            let callers: Vec<_> = (0..64)
                .map(|_| {
                    tokio::spawn(async {
                        let handle = spawn_with(&SLOTS, || {
                            STARTED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            std::thread::sleep(WORK);
                        })
                        .await;
                        let _ = handle.await;
                    })
                })
                .collect();
            // Precondition, true with or without the cap: work is running, at
            // least as much as the cap admits.
            let deadline = Instant::now() + Duration::from_secs(5);
            while STARTED.load(std::sync::atomic::Ordering::SeqCst) < 4 {
                assert!(Instant::now() < deadline, "no blocking work started");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            // The callers hang up.
            for caller in &callers {
                caller.abort();
            }

            // The control path's write: a blocking task outside the cap.
            let started = Instant::now();
            tokio::time::timeout(Duration::from_secs(10), tokio::task::spawn_blocking(|| ()))
                .await
                .expect("bounded")
                .unwrap();
            let waited = started.elapsed();
            assert!(
                waited < WORK / 3,
                "blocking work outside the cap waited {waited:?} behind request work"
            );
            // Why: the work kept its permits after its callers were gone.
            assert_eq!(
                SLOTS.available_permits(),
                0,
                "the work keeps its permits after its callers are gone"
            );
        });
        // Let the sleepers finish before the runtime is dropped.
        runtime.shutdown_timeout(Duration::from_secs(10));
    }

    /// Every API call site goes through the cap (DEC-505). A new
    /// `spawn_blocking` in `api/` fails here until it uses [`run`] or [`spawn`],
    /// or is added below with its reason. The exemptions are exact counts, so a
    /// removed one fails too.
    #[test]
    fn every_api_blocking_call_goes_through_the_cap() {
        // (file under src/api, count, why it is exempt)
        const EXEMPT: &[(&str, usize, &str)] = &[
            (
                "blocking.rs",
                2,
                "this module's call, and its test's task outside the cap",
            ),
            (
                "calibration.rs",
                1,
                "OpenFAN calibration write: diagnostic slot",
            ),
            (
                "diagnostic_gates.rs",
                1,
                "diagnostic write: diagnostic slot",
            ),
            (
                "handlers/discovery.rs",
                1,
                "discovery sample: diagnostic slot",
            ),
            (
                "handlers/gpu.rs",
                4,
                "GPU reset and verify: hold the GPU write lock",
            ),
            (
                "handlers/hwmon_ctl.rs",
                4,
                "verify and characterisation: diagnostic slot; the lease guard's Drop cannot await",
            ),
            (
                "handlers/mod.rs",
                1,
                "bounded_profile_io: its own static slots (FFA-b)",
            ),
            (
                "handlers/openfan.rs",
                3,
                "calibration start: diagnostic slot; the adoption loop's enumerate and \
                 the single-flight rescan probe the daemon's own loops run through",
            ),
            (
                "handlers/openfan_firmware.rs",
                3,
                "the daemon's own silent-board and recovery watch loops",
            ),
            (
                "handlers/stall_probe.rs",
                2,
                "stall-probe reads: diagnostic slot",
            ),
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
        let mut found = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    // Calls only: comment lines dropped, and the needle split so
                    // this test's own text is not a match.
                    let needle = ["spawn_", "blocking("].concat();
                    let count = text
                        .lines()
                        .filter(|l| !l.trim_start().starts_with("//"))
                        .map(|l| l.matches(needle.as_str()).count())
                        .sum::<usize>();
                    if count > 0 {
                        let rel = path.strip_prefix(&root).unwrap();
                        found.push((rel.to_string_lossy().into_owned(), count));
                    }
                }
            }
        }
        found.sort();
        assert!(
            found.iter().any(|(f, _)| f == "blocking.rs"),
            "precondition: the scan found this module ({found:?})"
        );
        let mut expected: Vec<(String, usize)> = EXEMPT
            .iter()
            .map(|(f, n, _)| ((*f).to_string(), *n))
            .collect();
        expected.sort();
        assert_eq!(
            found, expected,
            "every spawn_blocking in api/ goes through api::blocking (DEC-505)"
        );
    }
}
