//! AMD GPU fan endpoints: reset to automatic (firmware default) + verify.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;

use super::{error_response, json_ok, AppState};
use crate::api::responses::*;
use crate::constants;

/// POST /gpu/{gpu_id}/fan/reset — reset GPU fan to automatic (firmware default).
pub async fn gpu_reset_fan_handler(
    State(state): State<Arc<AppState>>,
    Path(gpu_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let gpu = state.amd_gpus.iter().find(|g| g.pci_bdf == gpu_id);
    let gpu = match gpu {
        Some(g) => g,
        None => {
            return error_response(
                StatusCode::NOT_FOUND,
                &ErrorEnvelope::validation(format!("GPU not found: {gpu_id}")),
            );
        }
    };

    if let Some(fan_curve_path) = &gpu.fan_curve_path {
        let path = fan_curve_path.clone();
        let zero_rpm = gpu.fan_zero_rpm_path.clone();
        let fan_id = format!("amd_gpu:{gpu_id}");

        // DEC-255: hold the GPU write lock across the whole reset so an engine
        // tick cannot interleave its own multi-write curve commit with ours.
        // The guard is OWNED and moved into the blocking task deliberately — if
        // the client disconnects (the GUI gives this 5 s), the handler future is
        // dropped, and a borrowed guard would be released while the write was
        // still in flight.
        let Some(write_guard) = state
            .cache
            .lock_gpu_writes_soon(constants::GPU_RESET_LOCK_WAIT)
            .await
        else {
            // Held for longer than an engine tick ever holds it, so the holder
            // is a `fan/verify` running its multi-second window. Say so rather
            // than blocking past the GUI's 5 s timeout.
            return error_response(
                StatusCode::CONFLICT,
                &ErrorEnvelope::validation(
                    "a GPU fan verify is in progress — retry once it completes",
                ),
            );
        };
        let cache = state.cache.clone();
        let task_fan_id = fan_id.clone();
        // Not capped (DEC-505): a GPU reset (holds the GPU write lock).
        let result = tokio::task::spawn_blocking(move || {
            let _write_guard = write_guard;
            // DEC-165/DEC-254: relinquish so the engine stops re-asserting the
            // profile's curve and the reset is durable under an active profile.
            // Claimed BEFORE the write so the flag covers it — and claimed HERE,
            // inside the blocking task, because this task is not cancelled when
            // the client goes away. Doing it in the handler left a disconnect
            // able to strand the fan: relinquished, never reset, and skipped by
            // the engine for the rest of the process's life.
            let newly_claimed = cache.relinquish_gpu_fan(&task_fan_id);
            match crate::hwmon::gpu_fan::reset_to_auto(&path, zero_rpm.as_deref()) {
                Ok(()) => {
                    // `GPU-e`: on firmware auto nothing commands the card, so no
                    // duty is recorded (DEC-448's hand-back does the same). A
                    // recorded 0 coalesced away a later 0–4 % profile command.
                    cache.clear_gpu_fan_commanded_pct(&task_fan_id);
                    // DEC-435: back on firmware auto, so no stop resets it again —
                    // a curve LACT puts on it afterwards is left alone.
                    cache.gpu_handback().note_handed_back(&task_fan_id);
                    Ok(())
                }
                Err(e) => {
                    // DEC-255: roll back only what THIS call claimed. An
                    // unconditional rollback lets a second, failing reset clear
                    // the flag a first, successful one owns — handing the fan
                    // back to the engine after the API said it was reset.
                    if newly_claimed {
                        cache.unrelinquish_gpu_fan(&task_fan_id);
                    }
                    Err(e)
                }
            }
            // A panic inside this closure skips the rollback. The panic is
            // contained (DEC-265: only a main-thread panic restores fans), so
            // the fan stays relinquished — not driven by the engine — until the
            // next activation, on whatever the interrupted reset left it at.
        })
        .await;

        match result {
            Ok(Ok(())) => {
                log::info!("GPU {gpu_id} fan reset to auto");
                json_ok(
                    StatusCode::OK,
                    serde_json::json!({
                        "api_version": API_VERSION,
                        "gpu_id": gpu_id,
                        "reset": true,
                    }),
                )
            }
            // M13: hardware_unavailable is a 503.
            Ok(Err(e)) => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &ErrorEnvelope::hardware_unavailable(format!("GPU fan reset failed: {e}")),
            ),
            Err(e) => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &ErrorEnvelope::internal(crate::api::blocking::join_failure("GPU fan reset", &e)),
            ),
        }
    } else if gpu.can_write_legacy_pwm() {
        let hwmon_path = gpu.hwmon_path.clone();
        let fan_id = format!("amd_gpu:{gpu_id}");

        // DEC-255: same shape as the PMFW arm above — see its comments.
        let Some(write_guard) = state
            .cache
            .lock_gpu_writes_soon(constants::GPU_RESET_LOCK_WAIT)
            .await
        else {
            // Held for longer than an engine tick ever holds it, so the holder
            // is a `fan/verify` running its multi-second window. Say so rather
            // than blocking past the GUI's 5 s timeout.
            return error_response(
                StatusCode::CONFLICT,
                &ErrorEnvelope::validation(
                    "a GPU fan verify is in progress — retry once it completes",
                ),
            );
        };
        let cache = state.cache.clone();
        let task_fan_id = fan_id.clone();
        // Not capped (DEC-505): a GPU reset (holds the GPU write lock).
        let result = tokio::task::spawn_blocking(move || {
            let _write_guard = write_guard;
            let newly_claimed = cache.relinquish_gpu_fan(&task_fan_id);
            match crate::hwmon::gpu_fan::reset_legacy_to_auto(&hwmon_path) {
                Ok(()) => {
                    // `GPU-e`: as the PMFW arm — on auto, no duty is recorded.
                    cache.clear_gpu_fan_commanded_pct(&task_fan_id);
                    Ok(())
                }
                Err(e) => {
                    if newly_claimed {
                        cache.unrelinquish_gpu_fan(&task_fan_id);
                    }
                    Err(e)
                }
            }
        })
        .await;

        match result {
            Ok(Ok(())) => {
                log::info!("GPU {gpu_id} legacy fan reset to auto");
                json_ok(
                    StatusCode::OK,
                    serde_json::json!({
                        "api_version": API_VERSION,
                        "gpu_id": gpu_id,
                        "reset": true,
                    }),
                )
            }
            Ok(Err(e)) => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &ErrorEnvelope::hardware_unavailable(format!("GPU legacy fan reset failed: {e}")),
            ),
            Err(e) => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &ErrorEnvelope::internal(crate::api::blocking::join_failure("GPU fan reset", &e)),
            ),
        }
    } else {
        error_response(
            StatusCode::BAD_REQUEST,
            &ErrorEnvelope::feature_unavailable(unsupported_fan_control_message(gpu)),
        )
    }
}

/// Record what a completed GPU restore left the hardware at (DEC-297).
///
/// **This must mirror `restore_pmfw`/`restore_legacy`'s own match arms.** Both
/// treat `Some(p) if p > 0` as "put the static speed back" and everything else
/// as "hand the fan to firmware-auto" — so stamping only the `Some(p)` case left
/// an auto-restored fan reporting the TEST SPEED forever, which is worse than not
/// stamping at all: `GpuBackend::apply` coalesces within `GPU_COALESCE_DELTA_PCT`,
/// so a later profile command near that value is skipped and the card silently
/// stays on the firmware curve.
///
/// The auto branch **clears** the duty rather than recording 0 (`GPU-e`,
/// DEC-468): a 0 coalesced away the same way, just for a profile command of
/// 0–4 %. "On firmware auto" means nothing commands the card, which is how the
/// engine's hand-back (DEC-448) and `gpu_reset_fan_handler` record it too, and
/// `restore_pmfw`/`restore_legacy` already read `None` as "restore to auto".
///
/// A FAILED restore is deliberately not stamped: the hardware is still at the
/// test speed, the cache already says so, and it must keep saying so or the
/// engine's coalescing will not correct it.
fn stamp_restored_pct(
    cache: &crate::health::cache::StateCache,
    fan_id: &str,
    prior_pct: Option<u8>,
    restore_failed: bool,
) {
    if restore_failed {
        return;
    }
    match prior_pct {
        Some(p) if p > 0 => cache.set_gpu_fan_commanded_pct(fan_id, p),
        _ => cache.clear_gpu_fan_commanded_pct(fan_id),
    }
}

/// Drop the card from the PMFW hand-back list when a verify's restore put it
/// back on firmware auto (DEC-435) — the same arms as `restore_pmfw`.
///
/// A restore to a prior static speed keeps it: the engine drove the card before
/// the verify and does again after it. A failed restore keeps it too: the card
/// is still at the test speed, so the stop must reset it.
fn note_pmfw_restored(
    cache: &crate::health::cache::StateCache,
    fan_id: &str,
    prior_pct: Option<u8>,
    restore_failed: bool,
) {
    if !restore_failed && !matches!(prior_pct, Some(p) if p > 0) {
        cache.gpu_handback().note_handed_back(fan_id);
    }
}

/// What a GPU verify's uncancellable sequence produced (DEC-297).
///
/// The whole test-write -> settle -> read-back -> restore sequence runs inside one
/// `spawn_blocking`, so it cannot `return` a response directly. It reports what
/// happened and the handler builds the response from it.
enum GpuVerifySequence {
    /// The test write itself was rejected; `final_state` is the post-restore read.
    WriteFailed {
        initial: GpuVerifyState,
        final_state: GpuVerifyState,
        test_speed: u8,
        restore_failed: bool,
        /// The complete, arm-specific message for the response — PMFW and legacy
        /// word their rejection differently.
        details: String,
    },
    Completed {
        initial: GpuVerifyState,
        final_state: GpuVerifyState,
        test_speed: u8,
        restore_failed: bool,
        /// The card's `fan_zero_rpm_stop_temperature`, read on the PMFW path
        /// when the kernel has the file (GPD-b); `None` otherwise and on the
        /// legacy path.
        zero_rpm_stop_c: Option<i32>,
    },
    /// DEC-502: the stop began first. Either no test write was made, or the
    /// settle was cut short and the card restored at once — to firmware auto on
    /// PMFW, to its original on the legacy path — so the result says nothing
    /// about the card and is not classified.
    Interrupted { restore_failed: bool },
}

/// DEC-502: where a fan test's restore sends a PMFW card. Once the stop has
/// begun only a hand-back may be written, so a restore to the engine's prior
/// fixed speed becomes firmware auto — what the stop's own reset would leave —
/// and the card leaves the hand-back list. Read once, at the restore, and passed
/// to the restore and both bookkeeping steps so they agree.
fn pmfw_restore_target(
    cache: &crate::health::cache::StateCache,
    prior_pct: Option<u8>,
) -> Option<u8> {
    if cache.shutdown_begun() {
        None
    } else {
        prior_pct
    }
}

/// DEC-502: the 503 a GPU fan test gets once the stop has begun — the same
/// retryable answer the header writes give (DEC-420).
fn gpu_test_stopping(restore_failed: bool) -> (StatusCode, Json<serde_json::Value>) {
    let message = if restore_failed {
        "the daemon is shutting down — the GPU fan test was cut short, and the fan \
         could not be restored; it may be left at its test speed"
    } else {
        "the daemon is shutting down — no GPU fan test was run, or it was cut short \
         and the fan given back"
    };
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &ErrorEnvelope::hardware_unavailable(message),
    )
}

