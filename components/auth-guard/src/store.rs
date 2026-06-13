//! Session and RBAC persistence on top of `kv`.

use serde::{Deserialize, Serialize};

use crate::bindings::exports::auth::identity::types::{
    AuthError, Permission, Principal, TokenPair,
};
use crate::bindings::wasi::clocks::wall_clock;
use crate::kv;
use crate::tokens;

/// Default session lifetime in seconds (1 hour).
const SESSION_TTL: u64 = 3600;

// ---- serde mirrors of the WIT records -----------------------------------
// The generated WIT types don't derive serde, so we (de)serialize through
// these plain structs.

#[derive(Serialize, Deserialize)]
struct PrincipalDto {
    subject: String,
    tenant: String,
    roles: Vec<String>,
    scopes: Vec<String>,
    expires_at: u64,
}

#[derive(Serialize, Deserialize)]
struct PermissionDto {
    target: String,
    action: String,
}

impl From<&Principal> for PrincipalDto {
    fn from(p: &Principal) -> Self {
        PrincipalDto {
            subject: p.subject.clone(),
            tenant: p.tenant.clone(),
            roles: p.roles.clone(),
            scopes: p.scopes.clone(),
            expires_at: p.expires_at,
        }
    }
}

impl From<PrincipalDto> for Principal {
    fn from(d: PrincipalDto) -> Self {
        Principal {
            subject: d.subject,
            tenant: d.tenant,
            roles: d.roles,
            scopes: d.scopes,
            expires_at: d.expires_at,
        }
    }
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn json_err<E: core::fmt::Debug>(e: E) -> AuthError {
    AuthError::Internal(format!("json: {e:?}"))
}

// ---- sessions -----------------------------------------------------------

pub fn session_issue(p: Principal) -> Result<TokenPair, AuthError> {
    let session_id = tokens::new_id(tokens::ACCESS_PREFIX);
    let refresh = tokens::new_id(tokens::REFRESH_PREFIX);

    let mut dto = PrincipalDto::from(&p);
    // Stamp expiry from issuance time if the caller didn't set one.
    if dto.expires_at == 0 {
        dto.expires_at = now() + SESSION_TTL;
    }
    let body = serde_json::to_string(&dto).map_err(json_err)?;

    kv::set(&format!("sess:{session_id}"), &body)?;
    kv::set(&format!("refresh:{refresh}"), &session_id)?;

    Ok(TokenPair {
        access_token: session_id.clone(),
        refresh_token: Some(refresh),
        expires_in: dto.expires_at.saturating_sub(now()),
        session_id: Some(session_id),
    })
}

pub fn session_refresh(refresh_token: &str) -> Result<TokenPair, AuthError> {
    let refresh_key = format!("refresh:{refresh_token}");
    let session_id = kv::get(&refresh_key)?
        .ok_or_else(|| AuthError::InvalidToken("unknown refresh token".into()))?;

    let principal = session_lookup(&session_id)?;

    // Rotate: invalidate the old refresh token, issue a brand new pair.
    kv::delete(&refresh_key)?;
    kv::delete(&format!("sess:{session_id}"))?;
    session_issue(principal)
}

pub fn session_revoke(session_id: &str) -> Result<(), AuthError> {
    // Idempotent: deleting an absent key is fine.
    kv::delete(&format!("sess:{session_id}"))
}

pub fn session_lookup(session_id: &str) -> Result<Principal, AuthError> {
    let body = kv::get(&format!("sess:{session_id}"))?
        .ok_or(AuthError::Expired)?;
    let dto: PrincipalDto = serde_json::from_str(&body).map_err(json_err)?;
    if dto.expires_at != 0 && dto.expires_at < now() {
        kv::delete(&format!("sess:{session_id}"))?;
        return Err(AuthError::Expired);
    }
    Ok(dto.into())
}

// ---- rbac ---------------------------------------------------------------

fn roles_key(tenant: &str, subject: &str) -> String {
    format!("rbac:{tenant}:subject:{subject}")
}

fn role_perms_key(tenant: &str, role: &str) -> String {
    format!("rbac:{tenant}:role:{role}")
}

pub fn rbac_roles_for(tenant: &str, subject: &str) -> Result<Vec<String>, AuthError> {
    match kv::get(&roles_key(tenant, subject))? {
        Some(body) => serde_json::from_str(&body).map_err(json_err),
        None => Ok(Vec::new()),
    }
}

pub fn rbac_permissions_of(tenant: &str, role: &str) -> Result<Vec<Permission>, AuthError> {
    match kv::get(&role_perms_key(tenant, role))? {
        Some(body) => {
            let dtos: Vec<PermissionDto> = serde_json::from_str(&body).map_err(json_err)?;
            Ok(dtos
                .into_iter()
                .map(|d| Permission { target: d.target, action: d.action })
                .collect())
        }
        None => Ok(Vec::new()),
    }
}

pub fn rbac_assign_role(tenant: &str, subject: &str, role: &str) -> Result<(), AuthError> {
    let mut roles = rbac_roles_for(tenant, subject)?;
    if !roles.iter().any(|r| r == role) {
        roles.push(role.to_string());
        let body = serde_json::to_string(&roles).map_err(json_err)?;
        kv::set(&roles_key(tenant, subject), &body)?;
    }
    Ok(())
}

pub fn rbac_revoke_role(tenant: &str, subject: &str, role: &str) -> Result<(), AuthError> {
    let mut roles = rbac_roles_for(tenant, subject)?;
    let before = roles.len();
    roles.retain(|r| r != role);
    if roles.len() != before {
        let body = serde_json::to_string(&roles).map_err(json_err)?;
        kv::set(&roles_key(tenant, subject), &body)?;
    }
    Ok(())
}

/// Pure-ish permission check. Tries scopes first (no I/O), then resolves the
/// principal's roles to permissions from kv. A permission matches if target and
/// action match, with "*" acting as a wildcard for either field.
pub fn rbac_check(p: &Principal, required: &Permission) -> bool {
    // 1. Direct scope grant, formatted "target:action".
    let scope = format!("{}:{}", required.target, required.action);
    if p.scopes.iter().any(|s| s == &scope || s == "*") {
        return true;
    }
    // 2. Role-derived permissions.
    for role in &p.roles {
        if let Ok(perms) = rbac_permissions_of(&p.tenant, role) {
            if perms.iter().any(|perm| perm_matches(perm, required)) {
                return true;
            }
        }
    }
    false
}

fn perm_matches(granted: &Permission, required: &Permission) -> bool {
    let target_ok = granted.target == "*" || granted.target == required.target;
    let action_ok = granted.action == "*" || granted.action == required.action;
    target_ok && action_ok
}
