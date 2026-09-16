//! Access control — the IP whitelist and the `/admin/*` code.
//!
//! ## Why read and control are separate lists
//!
//! This tool shows you a screen, but it also **presses buttons.** Those two abilities carry
//! entirely different risk, so the lists are split. Observation clients (monitoring,
//! dashboards) go on the read list only and the control list is kept minimal. Emptying
//! `allowed_ips_write` gives you observation-only mode.
//!
//! Classification is **by path, not by method**. `/capture` is a POST only because it takes a
//! body, and splitting by method would file that under control.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use log::warn;

use crate::state::SharedState;
use crate::web::ApiError;

/// Paths that drive the window. Everything else is read.
///
/// Note that `/admin/profile` is here — it is not **pressing something now**, it is rewriting
/// *what can be pressed from now on*, which is a stronger authority than a click. So it has
/// to clear the control list and `allow_profile_editing` together (and `admin_code` as well,
/// if one is configured — that part is optional).
///
/// The whitelist compares **numeric addresses only** (`ip_allowed`). Being the same PC is not
/// an automatic pass — `127.0.0.1` has to be on the list like anything else.
fn is_control_request(path: &str) -> bool {
    matches!(path, "/click" | "/click.png" | "/key" | "/menu")
        || path.starts_with("/window/")
        // Matched by prefix: GET, POST and DELETE on /admin/profile plus
        // /admin/profile/rename all have to sit at the same level. Any one of them left out
        // is a way around the rest.
        || path.starts_with("/admin/profile")
}

/// Whitelist exemption — `/ping` alone, which answers only whether this is alive.
///
/// `/health` is not exempt. Unlike a hub's `/health`, this one carries window titles and
/// privilege state, which is usable for reconnaissance. `/ping` covers the "is it alive" case.
fn is_exempt(method: &Method, path: &str) -> bool {
    method == Method::GET && path == "/ping"
}

fn requires_admin_code(path: &str) -> bool {
    path.starts_with("/admin")
}

/// Constant-time comparison. It leaks the length, but for a short code that matters little.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn ip_allowed(peer: &str, list: &[String]) -> bool {
    list.iter().any(|ip| ip == "*" || ip == peer)
}

pub async fn ip_whitelist_middleware(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<SharedState>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    if is_exempt(req.method(), &path) {
        return next.run(req).await;
    }
    let peer = addr.ip().to_string();
    let control = is_control_request(&path);
    let list = if control {
        &state.config.allowed_ips_write
    } else {
        &state.config.allowed_ips_read
    };
    if !ip_allowed(&peer, list) {
        let kind = if control { "control" } else { "read" };
        warn!("access denied: {peer} ({kind}) {path}");
        return ApiError::new(StatusCode::FORBIDDEN, format!("access denied: {peer}"))
            .with_detail(serde_json::json!({
                "kind": kind,
                "hint": format!("add {peer} to allowed_ips_{} in config.json and restart",
                                if control { "write" } else { "read" }),
            }))
            .into_response();
    }
    next.run(req).await
}

pub async fn admin_code_middleware(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    if !requires_admin_code(req.uri().path()) {
        return next.run(req).await;
    }
    let configured = state.config.admin_code.as_str();
    if configured.is_empty() {
        return next.run(req).await; // not opted in — the whitelist is the only protection
    }
    let provided = req
        .headers()
        .get("X-Admin-Code")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !constant_time_eq(provided.as_bytes(), configured.as_bytes()) {
        return ApiError::new(StatusCode::UNAUTHORIZED, "admin code missing or invalid")
            .with_detail(serde_json::json!({
                "header": "X-Admin-Code",
                "hint": "in the editor this is the 'admin code' box in the header bar. The value comes from admin_code in config.json, which is read at startup only — restart after changing it. Leave admin_code empty to drop this check.",
            }))
            .into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_control_paths_use_the_write_list() {
        assert!(is_control_request("/click"));
        assert!(is_control_request("/click.png"));
        assert!(is_control_request("/key"));
        // Invoking a menu item acts on the application; reading the menu does not.
        assert!(is_control_request("/menu"));
        assert!(!is_control_request("/menus"));
        assert!(is_control_request("/window/focus"));
        assert!(is_control_request("/window/fit"));

        // Saving a profile rewrites "what can be pressed", which outranks a click
        assert!(is_control_request("/admin/profile"));
        assert!(is_control_request("/admin/profile/rename"));

        // read — a POST is still not control
        assert!(!is_control_request("/capture"));
        assert!(!is_control_request("/buttons"));
        assert!(!is_control_request("/regions"));
        assert!(!is_control_request("/profiles"));
        assert!(!is_control_request("/preview.png"));
        assert!(!is_control_request("/controls"));
        // The contact sheet captures and crops. It presses nothing and raises nothing, so a
        // reader may ask for it — the same standing as /capture.
        assert!(!is_control_request("/sheet"));
        assert!(!is_control_request("/sheet.png"));
        assert!(!is_control_request("/editor"));
        assert!(!is_control_request("/window"));
        assert!(!is_control_request("/windows"));
        assert!(!is_control_request("/health"));
        assert!(!is_control_request("/admin/reload"));
    }

    #[test]
    fn health_is_not_whitelist_exempt() {
        // carries window titles and privilege state, so it is not open to anonymous callers
        assert!(!is_exempt(&Method::GET, "/health"));
        assert!(is_exempt(&Method::GET, "/ping"));
        assert!(!is_exempt(&Method::POST, "/ping"));
        assert!(!is_exempt(&Method::GET, "/ping/"));
    }

    #[test]
    fn wildcard_and_exact_ips() {
        assert!(ip_allowed("192.168.86.10", &["*".to_string()]));
        assert!(ip_allowed("192.168.86.10", &["192.168.86.10".to_string()]));
        assert!(!ip_allowed("192.168.86.11", &["192.168.86.10".to_string()]));
        assert!(!ip_allowed("192.168.86.10", &[]));
    }
}
