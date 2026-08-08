//! Organisations: who owns what, when a person can belong to several.
//!
//! `auth-guard` gives one identity with ONE tenant and roles scoped to it
//! (docs/adr/0009). That is right for authentication and wrong for ownership: a
//! person contracting for three companies has one identity and three sets of
//! things they can touch. So orgs sit above it — auth-guard still answers "who are
//! you", and this answers "on whose behalf, right now".
//!
//! **An org is the isolation unit.** Everything that used to be keyed by tenant —
//! the deployment record, the catalogue row, and critically the storage bucket a
//! running instance gets — is keyed by org id. That keeps ADR-0012's property
//! intact with a wider unit: two orgs cannot see each other's data for exactly the
//! same reason two tenants could not, because the host still names the bucket from
//! a control-plane record the guest cannot write.
//!
//! **Everyone gets a solo org on registration**, named after their personal tenant.
//! Without it every existing call would need an explicit org, and a person who
//! never joins a company would have nowhere to deploy. It also means "org" is never
//! an optional concept with two code paths.
//!
//! Membership is by invite code rather than by email. Sending mail is a whole
//! subsystem and this needs none of it: an owner mints a code, hands it over
//! however they like, and the holder redeems it.
//! ponytail: opaque codes, no email; add invitations-by-address when there is a
//! mail path to put them on.

use serde_json::{json, Map, Value};

use crate::bindings::records::store::store as records;

pub const ORGS: &str = "orgs";
pub const MEMBERS: &str = "members";
pub const INVITES: &str = "invites";

/// What a member may do. ADR-0009's vocabulary, unchanged.
///
/// Ordered, so a check is `role_of(..) >= Role::Member` rather than a set of
/// equality tests that has to be updated everywhere a role is added.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Role {
    /// Read the catalogue and the deployments. Changes nothing.
    Viewer,
    /// Upload components and deploy. The working role.
    Member,
    /// Everything, plus membership and deleting the org itself.
    Owner,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "member" => Some(Role::Member),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Member => "member",
            Role::Owner => "owner",
        }
    }
}

/// A DNS-label org id derived from a name. It ends up in a storage bucket name and
/// in a hostname, so it cannot be arbitrary text.
pub fn slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    s.chars().take(40).collect::<String>().trim_matches('-').to_string()
}

fn member_key(org: &str, subject: &str) -> String {
    format!("{org}/{subject}")
}

/// The caller's role in `org`, or `None` if they are not a member.
///
/// This is THE authorisation primitive. Every org-scoped handler goes through it,
/// so there is one place that decides whether someone is in an org rather than a
/// check repeated at each call site with a chance to be forgotten at one of them.
pub fn role_of(subject: &str, org: &str) -> Option<Role> {
    let key = member_key(org, subject);
    let row = crate::find_one(MEMBERS, "key", &key).map(|(_, _, v)| v)?;
    Role::parse(row["role"].as_str().unwrap_or_default())
}

/// Create an org with `subject` as its owner. Idempotent on the slug.
pub fn create(name: &str, subject: &str, email: &str) -> Result<Value, String> {
    let id = slug(name);
    if id.is_empty() {
        return Err("an organisation name must contain at least one letter or digit".into());
    }
    if crate::find_one(ORGS, "id", &id).is_some() {
        return Err(format!("`{id}` is taken"));
    }
    let doc = json!({ "id": id, "name": name, "created_by": subject, "created": crate::now() });
    records::create(ORGS, &doc.to_string(), &["id".to_string()])
        .map_err(|_| "could not create the organisation".to_string())?;
    add_member(&id, subject, email, Role::Owner)?;
    Ok(doc)
}

pub fn add_member(org: &str, subject: &str, email: &str, role: Role) -> Result<(), String> {
    let key = member_key(org, subject);
    if let Some((rec, rev, mut row)) = crate::find_one(MEMBERS, "key", &key) {
        // Re-adding is a role change, not a duplicate. A second row would make
        // `role_of` depend on which one the index returned first.
        row["role"] = json!(role.as_str());
        records::update(MEMBERS, &rec, &row.to_string(), rev)
            .map_err(|_| "could not update the membership".to_string())?;
        return Ok(());
    }
    let doc = json!({
        "key": key, "org": org, "subject": subject, "email": email,
        "role": role.as_str(), "joined": crate::now(),
    });
    records::create(MEMBERS, &doc.to_string(), &["key".to_string(), "org".to_string(), "subject".to_string()])
        .map_err(|_| "could not add the member".to_string())?;
    Ok(())
}

