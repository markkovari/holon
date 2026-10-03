//! `timesheet-domain` — router: auth (register/login/logout/me) and dispatch
//! into `handlers.rs` for the domain routes. The whole router is
//! `guestauth`/`guestio` macro expansions; nothing domain-specific lives
//! here — see `handlers.rs` for the actual timesheet logic (including its
//! own policy check, deliberately NOT a `guestauth` macro — see there).

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/ratelimit-guard",
            "../audit-log/wit",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../policy-guard/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "wit",
        ],
        world: "timesheet:tracking/timesheet-domain",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod handlers;

pub use guestauth::Route;

pub const TENANT: &str = "timesheet";
/// The only roles this app grants. Anything else in a register request falls
/// back to `member` — a role is a privilege, not a free-text field.
const ROLES: &[&str] = &["manager", "member"];

guestio::guest_p3_bearer!();
guestio::guest_p3_respond!();

const MAX_BODY_BYTES: usize = 1024 * 1024;
guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

guestauth::guest_auth_reply!();
guestauth::guest_introspect!();
guestauth::guest_role_check!(is_manager, "manager");
guestauth::guest_p3_audit!(TENANT);
guestauth::guest_accounts_endpoints!(TENANT, ROLES, "member");
guestauth::guest_p3_emit!();
guestauth::guest_p3_saas_router!();
