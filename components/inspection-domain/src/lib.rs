//! `inspection-domain` — router: auth (register/login/logout/me) and dispatch
//! into `handlers.rs` for the site-inspection routes. The router is the same
//! `guestauth`/`guestio` macro sequence every app here uses; the row-level
//! ownership rule (`guestauth::guest_owner_or_admin_policy!` over `inspector`,
//! the same macro `crm-domain` uses over `owner`) and the real generated PDF
//! both live in `handlers.rs`.

#[allow(warnings)]
mod bindings;
mod handlers;

pub use guestauth::Route;

pub const TENANT: &str = "inspection";
/// The roles this app grants. Anything else in a register request degrades to
/// `inspector` — a role is a privilege, not a free-text field.
const ROLES: &[&str] = &["admin", "inspector"];

guestio::guest_write_all!();
guestio::guest_bearer!();

const MAX_BODY_BYTES: usize = 1024 * 1024;
guestio::guest_read_body_text!(MAX_BODY_BYTES);

guestauth::guest_auth_reply!();
guestauth::guest_introspect!();
guestauth::guest_role_check!(is_admin, "admin");
guestauth::guest_audit!(TENANT);
guestauth::guest_accounts_endpoints!(TENANT, ROLES, "inspector");
guestauth::guest_emit!();
guestauth::guest_saas_router!();
