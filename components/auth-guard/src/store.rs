//! Session and RBAC persistence on top of `kv`.

use serde::{Deserialize, Serialize};

use crate::bindings::exports::auth::identity::types::{
    AuthError, Permission, Principal, TokenPair,
};
use crate::bindings::wasi::clocks::wall_clock;
use crate::config;
use crate::kv;
use crate::tokens;


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

/// A refresh token maps to its session AND its session family, so a stolen,
/// already-rotated refresh token can be detected and the whole family killed.
#[derive(Serialize, Deserialize)]
struct RefreshRecord {
    session_id: String,
    family: String,
}

/// Issue a brand-new session in a brand-new family (the login path).
pub fn session_issue(p: Principal) -> Result<TokenPair, AuthError> {
    let family = tokens::new_id("fam_");
    session_issue_in_family(p, &family)
}

/// Issue a session within an existing family (the rotate-on-refresh path).
fn session_issue_in_family(p: Principal, family: &str) -> Result<TokenPair, AuthError> {
    let session_id = tokens::new_id(tokens::ACCESS_PREFIX);
    let refresh = tokens::new_id(tokens::REFRESH_PREFIX);

    let mut dto = PrincipalDto::from(&p);
    if dto.expires_at == 0 {
        dto.expires_at = now() + config::session_ttl();
    }
    let body = serde_json::to_string(&dto).map_err(json_err)?;

    let rec = serde_json::to_string(&RefreshRecord {
        session_id: session_id.clone(),
        family: family.to_string(),
    })
    .map_err(json_err)?;

    kv::set(&format!("sess:{session_id}"), &body)?;
    kv::set(&format!("refresh:{refresh}"), &rec)?;
    // Track the session in its family so a breach can revoke siblings.
    family_add(family, &session_id)?;

    Ok(TokenPair {
        access_token: session_id.clone(),
        refresh_token: Some(refresh),
        expires_in: dto.expires_at.saturating_sub(now()),
        session_id: Some(session_id),
    })
}

pub fn session_refresh(refresh_token: &str) -> Result<TokenPair, AuthError> {
    let refresh_key = format!("refresh:{refresh_token}");

    let Some(raw) = kv::get(&refresh_key)? else {
        // Not an active refresh token. Was it already spent? If so this is a
        // REUSE of a rotated token — treat as a breach and kill the family.
        if let Some(family) = kv::get(&format!("spent:{refresh_token}"))? {
            revoke_family(&family)?;
            crate::audit::emit(
                "refresh_reuse",
                crate::audit::Outcome::Deny,
                "",
                "",
                "session family revoked",
            );
            return Err(AuthError::InvalidToken(
                "refresh token reuse detected; session family revoked".into(),
            ));
        }
        return Err(AuthError::InvalidToken("unknown refresh token".into()));
    };

    let rec: RefreshRecord = serde_json::from_str(&raw).map_err(json_err)?;
    let session_id = rec.session_id;
    let family = rec.family;
    let principal = session_lookup(&session_id)?;

    // Rotate: mark this refresh token spent (for reuse detection), drop the old
    // active mapping + session, mint a fresh pair in the SAME family.
    kv::set(&format!("spent:{refresh_token}"), &family)?;
    kv::delete(&refresh_key)?;
    kv::delete(&format!("sess:{session_id}"))?;
    session_issue_in_family(principal, &family)
}

pub fn session_revoke(session_id: &str) -> Result<(), AuthError> {
    // Idempotent: deleting an absent key is fine.
    kv::delete(&format!("sess:{session_id}"))
}

// ---- session families (for refresh-reuse breach response) ---------------

fn family_key(family: &str) -> String {
    format!("family:{family}")
}

/// Append a session id to its family's member list.
fn family_add(family: &str, session_id: &str) -> Result<(), AuthError> {
    let mut members: Vec<String> = match kv::get(&family_key(family))? {
        Some(b) => serde_json::from_str(&b).map_err(json_err)?,
        None => Vec::new(),
    };
    if !members.iter().any(|s| s == session_id) {
        members.push(session_id.to_string());
        let body = serde_json::to_string(&members).map_err(json_err)?;
        kv::set(&family_key(family), &body)?;
    }
    Ok(())
}

