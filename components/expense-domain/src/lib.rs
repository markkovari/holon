//! `expense-domain` — router: auth (register/login/logout/me) and dispatch
//! into `handlers.rs` for the domain routes. The whole router is
//! `guestauth`/`guestio` macro expansions; nothing domain-specific lives
//! here — see `handlers.rs` for the actual expense-report logic (currently
//! UNIMPLEMENTED — see `.comp/goals/expense-report.toml`).

#[allow(warnings)]
mod bindings;
mod handlers;

pub use guestauth::Route;

pub const TENANT: &str = "expense";
/// The only roles this app grants. Anything else in a register request falls
/// back to `employee` — a role is a privilege, not a free-text field.
const ROLES: &[&str] = &["admin", "employee"];

guestio::guest_write_all!();
guestio::guest_bearer!();

const MAX_BODY_BYTES: usize = 1024 * 1024;
guestio::guest_read_body_text!(MAX_BODY_BYTES);

guestauth::guest_auth_reply!();
guestauth::guest_introspect!();
guestauth::guest_role_check!(is_admin, "admin");
guestauth::guest_audit!(TENANT);
guestauth::guest_accounts_endpoints!(TENANT, ROLES, "employee");
guestauth::guest_emit!();
guestauth::guest_saas_router!();
