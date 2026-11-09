//! Unix socket HTTP server lifecycle.
//!
//! The daemon runs its own accept loop rather than `axum::serve` (DEC-504).
//! The socket is `0666` (DEC-049), so any local user can connect, and
//! `axum::serve` accepts without limit and, having no timer, keeps an idle
//! connection open for ever. Every connection holds a file descriptor, and the
//! thermal force opens a fresh one for each hwmon write
//! (`RealSysfsWriter::write_file`). A user holding enough idle connections could
//! therefore make the emergency's writes fail with `EMFILE`. Here a connection is
//! accepted only while one of [`MAX_CONNECTIONS`] slots is free. A client beyond
//! that waits in the kernel's listen queue, where it costs the daemon no
//! descriptor. A connection that sends no complete request head within
//! [`HEADER_READ_TIMEOUT`], first or next, is closed.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::handler::Handler;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use axum::Router;
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tower::ServiceExt;

use super::handlers::{self, AppState};

/// Error returned by [`serve`]. The accept loop no longer fails, so nothing
/// currently produces it; it is kept so `main.rs`'s error arm and the
/// integration tests' `unwrap` stay meaningful if that changes.
pub type ServeError = Box<dyn std::error::Error + Send + Sync>;

/// [SAFETY] At most this many client connections at once (DEC-504). This caps
/// the descriptors the API can hold, so the hwmon writes always find one free
/// under the raised soft limit (`rlimit::raise_nofile_soft_limit`). Real clients
/// hold a handful: each of the GUI's few HTTP clients keeps one or two,
/// `ventulus-diag` one, the tray one at a time. 64 leaves room for all of them.
pub const MAX_CONNECTIONS: usize = 64;

/// How long a connection may take to send a complete request head (DEC-504).
/// hyper restarts the deadline when it starts reading each request head, so
/// this also closes an idle keep-alive connection. It does not run while a
/// handler works or while a body is read, so a long held-open request (the
/// deprecated `calibrate`) is unaffected. It is longer than the GUI's httpx
/// keep-alive expiry (5 s), so the GUI stops reusing an idle connection before
/// the daemon closes it.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Request-body cap for every route without its own (DEC-504). Every client body
/// apart from a profile document and a firmware file is a small JSON object.
pub const DEFAULT_BODY_LIMIT: usize = 64 * 1024;

/// Request-body cap for `POST /profiles` and `PUT /profiles/{id}`. Matches the
/// file-read cap in `atomic_io`.
pub const PROFILE_BODY_LIMIT: usize = 4 * 1024 * 1024;

/// Uploads of each kind handled at once (DEC-504): one profile upload and,
/// separately, one firmware upload. A profile body is parsed into a
/// `serde_json::Value`, which takes many times the raw size, and a firmware file
/// is parsed into an image; one at a time bounds that memory whatever the number
/// of connections.
pub const MAX_CONCURRENT_UPLOADS: usize = 1;

/// How long an upload waits for its slot before it is refused with
/// `503 internal_error`, retryable, `details.reason: "upload_busy"` (DEC-504).
/// Shorter than the GUI's 5 s request timeout (`API_TIMEOUT_S`), so an upload
/// is never applied after the GUI has given up on it and told the user it
/// failed.
pub const UPLOAD_SLOT_WAIT: Duration = Duration::from_secs(4);

/// How long an upload holding the slot may take to send its body before it is
/// refused with `408 validation_error`, `details.reason: "body_timeout"`
/// (DEC-504). A local 4 MiB upload takes milliseconds; this only stops a client
/// that stalls from holding the slot.
pub const UPLOAD_BODY_DEADLINE: Duration = Duration::from_secs(10);

/// `details.reason` of the `503` an upload gets when its slot stays taken.
pub const UPLOAD_BUSY_REASON: &str = "upload_busy";

/// `details.reason` of the `408` an upload gets when its body does not arrive.
pub const BODY_TIMEOUT_REASON: &str = "body_timeout";

/// After a failed `accept`, wait this long before the next one rather than
/// spinning (axum's behaviour too).
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_secs(1);

/// The accept loop's limits. [`serve`] always uses [`ServerLimits::default`];
/// tests use smaller values.
#[derive(Clone, Copy, Debug)]
pub struct ServerLimits {
    /// Connections served at once.
    pub max_connections: usize,
    /// See [`HEADER_READ_TIMEOUT`].
    pub header_read_timeout: Duration,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            max_connections: MAX_CONNECTIONS,
            header_read_timeout: HEADER_READ_TIMEOUT,
        }
    }
}

/// Peer credentials of a Unix-socket client, captured at connection-accept time
/// and exposed to handlers as `ConnectInfo<UdsConnectInfo>` (DEC-205).
///
/// `uid` is the connecting process's effective user id read from `SO_PEERCRED`,
/// or `None` when it could not be read. Handlers treat `None` as untrusted and
/// fail closed. `pid` is the same read's process id, used only to name the
/// caller in the journal (DEC-503); it is never an authorisation input.
/// `uid` is consumed by `POST /config/profile-search-dirs` to confine a
/// non-root caller's added search directories to its own home directory on
/// multi-user hosts (the file-picker UX is preserved for single-user desktops
/// and for root/CLI callers, which are exempt).
#[derive(Clone, Debug)]
pub struct UdsConnectInfo {
    /// Effective uid of the peer, or `None` if `SO_PEERCRED` was unavailable.
    pub uid: Option<u32>,
    /// Process id of the peer at connect time, or `None` if unavailable.
    pub pid: Option<i32>,
}