/// POST /gpu/{gpu_id}/fan/verify — behavioural test of GPU fan-control
/// effectiveness (the GPU analogue of `hwmon_verify_handler`). Drives a test
/// speed (biased *upward* so cooling is never reduced), waits
/// `VERIFY_WAIT_SECONDS`, reads back the applied curve + `fan1_input` RPM,
/// restores the prior state, and classifies the outcome. No lease — GPU writes
/// never require one (DEC-045). See DEC-120.
pub async fn gpu_verify_handler(
    State(state): State<Arc<AppState>>,
    Path(gpu_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    // [SAFETY] DEC-502: no new GPU fan test once the stop has begun. The
    // sequence checks again under the GPU write lock, for a request that got
    // past here first.
    if state.cache.shutdown_begun() {
        return gpu_test_stopping(false);
    }
    // Phase 6 (DEC-201): refuse to start a verify while the system is hot — the
    // verify pauses the engine (incl. the thermal force_all_with_floor) for its window.
    if let Some(resp) = super::verify_thermal_guard(&state.cache) {
        return resp;
    }
    let gpu = match state.amd_gpus.iter().find(|g| g.pci_bdf == gpu_id) {
        Some(g) => g,
        None => {
            return error_response(
                StatusCode::NOT_FOUND,
                &ErrorEnvelope::validation(format!("GPU not found: {gpu_id}")),
            );
        }
    };

    let method = gpu.fan_control_method();
    if gpu.fan_curve_path.is_none() && !gpu.can_write_legacy_pwm() {
        // No write path — the same read-only case the set handler rejects.
        return error_response(
            StatusCode::BAD_REQUEST,
            &ErrorEnvelope::feature_unavailable(unsupported_fan_control_message(gpu)),
        );
    }

    // Single-flight + pause the engine's write phase for the verify's lifetime
    // so the engine's GPU backend does not overwrite our controlled test speed.
    // GPU writes need no lease (DEC-045); the pause is the only coordination.
    // NOT a suppression of the thermal `force_all_with_floor` — that runs before the engine's
    // `verify_active()` gate and always outranks a verify (corrected in DEC-297).
    let Some(verify_guard) =
        super::begin_verify_pause(&state.cache, constants::VERIFY_PAUSE_DEADMAN)
    else {
        return super::verify_slot_refusal(&state.cache);
    };

    // Release review, 2026-08-10. The verify pause above coordinates against the
    // ENGINE, and DEC-255's write lock coordinates the engine against
    // `fan/reset` — but nothing coordinated verify against reset, and verify is
    // the third multi-write PMFW producer. Take the write lock too, so a reset
    // arriving mid-verify cannot interleave its curve commit with the test
    // speed or the restore.
    //
    // Held for the whole verify window, not per-commit: this handler sleeps
    // between writing the test speed and reading back, and a lock dropped
    // across that sleep would reopen the exact gap. `reset` waits only
    // briefly for it (see `lock_gpu_writes_soon`) and reports a clear conflict
    // rather than hanging past the GUI's 5 s timeout.
    let gpu_write_guard = state.cache.lock_gpu_writes().await;

    let fan_id = format!("amd_gpu:{gpu_id}");
    let prior_pct = state
        .cache
        .snapshot()
        .gpu_fans
        .get(&fan_id)
        .and_then(|f| f.last_commanded_pct);

    // DEC-297: the ENTIRE test-write -> settle -> read-back -> restore sequence
    // runs inside one `spawn_blocking`, and BOTH guards are moved into it. This
    // is the DEC-290 shape, applied to the GPU analogue a release later.
    //
    // It used to run inline with `tokio::time::sleep(...).await` between the test
    // write and the restore. That `.await` is a cancellation point: a client
    // disconnect (or the GUI's own timeout) dropped the handler future, both
    // guards ran their `Drop`, and the restore never happened — leaving the fan
    // pinned at the test speed. `spawn_blocking` is not cancelled when its
    // `JoinHandle` is dropped, so the sequence always completes.
    //
    // NOT a copy of the hwmon fix: this path also holds an OWNED
    // `lock_gpu_writes` guard (DEC-255) for the whole window, which must move
    // inside too, and it takes no hwmon lease (GPU writes need none, DEC-045).
    let task_cache = state.cache.clone();
    let task_fan_id = fan_id.clone();

    let join = if let Some(fan_curve_path) = gpu.fan_curve_path.clone() {
        // ── PMFW fan_curve path (RDNA3+) ──────────────────────────────
        let zero_rpm_path = gpu.fan_zero_rpm_path.clone();
        let hwmon_path = gpu.hwmon_path.clone();
        // Not capped (DEC-505): the GPU verify (holds its guards).
        tokio::task::spawn_blocking(move || {
            let verify_guard = verify_guard;
            let _gpu_write_guard = gpu_write_guard;
            pmfw_verify_sequence(
                &fan_curve_path,
                zero_rpm_path.as_deref(),
                &hwmon_path,
                prior_pct,
                &task_cache,
                &task_fan_id,
                |speed| {
                    // Drive the test speed (disables zero-RPM, clamps to OD_RANGE, commits).
                    crate::hwmon::gpu_fan::set_static_speed(
                        &fan_curve_path,
                        zero_rpm_path.as_deref(),
                        speed,
                        constants::GPU_PMFW_NUM_CURVE_POINTS,
                    )
                },
                || {
                    // A blocking wait, deliberately: this task is the uncancellable unit,
                    // and an async sleep would reintroduce the cancellation point the
                    // restructure exists to remove. Woken by the stop (DEC-502), so a
                    // verify caught by one restores at once instead of after its
                    // settle, which would land after the stop's GPU reset.
                    task_cache.sleep_unless_shutdown(std::time::Duration::from_secs(
                        constants::VERIFY_WAIT_SECONDS as u64,
                    ));
                },
                || {
                    // DEC-296 liveness: prove we are still alive and keep the pause for
                    // the restore. Unlike the hwmon verify we hold no lease to lose, so
                    // a supersession cannot break the restore itself — but if the window
                    // lapses with no successor the engine resumes writing and the
                    // restore below races its curve output.
                    let _ = verify_guard.renew(constants::VERIFY_PAUSE_DEADMAN);
                },
            )
        })
    } else {
        // ── Legacy hwmon pwm1 path (pre-RDNA3) ────────────────────────
        let hwmon_path = gpu.hwmon_path.clone();
        // DEC-414 (`TS-aa`): where `ventulus-restore-auto` looks for the crash
        // backstop — the same runtime directory as the hwmon hand-back record,
        // and only when this daemon owns the records there (DEC-502).
        let record = state
            .cache
            .hand_back_record_dir()
            .map(|dir| dir.join(crate::hwmon::gpu_fan::GPU_RECORD_FILE_NAME));
        // Not capped (DEC-505): the GPU verify (holds its guards).
        tokio::task::spawn_blocking(move || {
            let verify_guard = verify_guard;
            let _gpu_write_guard = gpu_write_guard;
            legacy_verify_sequence(
                &hwmon_path,
                &task_cache,
                &task_fan_id,
                record.as_deref(),
                || {
                    // As on the PMFW path: woken by the stop (DEC-502).
                    task_cache.sleep_unless_shutdown(std::time::Duration::from_secs(
                        constants::VERIFY_WAIT_SECONDS as u64,
                    ));
                },
                || {
                    let _ = verify_guard.renew(constants::VERIFY_PAUSE_DEADMAN);
                },
            )
        })
    };

    let sequence = match join.await {
        Ok(seq) => seq,
        Err(e) => {
            // The sequence panicked. Both guards were owned by the task and have
            // been dropped by the unwind, so nothing is stranded held — but the
            // fan may still be at the test speed, which the caller must be told.
            log::error!("GPU verify task panicked for {fan_id}: {e}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &ErrorEnvelope::internal(format!(
                    "the GPU verify sequence panicked; the fan may be left at its test speed: {e}"
                )),
            );
        }
    };

    match sequence {
        GpuVerifySequence::Interrupted { restore_failed } => {
            log::info!(
                "GPU fan test on {fan_id} cut short by the stop (restore {})",
                if restore_failed { "failed" } else { "done" }
            );
            gpu_test_stopping(restore_failed)
        }
        GpuVerifySequence::WriteFailed {
            initial,
            final_state,
            test_speed,
            restore_failed,
            details,
        } => json_ok(
            StatusCode::OK,
            GpuVerifyResponse {
                gpu_id,
                result: "write_failed".into(),
                initial_state: initial,
                final_state,
                test_speed_pct: test_speed,
                wait_seconds: 0,
                fan_control_method: method.into(),
                details,
                restore_failed,
            },
        ),
        GpuVerifySequence::Completed {
            initial,
            final_state,
            test_speed,
            restore_failed,
            zero_rpm_stop_c,
        } => {
            let ctx = GpuVerifyContext::for_card(gpu.pci_device_id, zero_rpm_stop_c);
            let (result, details) =
                classify_gpu_verify_result(&initial, &final_state, test_speed, &ctx);
            json_ok(
                StatusCode::OK,
                GpuVerifyResponse {
                    gpu_id,
                    result,
                    initial_state: initial,
                    final_state,
                    test_speed_pct: test_speed,
                    wait_seconds: constants::VERIFY_WAIT_SECONDS,
                    fan_control_method: method.into(),
                    details,
                    restore_failed,
                },
            )
        }
    }
}

/// GPD-b: the RDNA4 arms of [`classify_gpu_verify_result`], reached once the
/// curve applied, the fan did not demonstrably respond and zero-RPM was not
/// reported on. `None` means "not one of these" — the caller's `no_rpm_effect`.
fn rdna4_zero_rpm_verdict(
    initial: &GpuVerifyState,
    final_state: &GpuVerifyState,
    final_rpm: u16,
    ctx: &GpuVerifyContext,
) -> Option<(String, String)> {
    // A fan seen spinning before the test that read 0 after it was not held
    // stopped by an idle firmware: that is a real no-effect, whatever the hotspot.
    let not_seen_spinning = matches!(initial.rpm, Some(0) | None);
    if !ctx.rdna4 || final_rpm != 0 || !not_seen_spinning {
        return None;
    }
    let hotspot = final_state.hotspot_c.or(initial.hotspot_c);
    match hotspot {
        Some(h) if h >= ctx.idle_band_below_c => (initial.rpm == Some(0)).then(|| {
            (
                "rpm_reads_zero".into(),
                format!(
                    "The curve was applied but the fan read 0 RPM before and after the test, \
                     with the GPU hotspot at {h:.0} \u{00b0}C. Some RDNA4 cards report 0 RPM \
                     while the fan spins (drm/amd #5514): look at the fan, or re-test under load."
                ),
            )
        }),
        _ => {
            let at = hotspot
                .map(|h| format!("at {h:.0} \u{00b0}C"))
                .unwrap_or_else(|| "unreadable".into());
            Some((
                "idle_fan_stop".into(),
                format!(
                    "The curve was applied but the fan read 0 RPM after the test with the GPU \
                     hotspot {at}. RDNA4 firmware can keep the fan stopped at idle even with \
                     zero-RPM off \
                     (drm/amd #5905). Inconclusive: re-test with the GPU under load."
                ),
            ))
        }
    }
}

