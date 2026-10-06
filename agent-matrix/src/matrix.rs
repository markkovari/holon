//! A thin Matrix client for an application service. Everything here acts as a
//! user in the appservice's namespace (the `user_id` query parameter), except
//! `join_as_owner`, which uses the owner's own session token.

use std::time::Duration;

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct MxErr {
    pub status: u16,
    pub errcode: String,
    pub msg: String,
}

impl std::fmt::Display for MxErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status, self.errcode, self.msg)
    }
}

impl From<reqwest::Error> for MxErr {
    fn from(e: reqwest::Error) -> Self {
        MxErr { status: 0, errcode: "TRANSPORT".into(), msg: e.to_string() }
    }
}

pub type R<T> = Result<T, MxErr>;

pub struct Matrix {
    base: String,
    as_token: String,
    admin_token: String,
    http: reqwest::blocking::Client,
}

fn enc(s: &str) -> String {
    // percent-encode everything but unreserved characters (room ids contain `!` and `:`)
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Matrix {
    pub fn new(base: &str, as_token: &str, admin_token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            as_token: as_token.to_string(),
            admin_token: admin_token.to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("http client"),
        }
    }

    fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        as_user: Option<&str>,
        token: &str,
        body: Option<Value>,
    ) -> R<Value> {
        let mut url = format!("{}{path}", self.base);
        if let Some(u) = as_user {
            url.push(if path.contains('?') { '&' } else { '?' });
            url.push_str(&format!("user_id={}", enc(u)));
        }
        let mut req = self.http.request(method, &url).bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send()?;
        let status = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            Ok(v)
        } else {
            Err(MxErr {
                status,
                errcode: v["errcode"].as_str().unwrap_or("").to_string(),
                msg: v["error"].as_str().map(String::from).unwrap_or(text),
            })
        }
    }

    fn as_call(
        &self,
        method: reqwest::Method,
        path: &str,
        user: &str,
        body: Option<Value>,
    ) -> R<Value> {
        self.call(method, path, Some(user), &self.as_token, body)
    }

    fn txn() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        format!(
            "holon-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            N.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Registers a user in the appservice's namespace. Already existing is fine.
    pub fn register(&self, localpart: &str) -> R<()> {
        match self.call(
            reqwest::Method::POST,
            "/_matrix/client/v3/register",
            None,
            &self.as_token,
            Some(json!({"type": "m.login.application_service", "username": localpart})),
        ) {
            Ok(_) => Ok(()),
            Err(e) if e.errcode == "M_USER_IN_USE" => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn set_displayname(&self, user: &str, name: &str) -> R<()> {
        self.as_call(
            reqwest::Method::PUT,
            &format!("/_matrix/client/v3/profile/{}/displayname", enc(user)),
            user,
            Some(json!({"displayname": name})),
        )
        .map(|_| ())
    }

    pub fn create_room(&self, as_user: &str, body: Value) -> R<String> {
        let v = self.as_call(
            reqwest::Method::POST,
            "/_matrix/client/v3/createRoom",
            as_user,
            Some(body),
        )?;
        v["room_id"].as_str().map(String::from).ok_or(MxErr {
            status: 200,
            errcode: "NO_ROOM_ID".into(),
            msg: v.to_string(),
        })
    }

    pub fn invite(&self, room: &str, by: &str, target: &str) -> R<()> {
        match self.as_call(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/invite", enc(room)),
            by,
            Some(json!({"user_id": target})),
        ) {
            Ok(_) => Ok(()),
            // already in or already invited
            Err(e)
                if e.msg.contains("already in the room") || e.msg.contains("is already joined") =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub fn join(&self, room: &str, user: &str) -> R<()> {
        self.as_call(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/join/{}", enc(room)),
            user,
            Some(json!({})),
        )
        .map(|_| ())
    }

    pub fn leave(&self, room: &str, user: &str) -> R<()> {
        self.as_call(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/leave", enc(room)),
            user,
            Some(json!({})),
        )
        .map(|_| ())
    }

    pub fn kick(&self, room: &str, by: &str, target: &str, reason: &str) -> R<()> {
        self.as_call(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/kick", enc(room)),
            by,
            Some(json!({"user_id": target, "reason": reason})),
        )
        .map(|_| ())
    }

    pub fn send_event(
        &self,
        room: &str,
        user: &str,
        event_type: &str,
        content: Value,
    ) -> R<String> {
        let v = self.as_call(
            reqwest::Method::PUT,
            &format!(
                "/_matrix/client/v3/rooms/{}/send/{}/{}",
                enc(room),
                enc(event_type),
                Self::txn()
            ),
            user,
            Some(content),
        )?;
        Ok(v["event_id"].as_str().unwrap_or_default().to_string())
    }

    pub fn send_text(
        &self,
        room: &str,
        user: &str,
        body: &str,
        reply_to: Option<&str>,
    ) -> R<String> {
        let mut c = json!({"msgtype": "m.text", "body": body});
        if let Some(e) = reply_to {
            c["m.relates_to"] = json!({"m.in_reply_to": {"event_id": e}});
        }
        self.send_event(room, user, "m.room.message", c)
    }

    pub fn set_state(
        &self,
        room: &str,
        user: &str,
        event_type: &str,
        state_key: &str,
        content: Value,
    ) -> R<()> {
        self.as_call(
            reqwest::Method::PUT,
            &format!(
                "/_matrix/client/v3/rooms/{}/state/{}/{}",
                enc(room),
                enc(event_type),
                enc(state_key)
            ),
            user,
            Some(content),
        )
        .map(|_| ())
    }

    pub fn typing(&self, room: &str, user: &str, on: bool) {
        let _ = self.as_call(
            reqwest::Method::PUT,
            &format!("/_matrix/client/v3/rooms/{}/typing/{}", enc(room), enc(user)),
            user,
            Some(if on {
                json!({"typing": true, "timeout": 120000})
            } else {
                json!({"typing": false})
            }),
        );
    }

    /// Users currently joined to the room.
    pub fn joined(&self, room: &str, as_user: &str) -> R<Vec<String>> {
        let v = self.as_call(
            reqwest::Method::GET,
            &format!("/_matrix/client/v3/rooms/{}/joined_members", enc(room)),
            as_user,
            None,
        )?;
        Ok(v["joined"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default())
    }

    /// A room's full state, as seen by `as_user` (who must be in it).
    pub fn room_state(&self, room: &str, as_user: &str) -> R<Vec<Value>> {
        let v = self.as_call(
            reqwest::Method::GET,
            &format!("/_matrix/client/v3/rooms/{}/state", enc(room)),
            as_user,
            None,
        )?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// The same, as the owner (for rooms the owner made and no agent is in yet).
    pub fn room_state_as_owner(&self, room: &str) -> R<Vec<Value>> {
        let v = self.call(
            reqwest::Method::GET,
            &format!("/_matrix/client/v3/rooms/{}/state", enc(room)),
            None,
            &self.admin_token,
            None,
        )?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    pub fn joined_as_owner(&self, room: &str) -> R<Vec<String>> {
        let v = self.call(
            reqwest::Method::GET,
            &format!("/_matrix/client/v3/rooms/{}/joined_members", enc(room)),
            None,
            &self.admin_token,
            None,
        )?;
        Ok(v["joined"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default())
    }

    /// Invites `target` to a room the OWNER is in (a Space or room they made in their
    /// own client), as the owner. The bridge is not a member of those rooms, so this
    /// is how agents get into them.
    pub fn invite_as_owner(&self, room: &str, target: &str) -> R<()> {
        match self.call(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/invite", enc(room)),
            None,
            &self.admin_token,
            Some(json!({"user_id": target})),
        ) {
            Ok(_) => Ok(()),
            Err(e)
                if e.msg.contains("already in the room") || e.msg.contains("is already joined") =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub fn kick_as_owner(&self, room: &str, target: &str, reason: &str) -> R<()> {
        self.call(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/kick", enc(room)),
            None,
            &self.admin_token,
            Some(json!({"user_id": target, "reason": reason})),
        )
        .map(|_| ())
    }

    /// Downloads media by `mxc://server/id` (authenticated media, as the bridge).
    pub fn download(&self, mxc: &str, as_user: &str) -> R<Vec<u8>> {
        let rest = mxc.strip_prefix("mxc://").ok_or(MxErr {
            status: 0,
            errcode: "BAD_MXC".into(),
            msg: format!("not an mxc uri: {mxc}"),
        })?;
        let url = format!(
            "{}/_matrix/client/v1/media/download/{rest}?user_id={}",
            self.base,
            enc(as_user)
        );
        let r = self.http.get(url).bearer_auth(&self.as_token).send()?;
        let status = r.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(MxErr {
                status,
                errcode: String::new(),
                msg: r.text().unwrap_or_default(),
            });
        }
        Ok(r.bytes()?.to_vec())
    }

    /// Uploads media as `as_user`; returns its `mxc://` uri.
    pub fn upload(
        &self,
        bytes: Vec<u8>,
        content_type: &str,
        filename: &str,
        as_user: &str,
    ) -> R<String> {
        let url = format!(
            "{}/_matrix/media/v3/upload?filename={}&user_id={}",
            self.base,
            enc(filename),
            enc(as_user)
        );
        let r = self
            .http
            .post(url)
            .bearer_auth(&self.as_token)
            .header("content-type", content_type)
            .body(bytes)
            .send()?;
        let status = r.status().as_u16();
        let v: Value = r.json().unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            v["content_uri"].as_str().map(String::from).ok_or(MxErr {
                status,
                errcode: "NO_URI".into(),
                msg: v.to_string(),
            })
        } else {
            Err(MxErr {
                status,
                errcode: v["errcode"].as_str().unwrap_or("").into(),
                msg: v["error"].as_str().unwrap_or("upload failed").into(),
            })
        }
    }

    /// Accepts the owner's pending invite to `room`, using the owner's own session.
    /// Every room the bridge makes invites the owner at creation; this is what turns
    /// "the agent made you a room" into a room that is already in your list, with no
    /// invite to find and accept. (Synapse's admin-join API was tried first and
    /// refuses invite-only rooms it has no member to invite from.)
    pub fn join_as_owner(&self, room: &str) -> R<()> {
        self.call(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/join/{}", enc(room)),
            None,
            &self.admin_token,
            Some(json!({})),
        )
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::enc;

    #[test]
    fn room_and_user_ids_are_percent_encoded() {
        assert_eq!(enc("!abc:server.net"), "%21abc%3Aserver.net");
        assert_eq!(enc("@agent-x:s"), "%40agent-x%3As");
        assert_eq!(enc("m.room.message"), "m.room.message");
        assert_eq!(enc("$ev/ent"), "%24ev%2Fent");
    }
}
