//! `auth-guard` — register and log a user in, hash their password, issue and introspect a session token, and check whether their role grants an action

#[allow(warnings)]
mod bindings;

mod accounts;
mod audit;
mod config;
mod jwt_verify;
mod kv;
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

impl Jwt for Component {
    fn verify(token: String) -> Result<Claims, AuthError> {
        jwt_verify::verify(&token)
    }
}

impl Oidc for Component {
    fn discover(issuer: String) -> Result<OidcConfig, AuthError> {
        oidc_client::discover(&issuer)
    }

    fn verify_id_token(token: String) -> Result<Claims, AuthError> {
        jwt_verify::verify(&token)
    }

    fn exchange_code(code: String, redirect_uri: String) -> Result<TokenPair, AuthError> {
        oidc_client::exchange_code(&code, &redirect_uri)
    }
}

impl Accounts for Component {
    fn register(email: String, password: String, tenant: String) -> Result<Principal, AuthError> {
        accounts::register(&email, &password, &tenant)
    }

    fn login(email: String, password: String, tenant: String) -> Result<TokenPair, AuthError> {
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

impl Rbac for Component {
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

    fn authorize_any(token: String, required: Vec<Permission>) -> Result<Principal, AuthError> {
        let (principal, doc) = resolve_principal_doc(&token)?;
        if required.iter().any(|r| store::rbac_check_with(&doc, &principal, r)) {
            Ok(principal)
        } else {
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

fn authorize_impl(
    token: &str,
    required: Permission,
    traceparent: &str,
) -> Result<Principal, AuthError> {
    let perm = format!("{}:{}", required.target, required.action);
    let (principal, doc) = match resolve_principal_doc(token) {
        Ok(x) => x,
        Err(e) => {
            audit::emit_traced("authorize", audit::Outcome::Error, "", "", &perm, traceparent);
            return Err(e);
        }
    };
    if store::rbac_check_with(&doc, &principal, &required) {
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

fn resolve_principal(token: &str) -> Result<Principal, AuthError> {
    resolve_principal_doc(token).map(|(p, _)| p)
}

/// Also returns the rbac doc, so authorize needs no second kv round trip.
fn resolve_principal_doc(token: &str) -> Result<(Principal, store::RbacDoc), AuthError> {
    // The session id is the whole token, prefix included, as stored at issue.
    if token.starts_with(tokens::ACCESS_PREFIX) {
        let mut p = store::session_lookup(token)?;
        // Roles are re-resolved per check, so a grant after login takes effect at once.
        let doc = match store::rbac_load(&p.tenant) {
            Ok(doc) => {
                p.roles = store::rbac_roles_for(&p.tenant, &p.subject)
                    .unwrap_or_else(|_| doc.roles_of(&p.subject));
                doc
            }
            // kv hiccup: role grants deny against the empty doc; scope grants still work.
            Err(_) => store::RbacDoc::default(),
        };
        return Ok((p, doc));
    }
    let claims = jwt_verify::verify(token)?;
    let tenant = claims_tenant(&claims);
    let doc = store::rbac_load(&tenant).unwrap_or_default();
    let roles = store::rbac_roles_for(&tenant, &claims.sub)
        .unwrap_or_else(|_| doc.roles_of(&claims.sub));
    Ok((
        Principal {
            subject: claims.sub,
            tenant,
            roles,
            scopes: claims.scopes,
            expires_at: claims.exp,
        },
        doc,
    ))
}

fn claims_tenant(claims: &Claims) -> String {
    for (k, v) in &claims.raw {
        if k == "tenant" || k == "org" || k == "urn:zitadel:iam:org:id" {
            return v.clone();
        }
    }
    config::default_tenant()
}

bindings::export!(Component with_types_in bindings);