/// Revoke every session in a family — the breach response to refresh reuse.
fn revoke_family(family: &str) -> Result<(), AuthError> {
    if let Some(b) = kv::get(&family_key(family))? {
        let members: Vec<String> = serde_json::from_str(&b).map_err(json_err)?;
        for sid in members {
            kv::delete(&format!("sess:{sid}"))?;
        }
    }
    kv::delete(&family_key(family))?;
    Ok(())
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
//
// All RBAC state for a tenant lives in ONE kv value (`rbac:{tenant}`):
// subject -> roles plus role -> permissions. Every kv op is a real backend
// round-trip, so the old per-key layout (roles read + one read per role)
// made every authorize 2+N trips; the single doc makes it one. Admin writes
// are read-modify-write of the doc — cold path, same no-CAS caveat the
// per-key layout already had.
// ponytail: one doc per tenant; split back out per subject if a tenant's
// subject count ever makes this value heavy.

#[derive(Serialize, Deserialize, Default)]
pub struct RbacDoc {
    subjects: std::collections::BTreeMap<String, Vec<String>>,
    roles: std::collections::BTreeMap<String, Vec<PermissionDto>>,
}

impl RbacDoc {
    pub fn roles_of(&self, subject: &str) -> Vec<String> {
        self.subjects.get(subject).cloned().unwrap_or_default()
    }
}

fn rbac_key(tenant: &str) -> String {
    format!("rbac:{tenant}")
}

/// Load a tenant's rbac doc — the ONE kv read an authorize pays for rbac.
pub fn rbac_load(tenant: &str) -> Result<RbacDoc, AuthError> {
    match kv::get(&rbac_key(tenant))? {
        Some(body) => serde_json::from_str(&body).map_err(json_err),
        None => Ok(RbacDoc::default()),
    }
}

fn rbac_store(tenant: &str, doc: &RbacDoc) -> Result<(), AuthError> {
    let body = serde_json::to_string(doc).map_err(json_err)?;
    kv::set(&rbac_key(tenant), &body)
}

pub fn rbac_roles_for(tenant: &str, subject: &str) -> Result<Vec<String>, AuthError> {
    Ok(rbac_load(tenant)?.roles_of(subject))
}

pub fn rbac_permissions_of(tenant: &str, role: &str) -> Result<Vec<Permission>, AuthError> {
    Ok(rbac_load(tenant)?
        .roles
        .get(role)
        .map(|dtos| {
            dtos.iter()
                .map(|d| Permission { target: d.target.clone(), action: d.action.clone() })
                .collect()
        })
        .unwrap_or_default())
}

pub fn rbac_set_role_permissions(
    tenant: &str,
    role: &str,
    perms: &[Permission],
) -> Result<(), AuthError> {
    let mut doc = rbac_load(tenant)?;
    doc.roles.insert(
        role.to_string(),
        perms
            .iter()
            .map(|p| PermissionDto { target: p.target.clone(), action: p.action.clone() })
            .collect(),
    );
    rbac_store(tenant, &doc)
}

pub fn rbac_assign_role(tenant: &str, subject: &str, role: &str) -> Result<(), AuthError> {
    let mut doc = rbac_load(tenant)?;
    let roles = doc.subjects.entry(subject.to_string()).or_default();
    if !roles.iter().any(|r| r == role) {
        roles.push(role.to_string());
        rbac_store(tenant, &doc)?;
    }
    Ok(())
}

pub fn rbac_revoke_role(tenant: &str, subject: &str, role: &str) -> Result<(), AuthError> {
    let mut doc = rbac_load(tenant)?;
    let Some(roles) = doc.subjects.get_mut(subject) else {
        return Ok(());
    };
    let before = roles.len();
    roles.retain(|r| r != role);
    if roles.len() != before {
        rbac_store(tenant, &doc)?;
    }
    Ok(())
}

/// Permission check against an already-loaded rbac doc — NO I/O. Tries scopes
/// first, then role-derived permissions. A permission matches if target and
/// action match, with "*" acting as a wildcard for either field.
pub fn rbac_check_with(doc: &RbacDoc, p: &Principal, required: &Permission) -> bool {
    // 1. Direct scope grant, formatted "target:action".
    let scope = format!("{}:{}", required.target, required.action);
    if p.scopes.iter().any(|s| s == &scope || s == "*") {
        return true;
    }
    // 2. Role-derived permissions.
    p.roles.iter().any(|role| {
        doc.roles
            .get(role)
            .is_some_and(|perms| perms.iter().any(|d| matches_parts(&d.target, &d.action, required)))
    })
}

/// One-shot check that loads the doc itself (the exported rbac `check`).
pub fn rbac_check(p: &Principal, required: &Permission) -> bool {
    // kv down -> default (empty) doc: only a direct scope grant can allow.
    let doc = rbac_load(&p.tenant).unwrap_or_default();
    rbac_check_with(&doc, p, required)
}

#[cfg(test)]
fn perm_matches(granted: &Permission, required: &Permission) -> bool {
    matches_parts(&granted.target, &granted.action, required)
}

fn matches_parts(target: &str, action: &str, required: &Permission) -> bool {
    let target_ok = target == "*" || target == required.target;
    let action_ok = action == "*" || action == required.action;
    target_ok && action_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perm(t: &str, a: &str) -> Permission {
        Permission { target: t.into(), action: a.into() }
    }

    #[test]
    fn exact_match() {
        assert!(perm_matches(&perm("orders", "read"), &perm("orders", "read")));
    }

    #[test]
    fn no_match_on_different_target_or_action() {
        assert!(!perm_matches(&perm("orders", "read"), &perm("orders", "write")));
        assert!(!perm_matches(&perm("orders", "read"), &perm("users", "read")));
    }

    #[test]
    fn wildcard_target_and_action() {
        assert!(perm_matches(&perm("*", "read"), &perm("orders", "read")));
        assert!(perm_matches(&perm("orders", "*"), &perm("orders", "delete")));
        assert!(perm_matches(&perm("*", "*"), &perm("anything", "anything")));
    }

    #[test]
    fn wildcard_does_not_widen_the_other_field() {
        // target wildcard but action still must match.
        assert!(!perm_matches(&perm("*", "read"), &perm("orders", "write")));
    }
}
