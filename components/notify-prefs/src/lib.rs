//! `notify-prefs` — read what a subject asked for, then tell them on those channels.
//!
//! Fan-out never fails as a whole: `notify` returns one outcome per channel it ATTEMPTED, and attempts every channel the
//! subject asked for even after one fails (a down email gateway must not lose the in-app copy already written). A caller
//! getting `[{in-app, ok}, {email, not ok, "gateway 503"}]` knows exactly what to retry, which a single `Err` cannot say.
//!
//! Absent is different from empty: an override with an empty channel list means "not this kind"; no override means "use the
//! defaults". Collapsing them would make it impossible to mute one kind without muting everything.

#[allow(warnings)]
mod bindings;

use bindings::exports::notify::prefs::preferences::{
    Channel, Guest, Outcome, Preference, PrefsError,
};
use bindings::mail::send::sender as mail;
use bindings::notify::inbox::inbox;
use bindings::records::store::store as records;
use serde_json::json;

const COLLECTION: &str = "notify-prefs";

struct Component;

fn back(ctx: &str) -> PrefsError {
    PrefsError::BackendUnavailable(ctx.to_string())
}

fn channel_name(c: Channel) -> &'static str {
    match c {
        Channel::InApp => "in-app",
        Channel::Email => "email",
    }
}

fn channel_of(s: &str) -> Option<Channel> {
    match s {
        "in-app" => Some(Channel::InApp),
        "email" => Some(Channel::Email),
        _ => None,
    }
}

fn channels_from(v: &serde_json::Value) -> Vec<Channel> {
    v.as_array()
        .map(|a| a.iter().filter_map(|c| c.as_str().and_then(channel_of)).collect())
        .unwrap_or_default()
}

/// `find_by` indexes the SERIALISED value, so a subject is stored under `"ada"` (quotes included). Getting this wrong returns
/// `Ok(vec![])`, indistinguishable from a subject with no preference, so every notification would silently use the defaults.
fn stored(subject: &str) -> Option<records::Entry> {
    let encoded = serde_json::to_string(subject).ok()?;
    records::find_by(COLLECTION, "subject", &encoded).ok()?.into_iter().next()
}

fn to_preference(subject: &str, doc: &serde_json::Value) -> Preference {
    Preference {
        subject: subject.to_string(),
        default_channels: channels_from(&doc["default_channels"]),
        overrides: doc["overrides"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), channels_from(v))).collect())
            .unwrap_or_default(),
        email_address: doc["email_address"].as_str().unwrap_or_default().to_string(),
    }
}

impl Guest for Component {
    fn get(subject: String) -> Result<Preference, PrefsError> {
        if subject.is_empty() {
            return Err(PrefsError::Invalid("subject is empty".into()));
        }
        match stored(&subject) {
            Some(e) => {
                let doc: serde_json::Value =
                    serde_json::from_str(&e.data).unwrap_or_else(|_| json!({}));
                Ok(to_preference(&subject, &doc))
            }
            // Never set is not an error: in-app only, no address, the default that cannot deliver anywhere it should not.
            None => Ok(Preference {
                subject,
                default_channels: vec![Channel::InApp],
                overrides: Vec::new(),
                email_address: String::new(),
            }),
        }
    }

    fn put(pref: Preference) -> Result<(), PrefsError> {
        if pref.subject.is_empty() {
            return Err(PrefsError::Invalid("subject is empty".into()));
        }
        if pref.email_address.contains('@') || pref.email_address.is_empty() {
        } else {
            return Err(PrefsError::Invalid(format!("not an address: {}", pref.email_address)));
        }
        let mut overrides = serde_json::Map::new();
        for (kind, chans) in &pref.overrides {
            overrides.insert(
                kind.clone(),
                json!(chans.iter().map(|c| channel_name(*c)).collect::<Vec<_>>()),
            );
        }
        let doc = json!({
            "subject": pref.subject,
            "default_channels": pref.default_channels.iter().map(|c| channel_name(*c)).collect::<Vec<_>>(),
            "overrides": overrides,
            "email_address": pref.email_address,
        })
        .to_string();

        match stored(&pref.subject) {
            Some(e) => records::update(COLLECTION, &e.id, &doc, e.revision)
                .map(|_| ())
                .map_err(|_| back("update")),
            None => records::create(COLLECTION, &doc, &["subject".to_string()])
                .map(|_| ())
                .map_err(|_| back("create")),
        }
    }

    fn notify(
        subject: String,
        kind: String,
        title: String,
        body: String,
        payload: String,
    ) -> Result<Vec<Outcome>, PrefsError> {
        let pref = Self::get(subject.clone())?;
        // An override wins, INCLUDING an empty one: that is how a kind is muted.
        let wanted: Vec<Channel> = pref
            .overrides
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, c)| c.clone())
            .unwrap_or(pref.default_channels);

        let mut out = Vec::new();
        for channel in wanted {
            let outcome = match channel {
                Channel::InApp => match inbox::deliver(&subject, &kind, &title, &body, &payload) {
                    Ok(seq) => Outcome { channel, ok: true, detail: seq.to_string() },
                    Err(e) => Outcome { channel, ok: false, detail: format!("{e:?}") },
                },
                Channel::Email => {
                    if pref.email_address.is_empty() {
                        // Reported, not silently skipped: somebody opted into email and is not getting any, and this is the only place to notice.
                        Outcome {
                            channel,
                            ok: false,
                            detail: "opted into email with no address set".into(),
                        }
                    } else {
                        let msg = mail::Email {
                            to: pref.email_address.clone(),
                            subject: title.clone(),
                            body: body.clone(),
                        };
                        match mail::send(&msg) {
                            Ok(id) => Outcome { channel, ok: true, detail: id },
                            Err(e) => Outcome { channel, ok: false, detail: format!("{e:?}") },
                        }
                    }
                }
            };
            out.push(outcome);
        }
        Ok(out)
    }
}

bindings::export!(Component with_types_in bindings);
