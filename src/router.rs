//! Route registration and middleware order.

use axum::routing::{get, post};
use axum::{Router, middleware};

use crate::acl::{admin_code_middleware, ip_whitelist_middleware};
use crate::api;
use crate::state::SharedState;

/// With `.layer()`, **the last one added is the outermost**, so this reads inside-out: a
/// request passes `ip_whitelist → admin_code → handler`.
/// The IP check comes first — not on the list means 403 with nothing else evaluated.
pub fn build(state: SharedState) -> Router {
    Router::new()
        // ── read ──
        // Where an agent that knows only the base URL lands first — the root and /help serve
        // the same page.
        .route("/", get(api::help))
        .route("/help", get(api::help))
        .route("/ping", get(api::ping))
        .route("/health", get(api::health))
        .route("/windows", get(api::windows))
        .route("/window", get(api::window_info))
        // The whole definition — with this, the two below are optional
        .route("/profiles", get(api::profiles))
        .route("/buttons", get(api::list_buttons))
        .route("/regions", get(api::list_regions))
        .route("/capture", post(api::capture_json))
        .route("/capture.png", get(api::capture_png))
        .route("/captures/{name}", get(api::capture_file))
        // Draw a candidate definition without saving it — presses nothing
        .route("/preview.png", post(api::preview_png))
        .route("/controls", get(api::controls))
        // The button editor (HTML). A static asset, so it is exempt from admin_code — the
        // page itself holds no secret, and the actual saving is what /admin/profile guards
        // three ways.
        .route("/editor", get(api::editor))
        // The browser asks for this by itself, on the same page load. A static drawing that
        // carries nothing about the machine, so it sits with the other read-only assets.
        .route("/favicon.ico", get(api::favicon))
        // ── control ──
        .route("/click", post(api::click_json))
        .route("/click.png", post(api::click_png))
        .route("/key", post(api::key))
        .route("/window/focus", post(api::window_focus))
        .route("/window/fit", post(api::window_fit))
        // ── admin ──
        .route("/admin/reload", post(api::admin_reload))
        // Read and write the saved shape — GET and POST on one path handle **the same
        // document**. Without that symmetry a client doing read-edit-write has to hand-write
        // a format converter, and the day the server gains a field, that converter drops it
        // silently.
        .route(
            "/admin/profile",
            get(api::admin_get_profile)
                .post(api::admin_save_profile)
                // PATCH edits one corner of the same document. It is on this path, not a
                // sub-path, so the ACL prefix that guards POST and DELETE guards it too — a
                // partial write is still a write.
                .patch(api::admin_patch_profile)
                .delete(api::admin_delete_profile),
        )
        .route("/admin/profile/rename", post(api::admin_rename_profile))
        .layer(middleware::from_fn_with_state(state.clone(), admin_code_middleware))
        .layer(middleware::from_fn_with_state(state.clone(), ip_whitelist_middleware))
        .with_state(state)
}