impl UdsConnectInfo {
    /// Read the peer's credentials from an accepted stream. A read failure
    /// degrades to `None` (fail-closed for the caller) rather than dropping the
    /// connection.
    fn of(stream: &UnixStream) -> Self {
        let cred = stream.peer_cred().ok();
        Self {
            uid: cred.map(|cred| cred.uid()),
            pid: cred.and_then(|cred| cred.pid()),
        }
    }
}

/// One kind of upload's slot and deadlines (DEC-504). Cloning shares the slot.
#[derive(Clone, Debug)]
pub struct UploadGate {
    slots: Arc<Semaphore>,
    max_body: usize,
    slot_wait: Duration,
    body_deadline: Duration,
}

impl UploadGate {
    /// A gate with [`MAX_CONCURRENT_UPLOADS`] slots for bodies of at most
    /// `max_body` bytes.
    #[must_use]
    pub fn new(max_body: usize, slot_wait: Duration, body_deadline: Duration) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(MAX_CONCURRENT_UPLOADS)),
            max_body,
            slot_wait,
            body_deadline,
        }
    }

    /// The profile uploads' gate: `POST /profiles` and `PUT /profiles/{id}`.
    #[must_use]
    pub fn profiles() -> Self {
        Self::new(PROFILE_BODY_LIMIT, UPLOAD_SLOT_WAIT, UPLOAD_BODY_DEADLINE)
    }

    /// The firmware upload's gate: `PUT /fans/openfan/firmware`.
    #[must_use]
    pub fn firmware() -> Self {
        Self::new(
            crate::serial::uf2::MAX_FILE_BYTES,
            UPLOAD_SLOT_WAIT,
            UPLOAD_BODY_DEADLINE,
        )
    }

    /// The semaphore behind the slot, so a test can hold it.
    #[must_use]
    pub fn slots(&self) -> Arc<Semaphore> {
        self.slots.clone()
    }
}

/// Middleware for the upload routes (DEC-504). It takes the gate's slot before
/// the body is read, refusing with `503` if the slot stays taken for
/// `slot_wait`; then reads the whole body within `body_deadline` (else `408`)
/// and at most `max_body` bytes (else `413`); and holds the slot until the
/// handler has answered. While it waits for the slot, nothing reads the
/// request's socket, so a client that gave up is not noticed; the wait is kept
/// shorter than the GUI's request timeout for that reason.
async fn upload_gate(State(gate): State<UploadGate>, request: Request, next: Next) -> Response {
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;

    let Ok(slot) = tokio::time::timeout(gate.slot_wait, gate.slots.clone().acquire_owned()).await
    else {
        let mut envelope = super::responses::ErrorEnvelope::internal(
            "another upload of this kind is in progress; try again",
        );
        envelope.error.details = Some(serde_json::json!({ "reason": UPLOAD_BUSY_REASON }));
        return handlers::error_response(axum::http::StatusCode::SERVICE_UNAVAILABLE, &envelope)
            .into_response();
    };
    let _slot = slot.expect("upload slots are never closed");

    let (parts, body) = request.into_parts();
    let limited = http_body_util::Limited::new(body, gate.max_body);
    let bytes = match tokio::time::timeout(gate.body_deadline, limited.collect()).await {
        Ok(Ok(collected)) => collected.to_bytes(),
        Ok(Err(e)) if e.is::<http_body_util::LengthLimitError>() => {
            // The framework's own 413 is plain text; this matches it.
            return (
                axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                "length limit exceeded",
            )
                .into_response();
        }
        Ok(Err(_)) => {
            return handlers::error_response(
                axum::http::StatusCode::BAD_REQUEST,
                &super::responses::ErrorEnvelope::validation("the request body could not be read"),
            )
            .into_response();
        }
        Err(_) => {
            let mut envelope = super::responses::ErrorEnvelope::validation(format!(
                "the request body did not arrive within {} s",
                gate.body_deadline.as_secs()
            ));
            envelope.error.details = Some(serde_json::json!({ "reason": BODY_TIMEOUT_REASON }));
            return handlers::error_response(axum::http::StatusCode::REQUEST_TIMEOUT, &envelope)
                .into_response();
        }
    };
    next.run(Request::from_parts(parts, axum::body::Body::from(bytes)))
        .await
}

/// Build the axum router with all endpoints and the production upload gates.
pub fn build_router(state: Arc<AppState>) -> Router {
    build_router_with_uploads(state, UploadGate::profiles(), UploadGate::firmware())
}

