//! Reference implementation of the `auth:identity` contract (`authority` world).
//!
//! Exports: types, jwt, oidc, session, rbac, authorizer.
//! Imports (host capabilities): wasi:keyvalue (store + atomics), wasi:http
//! (outgoing, for OIDC discovery / JWKS), wasi:clocks (expiry), wasi:random
//! (id generation).
//!
//! State layout in keyvalue (bucket "auth"):
//!   sess:{session-id}        -> JSON principal
//!   refresh:{refresh-token}  -> session-id
//!   jwks:{issuer}            -> cached JWKS JSON
//!   oidc:{issuer}            -> cached discovery doc JSON
//!   rbac:{tenant}:role:{role}      -> JSON list<permission>
//!   rbac:{tenant}:subject:{sub}    -> JSON list<role-name>

#[allow(warnings)]
mod bindings;

mod accounts;
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
}

// ---- authorizer (the verify-token guard) --------------------------------

impl Authorizer for Component {
    fn authorize(token: String, required: Permission) -> Result<Principal, AuthError> {
        let principal = resolve_principal(&token)?;
        if store::rbac_check(&principal, &required) {
            Ok(principal)
        } else {
            Err(AuthError::InsufficientScope(required))
        }
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
        return store::session_lookup(token);
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
