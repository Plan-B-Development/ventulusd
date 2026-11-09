//! [SAFETY] DEC-504 (`IPC-a`): local clients cannot starve the thermal force of
//! file descriptors.
//!
//! The socket is `0666`, and every hwmon write opens a fresh descriptor
//! (`RealSysfsWriter::write_file`). Before DEC-504 the server accepted without
//! limit, so a user holding enough idle connections used up the soft
//! `RLIMIT_NOFILE` and the emergency's `pwm_enable`/`pwm` writes failed with
//! `EMFILE`. This test gives the process exactly the descriptors the daemon's
//! own budget allows: what is open now, one per client, one per connection the
//! server may accept, and a few spare for the write. It then opens far more
//! connections than the cap and runs the thermal force's own call sequence
//! through the real writer against a fake sysfs tree.
//!
//! Its own test binary, with one test, because it changes the process's file
//! limit.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::{UnixListener, UnixStream};

use ventulusd::api::server::{self, ServerLimits};
use ventulusd::health::cache::StateCache;
use ventulusd::hwmon::lease::{HwmonWriter, LeaseManager};
use ventulusd::hwmon::pwm_control::{HwmonPwmController, RealSysfsWriter};
use ventulusd::hwmon::pwm_discovery::PwmHeaderDescriptor;
use ventulusd::rlimit;

/// Clients beyond the cap.
const EXTRA_CLIENTS: usize = 40;
/// Descriptors left for the forced write (it opens one at a time).
const SPARE: usize = 4;

/// The process's open descriptors: `(count, highest)`, or `None` when listing
/// them needs a descriptor and none is left (the starved case).
fn open_fds() -> Option<(usize, i32)> {
    let fds: Vec<i32> = std::fs::read_dir("/proc/self/fd")
        .ok()?
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    // The directory handle itself was among them and is closed now.
    let count = fds.len() - 1;
    Some((count, fds.into_iter().max().unwrap_or(0)))
}

/// Wait until the open-descriptor count stops rising: the server has accepted
/// everything it is going to. Returns at once when no descriptor is left.
async fn settle() {
    let deadline = Instant::now() + Duration::from_secs(5);
    let Some((mut last, _)) = open_fds() else {
        return;
    };
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let Some((now, _)) = open_fds() else {
            return;
        };
        if now == last || Instant::now() > deadline {
            return;
        }
        last = now;
    }
}

fn fake_header(dir: &Path) -> PwmHeaderDescriptor {
    let pwm = dir.join("pwm1");
    let enable = dir.join("pwm1_enable");
    std::fs::write(&pwm, "77\n").unwrap();
    std::fs::write(&enable, "2\n").unwrap();
    for f in [&pwm, &enable] {
        std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    PwmHeaderDescriptor {
        id: "hwmon:test:pwm1".to_string(),
        label: "CHA_FAN1".to_string(),
        chip_name: "it8696".to_string(),
        device_id: "it87.2624".to_string(),
        pwm_index: 1,
        supports_enable: true,
        pwm_path: pwm.to_str().unwrap().to_string(),
        enable_path: Some(enable.to_str().unwrap().to_string()),
        rpm_available: false,
        rpm_path: None,
        min_pwm_percent: 0,
        max_pwm_percent: 100,
        is_writable: true,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forced_hwmon_write_lands_while_clients_hold_more_connections_than_the_cap() {
    let sysfs = tempfile::tempdir().unwrap();
    let header = fake_header(sysfs.path());
    let mut controller = HwmonPwmController::new(
        vec![header.clone()],
        LeaseManager::new(),
        Box::new(RealSysfsWriter),
        Arc::new(StateCache::new()),
    );

    // The production server loop at the production cap.
    let limits = ServerLimits::default();
    let cap = limits.max_connections;
    let sock_dir = tempfile::tempdir().unwrap();
    let sock = sock_dir.path().join("t.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(server::serve_router(
        listener,
        axum::Router::new(),
        limits,
        async {
            let _ = stopped.await;
        },
    ));
    // The server is up, so everything the runtime opens is open.
    UnixStream::connect(&sock)
        .await
        .expect("server is accepting");
    settle().await;

    let clients = cap + EXTRA_CLIENTS;
    let (open, highest) = open_fds().expect("descriptors to spare before the clients connect");
    let budget = open + clients + cap + SPARE;
    assert!(
        (highest as usize) < budget,
        "precondition: every open descriptor ({highest}) is below the limit ({budget})"
    );
    let (original_soft, hard) = rlimit::nofile_limits().unwrap();
    assert!(
        budget as u64 <= hard,
        "precondition: the hard limit allows {budget}"
    );
    rlimit::set_nofile_soft_limit(budget as u64).unwrap();

    // Far more connections than the cap, every one idle. Past the cap the
    // kernel queues them; without the cap the server accepts them all and the
    // budget runs out (a connect that fails then is part of that, not a test
    // error).
    let mut held = Vec::new();
    let mut refused = 0usize;
    for _ in 0..clients {
        match UnixStream::connect(&sock).await {
            Ok(s) => held.push(s),
            Err(_) => refused += 1,
        }
    }
    settle().await;

    // The thermal force's own sequence (`HwmonBackend::force_all_with_floor`):
    // force-take the lease as ThermalSafety, then write 100 %.
    let lease = controller
        .lease_manager_mut()
        .force_take_lease(HwmonWriter::ThermalSafety)
        .lease_id;
    let forced = controller.set_pwm(&header.id, 100, &lease);

    // Read what landed, and restore the limit, before any assertion can fail.
    let connected = held.len();
    drop(held);
    rlimit::set_nofile_soft_limit(original_soft).unwrap();
    let enable = std::fs::read_to_string(header.enable_path.as_ref().unwrap()).unwrap();
    let duty = std::fs::read_to_string(&header.pwm_path).unwrap();
    let _ = stop.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;

    assert!(
        forced.is_ok(),
        "the forced write failed with {} idle clients connected ({refused} could not \
         connect): {forced:?}",
        connected
    );
    assert_eq!(
        refused, 0,
        "every client connects: past the cap the kernel queues them"
    );
    assert_eq!(enable.trim(), "1", "manual mode was set");
    assert_eq!(duty.trim(), "255", "100 % landed");

    // And the start-up raise lifts a systemd-default soft limit (1024) to the
    // daemon's target, capped by the hard limit.
    rlimit::set_nofile_soft_limit(1024).unwrap();
    let (before, after) = rlimit::raise_nofile_soft_limit().unwrap();
    assert_eq!(before, 1024);
    assert_eq!(
        after,
        rlimit::nofile_soft_goal(1024, hard, rlimit::NOFILE_SOFT_TARGET)
    );
    assert!(after > 1024 || hard <= 1024, "the soft limit was raised");
    rlimit::set_nofile_soft_limit(original_soft).unwrap();
}
