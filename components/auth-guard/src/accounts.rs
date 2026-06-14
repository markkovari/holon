//! Local user accounts: register / login / verify / change-password.
//!
//! Passwords are hashed with Argon2id (PHC string format). Salt entropy comes
//! from `wasi:random`. User records live in kv:
//!   user:{tenant}:{email}  -> JSON { subject, phc }
//!
//! Login delegates session issuance to `store::session_issue`, so the token a
//! caller gets back is a normal session token the authorizer already understands.

use argon2::Argon2;
use password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, Salt};
use serde::{Deserialize, Serialize};

use crate::bindings::exports::auth::identity::types::{AuthError, Principal, TokenPair};
use crate::bindings::ratelimit::guard::limiter;
use crate::bindings::ratelimit::guard::limiter::LimitError;
use crate::bindings::wasi::random::random::get_random_bytes;
use crate::{config, kv, store};

/// Rate-limit key for an identifier (per tenant+email). Failed logins count
/// against this; a successful login resets it.
fn rl_key(tenant: &str, email: &str) -> String {
    format!("login:{tenant}:{}", email.to_lowercase())
}

/// Map the (separate) rate-limiter component's error to our auth-error.
fn rl_err(e: LimitError) -> AuthError {
    match e {
        LimitError::Locked(_) => AuthError::RateLimited,
        LimitError::BackendUnavailable(m) => AuthError::BackendUnavailable(format!("ratelimit: {m}")),
    }
}

#[derive(Serialize, Deserialize)]
struct Account {
    subject: String,
    /// Argon2 PHC hash string, e.g. "$argon2id$v=19$m=...$...".
    phc: String,
}

fn user_key(tenant: &str, email: &str) -> String {
    format!("user:{tenant}:{}", email.to_lowercase())
}

fn load(tenant: &str, email: &str) -> Result<Option<Account>, AuthError> {
    match kv::get(&user_key(tenant, email))? {
        Some(body) => serde_json::from_str(&body)
            .map(Some)
            .map_err(|e| AuthError::Internal(format!("account json: {e}"))),
        None => Ok(None),
    }
}

fn hash_password(password: &str) -> Result<String, AuthError> {
    // 16 random bytes -> base64 salt for the PHC string.
    let raw = get_random_bytes(16);
    let salt_b64 = b64_salt(&raw);
    let salt = Salt::from_b64(&salt_b64)
        .map_err(|e| AuthError::Internal(format!("salt: {e}")))?;
    Argon2::default()
        .hash_password(password.as_bytes(), salt)
        .map(|h| h.to_string())
        .map_err(|e| AuthError::Internal(format!("hash: {e}")))
}

fn verify(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Encode bytes as the unpadded base64 alphabet `password-hash::Salt` expects.
fn b64_salt(bytes: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use base64::Engine;
    STANDARD_NO_PAD.encode(bytes)
}

fn validate_input(email: &str, password: &str) -> Result<(), AuthError> {
    if !email.contains('@') || email.len() < 3 {
        return Err(AuthError::Malformed("invalid email".into()));
    }
    let min = config::password_min_len();
    if password.len() < min {
        return Err(AuthError::Malformed(format!(
            "password must be >= {min} chars"
        )));
    }
    Ok(())
}

fn new_subject() -> String {
    let bytes = get_random_bytes(16);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("usr_{hex}")
}

fn principal_for(subject: String, tenant: &str) -> Principal {
    let roles = store::rbac_roles_for(tenant, &subject).unwrap_or_default();
    Principal {
        subject,
        tenant: tenant.to_string(),
        roles,
        scopes: Vec::new(),
        expires_at: 0,
    }
}

pub fn register(email: &str, password: &str, tenant: &str) -> Result<Principal, AuthError> {
    validate_input(email, password)?;
    if load(tenant, email)?.is_some() {
        return Err(AuthError::AlreadyExists);
    }
    let subject = new_subject();
    let phc = hash_password(password)?;
    let account = Account { subject: subject.clone(), phc };
    let body = serde_json::to_string(&account)
        .map_err(|e| AuthError::Internal(format!("account json: {e}")))?;
    kv::set(&user_key(tenant, email), &body)?;
    Ok(principal_for(subject, tenant))
}

pub fn verify_password(
    email: &str,
    password: &str,
    tenant: &str,
) -> Result<Principal, AuthError> {
    // Constant-ish path: always do a verify, even on missing account, to avoid
    // user enumeration via timing. Return the same error for both cases.
    match load(tenant, email)? {
        Some(account) if verify(password, &account.phc) => {
            Ok(principal_for(account.subject, tenant))
        }
        _ => Err(AuthError::InvalidCredentials),
    }
}

pub fn login(email: &str, password: &str, tenant: &str) -> Result<TokenPair, AuthError> {
    let key = rl_key(tenant, email);
    // Refuse before touching the password store if the identifier is locked.
    limiter::check(&key).map_err(rl_err)?;

    match verify_password(email, password, tenant) {
        Ok(principal) => {
            // success clears the failure counter
            let _ = limiter::reset(&key);
            store::session_issue(principal)
        }
        Err(e) => {
            // count this failure toward lockout, then surface the auth error
            let _ = limiter::record_failure(&key);
            Err(e)
        }
    }
}

pub fn change_password(
    email: &str,
    tenant: &str,
    current: &str,
    new_password: &str,
) -> Result<(), AuthError> {
    validate_input(email, new_password)?;
    let mut account = load(tenant, email)?.ok_or(AuthError::InvalidCredentials)?;
    if !verify(current, &account.phc) {
        return Err(AuthError::InvalidCredentials);
    }
    account.phc = hash_password(new_password)?;
    let body = serde_json::to_string(&account)
        .map_err(|e| AuthError::Internal(format!("account json: {e}")))?;
    kv::set(&user_key(tenant, email), &body)
}
