//! The daemon's open-file limit (DEC-504).
//!
//! systemd starts a service with a soft `RLIMIT_NOFILE` of 1024 and a much
//! higher hard limit, and expects a program that needs more descriptors to raise
//! its own soft limit, up to the hard one. The daemon opens a fresh descriptor
//! for every hwmon write and sensor read (`RealSysfsWriter`), the thermal
//! emergency's included, so it raises the limit at start-up. Together with the
//! API's connection cap (`api::server::MAX_CONNECTIONS`), that keeps those
//! opens from failing with `EMFILE` however many clients connect. The raise is
//! done in code, not with the unit's `LimitNOFILE=`, so it holds for a daemon
//! run by hand too.
//!
//! A soft limit above 1024 can break a child process that uses `select(2)`.
//! The daemon starts none in production (the unit's `NotifyAccess=main` comment
//! says the same).

/// The soft limit the daemon asks for, unless the hard limit is lower.
pub const NOFILE_SOFT_TARGET: u64 = 65_536;

/// The soft limit to set, given the current `soft` and `hard` limits: `target`,
/// capped at `hard`, and never below `soft` (this never lowers the limit).
#[must_use]
pub fn nofile_soft_goal(soft: u64, hard: u64, target: u64) -> u64 {
    soft.max(target.min(hard))
}

/// The process's `(soft, hard)` `RLIMIT_NOFILE`.
///
/// # Errors
/// The `getrlimit(2)` error.
pub fn nofile_limits() -> std::io::Result<(u64, u64)> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid, writable `rlimit` for the call's duration.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((limit.rlim_cur, limit.rlim_max))
}

/// Set the process's soft `RLIMIT_NOFILE` to `soft`, keeping the hard limit.
///
/// # Errors
/// The `getrlimit(2)` or `setrlimit(2)` error (`EINVAL` when `soft` is above
/// the hard limit).
pub fn set_nofile_soft_limit(soft: u64) -> std::io::Result<()> {
    let (_, hard) = nofile_limits()?;
    let limit = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    // SAFETY: `limit` is a valid `rlimit` for the call's duration.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Raise the soft `RLIMIT_NOFILE` toward [`NOFILE_SOFT_TARGET`] (see the module
/// documentation). Returns `(before, after)`; they are equal when the limit was
/// already high enough.
///
/// # Errors
/// The `getrlimit(2)` or `setrlimit(2)` error. The caller logs it and carries
/// on: the connection cap still bounds the API's share of the old limit.
pub fn raise_nofile_soft_limit() -> std::io::Result<(u64, u64)> {
    let (soft, hard) = nofile_limits()?;
    let goal = nofile_soft_goal(soft, hard, NOFILE_SOFT_TARGET);
    if goal > soft {
        set_nofile_soft_limit(goal)?;
    }
    Ok((soft, nofile_limits()?.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_goal_is_the_target_capped_at_the_hard_limit_and_never_lower() {
        // systemd's default: soft 1024, hard 524288.
        assert_eq!(nofile_soft_goal(1024, 524_288, 65_536), 65_536);
        // A hard limit below the target caps it.
        assert_eq!(nofile_soft_goal(1024, 4096, 65_536), 4096);
        // An unlimited hard limit (RLIM_INFINITY) does not lift the target.
        assert_eq!(nofile_soft_goal(1024, libc::RLIM_INFINITY, 65_536), 65_536);
        // Never lowered: an operator's higher soft limit stays.
        assert_eq!(nofile_soft_goal(200_000, 524_288, 65_536), 200_000);
    }

    /// The real call: afterwards the soft limit is the goal computed from the
    /// limits read before it. Raising this test process's soft limit cannot
    /// harm the tests running beside it.
    #[test]
    fn raising_sets_the_soft_limit_to_the_goal() {
        let (soft, hard) = nofile_limits().expect("getrlimit");
        let (before, after) = raise_nofile_soft_limit().expect("raise");
        assert_eq!(before, soft);
        assert_eq!(after, nofile_soft_goal(soft, hard, NOFILE_SOFT_TARGET));
        assert_eq!(nofile_limits().expect("getrlimit").0, after);
    }
}
