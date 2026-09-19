//! `billing-domain` — router: auth (register/login/logout/me) and dispatch
//! into `handlers.rs` for the domain routes. The whole router is
//! `guestauth`/`guestio` macro expansions; nothing domain-specific lives
//! here — see `handlers.rs` for the actual billing logic.

#[allow(warnings)]
mod bindings;
mod handlers;

pub use guestauth::Route;

pub const TENANT: &str = "billing";
/// The only roles this app grants. Anything else in a register request falls
/// back to `biller` — a role is a privilege, not a free-text field.
const ROLES: &[&str] = &["admin", "biller"];

guestio::guest_write_all!();
guestio::guest_bearer!();

const MAX_BODY_BYTES: usize = 1024 * 1024;
guestio::guest_read_body_text!(MAX_BODY_BYTES);

guestauth::guest_auth_reply!();
guestauth::guest_introspect!();
guestauth::guest_role_check!(is_admin, "admin");
guestauth::guest_audit!(TENANT);
guestauth::guest_accounts_endpoints!(TENANT, ROLES, "biller");
guestauth::guest_emit!();
guestauth::guest_saas_router!();