/// Every org this subject belongs to, with their role in each.
pub fn memberships(subject: &str) -> Vec<Value> {
    records::find_by(MEMBERS, "subject", &json!(subject).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .filter_map(|m| {
            let org = m["org"].as_str()?.to_string();
            let row = crate::find_one(ORGS, "id", &org).map(|(_, _, v)| v)?;
            Some(json!({ "id": org, "name": row["name"], "role": m["role"], "joined": m["joined"] }))
        })
        .collect()
}

pub fn members(org: &str) -> Vec<Value> {
    records::find_by(MEMBERS, "org", &json!(org).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .map(|m| json!({ "subject": m["subject"], "email": m["email"], "role": m["role"], "joined": m["joined"] }))
        .collect()
}

pub fn remove_member(org: &str, subject: &str) -> Result<(), String> {
    // The last owner cannot leave. An org with no owner can never have its
    // membership changed again, which is unrecoverable without a support ticket.
    if role_of(subject, org) == Some(Role::Owner) && owner_count(org) <= 1 {
        return Err("this is the last owner; promote someone else first".into());
    }
    let key = member_key(org, subject);
    let Some((rec, _, _)) = crate::find_one(MEMBERS, "key", &key) else {
        return Err("not a member".into());
    };
    records::delete(MEMBERS, &rec).map_err(|_| "could not remove the member".to_string())
}

fn owner_count(org: &str) -> usize {
    members(org).iter().filter(|m| m["role"] == json!("owner")).count()
}

/// Mint a join code. The code IS the credential, so it is generated from the
/// records store's own id rather than anything guessable like a counter.
pub fn invite(org: &str, role: Role, by: &str, ttl_secs: u64) -> Result<Value, String> {
    let doc = json!({
        "org": org, "role": role.as_str(), "created_by": by,
        "expires": crate::now() + ttl_secs, "code": "",
    });
    let rec = records::create(INVITES, &doc.to_string(), &["org".to_string()])
        .map_err(|_| "could not create the invite".to_string())?;
    // The record id is the code: unguessable, unique, and already stored.
    let (id, rev) = (rec.id.clone(), rec.revision);
    let mut doc = doc;
    doc["code"] = json!(id);
    let _ = records::update(INVITES, &id, &doc.to_string(), rev);
    Ok(json!({ "code": id, "org": org, "role": role.as_str(), "expires": doc["expires"] }))
}

/// Redeem a code. Single use: the invite is deleted on success, so a leaked code
/// that has already been used is worth nothing.
pub fn redeem(code: &str, subject: &str, email: &str) -> Result<Value, String> {
    let Ok(entry) = records::get(INVITES, code) else {
        return Err("no such invite".into());
    };
    let Ok(doc) = serde_json::from_str::<Value>(&entry.data) else {
        return Err("no such invite".into());
    };
    if doc["expires"].as_u64().unwrap_or(0) < crate::now() {
        let _ = records::delete(INVITES, code);
        return Err("that invite has expired".into());
    }
    let org = doc["org"].as_str().unwrap_or_default().to_string();
    let role = Role::parse(doc["role"].as_str().unwrap_or("member")).unwrap_or(Role::Member);
    add_member(&org, subject, email, role)?;
    let _ = records::delete(INVITES, code);
    Ok(json!({ "org": org, "role": role.as_str() }))
}

/// Which org a request is acting on behalf of.
///
/// `?org=` when given, otherwise the caller's personal org. Returning the role
/// alongside means a handler cannot accidentally check membership without
/// checking permission — there is only one call, and it returns both.
pub fn acting(
    subject: &str,
    personal: &str,
    query: &Map<String, Value>,
    need: Role,
) -> Result<(String, Role), (u16, String)> {
    let org = query
        .get("org")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(personal)
        .to_string();
    match role_of(subject, &org) {
        // 404 rather than 403 for a non-member: whether an org exists is itself
        // information, and an attacker enumerating names should learn nothing.
        None => Err((404, format!("no organisation `{org}` that you belong to"))),
        Some(have) if have < need => Err((
            403,
            format!("this needs {} in `{org}`; you are {}", need.as_str(), have.as_str()),
        )),
        Some(have) => Ok((org, have)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_ordered_so_a_check_is_a_comparison() {
        // The alternative is a set of equality tests at every call site, and one of
        // them being forgotten when a role is added.
        assert!(Role::Owner > Role::Member);
        assert!(Role::Member > Role::Viewer);
        assert!(Role::Viewer >= Role::Viewer);
        assert_eq!(Role::parse("owner"), Some(Role::Owner));
        assert_eq!(Role::parse("root"), None, "an unknown role must not parse to something");
    }

    #[test]
    fn a_slug_is_always_a_dns_label() {
        // It becomes a storage bucket name and part of a hostname.
        assert_eq!(slug("Acme Corp."), "acme-corp");
        assert_eq!(slug("  --Weird--  "), "weird");
        assert_eq!(slug(&"x".repeat(200)).len(), 40);
        assert_eq!(slug("!!!"), "", "a name with nothing usable yields nothing, not junk");
    }

    #[test]
    fn a_slug_cannot_smuggle_a_path_or_a_bucket_separator() {
        // It is concatenated into `b-app-<org>-<app>`; a `/` or a space there would
        // be a different bucket than the one intended.
        for hostile in ["a/b", "a b", "../x", "a\u{1f}b", "a.b"] {
            let s = slug(hostile);
            assert!(
                s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "{hostile:?} produced {s:?}"
            );
        }
    }
}