/// RPM at/above which (with `fan1_input` present) the fan is treated as
/// demonstrably spinning. GPU fans at ≥60% are typically well over 1000 RPM;
/// this floor distinguishes "spinning" from "stopped/idle".
const GPU_SPINNING_RPM: u16 = 200;

/// Tolerance (percentage points) for matching the read-back applied speed to
/// the requested test speed. PMFW/`pwm1` speeds are written exactly; this
/// absorbs raw↔percent rounding only.
const GPU_SPEED_MATCH_TOLERANCE: u16 = 3;

/// Choose a test speed for the GPU fan verify. Biased *upward* so the test
/// never reduces cooling on a hot GPU: from a low/idle start a jump to 75%
/// gives an unambiguous RPM rise; an already-driven-high fan is pushed to 100%
/// (the curve read-back still confirms the write even when RPM barely moves).
/// Clamped to the device OD_RANGE.
fn select_gpu_test_speed(current_pct: Option<u8>, od_min: u8, od_max: u8) -> u8 {
    let target: u8 = if current_pct.unwrap_or(0) < 60 {
        75
    } else {
        100
    };
    target.min(od_max).max(od_min.min(od_max))
}

/// Restore the GPU to its pre-verify state on the PMFW path: re-apply the prior
/// static speed if it was being driven, else reset to firmware-auto (which
/// re-enables zero-RPM). Mirrors how `hwmon_verify_handler` restores its prior
/// PWM. Returns `true` if the restore write failed (`restore_failed`).
fn restore_pmfw(
    prior_pct: Option<u8>,
    fan_curve_path: &std::path::Path,
    zero_rpm_path: Option<&std::path::Path>,
) -> bool {
    let result = match prior_pct {
        Some(p) if p > 0 => crate::hwmon::gpu_fan::set_static_speed(
            fan_curve_path,
            zero_rpm_path,
            p,
            constants::GPU_PMFW_NUM_CURVE_POINTS,
        ),
        _ => crate::hwmon::gpu_fan::reset_to_auto(fan_curve_path, zero_rpm_path),
    };
    if let Err(e) = &result {
        log::warn!("verify: GPU PMFW restore failed (fan may be left at test speed): {e}");
    }
    result.is_err()
}

/// The PMFW (RDNA3+) GPU verify: read, name the card, test write, settle, read
/// back, restore (DEC-297's uncancellable sequence).
///
/// A named function rather than the closure body, as `legacy_verify_sequence`
/// is, so the hand-back bookkeeping on each arm can be tested (`DC-cq`). Of the
/// two writes, only the test write is injected: it and the restore write the
/// same `fan_curve` file, so no sysfs fixture can refuse one and accept the
/// other. The restore stays
/// the real `restore_pmfw`. `settle` is the sleep and `renew` the deadman
/// renewal, in the order the handler has always run them.
#[allow(clippy::too_many_arguments)]
fn pmfw_verify_sequence(
    fan_curve_path: &std::path::Path,
    zero_rpm_path: Option<&std::path::Path>,
    hwmon_path: &std::path::Path,
    prior_pct: Option<u8>,
    task_cache: &crate::health::cache::StateCache,
    task_fan_id: &str,
    test_write: impl FnOnce(u8) -> Result<(), crate::error::HwmonError>,
    settle: impl FnOnce(),
    renew: impl FnOnce(),
) -> GpuVerifySequence {
    let read_rpm = |hwmon: &std::path::Path| -> Option<u16> {
        std::fs::read_to_string(hwmon.join("fan1_input"))
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok())
    };

    let initial_curve = crate::hwmon::gpu_fan::read_fan_curve(fan_curve_path).ok();
    let (od_min, od_max) = initial_curve
        .as_ref()
        .and_then(|c| c.speed_range)
        .unwrap_or((15, 100));
    let initial_state = GpuVerifyState {
        applied_speed_pct: initial_curve
            .as_ref()
            .and_then(crate::hwmon::gpu_fan::flat_speed_pct),
        rpm: read_rpm(hwmon_path),
        pwm_enable: None,
        zero_rpm_enabled: zero_rpm_path.and_then(crate::hwmon::gpu_fan::read_zero_rpm_enabled),
        hotspot_c: crate::hwmon::gpu_fan::read_junction_temp_c(hwmon_path),
    };

    let test_speed = select_gpu_test_speed(
        prior_pct.or(initial_state.applied_speed_pct),
        od_min,
        od_max,
    );

    // [SAFETY] DEC-502: under the GPU write lock the handler holds for the whole
    // sequence. A stop that began while this verify waited for the lock gets no
    // test write: the card is left exactly as it was.
    if task_cache.shutdown_begun() {
        return GpuVerifySequence::Interrupted {
            restore_failed: false,
        };
    }

    // DEC-435: name the card before the test write, so a crash mid-verify
    // leaves ExecStopPost a card to reset.
    task_cache
        .gpu_handback()
        .note_take(task_fan_id, fan_curve_path, zero_rpm_path);
    if let Err(e) = test_write(test_speed) {
        let restore_to = pmfw_restore_target(task_cache, prior_pct);
        let restore_failed = restore_pmfw(restore_to, fan_curve_path, zero_rpm_path);
        stamp_restored_pct(task_cache, task_fan_id, restore_to, restore_failed);
        note_pmfw_restored(task_cache, task_fan_id, restore_to, restore_failed);
        return GpuVerifySequence::WriteFailed {
            initial: initial_state,
            final_state: GpuVerifyState {
                applied_speed_pct: None,
                rpm: read_rpm(hwmon_path),
                pwm_enable: None,
                zero_rpm_enabled: None,
                hotspot_c: crate::hwmon::gpu_fan::read_junction_temp_c(hwmon_path),
            },
            test_speed,
            restore_failed,
            details: format!(
                "The PMFW fan_curve write was rejected by the driver/firmware: {e}. \
                 Manual fan control is not functional in this state."
            ),
        };
    }
    // DEC-297: keep the cache truthful about what was last COMMANDED.
    // The engine's `apply` coalesces against this value (5% band), so a
    // cache still reporting the pre-verify duty would suppress the
    // engine's correction if the restore below ever fails — the strand
    // would then survive even under an active profile, which is the
    // opposite of what the register row assumed.
    task_cache.set_gpu_fan_commanded_pct(task_fan_id, test_speed);

    settle();

    // [SAFETY] DEC-502: a stop woke the settle. Restore now — to firmware auto,
    // the only write the stop allows — so the card is back before the stop's
    // own GPU reset runs, and nothing this verify writes can land after it.
    if task_cache.shutdown_begun() {
        let restore_to = pmfw_restore_target(task_cache, prior_pct);
        let restore_failed = restore_pmfw(restore_to, fan_curve_path, zero_rpm_path);
        stamp_restored_pct(task_cache, task_fan_id, restore_to, restore_failed);
        note_pmfw_restored(task_cache, task_fan_id, restore_to, restore_failed);
        return GpuVerifySequence::Interrupted { restore_failed };
    }

    let final_curve = crate::hwmon::gpu_fan::read_fan_curve(fan_curve_path).ok();
    let final_state = GpuVerifyState {
        applied_speed_pct: final_curve
            .as_ref()
            .and_then(crate::hwmon::gpu_fan::flat_speed_pct),
        rpm: read_rpm(hwmon_path),
        pwm_enable: None,
        zero_rpm_enabled: zero_rpm_path.and_then(crate::hwmon::gpu_fan::read_zero_rpm_enabled),
        hotspot_c: crate::hwmon::gpu_fan::read_junction_temp_c(hwmon_path),
    };
    let zero_rpm_stop_c = crate::hwmon::gpu_fan::read_zero_rpm_stop_temp(fan_curve_path);

    renew();

    // DEC-502: re-read here too — a stop that begins during the read-back still
    // turns a restore to a fixed speed into a hand-back.
    let restore_to = pmfw_restore_target(task_cache, prior_pct);
    let restore_failed = restore_pmfw(restore_to, fan_curve_path, zero_rpm_path);
    stamp_restored_pct(task_cache, task_fan_id, restore_to, restore_failed);
    note_pmfw_restored(task_cache, task_fan_id, restore_to, restore_failed);
    GpuVerifySequence::Completed {
        initial: initial_state,
        final_state,
        test_speed,
        restore_failed,
        zero_rpm_stop_c,
    }
}

/// The legacy (pre-RDNA3) GPU verify: read, record, write manual mode, settle,
/// read back, restore (DEC-297's uncancellable sequence).
///
/// A named function rather than the closure body so the crash backstop's
/// placement can be tested: `note_legacy_take` must land before the
/// `pwm1_enable=1` write, and the line must go only once the fan is back as
/// it was found (DEC-414, `TS-aa`). `settle` is the sleep and `renew` the
/// deadman renewal, in the order the handler has always run them.
///
/// DEC-447: the restore targets the fan's ORIGINAL — the state the daemon
/// first found it in — not the cache's last commanded duty. No engine writes a
/// legacy card (DEC-446), so that duty was only ever an earlier verify's test
/// speed, and a restore to it after a failed one put the card back in manual at
/// 75–100 % (`TS-bi`). The original is what `note_legacy_take` returns: the
/// record's standing line when an earlier take was never given back, else what
/// was read here. A restore that lands therefore always gives the card back.
///
/// `record` is `None` when this daemon owns no hand-back records (DEC-502:
/// another daemon holds them, or there is no runtime directory). The verify
/// then runs without the crash backstop, as it always has when the record
/// could not be written.
fn legacy_verify_sequence(
    hwmon_path: &std::path::Path,
    task_cache: &crate::health::cache::StateCache,
    task_fan_id: &str,
    record: Option<&std::path::Path>,
    settle: impl FnOnce(),
    renew: impl FnOnce(),
) -> GpuVerifySequence {
    let read_rpm = |hwmon: &std::path::Path| -> Option<u16> {
        std::fs::read_to_string(hwmon.join("fan1_input"))
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok())
    };
    let read_raw_pwm = |hwmon: &std::path::Path| -> Option<u8> {
        std::fs::read_to_string(hwmon.join("pwm1"))
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok())
    };
    let read_enable = |hwmon: &std::path::Path| -> Option<u8> {
        std::fs::read_to_string(hwmon.join("pwm1_enable"))
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok())
    };

    let initial_raw = read_raw_pwm(hwmon_path);
    let initial_state = GpuVerifyState {
        applied_speed_pct: initial_raw.map(crate::pwm::raw_to_percent),
        rpm: read_rpm(hwmon_path),
        pwm_enable: read_enable(hwmon_path),
        zero_rpm_enabled: None,
        hotspot_c: None,
    };

    let test_speed = select_gpu_test_speed(initial_state.applied_speed_pct, 0, 100);

    // [SAFETY] DEC-502: as on the PMFW path — a stop that began while this
    // verify waited for the GPU write lock gets no manual-mode write.
    if task_cache.shutdown_begun() {
        return GpuVerifySequence::Interrupted {
            restore_failed: false,
        };
    }

    // DEC-414 (`TS-aa`): recorded BEFORE the manual-mode write, so a crash
    // from here on is given back by ExecStopPost. DEC-447 (`TS-bj`): a card
    // found in manual is recorded with its duty, not as bare `mode 1`.
    let read =
        crate::hwmon::gpu_fan::LegacyOriginal::from_read(initial_state.pwm_enable, initial_raw);
    let original = match record {
        Some(record) => crate::hwmon::gpu_fan::note_legacy_take(record, hwmon_path, read),
        None => read,
    };

    if let Err(e) = crate::hwmon::gpu_fan::set_legacy_pwm(hwmon_path, test_speed) {
        let restore_failed = restore_legacy(original, hwmon_path);
        note_legacy_restored(
            task_cache,
            task_fan_id,
            record,
            hwmon_path,
            original,
            restore_failed,
        );
        return GpuVerifySequence::WriteFailed {
            initial: initial_state,
            final_state: GpuVerifyState {
                applied_speed_pct: read_raw_pwm(hwmon_path).map(crate::pwm::raw_to_percent),
                rpm: read_rpm(hwmon_path),
                pwm_enable: read_enable(hwmon_path),
                zero_rpm_enabled: None,
                hotspot_c: None,
            },
            test_speed,
            restore_failed,
            details: format!(
                "The legacy pwm1 write was rejected: {e}. Manual fan control is not \
                 functional in this state."
            ),
        };
    }
    task_cache.set_gpu_fan_commanded_pct(task_fan_id, test_speed);

    settle();

    // [SAFETY] DEC-502: a stop woke the settle. Give the card its original back
    // now — a hand-back, which the stop allows — so the record line is gone
    // before the process exits and nothing depends on ExecStopPost. The stop's
    // own restore does not read this record, so this is the only in-process
    // hand-back a legacy card gets.
    if task_cache.shutdown_begun() {
        let restore_failed = restore_legacy(original, hwmon_path);
        note_legacy_restored(
            task_cache,
            task_fan_id,
            record,
            hwmon_path,
            original,
            restore_failed,
        );
        return GpuVerifySequence::Interrupted { restore_failed };
    }

    let final_state = GpuVerifyState {
        applied_speed_pct: read_raw_pwm(hwmon_path).map(crate::pwm::raw_to_percent),
        rpm: read_rpm(hwmon_path),
        pwm_enable: read_enable(hwmon_path),
        zero_rpm_enabled: None,
        hotspot_c: None,
    };

    renew();

    let restore_failed = restore_legacy(original, hwmon_path);
    note_legacy_restored(
        task_cache,
        task_fan_id,
        record,
        hwmon_path,
        original,
        restore_failed,
    );
    GpuVerifySequence::Completed {
        initial: initial_state,
        final_state,
        test_speed,
        restore_failed,
        zero_rpm_stop_c: None,
    }
}

