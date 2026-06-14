//! Reference implementation of the `auth:identity` contract (`authority` world).
//!
//! # Exports / imports
//! Exports: types, jwt, oidc, session, accounts, rbac, authorizer.
//! Imports (host capabilities): wasi:keyvalue (store + atomics), wasi:http
//! (outgoing, for OIDC discovery / JWKS), wasi:clocks (expiry), wasi:random
//! (id generation), wasi:config/runtime (policy knobs).
//!
//! # Module map
//! - [`config`]       — policy knobs read from wasi:config (session-ttl,
//!                       password-min-len, expected-issuer/audience, allowed-algs, …).
//! - [`jwt_verify`]   — stateless JWS verification + claim validation (the
//!                       pure `validate_claims` is unit-tested).
//! - [`oidc_client`]  — OIDC discovery + JWKS over wasi:http, TTL-cached.
//! - [`accounts`]     — local accounts: argon2id register/login.
//! - [`store`]        — sessions (with families + refresh-reuse detection) and RBAC.
//! - [`kv`]           — wasi:keyvalue wrapper; sanitizes keys to NATS-legal chars.
//! - [`tokens`]       — id generation and token prefixes.
//!
//! # Claim handling
//! A verified token's claims become a `principal` per the table in the WIT
//! (`interface types`): `sub`->subject; `scope`/`scp`->scopes; tenant from
//! `tenant`/`org`/`urn:zitadel:iam:org:id` else `default-tenant`; roles are
//! resolved from the RBAC store by (tenant, subject), never trusted from the
//! token. See [`claims_tenant`] and [`resolve_principal`].
//!
//! # Storage layout (wasi:keyvalue, link name "default")
//! Logical keys (then run through [`kv::safe`] so `:`/`@`/`.` become `_XX`):
//!   sess:{session-id}        -> JSON principal (+ expiry)
//!   refresh:{refresh-token}  -> JSON { session-id, family }   (active)
//!   spent:{refresh-token}    -> family                        (rotated; reuse = breach)
//!   family:{family}          -> JSON list<session-id>         (revoke-all on breach)
//!   user:{tenant}:{email}    -> JSON { subject, argon2-phc }
//!   rbac:{tenant}:subject:{sub}  -> JSON list<role-name>
//!   rbac:{tenant}:role:{role}    -> JSON list<permission>
//!   jwks:{issuer} / oidc:{issuer} -> "{expiry}:{json}"        (TTL-cached)
//!   oidc:issuer / oidc:client-id / oidc:client-secret / hs256-secret  (config)

#[allow(warnings)]
mod bindings;

mod accounts;
mod audit;
mod config;
mod kv;
mod jwt_verify;
mod oidc_client;
mod store;
mod tokens;

use bindings::exports::auth::identity::accounts::Guest as Accounts;
use bindings::exports::auth::identity::authorizer::Guest as Authorizer;
use bindings::exports::auth::identity::jwt::Guest as Jwt;
use bindings::exports::auth::identity::oidc::{Guest as Oidc, OidcConfig};
use bindings::exports::auth::identity::rbac::Guest as Rbac;
use bindings::exports::auth::identity::session::Guest as Session;
use bindings::exports::auth::identity::types::{
    AuthError, Claims, Permission, Principal, TokenPair,
};

struct Component;

// ---- jwt ----------------------------------------------------------------

impl Jwt for Component {
    fn verify(token: String) -> Result<Claims, AuthError> {
        jwt_verify::verify(&token)
    }
}

// ---- oidc ---------------------------------------------------------------

impl Oidc for Component {
    fn discover(issuer: String) -> Result<OidcConfig, AuthError> {
        oidc_client::discover(&issuer)
    }

    fn verify_id_token(token: String) -> Result<Claims, AuthError> {
        // An id_token is a JWT signed by the issuer; verification path is the
        // same as `jwt::verify` (issuer JWKS is resolved from the `iss` claim).
        jwt_verify::verify(&token)
    }

    fn exchange_code(code: String, redirect_uri: String) -> Result<TokenPair, AuthError> {
        oidc_client::exchange_code(&code, &redirect_uri)
    }
}

// ---- accounts -----------------------------------------------------------

impl Accounts for Component {
    fn register(
        email: String,
        password: String,
        tenant: String,
    ) -> Result<Principal, AuthError> {
        accounts::register(&email, &password, &tenant)
    }

    fn login(
        email: String,
        password: String,
        tenant: String,
    ) -> Result<TokenPair, AuthError> {
        accounts::login(&email, &password, &tenant)
    }

    fn verify_password(
        email: String,
        password: String,
        tenant: String,
    ) -> Result<Principal, AuthError> {
        accounts::verify_password(&email, &password, &tenant)
    }

    fn change_password(
        email: String,
        tenant: String,
        current_password: String,
        new_password: String,
    ) -> Result<(), AuthError> {
        accounts::change_password(&email, &tenant, &current_password, &new_password)
    }
}

// ---- session ------------------------------------------------------------

impl Session for Component {
    fn issue(p: Principal) -> Result<TokenPair, AuthError> {
        store::session_issue(p)
    }

    fn refresh(refresh_token: String) -> Result<TokenPair, AuthError> {
        store::session_refresh(&refresh_token)
    }