/// [`build_router`] with the upload gates given, so a test can hold a slot or
/// shorten a deadline. Each router gets its own gates, so each test server has
/// its own slots.
pub fn build_router_with_uploads(
    state: Arc<AppState>,
    profile_uploads: UploadGate,
    firmware_uploads: UploadGate,
) -> Router {
    Router::new()
        // Read endpoints
        .route("/status", get(handlers::status_handler))
        .route("/sensors", get(handlers::sensors_handler))
        .route("/fans", get(handlers::fans_handler))
        .route("/poll", get(handlers::poll_handler))
        .route("/sensors/history", get(handlers::history_handler))
        // OpenFanController calibration (DEC-452): 202 + detached, like the
        // stall probe, on the SAME single verify slot. Capability-gated on
        // `control.openfan_calibration`. The bare PWM/RPM write endpoints were
        // retired at 2.0.0 (DEC-165) — the profile engine is the sole writer.
        .route(
            "/fans/openfan/{channel}/calibration",
            post(handlers::openfan_calibration_handler),
        )
        .route(
            "/diagnostics/openfan-calibration",
            get(handlers::openfan_calibration_status_handler)
                .delete(handlers::openfan_calibration_cancel_handler),
        )
        // DEPRECATED (DEC-452): the same run, held open until it ends.
        .route(
            "/fans/openfan/{channel}/calibrate",
            post(handlers::calibrate_openfan_handler),
        )
        // Capabilities
        .route("/capabilities", get(handlers::capabilities_handler))
        // GPU fan endpoints — the bare PWM write was retired at 2.0.0
        // (DEC-165); reset (daemon-mediated) + verify remain.
        .route(
            "/gpu/{gpu_id}/fan/reset",
            post(handlers::gpu_reset_fan_handler),
        )
        .route(
            "/gpu/{gpu_id}/fan/verify",
            post(handlers::gpu_verify_handler),
        )
        // Hwmon endpoints — the lease quartet and the bare PWM write were
        // retired at 2.0.0 (DEC-165): the engine self-leases and is the sole
        // writer. Header listing + verify (daemon-performed) remain.
        .route("/hwmon/headers", get(handlers::hwmon_headers_handler))
        .route(
            "/hwmon/{header_id}/verify",
            post(handlers::hwmon_verify_handler),
        )
        // AIO-MB Phase 3: the deeper PWM/RPM sweep that sits ALONGSIDE the quick
        // verify above (never in place of it). Returns 202 and runs detached;
        // the client polls the /diagnostics/characterization pair below.
        .route(
            "/hwmon/{header_id}/characterize",
            post(handlers::hwmon_characterize_handler),
        )
        .route(
            "/diagnostics/characterization",
            get(handlers::characterization_status_handler)
                .delete(handlers::characterization_cancel_handler),
        )
        // AIO Phase 8 Batch 1: read-only safety preflight. Takes no lease and no
        // slot, so calling it reserves nothing — the POST below still runs its
        // own guards. Capability-gated on `control.diagnostic_preflight`.
        .route(
            "/diagnostics/preflight",
            get(handlers::discovery::preflight_handler),
        )
        // AIO Phase 8 Batch 1: PWM to tach control-path discovery. Establishes
        // which output actually drives which tach by measurement rather than by
        // sysfs numbering. Returns 202 and runs detached, like characterise; it
        // claims the SAME single verify slot, so at most one of the four
        // diagnostics ever drives hardware. Capability-gated on
        // `control.control_path_discovery`.
        .route(
            "/hwmon/{header_id}/discover-control-path",
            post(handlers::discovery::discover_control_path_handler),
        )
        .route(
            "/diagnostics/control-path",
            get(handlers::discovery::control_path_status_handler)
                .delete(handlers::discovery::control_path_cancel_handler),
        )
        // DEC-407 (DEC-404 Stage 3): the stall/restart probe, the one diagnostic
        // that writes below 20 % on purpose. 202 + detached, like characterise,
        // on the SAME single verify slot. Capability-gated on
        // `control.stall_probe`.
        .route(
            "/hwmon/{header_id}/stall-probe",
            post(handlers::stall_probe::stall_probe_handler),
        )
        .route(
            "/diagnostics/stall-probe",
            get(handlers::stall_probe::stall_probe_status_handler)
                .delete(handlers::stall_probe::stall_probe_cancel_handler),
        )
        // AIO-MB Phase 5: validation sessions. A session RECORDS what an
        // already-configured cooler did, and may ORCHESTRATE the two diagnostics
        // above — it never writes a duty itself, so it adds no second PWM
        // ownership path (§2). Capability-gated on `control.validation_sessions`.
        .route(
            "/validation/session",
            post(handlers::validation::start_session_handler)
                .get(handlers::validation::get_session_handler)
                .delete(handlers::validation::cancel_session_handler),
        )
        .route(
            "/validation/session/stop",
            post(handlers::validation::stop_session_handler),
        )
        .route(
            "/validation/session/event",
            post(handlers::validation::post_event_handler),
        )
        .route(
            "/validation/session/measurement",
            post(handlers::validation::post_measurement_handler),
        )
        .route(
            "/validation/sessions",
            get(handlers::validation::list_sessions_handler),
        )
        .route(
            "/validation/sessions/{session_id}",
            get(handlers::validation::get_session_by_id_handler),
        )
        // Hwmon rescan
        .route("/hwmon/rescan", post(handlers::hwmon_rescan_handler))
        // DEC-265: adopt an OpenFanController that appeared after boot,
        // or that failed its identity probe once at startup.
        .route(
            "/fans/openfan/rescan",
            post(handlers::openfan_rescan_handler),
        )
        // `ROLE-f`: each OpenFan channel's role and pump protection.
        .route("/fans/openfan/roles", get(handlers::openfan_roles_handler))
        // DEC-481: the OpenFan firmware update.
        .route(
            "/fans/openfan/device",
            get(handlers::openfan_device_handler),
        )
        // DEC-483: a firmware file for the daemon to write, at most 1 MiB.
        .route(
            "/fans/openfan/firmware",
            put(handlers::openfan_firmware_stage_handler.layer(
                axum::middleware::from_fn_with_state(firmware_uploads, upload_gate),
            ))
            .layer(DefaultBodyLimit::max(crate::serial::uf2::MAX_FILE_BYTES)),
        )
        .route(
            "/fans/openfan/maintenance",
            get(handlers::openfan_maintenance_status_handler)
                .post(handlers::openfan_maintenance_start_handler)
                .delete(handlers::openfan_maintenance_cancel_handler),
        )
        // Hardware diagnostics
        .route(
            "/diagnostics/hardware",
            get(handlers::hardware_diagnostics_handler),
        )
        // Read-only hwmon inventory (Phase 1): structured CPU/motherboard
        // sensors, controllable PWM headers, and monitor-only fan tachometers
        // (fanN_input with no matching pwmN). Additive; never writes hardware.
        .route("/inventory/hwmon", get(handlers::hwmon_inventory_handler))
        // AIO-MB Phase 4: read side of the topology. On /inventory/* rather
        // than /config because GET /config has never carried per-device state.
        .route(
            "/inventory/cooling-devices",
            get(handlers::cooling_devices_handler),
        )
        // Structured hardware-readiness list (Phase 3): actionable diagnose-and-
        // guide items (severity + recommended action + blocks-flags). Read-only.
        .route(
            "/inventory/readiness",
            get(handlers::hwmon_readiness_handler),
        )
        // Combined readiness + Super-I/O snapshot (DEC-207): one atomic fetch for
        // the merged "Cooling Hardware Readiness" GUI page, served from a single
        // shared passive scan. Read-only; `?refresh=true` forces a fresh
        // (coalesced) scan. 404-gated on older daemons.
        .route(
            "/inventory/hardware-readiness",
            get(handlers::hardware_readiness_handler),
        )
        // Passive Super-I/O chip detection (DEC-202): per-chip presence +
        // allowlisted "load this driver" recommendations. Read-only — never
        // probes I/O ports, loads modules, or writes hardware. 404-gated.
        .route("/inventory/superio", get(handlers::superio_handler))
        // Opt-in ACTIVE Super-I/O port probe (DEC-203): a deliberate, one-shot
        // /dev/port read to identify an UNBOUND chip. Gated by
        // [detection] allow_port_probe + CAP_SYS_RAWIO; refuses ports owned by a
        // bound driver or ACPI. Off by default; reports availability truthfully.
        .route(
            "/inventory/superio/probe",
            post(handlers::superio_probe_handler),
        )
        // Manual override + fan identify (DEC-163 / DEC-166). axum 0.8 needs
        // method-chaining on a single route per path (duplicate paths panic).
        .route(
            "/control/{control_id}/override",
            post(handlers::override_take_handler).delete(handlers::override_release_handler),
        )
        .route(
            "/control/{control_id}/override/renew",
            post(handlers::override_renew_handler),
        )
        .route(
            "/fans/{fan_id}/identify",
            post(handlers::fan_identify_handler),
        )
        // Profile management
        .route("/profile/active", get(handlers::active_profile_handler))
        .route(
            "/profile/activate",
            post(handlers::activate_profile_handler),
        )
        .route(
            "/profile/deactivate",
            post(handlers::deactivate_profile_handler),
        )
        // Profile CRUD (DEC-160) — daemon-owned profile store. axum 0.8 needs
        // method-chaining on a single route per path (duplicate paths panic).
        //
        // DEC-504: the two upload routes take a profile-sized body, one upload
        // at a time, through `upload_gate` (slot before the body, both bounded
        // in time). The layers sit on the handler, not the route, so a list,
        // get or delete never waits.
        .route(
            "/profiles",
            get(handlers::list_profiles_handler).post(
                handlers::create_profile_handler
                    .layer(DefaultBodyLimit::max(PROFILE_BODY_LIMIT))
                    .layer(axum::middleware::from_fn_with_state(
                        profile_uploads.clone(),
                        upload_gate,
                    )),
            ),
        )
        .route(
            "/profiles/{id}",
            get(handlers::get_profile_handler)
                .put(
                    handlers::update_profile_handler
                        .layer(DefaultBodyLimit::max(PROFILE_BODY_LIMIT))
                        .layer(axum::middleware::from_fn_with_state(
                            profile_uploads,
                            upload_gate,
                        )),
                )
                .delete(handlers::delete_profile_handler),
        )
        // Config management. GET /config is the read side (DEC-243) — before it
        // the writable knobs were write-only, so a client could only guess what
        // the daemon was actually configured with.
        .route("/config", get(handlers::get_config_handler))
        .route(
            "/config/profile-search-dirs",
            post(handlers::update_profile_search_dirs_handler),
        )
        .route(
            "/config/startup-delay",
            post(handlers::update_startup_delay_handler),
        )
        // DEC-388: the clean-stop exit floor. Applies live, not at next start.
        .route(
            "/config/exit-floor",
            post(handlers::update_exit_floor_handler),
        )
        // DEC-443: the coolant limit — applies live, like the exit floor.
        .route(
            "/config/coolant-limit",
            post(handlers::update_coolant_limit_handler),
        )
        // Persisted preferred CPU / motherboard sensor (Phase 5, DEC-200). Set
        // via {"sensor_id": "<id>"} or clear via {"sensor_id": null}. Advisory —
        // thermal safety still uses the hottest CpuTemp.
        .route(
            "/config/preferred-cpu-sensor",
            post(handlers::update_preferred_cpu_sensor_handler),
        )
        .route(
            "/config/preferred-mb-sensor",
            post(handlers::update_preferred_mb_sensor_handler),
        )
        // DEC-311 (AIO-MB Phase 1): assign a PWM header's role via
        // {"header_id": "<id>", "role": "pump"} or clear via {"role": null}.
        // NOT advisory, unlike the two above — a `pump` assignment earns the
        // header the 30% hard floor and pump-safe identify, and takes effect
        // immediately rather than at next start.
        .route(
            "/config/header-role",
            post(handlers::update_header_role_handler),
        )
        // AIO-MB Phase 4 (DEC-316): cooling-device topology. Metadata — no
        // engine path reads a device — so unlike the role setter above this
        // grants no floor and needs no follow-on safety action.
        .route(
            "/config/cooling-device",
            post(handlers::set_cooling_device_handler),
        )
        .route(
            "/config/cooling-device/{id}",
            delete(handlers::delete_cooling_device_handler),
        )
        // DEC-243: admin keys made runtime-mutable via the runtime.toml overlay
        // (ADR-002) rather than a privileged helper. All are start-only, so each
        // response says so and GET /config reports restart_pending per key. The
        // two [detection] opt-ins additionally need a root systemd drop-in that
        // no API can install — the responses state that rather than implying the
        // flag alone enables the feature.
        .route(
            "/config/poll-interval",
            post(handlers::update_poll_interval_handler),
        )
        .route(
            "/config/serial-port",
            post(handlers::update_serial_port_handler),
        )
        .route(
            "/config/serial-timeout",
            post(handlers::update_serial_timeout_handler),
        )
        .route(
            "/config/allow-port-probe",
            post(handlers::update_allow_port_probe_handler),
        )
        .route(
            "/config/nvidia-telemetry",
            post(handlers::update_nvidia_telemetry_handler),
        )
        .fallback(handlers::fallback_handler)
        // Names every diagnostic start and its caller in the journal (DEC-503).
        // Observes only: it reads `MatchedPath`, so it sits after the routes.
        .layer(axum::middleware::from_fn(
            super::diagnostic_log::log_diagnostic_requests,
        ))
        // DEC-504: 64 KiB for every route that sets no cap of its own (the
        // profile uploads and the firmware file above do). A layer set closer
        // to the handler wins over this one.
        .layer(DefaultBodyLimit::max(DEFAULT_BODY_LIMIT))
        .with_state(state)
}

