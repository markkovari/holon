//! `inspection-domain` — router: auth (register/login/logout/me) and dispatch
//! into `handlers.rs` for the site-inspection routes. The router is the same
//! `guestauth`/`guestio` macro sequence every app here uses; the row-level
//! ownership rule (`guestauth::guest_owner_or_admin_policy!` over `inspector`,
//! the same macro `crm-domain` uses over `owner`) and the real generated PDF
//! both live in `handlers.rs`.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../audit-log/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/ratelimit-guard",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../policy-guard/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../id-generate/wit",
            "../pdf/wit",
            "wit",
        ],
        world: "inspect:report/inspection-domain",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
    }
}
mod handlers;

pub use guestauth::Route;

pub const TENANT: &str = "inspection";
/// The roles this app grants. Anything else in a register request degrades to
/// `inspector` — a role is a privilege, not a free-text field.
const ROLES: &[&str] = &["admin", "inspector"];

guestio::guest_p3_respond!();
guestio::guest_p3_bearer!();

const MAX_BODY_BYTES: usize = 1024 * 1024;
guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

guestauth::guest_auth_reply!();
guestauth::guest_introspect!();
guestauth::guest_role_check!(is_admin, "admin");
guestauth::guest_p3_audit!(TENANT);
guestauth::guest_accounts_endpoints!(TENANT, ROLES, "inspector");
guestauth::guest_p3_emit!();
guestauth::guest_p3_saas_router!();
