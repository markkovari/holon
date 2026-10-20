//! `login-app` — a consumer component that composes three universal capabilities into one login surface.
#[allow(warnings)]
mod bindings;

use bindings::exports::login::app::auth::{AuthError, Guest, Identity, LoginResult};

use bindings::config::store::store as cfg;
use bindings::secrets::vault::vault as secrets;
use bindings::session::store::store as sessions;

struct Component;

const DEFAULT_SESSION_TTL: u64 = 3600;
const PEPPER_SECRET: &str = "auth-pepper";

fn session_ttl() -> u64 {
    match cfg::get("app", "session-ttl") {
        Ok(entry) => match entry.value {
            cfg::Value::Integer(n) if n > 0 => n as u64,
            _ => DEFAULT_SESSION_TTL,
        },
        // An unset knob is normal, not fatal.
        Err(_) => DEFAULT_SESSION_TTL,
    }
}

/// Unseeded vault -> empty pepper, so the demo runs with a bare vault.
fn pepper() -> Vec<u8> {
    secrets::get(PEPPER_SECRET).unwrap_or_default()
}

impl Guest for Component {
    fn login(user: String, password: String) -> Result<LoginResult, AuthError> {
        // Demo only proves composition: any non-empty credential is accepted.
        if user.is_empty() || password.is_empty() {
            return Err(AuthError::InvalidCredentials);
        }

        let peppered = !pepper().is_empty();

        let ttl = session_ttl();

        let payload = format!("{{\"user\":\"{user}\",\"peppered\":{peppered}}}");
        let s = sessions::create(payload.as_bytes(), ttl)
            .map_err(|e| AuthError::Capability(format!("session.create: {e:?}")))?;

        Ok(LoginResult { token: s.id, csrf: s.csrf_token, expires: s.expires })
    }

    fn whoami(token: String) -> Result<Identity, AuthError> {
        let s = match sessions::get(&token) {
            Ok(s) => s,
            Err(sessions::SessionError::NotFound) => return Err(AuthError::NoSession),
            Err(e) => return Err(AuthError::Capability(format!("session.get: {e:?}"))),
        };
        let body = String::from_utf8(s.data).unwrap_or_default();
        let user = body
            .split_once("\"user\":\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(u, _)| u.to_string())
            .unwrap_or_default();
        Ok(Identity { user, expires: s.expires })
    }

    fn logout(token: String) -> Result<(), AuthError> {
        sessions::revoke(&token)
            .map_err(|e| AuthError::Capability(format!("session.revoke: {e:?}")))
    }
}

bindings::export!(Component with_types_in bindings);