/// Put a legacy fan back as it was found. Returns `true` if the restore write
/// failed (`restore_failed`), as `restore_pmfw` does.
fn restore_legacy(
    original: crate::hwmon::gpu_fan::LegacyOriginal,
    hwmon_path: &std::path::Path,
) -> bool {
    use crate::hwmon::gpu_fan::LegacyOriginal;
    let result = match original {
        LegacyOriginal::Mode(mode) => crate::hwmon::gpu_fan::set_legacy_mode(hwmon_path, mode),
        LegacyOriginal::Manual(raw) => crate::hwmon::gpu_fan::set_legacy_raw(hwmon_path, raw),
    };
    if let Err(e) = &result {
        log::warn!("verify: GPU legacy restore failed (fan may be left at test speed): {e}");
    }
    result.is_err()
}

/// The bookkeeping after a legacy restore: a restore that landed gave the fan
/// back as found, so its record line goes and the cache says what is now on it
/// — the original manual duty, or no duty for a mode the firmware drives
/// (`GPU-e`, DEC-468: as `gpu_reset_fan_handler` and `stamp_restored_pct` record
/// it; a 0 could be coalesced against). A FAILED restore changes neither:
/// the fan is at the test speed the cache already holds, and the line must stay
/// for `ExecStopPost` (DEC-414, DEC-297).
fn note_legacy_restored(
    task_cache: &crate::health::cache::StateCache,
    task_fan_id: &str,
    record: Option<&std::path::Path>,
    hwmon_path: &std::path::Path,
    original: crate::hwmon::gpu_fan::LegacyOriginal,
    restore_failed: bool,
) {
    use crate::hwmon::gpu_fan::LegacyOriginal;
    if restore_failed {
        return;
    }
    if let Some(record) = record {
        crate::hwmon::gpu_fan::note_legacy_handed_back(record, hwmon_path);
    }
    match original {
        LegacyOriginal::Manual(raw) => {
            task_cache.set_gpu_fan_commanded_pct(task_fan_id, crate::pwm::raw_to_percent(raw))
        }
        LegacyOriginal::Mode(_) => task_cache.clear_gpu_fan_commanded_pct(task_fan_id),
    }
}

/// What the classifier knows about the card beyond its two states (GPD-b).
///
/// Built only by [`GpuVerifyContext::for_card`], from the card's PCI device id
/// and the stop temperature the sequence read, so a caller cannot classify an
/// RDNA4 card without the RDNA4 rules.
#[derive(Debug, Clone, Copy, PartialEq)]
struct GpuVerifyContext {
    /// RDNA4: in curve mode the firmware stops the fan below its zero-RPM stop
    /// temperature whatever `fan_zero_rpm_enable` says (drm/amd #5905), and some
    /// cards' `fan1_input` reads 0 while the fan spins (#5514).
    rdna4: bool,
    /// Hotspot (°C) below which a stopped RDNA4 fan may be the firmware's idle
    /// stop: the card's stop temperature (or the default) plus the margin.
    idle_band_below_c: f64,
}

impl GpuVerifyContext {
    fn for_card(pci_device_id: u16, zero_rpm_stop_c: Option<i32>) -> Self {
        let stop = zero_rpm_stop_c.unwrap_or(constants::GPU_RDNA4_DEFAULT_ZERO_RPM_STOP_C);
        Self {
            rdna4: crate::hwmon::gpu_detect::is_rdna4(pci_device_id),
            idle_band_below_c: f64::from(stop + constants::GPU_RDNA4_IDLE_BAND_MARGIN_C),
        }
    }
}

/// Classify the GPU fan verify outcome from the before/after state and the
/// (OD_RANGE-clamped) requested test speed. Avoids the documented false-failure
/// traps: OD_RANGE clamping is absorbed by the caller, zero-RPM idle is reported
/// as normal (not a fault), and a silent-ignore is distinguished from a real
/// no-effect by the read-back speed match.
///
/// GPD-b: on RDNA4 with zero-RPM not reported on, a fan still at 0 RPM is
/// `idle_fan_stop` when it was not seen spinning before (0 or no reading) and
/// the hotspot is below the idle band (or unreadable), and `rpm_reads_zero` when
/// it read 0 before and after with the hotspot at or above the band. Both are
/// inconclusive, never a pass. A fan that read non-zero after the test, or was
/// seen spinning before it, stays `no_rpm_effect`.
fn classify_gpu_verify_result(
    initial: &GpuVerifyState,
    final_state: &GpuVerifyState,
    test_speed: u8,
    ctx: &GpuVerifyContext,
) -> (String, String) {
    // Legacy path: pwm1_enable left manual (1) during the test. `BRD-o`: what
    // reads back here is the card's own driver/SMU state, never motherboard
    // firmware — so a BIOS "Smart Fan" option, which this text used to blame,
    // is not a cause. Another GPU fan tool, or a GPU reset, is. (Resume from
    // suspend is not: on SMU11 the driver's restore path leaves a manual fan
    // manual, curator GPD-12.) 0 is "no fan control" (full speed), not
    // automatic, so the value is named rather than assumed.
    if let Some(en) = final_state.pwm_enable {
        if en != 1 {
            let mode = match en {
                0 => "0 (no fan control: full speed)".to_string(),
                2 => "2 (automatic)".to_string(),
                other => other.to_string(),
            };
            return (
                "pwm_enable_reverted".into(),
                format!(
                    "pwm1_enable changed from 1 (manual) to {mode} during the test, so \
                     something other than this test changed the card's fan mode. The usual \
                     cause is another GPU fan tool (for example LACT, CoreCtrl or amdgpu-fan): \
                     stop it, then re-test. A GPU reset also hands the fan back to the \
                     card's own automatic control. Motherboard BIOS fan settings do not drive \
                     a graphics card's fan."
                ),
            );
        }
    }

    // Did the requested speed actually get applied (read-back match)?
    let curve_applied = final_state
        .applied_speed_pct
        .map(|s| (s as i16 - test_speed as i16).unsigned_abs() <= GPU_SPEED_MATCH_TOLERANCE)
        .unwrap_or(false);
    if !curve_applied {
        let seen = final_state
            .applied_speed_pct
            .map(|s| format!("{s}%"))
            .unwrap_or_else(|| "unreadable".into());
        return (
            "curve_not_applied".into(),
            format!(
                "Requested {test_speed}% but the read-back fan speed was {seen}. The write was \
                 accepted at sysfs but silently not applied — typically amdgpu.ppfeaturemask bit \
                 14 (0x4000) is unset or a BIOS overdrive lock. Add \
                 'amdgpu.ppfeaturemask=0xfff7ffff' (or any mask with bit 14 set) to the kernel \
                 parameters and reboot."
            ),
        );
    }

    // Speed applied — did the fan physically respond?
    match final_state.rpm {
        Some(final_rpm) => {
            let init_rpm_s = initial
                .rpm
                .map(|r| r.to_string())
                .unwrap_or_else(|| "?".into());
            let rose = match initial.rpm {
                Some(ir) => (final_rpm as u32) > (ir as u32) + (ir as u32 / 5) + 50,
                None => final_rpm > GPU_SPINNING_RPM,
            };
            let already_fast =
                initial.applied_speed_pct.unwrap_or(0) >= 50 && final_rpm > GPU_SPINNING_RPM;
            if rose || already_fast {
                (
                    "effective".into(),
                    format!(
                        "GPU fan control verified: speed applied at {test_speed}%, \
                         RPM {init_rpm_s} \u{2192} {final_rpm}."
                    ),
                )
            } else if final_state.zero_rpm_enabled == Some(true) {
                (
                    "zero_rpm_suppressed".into(),
                    "The curve was applied but zero-RPM is enabled, so the fan stays stopped while \
                     the GPU is below its zero-RPM stop temperature. This is normal firmware \
                     behaviour — the fan will spin up under load."
                        .into(),
                )
            } else if let Some(verdict) =
                rdna4_zero_rpm_verdict(initial, final_state, final_rpm, ctx)
            {
                verdict
            } else {
                (
                    "no_rpm_effect".into(),
                    format!(
                        "The curve was applied at {test_speed}% with zero-RPM disabled, but RPM did \
                         not respond ({init_rpm_s} \u{2192} {final_rpm}). The write is accepted but \
                         has no hardware effect — a possible SMU firmware issue or a known kernel \
                         regression for this GPU."
                    ),
                )
            }
        }
        None => (
            "rpm_unavailable".into(),
            format!(
                "Fan-speed write confirmed via curve read-back ({test_speed}%), but no fan1_input \
                 RPM sensor is available to corroborate the physical effect."
            ),
        ),
    }
}