/// Serve axum over an already-bound Unix listener.
///
/// Binding, stale-socket removal, parent-dir creation, and the 0o666 chmod
/// all happen in `main::preflight_socket` *before* any subsystem is spawned,
/// so that a bind failure is surfaced immediately as a fatal startup error
/// (see ADR-002 for the rationale — we don't want a half-started daemon
/// running polling loops with no one to talk to).
///
/// `socket_path` is kept around only for logging and for unlinking the
/// socket file on clean shutdown.
pub async fn serve(
    listener: UnixListener,
    socket_path: String,
    state: Arc<AppState>,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), ServeError> {
    log::info!("IPC server listening on {socket_path}");

    serve_router(
        listener,
        build_router(state),
        ServerLimits::default(),
        async {
            let _ = shutdown.await;
            log::info!("IPC server shutting down");
        },
    )
    .await;

    // Clean up socket file on clean shutdown.
    let path = Path::new(&socket_path);
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }

    Ok(())
}

/// Serve `router` on `listener` within `limits` until `shutdown` resolves
/// (DEC-504).
///
/// A connection slot is taken **before** `accept`, so at the cap the loop stops
/// accepting and further clients wait in the kernel's listen queue, holding no
/// descriptor of the daemon's. The slot is released when the connection ends.
/// Each connection is HTTP/1 only (as under `axum::serve` with axum's `http1`
/// feature alone), with a timer so `header_read_timeout` takes effect. The
/// caller's credentials go into every request as `ConnectInfo<UdsConnectInfo>`
/// (DEC-205), which is what handlers extract.
///
/// On `shutdown` it stops accepting, closes idle connections and waits for
/// in-flight requests to finish, as `axum::serve`'s graceful shutdown did.
/// `main::shutdown_sequence` bounds that wait.
pub async fn serve_router<F>(
    listener: UnixListener,
    router: Router,
    limits: ServerLimits,
    shutdown: F,
) where
    F: Future<Output = ()>,
{
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    let graceful = GracefulShutdown::new();
    let mut at_cap_logged = false;
    tokio::pin!(shutdown);

    loop {
        // Log the first time the cap is reached, not on every connection after.
        if slots.available_permits() == 0 {
            if !at_cap_logged {
                log::warn!(
                    "IPC server: {} connections open, the most it serves at once; \
                     new clients wait until one closes",
                    limits.max_connections
                );
                at_cap_logged = true;
            }
        } else {
            at_cap_logged = false;
        }

        let slot = tokio::select! {
            slot = slots.clone().acquire_owned() => {
                slot.expect("connection slots are never closed")
            }
            () = &mut shutdown => break,
        };

        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _addr)) => stream,
                Err(e) => {
                    // EMFILE, ENFILE, ENOMEM and the like: wait rather than spin,
                    // but stay responsive to shutdown.
                    log::warn!(
                        "IPC server: accept failed: {e}; retrying in {} s",
                        ACCEPT_ERROR_BACKOFF.as_secs()
                    );
                    drop(slot);
                    tokio::select! {
                        () = tokio::time::sleep(ACCEPT_ERROR_BACKOFF) => continue,
                        () = &mut shutdown => break,
                    }
                }
            },
            () = &mut shutdown => break,
        };

        let info = UdsConnectInfo::of(&stream);
        let router = router.clone();
        let service = hyper::service::service_fn(move |mut request: hyper::Request<Incoming>| {
            request.extensions_mut().insert(ConnectInfo(info.clone()));
            router.clone().oneshot(request)
        });
        let mut builder = hyper::server::conn::http1::Builder::new();
        builder
            .timer(TokioTimer::new())
            .header_read_timeout(limits.header_read_timeout);
        let connection = graceful.watch(builder.serve_connection(TokioIo::new(stream), service));
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                // A header timeout, a client that hung up mid-request, a
                // malformed request: the client's problem, not the daemon's.
                log::debug!("IPC connection ended: {e}");
            }
            drop(slot);
        });
    }

    drop(listener);
    graceful.shutdown().await;
}