    fn revoke(session_id: String) -> Result<(), AuthError> {
        store::session_revoke(&session_id)
    }

    fn lookup(session_id: String) -> Result<Principal, AuthError> {
        store::session_lookup(&session_id)
    }
}

// ---- rbac ---------------------------------------------------------------

impl Rbac for Component {
    /// Pure check: does the principal hold a role that grants `required`,
    /// or a matching scope? No I/O — uses what's baked into the principal.
    fn check(p: Principal, required: Permission) -> bool {
        store::rbac_check(&p, &required)
    }

    fn assign_role(tenant: String, subject: String, role: String) -> Result<(), AuthError> {
        store::rbac_assign_role(&tenant, &subject, &role)
    }

    fn revoke_role(tenant: String, subject: String, role: String) -> Result<(), AuthError> {
        store::rbac_revoke_role(&tenant, &subject, &role)
    }

    fn roles_for(tenant: String, subject: String) -> Result<Vec<String>, AuthError> {
        store::rbac_roles_for(&tenant, &subject)
    }

    fn permissions_of(tenant: String, role: String) -> Result<Vec<Permission>, AuthError> {
        store::rbac_permissions_of(&tenant, &role)
    }

    fn set_role_permissions(
        tenant: String,
        role: String,
        permissions: Vec<Permission>,
    ) -> Result<(), AuthError> {
        store::rbac_set_role_permissions(&tenant, &role, &permissions)
    }
}

// ---- authorizer (the verify-token guard) --------------------------------

impl Authorizer for Component {
    fn authorize(token: String, required: Permission) -> Result<Principal, AuthError> {
        authorize_impl(&token, required, "")
    }

    fn authorize_traced(
        token: String,
        required: Permission,
        traceparent: String,
    ) -> Result<Principal, AuthError> {
        authorize_impl(&token, required, &traceparent)
    }

    fn authorize_any(
        token: String,
        required: Vec<Permission>,
    ) -> Result<Principal, AuthError> {
        let principal = resolve_principal(&token)?;
        if required.iter().any(|r| store::rbac_check(&principal, r)) {
            Ok(principal)
        } else {
            // Report the first requirement as the unmet one.
            let first = required
                .into_iter()
                .next()
                .unwrap_or(Permission { target: String::new(), action: String::new() });
            Err(AuthError::InsufficientScope(first))
        }
    }

    fn introspect(token: String) -> Result<Principal, AuthError> {
        resolve_principal(&token)
    }
}

/// Shared authorize logic; `traceparent` is the caller's W3C trace context
/// ("" when none) used to correlate the audit event.
fn authorize_impl(
    token: &str,
    required: Permission,
    traceparent: &str,
) -> Result<Principal, AuthError> {
    let perm = format!("{}:{}", required.target, required.action);
    let principal = match resolve_principal(token) {
        Ok(p) => p,
        Err(e) => {
            audit::emit_traced("authorize", audit::Outcome::Error, "", "", &perm, traceparent);
            return Err(e);
        }
    };
    if store::rbac_check(&principal, &required) {
        audit::emit_traced(
            "authorize",
            audit::Outcome::Allow,
            &principal.tenant,
            &principal.subject,
            &perm,
            traceparent,
        );
        Ok(principal)
    } else {
        audit::emit_traced(
            "authorize",
            audit::Outcome::Deny,
            &principal.tenant,
            &principal.subject,
            &perm,
            traceparent,
        );
        Err(AuthError::InsufficientScope(required))
    }
}

/// Detect the token kind and verify it into a `Principal`.
///
/// - Opaque session tokens are issued with a `sess_` prefix and resolved via kv.
/// - Anything else is treated as a JWS (JWT / OIDC id_token) and verified
///   against the issuer's key material.
fn resolve_principal(token: &str) -> Result<Principal, AuthError> {
    // Session tokens carry the `sess_` prefix. The session id IS the whole
    // token (prefix included) — that's how it was stored at issue time — so we
    // pass `token` as-is, not the stripped remainder.
    if token.starts_with(tokens::ACCESS_PREFIX) {
        let mut p = store::session_lookup(token)?;
        // Re-resolve roles from the store on each check so a role granted AFTER
        // login takes effect immediately (the principal stored at issue time may
        // predate the grant). Scopes stay as issued.
        p.roles = store::rbac_roles_for(&p.tenant, &p.subject).unwrap_or(p.roles);
        return Ok(p);
    }
    let claims = jwt_verify::verify(token)?;
    let tenant = claims_tenant(&claims);
    let roles = store::rbac_roles_for(&tenant, &claims.sub).unwrap_or_default();
    Ok(Principal {
        subject: claims.sub,
        tenant,
        roles,
        scopes: claims.scopes,
        expires_at: claims.exp,
    })
}

/// Extract a tenant id from claims. Looks for an `org`/`tenant` custom claim,
/// falling back to "" for single-tenant deployments.
fn claims_tenant(claims: &Claims) -> String {
    for (k, v) in &claims.raw {
        if k == "tenant" || k == "org" || k == "urn:zitadel:iam:org:id" {
            return v.clone();
        }
    }
    config::default_tenant()
}

bindings::export!(Component with_types_in bindings);