/// Build a `feature_unavailable` message tailored to *why* the GPU has no
/// write path. An RDNA3/RDNA4 card without its PMFW `fan_curve` gets the
/// reason `AmdGpuInfo::rdna_read_only_reason` gives — keyed on the device id,
/// not on `pwm1_enable`, which an RX 7000 exposes although it cannot be used
/// (DEC-430) — and anything else the generic read-only message.
fn unsupported_fan_control_message(gpu: &crate::hwmon::gpu_detect::AmdGpuInfo) -> String {
    if let Some(reason) = gpu.rdna_read_only_reason() {
        return reason;
    }
    format!(
        "GPU {} fan control is read-only ({}); manual fan writes are not \
         supported in this hardware/firmware mode.",
        gpu.pci_bdf,
        gpu.fan_control_method()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pre-RDNA3 amdgpu hwmon dir in automatic mode, and where its crash record goes.
    fn legacy_card() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let hwmon = tmp.path().join("hwmon3");
        std::fs::create_dir_all(&hwmon).unwrap();
        std::fs::write(hwmon.join("pwm1_enable"), "2\n").unwrap();
        std::fs::write(hwmon.join("pwm1"), "100\n").unwrap();
        std::fs::write(hwmon.join("fan1_input"), "1200\n").unwrap();
        let record = tmp.path().join(crate::hwmon::gpu_fan::GPU_RECORD_FILE_NAME);
        (tmp, hwmon, record)
    }

    /// [SAFETY] DEC-414 (`TS-aa`): while the legacy verify holds the card in
    /// manual mode — the window a crash leaves it stuck in — the record names
    /// the card with the mode it had; once the verify has restored it, the line
    /// is gone, so a later stop does not touch the card again.
    #[test]
    fn the_legacy_verify_records_its_take_for_the_whole_manual_window() {
        let (_tmp, hwmon, record) = legacy_card();
        let cache = crate::health::cache::StateCache::new();
        let seen_mid_verify = std::cell::RefCell::new(None);
        let sequence = legacy_verify_sequence(
            &hwmon,
            &cache,
            "amd_gpu:0000:03:00.0",
            Some(&record),
            || {
                *seen_mid_verify.borrow_mut() = Some((
                    std::fs::read_to_string(hwmon.join("pwm1_enable")).unwrap(),
                    std::fs::read_to_string(&record).ok(),
                ));
            },
            || {},
        );
        let (mode, line) = seen_mid_verify.into_inner().expect("the settle hook ran");
        assert_eq!(
            mode.trim(),
            "1",
            "precondition: the card is in manual mode mid-verify"
        );
        let line = line.expect("the take must be recorded while the card is in manual mode");
        assert!(
            line.contains(&format!(
                "{}\t{}\tmode\t2",
                hwmon.join("pwm1_enable").display(),
                hwmon.join("pwm1").display()
            )),
            "the record must give back the mode the card had (2): {line:?}"
        );
        assert!(
            matches!(
                sequence,
                GpuVerifySequence::Completed {
                    restore_failed: false,
                    ..
                }
            ),
            "precondition: the verify restored the card"
        );
        assert!(!record.exists(), "a restored card's line must be dropped");
    }

    /// DEC-447 (`TS-bi`). A failed restore leaves the card in the daemon's
    /// manual mode at the test speed (DEC-297 keeps that duty in the cache), so
    /// the NEXT verify reads the daemon's own leftover as its starting state.
    /// It used to restore to the cache's "prior" — that test duty — and so put
    /// the card back in manual at 75–100 %. It must restore to the ORIGINAL the
    /// first take recorded (automatic here), and hand the card back.
    #[test]
    fn the_verify_after_a_failed_restore_gives_back_the_first_original() {
        let (_tmp, hwmon, record) = legacy_card();
        let cache = crate::health::cache::StateCache::new();
        let fan_id = "amd_gpu:0000:03:00.0";
        let enable = hwmon.join("pwm1_enable");
        // Verify #1: its restore fails.
        let first = legacy_verify_sequence(
            &hwmon,
            &cache,
            fan_id,
            Some(&record),
            || {
                std::fs::remove_file(&enable).unwrap();
                std::fs::create_dir(&enable).unwrap();
            },
            || {},
        );
        assert!(matches!(
            first,
            GpuVerifySequence::Completed {
                restore_failed: true,
                ..
            }
        ));
        // The card is still in manual, as the failed restore left it.
        std::fs::remove_dir(&enable).unwrap();
        std::fs::write(&enable, "1\n").unwrap();
        assert!(
            std::fs::read_to_string(&record).is_ok_and(|b| b.contains("\tmode\t2")),
            "precondition: the first take's original (2) is still recorded"
        );

        let second = legacy_verify_sequence(&hwmon, &cache, fan_id, Some(&record), || {}, || {});
        assert!(
            matches!(
                second,
                GpuVerifySequence::Completed {
                    restore_failed: false,
                    ..
                }
            ),
            "precondition: verify #2's restore succeeded"
        );
        assert_eq!(
            std::fs::read_to_string(&enable).unwrap().trim(),
            "2",
            "the card must go back to the mode it was FOUND in, not the leftover \
             manual mode verify #2 read"
        );
        assert!(!record.exists(), "given back, so its line is gone");
        assert_eq!(
            cache
                .snapshot()
                .gpu_fans
                .get(fan_id)
                .and_then(|f| f.last_commanded_pct),
            None,
            "back on automatic records no duty (`GPU-e`), not the test speed"
        );
    }

    /// DEC-447 (`TS-bj`): a card another tool had in manual at a duty goes back
    /// to exactly that duty — the record names it, and the verify restores it.
    #[test]
    fn a_card_found_in_manual_is_restored_to_its_duty() {
        let (_tmp, hwmon, record) = legacy_card();
        std::fs::write(hwmon.join("pwm1_enable"), "1\n").unwrap();
        std::fs::write(hwmon.join("pwm1"), "120\n").unwrap();
        let cache = crate::health::cache::StateCache::new();
        let fan_id = "amd_gpu:0000:03:00.0";
        let seen_mid_verify = std::cell::RefCell::new(None);
        let sequence = legacy_verify_sequence(
            &hwmon,
            &cache,
            fan_id,
            Some(&record),
            || {
                *seen_mid_verify.borrow_mut() = Some((
                    std::fs::read_to_string(hwmon.join("pwm1")).unwrap(),
                    std::fs::read_to_string(&record).ok(),
                ))
            },
            || {},
        );
        let (mid_pwm, mid_line) = seen_mid_verify.into_inner().expect("the settle hook ran");
        assert_ne!(
            mid_pwm.trim(),
            "120",
            "precondition: the verify drove a different test duty"
        );
        assert!(
            mid_line.is_some_and(|l| l.contains("\tmanual\t120")),
            "the crash record must name the duty it found"
        );
        assert!(matches!(
            sequence,
            GpuVerifySequence::Completed {
                restore_failed: false,
                ..
            }
        ));
        assert_eq!(
            std::fs::read_to_string(hwmon.join("pwm1_enable"))
                .unwrap()
                .trim(),
            "1"
        );
        assert_eq!(
            std::fs::read_to_string(hwmon.join("pwm1")).unwrap().trim(),
            "120"
        );
        assert!(!record.exists(), "given back as found, so its line is gone");
        assert_eq!(
            cache
                .snapshot()
                .gpu_fans
                .get(fan_id)
                .and_then(|f| f.last_commanded_pct),
            Some(crate::pwm::raw_to_percent(120)),
            "the cache says what is now on the card"
        );
    }

    /// The line stays when the verify's own restore fails, so ExecStopPost —
    /// which also runs after a clean stop — still gives the card back.
    #[test]
    fn a_failed_legacy_restore_keeps_the_record() {
        let (_tmp, hwmon, record) = legacy_card();
        let cache = crate::health::cache::StateCache::new();
        let enable = hwmon.join("pwm1_enable");
        let sequence = legacy_verify_sequence(
            &hwmon,
            &cache,
            "amd_gpu:0000:03:00.0",
            Some(&record),
            || {
                // The restore's write of `2` to pwm1_enable now fails.
                std::fs::remove_file(&enable).unwrap();
                std::fs::create_dir(&enable).unwrap();
            },
            || {},
        );
        assert!(
            matches!(
                sequence,
                GpuVerifySequence::Completed {
                    restore_failed: true,
                    ..
                }
            ),
            "precondition: the restore failed"
        );
        let body =
            std::fs::read_to_string(&record).expect("the record must survive a failed restore");
        assert!(body.contains("\tmode\t2"), "{body:?}");
    }

    /// DEC-435: only a restore that actually put the card back on firmware auto
    /// takes it off the stop list — the same arms as `restore_pmfw`.
    #[test]
    fn only_a_successful_auto_restore_hands_the_card_back() {
        let cases = [
            (None, false, false, "restored to auto"),
            (Some(0), false, false, "a prior 0 is the auto arm too"),
            (Some(40), false, true, "restored to the engine's prior duty"),
            (
                None,
                true,
                true,
                "the restore failed: still at the test speed",
            ),
        ];
        for (prior, failed, kept, why) in cases {
            let cache = crate::health::cache::StateCache::new();
            let fan_id = "amd_gpu:0000:03:00.0";
            cache.gpu_handback().note_take(
                fan_id,
                std::path::Path::new("/sys/x/gpu_od/fan_ctrl/fan_curve"),
                None,
            );
            note_pmfw_restored(&cache, fan_id, prior, failed);
            assert_eq!(cache.gpu_handback().is_taken(fan_id), kept, "{why}");
        }
    }

    /// `DC-cq` (DEC-435's residual): the PMFW verify's `WriteFailed` arm runs the
    /// same hand-back bookkeeping as its `Completed` arm. The test write is
    /// refused and the REAL restore runs, so each case's outcome comes from what
    /// the restore actually did to the file — a regular file for a restore that
    /// lands, a directory for one that fails.
    #[test]
    fn a_refused_pmfw_test_write_hands_back_only_a_card_restored_to_auto() {
        // (prior, restore fails, kept on the stop list, commanded duty after, why).
        // A failed restore stamps nothing, and this arm never stamps the test
        // speed, so the cache then holds no duty for the card.
        let cases = [
            (
                None,
                false,
                false,
                None,
                "restored to auto: off the stop list, no duty recorded",
            ),
            (
                Some(40),
                false,
                true,
                Some(40),
                "restored to the engine's prior duty: still the daemon's",
            ),
            (
                None,
                true,
                true,
                None,
                "the restore failed: the stop must reset it",
            ),
        ];
        for (prior, restore_fails, kept, commanded, why) in cases {
            let dir = tempfile::tempdir().unwrap();
            let curve = dir.path().join("fan_curve");
            if restore_fails {
                std::fs::create_dir(&curve).unwrap();
            } else {
                std::fs::write(&curve, "0: 0C 40%\n").unwrap();
            }
            let cache = crate::health::cache::StateCache::new();
            let fan_id = "amd_gpu:0000:03:00.0";
            let taken_at_write = std::cell::Cell::new(false);
            let sequence = pmfw_verify_sequence(
                &curve,
                None,
                dir.path(),
                prior,
                &cache,
                fan_id,
                |_| {
                    taken_at_write.set(cache.gpu_handback().is_taken(fan_id));
                    Err(crate::error::HwmonError::WriteError {
                        path: "fan_curve".into(),
                        message: "EINVAL".into(),
                    })
                },
                || panic!("a refused test write must not settle"),
                || panic!("a refused test write must not renew"),
            );
            assert!(
                taken_at_write.get(),
                "precondition ({why}): the card is on the stop list when the test write runs"
            );
            assert!(
                matches!(
                    sequence,
                    GpuVerifySequence::WriteFailed { restore_failed, .. }
                        if restore_failed == restore_fails
                ),
                "precondition ({why}): the WriteFailed arm ran with this restore outcome"
            );
            assert_eq!(cache.gpu_handback().is_taken(fan_id), kept, "{why}");
            assert_eq!(
                cache
                    .snapshot()
                    .gpu_fans
                    .get(fan_id)
                    .and_then(|f| f.last_commanded_pct),
                commanded,
                "{why}: the restore's duty must be stamped as the Completed arm stamps it"
            );
        }
    }

    /// AppState carrying one PMFW GPU whose `fan_curve` is a real temp file, so
    /// the verify's writes and its restore are observable.
    fn pmfw_verify_state(curve: std::path::PathBuf) -> Arc<AppState> {
        use crate::hwmon::gpu_detect::AmdGpuInfo;
        gpu_test_state(AmdGpuInfo {
            pci_bdf: "0000:03:00.0".into(),
            pci_device_id: 0x7550,
            pci_revision: 0xC0,
            pci_class: 0x030000,
            marketing_name: Some("RX 9070 XT".into()),
            hwmon_path: std::path::PathBuf::from("/nonexistent/hwmon"),
            fan_curve_path: Some(curve),
            fan_zero_rpm_path: None,
            is_discrete: true,
            has_fan_rpm: false,
            has_pwm: false,
            has_pwm_enable: false,
            overdrive_enabled: true,
        })
    }

    /// AppState carrying a pre-RDNA3 card on the legacy `pwm1` path, its hwmon
    /// dir a real temp dir (`legacy_card`), so the reset's write is observable.
    fn legacy_reset_state(hwmon: std::path::PathBuf) -> Arc<AppState> {
        use crate::hwmon::gpu_detect::AmdGpuInfo;
        let gpu = AmdGpuInfo {
            pci_bdf: "0000:03:00.0".into(),
            // Navi 21 (RX 6800 XT): pre-RDNA3, so the legacy path is open.
            pci_device_id: 0x73BF,
            pci_revision: 0xC1,
            pci_class: 0x030000,
            marketing_name: Some("RX 6800 XT".into()),
            hwmon_path: hwmon,
            fan_curve_path: None,
            fan_zero_rpm_path: None,
            is_discrete: true,
            has_fan_rpm: true,
            has_pwm: true,
            has_pwm_enable: true,
            overdrive_enabled: false,
        };
        assert!(
            gpu.can_write_legacy_pwm() && gpu.fan_curve_path.is_none(),
            "fixture check: the reset must take the legacy arm"
        );
        gpu_test_state(gpu)
    }

    fn gpu_test_state(gpu: crate::hwmon::gpu_detect::AmdGpuInfo) -> Arc<AppState> {
        let cache = Arc::new(crate::health::cache::StateCache::new());
        let readiness_rollup = Arc::new(parking_lot::Mutex::new(None));
        let (_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        Arc::new(AppState {
            cache,
            staleness_config: crate::health::staleness::StalenessConfig::default(),
            daemon_version: "0.0.0-test".into(),
            fan_controller: Arc::new(parking_lot::RwLock::new(None)),
            openfan_runtime: crate::api::handlers::OpenFanRuntime {
                timeout: std::time::Duration::from_millis(500),
                interval: std::time::Duration::from_millis(1000),
                shutdown: shutdown_rx,
            },
            hwmon_controller: None,
            start_time: std::time::Instant::now(),
            history: Arc::new(crate::health::history::HistoryRing::new(250)),
            active_profile: Arc::new(parking_lot::Mutex::new(None)),
            openfan_calibration: Default::default(),
            characterization: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            validation: std::sync::Arc::new(Default::default()),
            characterization_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            control_path: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            control_path_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            stall_probe: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            stall_probe_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            control_paths: std::sync::Arc::new(parking_lot::RwLock::new(Default::default())),
            pwm_baselines: Default::default(),
            pwm_verification: Default::default(),
            openfan_rescanning: Default::default(),
            last_openfan_rescan: Arc::new(parking_lot::Mutex::new(None)),
            adopted_poll_tasks: Arc::new(parking_lot::Mutex::new(Default::default())),
            openfan_maintenance: Default::default(),
            amd_gpus: vec![gpu],
            intel_gpus: Vec::new(),
            nvidia_gpus: Vec::new(),
            profile_search_dirs: parking_lot::RwLock::new(Vec::new()),
            config_path: String::new(),
            runtime_config_path: std::path::PathBuf::new(),
            sensor_rescan_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            header_roles: Arc::new(parking_lot::RwLock::new(Arc::new(
                std::collections::HashMap::new(),
            ))),
            cooling_devices: Arc::new(parking_lot::RwLock::new(Arc::new(Vec::new()))),
            override_table: Arc::new(parking_lot::Mutex::new(
                crate::control_override::OverrideTable::new(),
            )),
            allow_port_probe: false,
            board_context: Default::default(),
            running_config: Default::default(),
            readiness_rollup: readiness_rollup.clone(),
            config_write: Default::default(),
            runtime_config_degraded: Default::default(),
            assessment: Arc::new(crate::api::handlers::AssessmentCache::new(readiness_rollup)),
        })
    }

    /// `GPU-e` (DEC-468): a successful reset puts the card on firmware auto, and
    /// "on auto" records NO duty — the state the engine's own hand-back leaves
    /// (DEC-448). It used to record 0, which `GpuBackend::apply`'s 5 % coalescing
    /// then held against a profile command of 0–4 % once an activation gave the
    /// card back to the engine, leaving it on firmware auto while the profile
    /// named it. Both arms: PMFW and legacy `pwm1`.
    #[tokio::test]
    async fn a_reset_records_no_duty_on_either_arm() {
        let fan_id = "amd_gpu:0000:03:00.0";

        let dir = tempfile::tempdir().unwrap();
        let curve = dir.path().join("fan_curve");
        std::fs::write(&curve, "").unwrap();
        let pmfw = pmfw_verify_state(curve.clone());

        let (_tmp, hwmon, _record) = legacy_card();
        std::fs::write(hwmon.join("pwm1_enable"), "1\n").unwrap();
        let legacy = legacy_reset_state(hwmon.clone());

        for (arm, state) in [("pmfw", &pmfw), ("legacy", &legacy)] {
            // The engine drove the card at 40 % before the reset.
            state.cache.set_gpu_fan_commanded_pct(fan_id, 40);

            let (status, _) = gpu_reset_fan_handler(
                axum::extract::State(state.clone()),
                axum::extract::Path("0000:03:00.0".to_string()),
            )
            .await;

            assert_eq!(status, StatusCode::OK, "{arm}: the reset succeeded");
            assert_eq!(
                state
                    .cache
                    .snapshot()
                    .gpu_fans
                    .get(fan_id)
                    .and_then(|f| f.last_commanded_pct),
                None,
                "{arm}: a card on firmware auto must record no duty — 40 means the \
                 reset left the engine's duty, 0 means a later 0–4 % profile command \
                 is coalesced away"
            );
        }
        // Each reset really took its own arm and wrote the hardware.
        assert_eq!(
            std::fs::read_to_string(&curve).unwrap(),
            "c\n",
            "pmfw: reset written"
        );
        assert_eq!(
            std::fs::read_to_string(hwmon.join("pwm1_enable"))
                .unwrap()
                .trim(),
            "2",
            "legacy: back on automatic"
        );
    }

    /// DEC-297 remediation. Both reviewers independently found that the restore's
    /// **auto** branch was never stamped into the cache, and the test above misses
    /// it because it seeds a prior duty (40) and so always takes the `Some(p)`
    /// branch.
    ///
    /// This is the common Hardware-page case: a GPU no profile has commanded yet.
    /// The restore correctly hands the card back to firmware-auto, but the cache
    /// was left reporting the TEST SPEED — so `/fans` and `/poll` claimed a manual
    /// duty for a card on the firmware curve, and `GpuBackend::apply`'s 5% coalescing
    /// would then skip a later profile command near that value, leaving the GPU on
    /// firmware-auto while the daemon believed it was commanding.
    #[tokio::test]
    async fn a_gpu_verify_with_no_prior_duty_records_the_auto_restore() {
        let dir = tempfile::tempdir().unwrap();
        let curve = dir.path().join("fan_curve");
        std::fs::write(&curve, "0: 0C 40%\n").unwrap();
        let state = pmfw_verify_state(curve);
        let cache = state.cache.clone();
        let fan_id = "amd_gpu:0000:03:00.0";

        // No `set_gpu_fan_commanded_pct` seed: `prior_pct` is None, so the restore
        // takes the reset-to-auto branch.
        assert!(
            cache
                .snapshot()
                .gpu_fans
                .get(fan_id)
                .and_then(|f| f.last_commanded_pct)
                .is_none(),
            "fixture check: this test is about the NO-prior-duty path"
        );

        let _ = gpu_verify_handler(
            axum::extract::State(state.clone()),
            axum::extract::Path("0000:03:00.0".to_string()),
        )
        .await;

        let commanded = cache
            .snapshot()
            .gpu_fans
            .get(fan_id)
            .and_then(|f| f.last_commanded_pct);
        assert_eq!(
            commanded, None,
            "an auto-restore must record no duty (`GPU-e`), not the test speed and not \
             0 — either would let the engine coalesce away a later profile command near \
             it, and the test speed would also show a manual duty for a firmware-controlled \
             card"
        );
        // DEC-435: back on firmware auto, so the card is off the stop list — a stop
        // must not reset a curve another tool puts on it afterwards. (The sibling
        // cancellation test proves the verify put it on the list.)
        assert!(
            !cache.gpu_handback().is_taken(fan_id),
            "a verify that restored to auto must hand the card back"
        );
    }

    /// DEC-297 (AUD-b2). The GPU verify used to run inline with a
    /// `tokio::time::sleep(...).await` between the test write and the restore, so
    /// dropping the handler future — a client disconnect, or the GUI's own
    /// timeout — skipped the restore and left the fan pinned at the test speed.
    /// This is the same defect DEC-290 fixed for the hwmon verify, one endpoint
    /// over, and the handler's own doc claimed it already "mirrors" that one.
    ///
    /// Less severe than the hwmon case on purpose — `select_gpu_test_speed`
    /// biases UPWARD, so a strand leaves the fan too fast rather than too slow —
    /// but a strand it is: with no active profile nothing rewrites it.
    #[tokio::test]
    async fn a_cancelled_gpu_verify_still_restores_the_curve() {
        let dir = tempfile::tempdir().unwrap();
        let curve = dir.path().join("fan_curve");
        std::fs::write(&curve, "0: 0C 40%\n").unwrap();
        let state = pmfw_verify_state(curve.clone());
        let cache = state.cache.clone();
        let fan_id = "amd_gpu:0000:03:00.0";

        // Seed the prior duty. This is what the restore must put back, and it is
        // ALSO the only observable: `fan_curve` is a PMFW command channel, not a
        // state file — each write replaces the last, so after a run the file
        // holds only the trailing commit token and reading it back proves
        // nothing. The cache is where "what was last commanded" actually lives.
        cache.set_gpu_fan_commanded_pct(fan_id, 40);
        let test_speed = select_gpu_test_speed(Some(40), 15, 100);
        assert_ne!(
            test_speed, 40,
            "fixture check: the test speed must differ from the prior duty, or the \
             assertion below cannot discriminate a restore from its absence"
        );

        {
            let fut = gpu_verify_handler(
                axum::extract::State(state.clone()),
                axum::extract::Path("0000:03:00.0".to_string()),
            );
            tokio::pin!(fut);
            tokio::select! {
                _ = &mut fut => panic!("the verify completed too fast to model a cancellation"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            }
        } // <- handler future dropped, exactly as axum does on a client disconnect

        // The abandoned `spawn_blocking` keeps running. Poll for it rather than
        // sleeping a fixed span; the bound is a backstop, not the expected path.
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(constants::VERIFY_WAIT_SECONDS as u64 + 6);
        while cache.verify_active() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            !cache.verify_active(),
            "the abandoned verify never finished — it must run to completion"
        );

        let commanded = cache
            .snapshot()
            .gpu_fans
            .get(fan_id)
            .and_then(|f| f.last_commanded_pct);
        assert_eq!(
            commanded,
            Some(40),
            "a cancelled GPU verify must still restore the prior duty; \
             {test_speed:?} means it was left at the test speed"
        );
        // DEC-435: the verify named the card before its test write (nothing else
        // in this fixture writes a curve), and a restore to a prior duty keeps it
        // on the stop list — the engine drove it before and drives it after.
        assert!(
            cache.gpu_handback().is_taken(fan_id),
            "the verify must put the card on the stop list before its test write"
        );
    }

    // ── DEC-502: a GPU fan test caught by the stop ──────────────────────

    /// How long after the stop begins a woken fan test may take to answer. Far
    /// under `VERIFY_WAIT_SECONDS`, so a settle that slept its span out reds.
    const WOKEN_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);
    const _: () = assert!(WOKEN_WITHIN.as_secs() * 2 < constants::VERIFY_WAIT_SECONDS as u64);

    /// Poll `cond` until it holds, failing loudly after a deadline.
    async fn wait_until(what: &str, cond: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cond() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// [SAFETY] G1/G2 on the PMFW path. A stop that begins mid-settle wakes the
    /// test, which restores at once — inside the stop's 3 s server drain, so
    /// before its GPU reset — and restores to firmware AUTO, not to the
    /// engine's prior 40 %: once the stop has begun no curve may reach a card.
    /// Each half reds on its own: with `thread::sleep` back the answer takes the
    /// whole settle; with the restore target ignored the card keeps 40 % and
    /// stays on the stop list.
    #[tokio::test]
    async fn a_stop_cuts_a_pmfw_fan_test_short_and_hands_the_card_back() {
        let dir = tempfile::tempdir().unwrap();
        let curve = dir.path().join("fan_curve");
        std::fs::write(&curve, "0: 0C 40%\n").unwrap();
        let state = pmfw_verify_state(curve);
        let cache = state.cache.clone();
        let fan_id = "amd_gpu:0000:03:00.0";
        cache.set_gpu_fan_commanded_pct(fan_id, 40);
        let test_speed = select_gpu_test_speed(Some(40), 15, 100);
        assert_ne!(test_speed, 40, "fixture check: the two duties differ");
        let commanded = || {
            cache
                .snapshot()
                .gpu_fans
                .get(fan_id)
                .and_then(|f| f.last_commanded_pct)
        };

        let verify = tokio::spawn(gpu_verify_handler(
            axum::extract::State(state.clone()),
            axum::extract::Path("0000:03:00.0".to_string()),
        ));
        wait_until("the test write", || commanded() == Some(test_speed)).await;
        let stop = std::time::Instant::now();
        cache.begin_shutdown();
        let (status, body) = tokio::time::timeout(std::time::Duration::from_secs(15), verify)
            .await
            .expect("the verify answers")
            .expect("the verify did not panic");
        let took = stop.elapsed();

        assert!(
            took < WOKEN_WITHIN,
            "the test answered {took:?} after the stop began — it slept out its settle, \
             so its restore would land after the stop's GPU reset"
        );
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body:?}");
        assert_eq!(body.0["error"]["code"], "hardware_unavailable");
        assert_eq!(body.0["error"]["retryable"], true);
        assert_eq!(
            commanded(),
            None,
            "restored to firmware auto; Some(40) is a curve written after the stop began"
        );
        assert!(
            !cache.gpu_handback().is_taken(fan_id),
            "back on auto, so off the stop list"
        );
        assert!(!cache.verify_active(), "the pause is released");
    }

    /// [SAFETY] G1 on the legacy path: the card in manual mode at its test speed
    /// gets its ORIGINAL mode back at once, and its crash-record line goes, so
    /// nothing is left for `ExecStopPost` — the one hand-back a daemon run by
    /// hand never gets.
    #[tokio::test]
    async fn a_stop_cuts_a_legacy_fan_test_short_and_gives_the_card_back() {
        let (tmp, hwmon, record) = legacy_card();
        let state = legacy_reset_state(hwmon.clone());
        let cache = state.cache.clone();
        assert!(cache.set_hand_back_record_dir(tmp.path().to_path_buf()));
        let enable = hwmon.join("pwm1_enable");

        let verify = tokio::spawn(gpu_verify_handler(
            axum::extract::State(state.clone()),
            axum::extract::Path("0000:03:00.0".to_string()),
        ));
        wait_until("manual mode", || {
            std::fs::read_to_string(&enable).is_ok_and(|v| v.trim() == "1")
        })
        .await;
        assert!(record.exists(), "precondition: the take is on the record");
        let stop = std::time::Instant::now();
        cache.begin_shutdown();
        let (status, body) = tokio::time::timeout(std::time::Duration::from_secs(15), verify)
            .await
            .expect("the verify answers")
            .expect("the verify did not panic");
        let took = stop.elapsed();

        assert!(
            took < WOKEN_WITHIN,
            "answered {took:?} after the stop began"
        );
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body:?}");
        assert_eq!(
            std::fs::read_to_string(&enable).unwrap().trim(),
            "2",
            "the card's original mode is back"
        );
        assert!(!record.exists(), "a restored card's line is dropped");
    }

    /// [SAFETY] No fan test starts once the stop has begun: nothing is written,
    /// the card is not put on the stop list, and the pause is not taken.
    #[tokio::test]
    async fn no_gpu_fan_test_starts_once_the_stop_has_begun() {
        let dir = tempfile::tempdir().unwrap();
        let curve = dir.path().join("fan_curve");
        std::fs::write(&curve, "0: 0C 40%\n").unwrap();
        let state = pmfw_verify_state(curve.clone());
        state.cache.begin_shutdown();

        let (status, body) = gpu_verify_handler(
            axum::extract::State(state.clone()),
            axum::extract::Path("0000:03:00.0".to_string()),
        )
        .await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body:?}");
        assert_eq!(std::fs::read_to_string(&curve).unwrap(), "0: 0C 40%\n");
        assert!(!state.cache.gpu_handback().is_taken("amd_gpu:0000:03:00.0"));
        assert!(!state.cache.verify_active());
    }

    /// [SAFETY] The check under the GPU write lock, for a test that passed the
    /// handler's own check and then waited for the lock while the stop began:
    /// no test write, and the card is not put on the stop list. Both sequences.
    #[test]
    fn a_stop_that_began_while_the_test_waited_for_the_lock_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let curve = dir.path().join("fan_curve");
        std::fs::write(&curve, "0: 0C 40%\n").unwrap();
        let cache = crate::health::cache::StateCache::new();
        cache.begin_shutdown();
        let fan_id = "amd_gpu:0000:03:00.0";

        let pmfw = pmfw_verify_sequence(
            &curve,
            None,
            dir.path(),
            Some(40),
            &cache,
            fan_id,
            |_| panic!("no test write once the stop has begun"),
            || panic!("no settle"),
            || panic!("no renew"),
        );
        assert!(matches!(
            pmfw,
            GpuVerifySequence::Interrupted {
                restore_failed: false
            }
        ));
        assert!(!cache.gpu_handback().is_taken(fan_id));
        assert_eq!(std::fs::read_to_string(&curve).unwrap(), "0: 0C 40%\n");

        let (_tmp, hwmon, record) = legacy_card();
        let legacy = legacy_verify_sequence(
            &hwmon,
            &cache,
            fan_id,
            Some(&record),
            || panic!("no settle"),
            || panic!("no renew"),
        );
        assert!(matches!(
            legacy,
            GpuVerifySequence::Interrupted {
                restore_failed: false
            }
        ));
        assert_eq!(
            std::fs::read_to_string(hwmon.join("pwm1_enable"))
                .unwrap()
                .trim(),
            "2",
            "never put in manual mode"
        );
        assert!(!record.exists(), "nothing taken, nothing recorded");
    }

    fn st(
        applied: Option<u8>,
        rpm: Option<u16>,
        pwm_enable: Option<u8>,
        zero_rpm: Option<bool>,
    ) -> GpuVerifyState {
        GpuVerifyState {
            applied_speed_pct: applied,
            rpm,
            pwm_enable,
            zero_rpm_enabled: zero_rpm,
            hotspot_c: None,
        }
    }

    #[test]
    fn test_speed_biases_upward_from_idle() {
        assert_eq!(select_gpu_test_speed(None, 15, 100), 75);
        assert_eq!(select_gpu_test_speed(Some(0), 15, 100), 75);
        assert_eq!(select_gpu_test_speed(Some(40), 15, 100), 75);
    }

    #[test]
    fn test_speed_pushes_to_max_when_already_high() {
        assert_eq!(select_gpu_test_speed(Some(70), 15, 100), 100);
        assert_eq!(select_gpu_test_speed(Some(95), 15, 100), 100);
    }

    #[test]
    fn test_speed_clamped_to_od_range() {
        // Narrow OD_RANGE caps the upward target — never below od_min.
        assert_eq!(select_gpu_test_speed(None, 30, 50), 50);
        assert_eq!(select_gpu_test_speed(Some(80), 20, 60), 60);
    }

    #[test]
    fn classify_effective_via_rpm_rise_from_idle() {
        let init = st(None, Some(0), None, Some(false));
        let fin = st(Some(75), Some(1600), None, Some(false));
        let (r, _) = classify_gpu_verify_result(&init, &fin, 75, &NOT_RDNA4);
        assert_eq!(r, "effective");
    }

    #[test]
    fn classify_effective_when_already_fast() {
        // Driven at 80% and spinning; test pushed to 100% — RPM barely moves but
        // the fan is demonstrably spinning and the curve applied.
        let init = st(Some(80), Some(1500), None, Some(false));
        let fin = st(Some(100), Some(1550), None, Some(false));
        let (r, _) = classify_gpu_verify_result(&init, &fin, 100, &NOT_RDNA4);
        assert_eq!(r, "effective");
    }

    #[test]
    fn classify_curve_not_applied_when_readback_differs() {
        // Write silently ignored — old curve persists.
        let init = st(None, Some(0), None, Some(true));
        let fin = st(Some(30), Some(0), None, Some(true));
        let (r, d) = classify_gpu_verify_result(&init, &fin, 75, &NOT_RDNA4);
        assert_eq!(r, "curve_not_applied");
        assert!(d.contains("ppfeaturemask"));
    }

    #[test]
    fn classify_curve_not_applied_when_unreadable() {
        let init = st(None, Some(0), None, Some(false));
        let fin = st(None, Some(0), None, Some(false));
        let (r, _) = classify_gpu_verify_result(&init, &fin, 75, &NOT_RDNA4);
        assert_eq!(r, "curve_not_applied");
    }

    #[test]
    fn classify_no_rpm_effect_when_applied_but_dead() {
        // Curve applied, zero-RPM disabled, fan still stopped → genuine no-effect.
        let init = st(None, Some(0), None, Some(false));
        let fin = st(Some(75), Some(0), None, Some(false));
        let (r, d) = classify_gpu_verify_result(&init, &fin, 75, &NOT_RDNA4);
        assert_eq!(r, "no_rpm_effect");
        assert!(d.contains("no hardware effect"));
    }

    #[test]
    fn classify_zero_rpm_suppressed_is_not_a_failure() {
        // Curve applied but zero-RPM still on (idle) → normal, not a fault.
        let init = st(None, Some(0), None, Some(true));
        let fin = st(Some(75), Some(0), None, Some(true));
        let (r, _) = classify_gpu_verify_result(&init, &fin, 75, &NOT_RDNA4);
        assert_eq!(r, "zero_rpm_suppressed");
    }

    #[test]
    fn classify_rpm_unavailable_when_no_tach() {
        let init = st(None, None, None, Some(false));
        let fin = st(Some(75), None, None, Some(false));
        let (r, _) = classify_gpu_verify_result(&init, &fin, 75, &NOT_RDNA4);
        assert_eq!(r, "rpm_unavailable");
    }

    #[test]
    fn classify_pwm_enable_reverted_on_legacy_path() {
        // Legacy path: pwm1_enable left manual during the test. `BRD-o`: the
        // text names the value read and blames no motherboard firmware.
        let init = st(Some(30), Some(800), Some(1), None);
        let fin = st(Some(75), Some(820), Some(2), None);
        let (r, d) = classify_gpu_verify_result(&init, &fin, 75, &NOT_RDNA4);
        assert_eq!(r, "pwm_enable_reverted");
        assert!(d.contains("to 2 (automatic)"), "{d}");
        assert!(d.contains("another GPU fan tool"), "{d}");
        assert!(!d.contains("Smart Fan"), "{d}");

        // Opposite arm: 0 is full speed, and must not be called automatic.
        let fin = st(Some(75), Some(820), Some(0), None);
        let (r, d) = classify_gpu_verify_result(&init, &fin, 75, &NOT_RDNA4);
        assert_eq!(r, "pwm_enable_reverted");
        assert!(d.contains("to 0 (no fan control: full speed)"), "{d}");
        assert!(!d.contains("automatic) during"), "{d}");
    }

    /// A card that is not RDNA4: the GPD-b arms never apply.
    const NOT_RDNA4: GpuVerifyContext = GpuVerifyContext {
        rdna4: false,
        idle_band_below_c: 60.0,
    };

    fn hot(mut state: GpuVerifyState, hotspot: Option<f64>) -> GpuVerifyState {
        state.hotspot_c = hotspot;
        state
    }

    /// GPD-b, one fixture per verdict on an RX 9070 XT (0x7550) with no stop
    /// temperature file (default band): an idle stopped fan is `idle_fan_stop`,
    /// unreadable hotspot too; a fan at 0 before and after above the band is
    /// `rpm_reads_zero`; a fan that read non-zero and did not rise stays
    /// `no_rpm_effect`.
    #[test]
    fn classify_rdna4_zero_rpm_verdicts() {
        let ctx = GpuVerifyContext::for_card(0x7550, None);
        let band = ctx.idle_band_below_c;
        assert!(ctx.rdna4);
        let below = band - 15.0;
        let above = band + 5.0;
        let init = |h| hot(st(None, Some(0), None, Some(false)), h);
        let fin = |h| hot(st(Some(75), Some(0), None, Some(false)), h);

        let (r, d) = classify_gpu_verify_result(&init(Some(below)), &fin(Some(below)), 75, &ctx);
        assert_eq!(r, "idle_fan_stop", "{d}");
        assert!(
            d.contains(&format!("{below:.0}")),
            "names the measured hotspot: {d}"
        );

        let (r, _) = classify_gpu_verify_result(&init(None), &fin(None), 75, &ctx);
        assert_eq!(r, "idle_fan_stop", "unreadable hotspot counts as idle");

        let (r, d) = classify_gpu_verify_result(&init(Some(above)), &fin(Some(above)), 75, &ctx);
        assert_eq!(r, "rpm_reads_zero", "{d}");

        // Exactly at the band is not below it.
        let (r, _) = classify_gpu_verify_result(&init(Some(band)), &fin(Some(band)), 75, &ctx);
        assert_eq!(r, "rpm_reads_zero");

        // Non-zero and not rising: a real no-effect, at any hotspot.
        let spun = hot(st(None, Some(900), None, Some(false)), Some(below));
        let still = hot(st(Some(75), Some(910), None, Some(false)), Some(below));
        let (r, _) = classify_gpu_verify_result(&spun, &still, 75, &ctx);
        assert_eq!(r, "no_rpm_effect");

        // Spinning before, 0 after: not an idle stop and not the stuck-tach
        // shape, below the band, above it or with no hotspot.
        for h in [Some(below), Some(above), None] {
            let spun = hot(st(None, Some(900), None, Some(false)), h);
            let (r, _) = classify_gpu_verify_result(&spun, &fin(h), 75, &ctx);
            assert_eq!(r, "no_rpm_effect", "hotspot {h:?}");
        }

        // No RPM reading before the test, 0 after, cool: idle stop.
        let unread = hot(st(None, None, None, Some(false)), Some(below));
        let (r, _) = classify_gpu_verify_result(&unread, &fin(Some(below)), 75, &ctx);
        assert_eq!(r, "idle_fan_stop");
    }

    /// GPD-b: `hotspot_c` is omitted from the wire when unread and written as a
    /// number when read, so an older GUI sees no new key from a legacy card.
    #[test]
    fn hotspot_c_is_omitted_when_unread_and_a_number_when_read() {
        let unread = serde_json::to_value(st(Some(75), Some(0), None, Some(false))).unwrap();
        assert!(unread.get("hotspot_c").is_none(), "{unread}");
        let read = serde_json::to_value(hot(st(Some(75), Some(0), None, Some(false)), Some(44.5)))
            .unwrap();
        assert_eq!(read["hotspot_c"], serde_json::json!(44.5));
    }

    /// GPD-b opposite branches: the same idle stop on a non-RDNA4 card (RDNA3
    /// RX 7900 XTX) stays `no_rpm_effect`, and an RDNA4 card reporting zero-RPM
    /// on stays `zero_rpm_suppressed` — the new arms sit after that one.
    #[test]
    fn classify_rdna4_arms_need_rdna4_and_zero_rpm_not_on() {
        let init = hot(st(None, Some(0), None, Some(false)), Some(35.0));
        let fin = hot(st(Some(75), Some(0), None, Some(false)), Some(35.0));
        let rdna3 = GpuVerifyContext::for_card(0x744C, None);
        assert!(!rdna3.rdna4);
        let (r, _) = classify_gpu_verify_result(&init, &fin, 75, &rdna3);
        assert_eq!(r, "no_rpm_effect");

        let rdna4 = GpuVerifyContext::for_card(0x7590, None);
        let init_on = hot(st(None, Some(0), None, Some(true)), Some(35.0));
        let fin_on = hot(st(Some(75), Some(0), None, Some(true)), Some(35.0));
        let (r, _) = classify_gpu_verify_result(&init_on, &fin_on, 75, &rdna4);
        assert_eq!(r, "zero_rpm_suppressed");
    }

    /// GPD-b: the card's own stop temperature moves the band; without one the
    /// default applies.
    #[test]
    fn rdna4_idle_band_follows_the_cards_stop_temperature() {
        let default = GpuVerifyContext::for_card(0x7551, None);
        assert_eq!(
            default.idle_band_below_c,
            f64::from(
                constants::GPU_RDNA4_DEFAULT_ZERO_RPM_STOP_C
                    + constants::GPU_RDNA4_IDLE_BAND_MARGIN_C
            )
        );
        let card = GpuVerifyContext::for_card(0x7551, Some(70));
        assert_eq!(
            card.idle_band_below_c,
            f64::from(70 + constants::GPU_RDNA4_IDLE_BAND_MARGIN_C)
        );
    }

    /// GPD-b wiring: the PMFW sequence reads the hotspot (`junction`) into both
    /// states and the card's stop temperature from the `fan_curve` sibling, and
    /// what it returns, classified as the handler does, gives the verdict. The
    /// stop-temperature file is what moves the same 44 °C reading from "idle"
    /// to "reads zero".
    #[test]
    fn pmfw_sequence_feeds_hotspot_and_stop_temperature_to_the_classifier() {
        fn flat(speed: u8) -> String {
            let mut c = String::from("OD_FAN_CURVE:\n");
            for i in 0..5 {
                c.push_str(&format!("{i}: {}C {speed}%\n", 40 + 10 * i));
            }
            c.push_str(
                "OD_RANGE:\nFAN_CURVE(hotspot temp): 25C 100C\nFAN_CURVE(fan speed): 15% 100%\n",
            );
            c
        }
        let run = |stop_file: Option<&str>| {
            let dir = tempfile::tempdir().unwrap();
            let fan_ctrl = dir.path().join("gpu_od/fan_ctrl");
            std::fs::create_dir_all(&fan_ctrl).unwrap();
            let curve = fan_ctrl.join("fan_curve");
            std::fs::write(&curve, flat(30)).unwrap();
            if let Some(stop) = stop_file {
                std::fs::write(fan_ctrl.join("fan_zero_rpm_stop_temperature"), stop).unwrap();
            }
            let hwmon = dir.path().join("hwmon3");
            std::fs::create_dir_all(&hwmon).unwrap();
            std::fs::write(hwmon.join("fan1_input"), "0\n").unwrap();
            std::fs::write(hwmon.join("temp2_label"), "junction\n").unwrap();
            std::fs::write(hwmon.join("temp2_input"), "44000\n").unwrap();
            let cache = crate::health::cache::StateCache::new();
            let seq = pmfw_verify_sequence(
                &curve,
                None,
                &hwmon,
                None,
                &cache,
                "amd_gpu:0000:03:00.0",
                |speed| {
                    std::fs::write(&curve, flat(speed)).unwrap();
                    Ok(())
                },
                || {},
                || {},
            );
            let GpuVerifySequence::Completed {
                initial,
                final_state,
                test_speed,
                zero_rpm_stop_c,
                ..
            } = seq
            else {
                panic!("the sequence must complete");
            };
            assert_eq!(initial.hotspot_c, Some(44.0));
            assert_eq!(final_state.hotspot_c, Some(44.0));
            let ctx = GpuVerifyContext::for_card(0x7550, zero_rpm_stop_c);
            classify_gpu_verify_result(&initial, &final_state, test_speed, &ctx).0
        };

        assert_eq!(
            run(None),
            "idle_fan_stop",
            "44 °C is below the default band"
        );
        let low_stop =
            "FAN_ZERO_RPM_STOP_TEMPERATURE:\n30\nOD_RANGE:\nZERO_RPM_STOP_TEMPERATURE: 30 110\n";
        assert_eq!(
            run(Some(low_stop)),
            "rpm_reads_zero",
            "a 30 °C stop puts 44 °C above the band"
        );
    }

    #[test]
    fn classify_speed_match_absorbs_rounding() {
        // raw↔percent rounding within tolerance still counts as applied.
        let init = st(None, Some(0), None, Some(false));
        let fin = st(Some(73), Some(1400), None, Some(false));
        let (r, _) = classify_gpu_verify_result(&init, &fin, 75, &NOT_RDNA4);
        assert_eq!(r, "effective");
    }
}