#[cfg(test)]
mod tests {
    /// What a route does while an OpenFan firmware update holds the controller
    /// (DEC-481).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DuringUpdate {
        /// Refused: 409 `validation_error`, `details.reason: "openfan_maintenance"`.
        Refused,
        /// Refused only when it targets an OpenFan channel.
        RefusedForOpenFan,
        /// The update's own route.
        Itself,
        /// Answers as it always does: it neither drives the OpenFan controller
        /// nor claims the diagnostic slot an update holds.
        Unaffected,
    }
    use DuringUpdate::*;

    /// Every route the router serves, classified. A route missing from here
    /// fails [`every_route_says_what_it_does_during_a_firmware_update`] until
    /// someone decides (DEC-367): a new route that drives the OpenFan
    /// controller, or claims the diagnostic slot, must be refused while an
    /// update holds the controller. The refusals are proven live in
    /// `daemon/tests/ipc_integration.rs` (identify, override, rescan) and at
    /// `verify_slot_refusal`'s call sites.
    const ROUTES: &[(&str, &str, DuringUpdate)] = &[
        ("GET", "/status", Unaffected),
        ("GET", "/sensors", Unaffected),
        ("GET", "/fans", Unaffected),
        ("GET", "/poll", Unaffected),
        ("GET", "/sensors/history", Unaffected),
        ("POST", "/fans/openfan/{channel}/calibration", Refused),
        ("GET", "/diagnostics/openfan-calibration", Unaffected),
        ("DELETE", "/diagnostics/openfan-calibration", Unaffected),
        ("POST", "/fans/openfan/{channel}/calibrate", Refused),
        ("GET", "/capabilities", Unaffected),
        ("POST", "/gpu/{gpu_id}/fan/reset", Unaffected),
        ("POST", "/gpu/{gpu_id}/fan/verify", Refused),
        ("GET", "/hwmon/headers", Unaffected),
        ("POST", "/hwmon/{header_id}/verify", Refused),
        ("POST", "/hwmon/{header_id}/characterize", Refused),
        ("GET", "/diagnostics/characterization", Unaffected),
        ("DELETE", "/diagnostics/characterization", Unaffected),
        ("GET", "/diagnostics/preflight", Unaffected),
        ("POST", "/hwmon/{header_id}/discover-control-path", Refused),
        ("GET", "/diagnostics/control-path", Unaffected),
        ("DELETE", "/diagnostics/control-path", Unaffected),
        ("POST", "/hwmon/{header_id}/stall-probe", Refused),
        ("GET", "/diagnostics/stall-probe", Unaffected),
        ("DELETE", "/diagnostics/stall-probe", Unaffected),
        ("POST", "/validation/session", Refused),
        ("GET", "/validation/session", Unaffected),
        ("DELETE", "/validation/session", Unaffected),
        ("POST", "/validation/session/stop", Unaffected),
        ("POST", "/validation/session/event", Unaffected),
        ("POST", "/validation/session/measurement", Unaffected),
        ("GET", "/validation/sessions", Unaffected),
        ("GET", "/validation/sessions/{session_id}", Unaffected),
        ("POST", "/hwmon/rescan", Unaffected),
        ("POST", "/fans/openfan/rescan", Refused),
        ("GET", "/fans/openfan/roles", Unaffected),
        ("GET", "/fans/openfan/device", Itself),
        ("PUT", "/fans/openfan/firmware", Itself),
        ("GET", "/fans/openfan/maintenance", Itself),
        ("POST", "/fans/openfan/maintenance", Itself),
        ("DELETE", "/fans/openfan/maintenance", Itself),
        ("GET", "/diagnostics/hardware", Unaffected),
        ("GET", "/inventory/hwmon", Unaffected),
        ("GET", "/inventory/cooling-devices", Unaffected),
        ("GET", "/inventory/readiness", Unaffected),
        ("GET", "/inventory/hardware-readiness", Unaffected),
        ("GET", "/inventory/superio", Unaffected),
        ("POST", "/inventory/superio/probe", Unaffected),
        ("POST", "/control/{control_id}/override", RefusedForOpenFan),
        // Release and renew stay open: a client can always let go, and a renew
        // of an override the update's start released is refused as stale.
        ("DELETE", "/control/{control_id}/override", Unaffected),
        ("POST", "/control/{control_id}/override/renew", Unaffected),
        ("POST", "/fans/{fan_id}/identify", RefusedForOpenFan),
        ("GET", "/profile/active", Unaffected),
        ("POST", "/profile/activate", Unaffected),
        ("POST", "/profile/deactivate", Unaffected),
        ("GET", "/profiles", Unaffected),
        ("POST", "/profiles", Unaffected),
        ("GET", "/profiles/{id}", Unaffected),
        ("PUT", "/profiles/{id}", Unaffected),
        ("DELETE", "/profiles/{id}", Unaffected),
        ("GET", "/config", Unaffected),
        ("POST", "/config/profile-search-dirs", Unaffected),
        ("POST", "/config/startup-delay", Unaffected),
        ("POST", "/config/exit-floor", Unaffected),
        ("POST", "/config/coolant-limit", Unaffected),
        ("POST", "/config/preferred-cpu-sensor", Unaffected),
        ("POST", "/config/preferred-mb-sensor", Unaffected),
        ("POST", "/config/header-role", Unaffected),
        ("POST", "/config/cooling-device", Unaffected),
        ("DELETE", "/config/cooling-device/{id}", Unaffected),
        // The serial and polling setters persist for the next start.
        ("POST", "/config/poll-interval", Unaffected),
        ("POST", "/config/serial-port", Unaffected),
        ("POST", "/config/serial-timeout", Unaffected),
        ("POST", "/config/allow-port-probe", Unaffected),
        ("POST", "/config/nvidia-telemetry", Unaffected),
    ];

    /// `(method, path)` for every `.route(` in `build_router`'s body — the
    /// function's own text, brace-matched, with `//` comments dropped, so this
    /// test's table and any comment naming a route are not read as routes.
    fn served_routes() -> Vec<(String, String)> {
        let src = include_str!("server.rs");
        let start = src
            .find("pub fn build_router_with_uploads")
            .expect("build_router_with_uploads");
        let open = start + src[start..].find('{').expect("body");
        let mut depth = 0usize;
        let mut end = open;
        for (i, c) in src[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body: String = src[open..end]
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let mut routes = Vec::new();
        let mut at = 0;
        while let Some(found) = body[at..].find(".route(") {
            let args_start = at + found + ".route(".len();
            let mut depth = 1usize;
            let mut args_end = args_start;
            for (i, c) in body[args_start..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            args_end = args_start + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            // Whitespace dropped, so a handler rustfmt moved onto its own line
            // (`.post(\n handlers::…`) still reads as `post(handlers::…`.
            let args: String = body[args_start..args_end]
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            let path = args.split('"').nth(1).expect("a route's path literal");
            for method in ["get", "post", "put", "delete", "patch"] {
                let call = format!("{method}(handlers::");
                if args
                    .match_indices(&call)
                    .any(|(i, _)| i == 0 || !args.as_bytes()[i - 1].is_ascii_alphanumeric())
                {
                    routes.push((method.to_ascii_uppercase(), path.to_string()));
                }
            }
            at = args_end;
        }
        routes
    }

    #[test]
    fn every_route_says_what_it_does_during_a_firmware_update() {
        let served = served_routes();
        assert!(
            served.len() >= 70,
            "precondition: the parse found the router's routes ({})",
            served.len()
        );
        let unclassified: Vec<_> = served
            .iter()
            .filter(|(m, p)| !ROUTES.iter().any(|(rm, rp, _)| rm == m && rp == p))
            .collect();
        assert!(
            unclassified.is_empty(),
            "classify these in ROUTES — does each one drive the OpenFan controller or \
             claim the diagnostic slot? {unclassified:?}"
        );
        let stale: Vec<_> = ROUTES
            .iter()
            .filter(|(m, p, _)| !served.iter().any(|(sm, sp)| sm == m && sp == p))
            .collect();
        assert!(stale.is_empty(), "no longer served: {stale:?}");
    }

    /// DEC-503: every route that claims the diagnostic slot is a logged start
    /// (rescan is the one `Refused` route that starts no diagnostic), the port
    /// probe is logged although it claims no slot, and every listed start is
    /// still served.
    #[test]
    fn every_diagnostic_start_route_is_logged() {
        use super::super::diagnostic_log::DIAGNOSTIC_START_ROUTES;
        let served = served_routes();
        let listed = |m: &str, p: &str| {
            DIAGNOSTIC_START_ROUTES
                .iter()
                .any(|(lm, lp)| *lm == m && *lp == p)
        };
        let claimants: Vec<_> = ROUTES
            .iter()
            .filter(|(_, p, d)| *d == Refused && *p != "/fans/openfan/rescan")
            .collect();
        assert!(claimants.len() >= 8, "precondition: {claimants:?}");
        let unlogged: Vec<_> = claimants.iter().filter(|(m, p, _)| !listed(m, p)).collect();
        assert!(
            unlogged.is_empty(),
            "slot claimants not logged: {unlogged:?}"
        );
        assert!(listed("POST", "/inventory/superio/probe"));
        let stale: Vec<_> = DIAGNOSTIC_START_ROUTES
            .iter()
            .filter(|(m, p)| !served.iter().any(|(sm, sp)| sm == m && sp == p))
            .collect();
        assert!(stale.is_empty(), "listed but not served: {stale:?}");
    }
}

/// The accept loop's limits (DEC-504), over a real Unix socket with a small
/// router of its own.
#[cfg(test)]
mod accept_loop_tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Every wait in these tests is bounded by this.
    const DEADLINE: Duration = Duration::from_secs(5);

    struct TestServer {
        path: PathBuf,
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        task: tokio::task::JoinHandle<()>,
        /// Notified when a `/slow` handler starts.
        slow_started: Arc<tokio::sync::Notify>,
        _dir: tempfile::TempDir,
    }

    fn router(slow_started: Arc<tokio::sync::Notify>) -> Router {
        Router::new()
            .route("/ping", get(|| async { "pong" }))
            .route(
                "/slow",
                get(move || async move {
                    slow_started.notify_one();
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    "done"
                }),
            )
            .route(
                "/whoami",
                get(
                    |ConnectInfo(info): ConnectInfo<UdsConnectInfo>| async move {
                        format!("{:?}", info.uid)
                    },
                ),
            )
    }

    async fn start(max_connections: usize, header_read_timeout: Duration) -> TestServer {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let limits = ServerLimits {
            max_connections,
            header_read_timeout,
        };
        let slow_started = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn(serve_router(
            listener,
            router(slow_started.clone()),
            limits,
            async {
                let _ = stopped.await;
            },
        ));
        TestServer {
            path,
            stop: Some(stop),
            task,
            slow_started,
            _dir: dir,
        }
    }

    /// Send a keep-alive `GET` on `stream` and return the response body.
    async fn get_on(stream: &mut UnixStream, path: &str) -> String {
        let request = format!("GET {path} HTTP/1.1\r\nhost: t\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "the server closed the connection mid-response");
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_string)
                    })
                    .and_then(|v| v.trim().parse().ok())
                    .expect("a content-length");
                if body.len() >= length {
                    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
                    return body[..length].to_string();
                }
            }
        }
    }

    /// Wait for the server to close `stream`; returns how long that took.
    async fn closed_within(stream: &mut UnixStream, limit: Duration) -> Duration {
        let started = Instant::now();
        let mut chunk = [0u8; 64];
        let n = tokio::time::timeout(limit, stream.read(&mut chunk))
            .await
            .expect("the server did not close the connection in time")
            .unwrap_or(0);
        assert_eq!(n, 0, "expected the server to close the connection");
        started.elapsed()
    }

    /// At the cap the server stops accepting. A client beyond it is connected
    /// (the kernel queues it) but not served; it is served as soon as a slot
    /// frees.
    #[tokio::test]
    async fn at_the_cap_a_new_client_waits_and_is_served_when_a_slot_frees() {
        let server = start(2, Duration::from_secs(30)).await;
        let mut a = UnixStream::connect(&server.path).await.unwrap();
        let mut b = UnixStream::connect(&server.path).await.unwrap();
        // Both are served, so both hold a slot (keep-alive keeps them open).
        assert_eq!(get_on(&mut a, "/ping").await, "pong");
        assert_eq!(get_on(&mut b, "/ping").await, "pong");

        let path = server.path.clone();
        let mut waiting = tokio::spawn(async move {
            let mut c = UnixStream::connect(&path).await.unwrap();
            get_on(&mut c, "/ping").await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(500), &mut waiting)
                .await
                .is_err(),
            "a third client was served while two of two slots were held"
        );

        drop(a);
        let body = tokio::time::timeout(DEADLINE, waiting)
            .await
            .expect("the waiting client is served once a slot frees")
            .unwrap();
        assert_eq!(body, "pong");
        drop(b);
    }

    /// A client that connects and never sends a request is closed after the
    /// header timeout, and its slot goes to the next client.
    #[tokio::test]
    async fn a_client_that_sends_nothing_is_closed_and_frees_its_slot() {
        let timeout = Duration::from_millis(300);
        let server = start(1, timeout).await;
        let mut silent = UnixStream::connect(&server.path).await.unwrap();
        // Queued behind the silent client, which holds the only slot.
        let path = server.path.clone();
        let next = tokio::spawn(async move {
            let mut c = UnixStream::connect(&path).await.unwrap();
            get_on(&mut c, "/ping").await
        });

        let took = closed_within(&mut silent, DEADLINE).await;
        assert!(
            took >= timeout / 2,
            "closed after {took:?}, before the header timeout could have run"
        );
        let body = tokio::time::timeout(DEADLINE, next)
            .await
            .expect("the next client is served once the silent one is closed")
            .unwrap();
        assert_eq!(body, "pong");
    }

    /// A keep-alive connection left idle after a response is closed too.
    #[tokio::test]
    async fn an_idle_keep_alive_connection_is_closed() {
        let server = start(4, Duration::from_millis(300)).await;
        let mut c = UnixStream::connect(&server.path).await.unwrap();
        assert_eq!(get_on(&mut c, "/ping").await, "pong");
        closed_within(&mut c, DEADLINE).await;
    }

    /// The timeout covers reading a request head, not a handler's work.
    #[tokio::test]
    async fn a_handler_slower_than_the_timeout_still_answers() {
        let server = start(4, Duration::from_millis(200)).await;
        let mut c = UnixStream::connect(&server.path).await.unwrap();
        let body = tokio::time::timeout(DEADLINE, get_on(&mut c, "/slow"))
            .await
            .expect("bounded");
        assert_eq!(body, "done");
    }

    /// Handlers still get the caller's uid (DEC-205), now inserted per request.
    #[tokio::test]
    async fn handlers_get_the_callers_uid() {
        let server = start(4, Duration::from_secs(30)).await;
        let mut c = UnixStream::connect(&server.path).await.unwrap();
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        assert_eq!(get_on(&mut c, "/whoami").await, format!("{:?}", Some(uid)));
    }

    /// On shutdown an in-flight request still finishes, an idle connection is
    /// closed at once (not after the header timeout), the loop returns, and no
    /// new client is accepted.
    #[tokio::test]
    async fn shutdown_finishes_in_flight_requests_and_closes_idle_ones() {
        let mut server = start(4, Duration::from_secs(30)).await;
        let mut idle = UnixStream::connect(&server.path).await.unwrap();
        assert_eq!(get_on(&mut idle, "/ping").await, "pong");

        let path = server.path.clone();
        let in_flight = tokio::spawn(async move {
            let mut c = UnixStream::connect(&path).await.unwrap();
            get_on(&mut c, "/slow").await
        });
        // Stop only once the request is being handled.
        tokio::time::timeout(DEADLINE, server.slow_started.notified())
            .await
            .expect("the /slow handler started");
        let _ = server.stop.take().unwrap().send(());

        closed_within(&mut idle, DEADLINE).await;
        let body = tokio::time::timeout(DEADLINE, in_flight)
            .await
            .expect("bounded")
            .unwrap();
        assert_eq!(body, "done", "the in-flight request finished");
        tokio::time::timeout(DEADLINE, &mut server.task)
            .await
            .expect("the accept loop returns once its connections have closed")
            .unwrap();
        assert!(
            UnixStream::connect(&server.path).await.is_err(),
            "nothing accepts after shutdown"
        );
    }
}
