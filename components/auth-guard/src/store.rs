use serde::{Deserialize, Serialize};

use crate::bindings::exports::auth::identity::types::{
    AuthError, Permission, Principal, TokenPair,
};
use crate::bindings::wasi::clocks::wall_clock;
use crate::config;
use crate::kv;
use crate::tokens;

// The generated WIT types do not derive serde.

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

/// The family lets an already-rotated (stolen) refresh token kill every sibling session.
#[derive(Serialize, Deserialize)]
struct RefreshRecord {
    session_id: String,
    family: String,
}

pub fn session_issue(p: Principal) -> Result<TokenPair, AuthError> {
    let family = tokens::new_id("fam_");
    session_issue_in_family(p, &family)
}

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
        // Reuse of a rotated token is a breach: kill the family.
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

    kv::set(&format!("spent:{refresh_token}"), &family)?;
    kv::delete(&refresh_key)?;
    kv::delete(&format!("sess:{session_id}"))?;
    session_issue_in_family(principal, &family)
}

pub fn session_revoke(session_id: &str) -> Result<(), AuthError> {
    kv::delete(&format!("sess:{session_id}"))
}

fn family_key(family: &str) -> String {
    format!("family:{family}")
}

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
    let body = kv::get(&format!("sess:{session_id}"))?.ok_or(AuthError::Expired)?;
    let dto: PrincipalDto = serde_json::from_str(&body).map_err(json_err)?;
    if dto.expires_at != 0 && dto.expires_at < now() {
        kv::delete(&format!("sess:{session_id}"))?;
        return Err(AuthError::Expired);
    }
    Ok(dto.into())
}

// Per-subject indexing (`rbac:{tenant}:sub:{subject}`) splits user role assignments out
// of the monolithic tenant document (`rbac:{tenant}`). This eliminates write amplification
// and cross-subject serialization contention, allowing thousands of subjects per tenant
// to be updated independently without lock or CAS conflicts, while falling back to the
// tenant doc for backward compatibility.

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

fn rbac_subject_key(tenant: &str, subject: &str) -> String {
    format!("rbac:{tenant}:sub:{subject}")
}

fn rbac_subject_load(tenant: &str, subject: &str) -> Result<Option<Vec<String>>, AuthError> {
    match kv::get(&rbac_subject_key(tenant, subject))? {
        Some(body) => serde_json::from_str(&body).map_err(json_err).map(Some),
        None => Ok(None),
    }
}

fn rbac_subject_store(tenant: &str, subject: &str, roles: &[String]) -> Result<(), AuthError> {
    let body = serde_json::to_string(roles).map_err(json_err)?;
    kv::set(&rbac_subject_key(tenant, subject), &body)
}

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
    if let Some(roles) = rbac_subject_load(tenant, subject)? {
        return Ok(roles);
    }
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
    let mut roles = rbac_roles_for(tenant, subject)?;
    if !roles.iter().any(|r| r == role) {
        roles.push(role.to_string());
        rbac_subject_store(tenant, subject, &roles)?;
    }
    Ok(())
}

pub fn rbac_revoke_role(tenant: &str, subject: &str, role: &str) -> Result<(), AuthError> {
    let mut roles = rbac_roles_for(tenant, subject)?;
    let before = roles.len();
    roles.retain(|r| r != role);
    if roles.len() != before {
        rbac_subject_store(tenant, subject, &roles)?;
    }
    Ok(())
}

/// No I/O. "*" is a wildcard for target or action.
pub fn rbac_check_with(doc: &RbacDoc, p: &Principal, required: &Permission) -> bool {
    let scope = format!("{}:{}", required.target, required.action);
    if p.scopes.iter().any(|s| s == &scope || s == "*") {
        return true;
    }
    p.roles.iter().any(|role| {
        doc.roles.get(role).is_some_and(|perms| {
            perms.iter().any(|d| matches_parts(&d.target, &d.action, required))
        })
    })
}

pub fn rbac_check(p: &Principal, required: &Permission) -> bool {
    // kv down: empty doc, so only a direct scope grant can allow.
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
        assert!(!perm_matches(&perm("*", "read"), &perm("orders", "write")));
    }

    #[test]
    fn rbac_doc_roles_of_and_subject_keys() {
        let mut doc = RbacDoc::default();
        doc.subjects.insert("alice".into(), vec!["admin".into(), "user".into()]);
        assert_eq!(doc.roles_of("alice"), vec!["admin".to_string(), "user".to_string()]);
        assert_eq!(doc.roles_of("bob"), Vec::<String>::new());
        assert_eq!(rbac_subject_key("t1", "alice"), "rbac:t1:sub:alice");
    }
}
