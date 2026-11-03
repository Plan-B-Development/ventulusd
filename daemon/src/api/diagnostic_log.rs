//! Journal lines for the requests that start a hardware-writing diagnostic
//! (DEC-503 decision 7).
//!
//! The profile engine's writes and a deliberate test's writes look the same in
//! the journal unless the test's start is named. Each start route is logged
//! once when it arrives, with the caller's uid and pid (`SO_PEERCRED`), and
//! once with the daemon's answer. The layer only observes: it never reads or
//! changes a request or response body, and it never refuses anything — every
//! guard stays in the handler.

use axum::extract::{ConnectInfo, MatchedPath, Request};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use super::server::UdsConnectInfo;

/// Every route whose request can start a diagnostic that writes hardware:
/// `(method, route template)`. A test ties this list to the router and to the
/// routes that claim the diagnostic slot, so a new start route cannot be left
/// out silently.
pub const DIAGNOSTIC_START_ROUTES: &[(&str, &str)] = &[
    ("POST", "/hwmon/{header_id}/verify"),
    ("POST", "/hwmon/{header_id}/characterize"),
    ("POST", "/hwmon/{header_id}/discover-control-path"),
    ("POST", "/hwmon/{header_id}/stall-probe"),
    ("POST", "/fans/openfan/{channel}/calibration"),
    ("POST", "/fans/openfan/{channel}/calibrate"),
    ("POST", "/gpu/{gpu_id}/fan/verify"),
    ("POST", "/validation/session"),
    ("POST", "/inventory/superio/probe"),
];

/// The longest request path a line quotes. A path comes from the client; this
/// bounds what one request can put in the journal.
const MAX_LOGGED_PATH_CHARS: usize = 200;

/// Whether `(method, route template)` starts a hardware-writing diagnostic.
pub fn is_diagnostic_start(method: &Method, route: &str) -> bool {
    DIAGNOSTIC_START_ROUTES
        .iter()
        .any(|(m, r)| *m == method.as_str() && *r == route)
}

fn bounded_path(path: &str) -> String {
    if path.chars().count() <= MAX_LOGGED_PATH_CHARS {
        path.to_string()
    } else {
        let head: String = path.chars().take(MAX_LOGGED_PATH_CHARS).collect();
        format!("{head}…")
    }
}

/// The caller as the kernel reported it; an unread credential says so rather
/// than being left out.
fn caller(peer: Option<&UdsConnectInfo>) -> String {
    let uid = peer
        .and_then(|p| p.uid)
        .map_or_else(|| "unknown".to_string(), |u| u.to_string());
    let pid = peer
        .and_then(|p| p.pid)
        .map_or_else(|| "unknown".to_string(), |p| p.to_string());
    format!("uid {uid}, pid {pid}")
}

/// The line logged when a start request arrives.
pub fn request_line(method: &Method, path: &str, peer: Option<&UdsConnectInfo>) -> String {
    format!(
        "hardware diagnostic requested: {method} {} by {}",
        bounded_path(path),
        caller(peer)
    )
}

/// The line logged with the daemon's answer to a start request.
pub fn answer_line(method: &Method, path: &str, status: StatusCode) -> String {
    let outcome = if status.is_success() {
        "accepted"
    } else {
        "refused"
    };
    format!(
        "hardware diagnostic {outcome}: {method} {} answered {}",
        bounded_path(path),
        status.as_u16()
    )
}

/// Router layer: logs a diagnostic start request and its answer; passes every
/// other request through untouched.
pub async fn log_diagnostic_requests(request: Request, next: Next) -> Response {
    let is_start = request
        .extensions()
        .get::<MatchedPath>()
        .is_some_and(|route| is_diagnostic_start(request.method(), route.as_str()));
    if !is_start {
        return next.run(request).await;
    }
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let peer = request
        .extensions()
        .get::<ConnectInfo<UdsConnectInfo>>()
        .map(|ConnectInfo(peer)| peer.clone());
    log::info!("{}", request_line(&method, &path, peer.as_ref()));
    let response = next.run(request).await;
    log::info!("{}", answer_line(&method, &path, response.status()));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(uid: Option<u32>, pid: Option<i32>) -> UdsConnectInfo {
        UdsConnectInfo { uid, pid }
    }

    #[test]
    fn a_request_line_names_the_route_and_the_caller() {
        let line = request_line(
            &Method::POST,
            "/hwmon/hwmon:it87:it8696:pwm2/stall-probe",
            Some(&peer(Some(1000), Some(4242))),
        );
        assert!(
            line.contains("POST /hwmon/hwmon:it87:it8696:pwm2/stall-probe"),
            "{line}"
        );
        assert!(line.contains("uid 1000, pid 4242"), "{line}");
    }

    #[test]
    fn an_unread_credential_is_said_not_dropped() {
        let none = request_line(&Method::POST, "/validation/session", None);
        assert!(none.contains("uid unknown, pid unknown"), "{none}");
        let half = request_line(
            &Method::POST,
            "/validation/session",
            Some(&peer(Some(0), None)),
        );
        assert!(half.contains("uid 0, pid unknown"), "{half}");
    }

    #[test]
    fn the_answer_says_accepted_or_refused_with_the_status() {
        let ok = answer_line(&Method::POST, "/gpu/card1/fan/verify", StatusCode::ACCEPTED);
        assert!(
            ok.contains("accepted") && ok.ends_with("answered 202"),
            "{ok}"
        );
        let no = answer_line(&Method::POST, "/gpu/card1/fan/verify", StatusCode::CONFLICT);
        assert!(
            no.contains("refused") && no.ends_with("answered 409"),
            "{no}"
        );
    }

    #[test]
    fn a_long_client_path_is_bounded() {
        let long = format!("/hwmon/{}/verify", "x".repeat(10 * MAX_LOGGED_PATH_CHARS));
        assert!(long.chars().count() > MAX_LOGGED_PATH_CHARS, "precondition");
        let line = request_line(&Method::POST, &long, None);
        let quoted = line
            .strip_prefix("hardware diagnostic requested: POST ")
            .and_then(|rest| rest.split(" by ").next())
            .expect("the line's shape");
        assert_eq!(
            quoted.chars().count(),
            MAX_LOGGED_PATH_CHARS + 1,
            "{quoted}"
        );
        assert!(quoted.ends_with('…'));
    }

    #[test]
    fn only_listed_method_and_route_pairs_are_starts() {
        assert!(is_diagnostic_start(
            &Method::POST,
            "/hwmon/{header_id}/verify"
        ));
        // The same family's status and cancel routes are not starts.
        assert!(!is_diagnostic_start(
            &Method::DELETE,
            "/diagnostics/characterization"
        ));
        assert!(!is_diagnostic_start(&Method::GET, "/validation/session"));
        assert!(!is_diagnostic_start(&Method::POST, "/profile/activate"));
    }
}
