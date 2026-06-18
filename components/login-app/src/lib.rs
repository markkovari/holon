//! `login-app` — a consumer component that composes three universal
//! capabilities into one login surface.
//!
//! It imports `session:store`, `config:store` and `secrets:vault` and wires
//! them together; `wac plug` satisfies those three imports with the real
//! capability components at build time (see `just compose-login`), producing a
//! single self-contained component. This is the multi-capability counterpart to
//! auth-guard composing rate-limiter: there one component imported one other;
//! here one component imports three.
//!
//! Flow:
//!   login  -> config:store gives the session ttl; secrets:vault supplies an
//!             auth "pepper" (proving the vault is wired in — a real app would
//!             use it to verify a credential MAC); session:store mints the
//!             server-side session.
//!   whoami -> session:store lookup -> identity.
//!   logout -> session:store revoke.

#[allow(warnings)]
mod bindings;

use bindings::exports::login::app::auth::{AuthError, Guest, Identity, LoginResult};

// The three composed capabilities (imported interfaces).
use bindings::config::store::store as cfg;
use bindings::secrets::vault::vault as secrets;
use bindings::session::store::store as sessions;

struct Component;

const DEFAULT_SESSION_TTL: u64 = 3600;
/// Name of the secret holding the per-deployment auth pepper. Seeded by the
/// host / a bootstrap; `login` fetches it to show the vault path is live.
const PEPPER_SECRET: &str = "auth-pepper";

/// Read the session ttl from config:store (`app`/`session-ttl`), falling back
/// to the default if unset or non-integer.
fn session_ttl() -> u64 {
    match cfg::get("app", "session-ttl") {
        Ok(entry) => match entry.value {
            cfg::Value::Integer(n) if n > 0 => n as u64,
            _ => DEFAULT_SESSION_TTL,
        },
        // not-found (or any error) -> default. The whole point of config:store
        // is that an unset knob is normal, not fatal.
        Err(_) => DEFAULT_SESSION_TTL,
    }
}

/// Fetch the auth pepper from secrets:vault. If it isn't seeded yet, fall back
/// to an empty pepper rather than failing login — the demo should run with a
/// bare vault. (A production policy might instead refuse to start.)
fn pepper() -> Vec<u8> {
    secrets::get(PEPPER_SECRET).unwrap_or_default()
}

impl Guest for Component {
    fn login(user: String, password: String) -> Result<LoginResult, AuthError> {
        // Credential "check": this demo only proves composition, so it rejects
        // empty inputs and accepts the rest. A real app would verify against an
        // accounts store (e.g. argon2 in auth-guard) and MAC the password with
        // the pepper below.
        if user.is_empty() || password.is_empty() {
            return Err(AuthError::InvalidCredentials);
        }

        // secrets:vault — fetch the pepper (wired in; tag the session payload
        // with whether a pepper is configured so the call isn't dead code).
        let peppered = !pepper().is_empty();

        // config:store — how long the session should live.
        let ttl = session_ttl();

        // session:store — mint the server-side session. Payload is an opaque
        // app blob; here a tiny record the component owns.
        let payload = format!("{{\"user\":\"{user}\",\"peppered\":{peppered}}}");
        let s = sessions::create(payload.as_bytes(), ttl)
            .map_err(|e| AuthError::Capability(format!("session.create: {e:?}")))?;

        Ok(LoginResult {
            token: s.id,
            csrf: s.csrf_token,
            expires: s.expires,
        })
    }

    fn whoami(token: String) -> Result<Identity, AuthError> {
        let s = match sessions::get(&token) {
            Ok(s) => s,
            Err(sessions::SessionError::NotFound) => return Err(AuthError::NoSession),
            Err(e) => return Err(AuthError::Capability(format!("session.get: {e:?}"))),
        };
        // Recover the username from the payload we stored at login. Keep it
        // dependency-free: a tiny hand parse of the known shape.
        let body = String::from_utf8(s.data).unwrap_or_default();
        let user = body
            .split_once("\"user\":\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(u, _)| u.to_string())
            .unwrap_or_default();
        Ok(Identity {
            user,
            expires: s.expires,
        })
    }

    fn logout(token: String) -> Result<(), AuthError> {
        sessions::revoke(&token).map_err(|e| AuthError::Capability(format!("session.revoke: {e:?}")))
    }
}

bindings::export!(Component with_types_in bindings);
