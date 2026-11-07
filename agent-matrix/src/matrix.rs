//! Acts as a namespace user via `user_id`; the `*_as_owner` calls use the owner's session.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::http::{bearer, enc, Http};

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

impl From<String> for MxErr {
    fn from(msg: String) -> Self {
        MxErr { status: 0, errcode: "TRANSPORT".into(), msg }
    }
}

pub type R<T> = Result<T, MxErr>;

pub struct Matrix {
    base: String,
    as_token: String,
    admin_token: String,
    http: Arc<dyn Http>,
}

impl Matrix {
    pub fn new(http: Arc<dyn Http>, base: &str, as_token: &str, admin_token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            as_token: as_token.to_string(),
            admin_token: admin_token.to_string(),
            http,
        }
    }

    fn call(
        &self,
        method: &str,
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
        let auth = bearer(token);
        let mut headers = vec![("authorization", auth.as_str())];
        let body = body.map(|b| b.to_string());
        if body.is_some() {
            headers.push(("content-type", "application/json"));
        }
        let resp = self.http.send(method, &url, &headers, body.as_deref().map(str::as_bytes))?;
        let status = resp.status;
        let text = resp.text();
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

    fn as_call(&self, method: &str, path: &str, user: &str, body: Option<Value>) -> R<Value> {
        self.call(method, path, Some(user), &self.as_token, body)
    }

    fn txn() -> String {
        use std::hash::BuildHasher;
        // Random, not a counter: each component instance would start its counter at 0 again.
        format!(
            "holon-{}-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            std::collections::hash_map::RandomState::new().hash_one(0u8)
        )
    }

    pub fn register(&self, localpart: &str) -> R<()> {
        match self.call(
            "POST",
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
            "PUT",
            &format!("/_matrix/client/v3/profile/{}/displayname", enc(user)),
            user,
            Some(json!({"displayname": name})),
        )
        .map(|_| ())
    }

    pub fn create_room(&self, as_user: &str, body: Value) -> R<String> {
        let v = self.as_call("POST", "/_matrix/client/v3/createRoom", as_user, Some(body))?;
        v["room_id"].as_str().map(String::from).ok_or(MxErr {
            status: 200,
            errcode: "NO_ROOM_ID".into(),
            msg: v.to_string(),
        })
    }

    pub fn invite(&self, room: &str, by: &str, target: &str) -> R<()> {
        match self.as_call(
            "POST",
            &format!("/_matrix/client/v3/rooms/{}/invite", enc(room)),
            by,
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

    pub fn join(&self, room: &str, user: &str) -> R<()> {
        self.as_call(
            "POST",
            &format!("/_matrix/client/v3/join/{}", enc(room)),
            user,
            Some(json!({})),
        )
        .map(|_| ())
    }

    pub fn leave(&self, room: &str, user: &str) -> R<()> {
        self.as_call(
            "POST",
            &format!("/_matrix/client/v3/rooms/{}/leave", enc(room)),
            user,
            Some(json!({})),
        )
        .map(|_| ())
    }

    pub fn kick(&self, room: &str, by: &str, target: &str, reason: &str) -> R<()> {
        self.as_call(
            "POST",
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
            "PUT",
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

    pub fn send_markdown(
        &self,
        room: &str,
        user: &str,
        body: &str,
        reply_to: Option<&str>,
    ) -> R<String> {
        self.send_markdown_threaded(room, user, body, None, reply_to)
    }

    pub fn send_markdown_threaded(
        &self,
        room: &str,
        user: &str,
        body: &str,
        thread_id: Option<&str>,
        reply_to: Option<&str>,
    ) -> R<String> {
        let mut c = json!({"msgtype": "m.text", "body": body});
        if let Some(html) = crate::md::to_html(body) {
            c["format"] = json!("org.matrix.custom.html");
            c["formatted_body"] = json!(html);
        }
        if let Some(t) = thread_id {
            let mut relates = json!({
                "rel_type": "m.thread",
                "event_id": t,
                "is_falling_back": true,
            });
            if let Some(e) = reply_to {
                relates["m.in_reply_to"] = json!({"event_id": e});
            }
            c["m.relates_to"] = relates;
        } else if let Some(e) = reply_to {
            c["m.relates_to"] = json!({"m.in_reply_to": {"event_id": e}});
        }
        self.send_event(room, user, "m.room.message", c)
    }

    /// Edits an existing message event (Matrix MSC2676 replacement).
    pub fn edit_markdown(
        &self,
        room: &str,
        user: &str,
        original_event_id: &str,
        body: &str,
    ) -> R<String> {
        let mut new_content = json!({"msgtype": "m.text", "body": body});
        if let Some(html) = crate::md::to_html(body) {
            new_content["format"] = json!("org.matrix.custom.html");
            new_content["formatted_body"] = json!(html);
        }
        let c = json!({
            "msgtype": "m.text",
            "body": format!("* {body}"),
            "m.new_content": new_content,
            "m.relates_to": {
                "rel_type": "m.replace",
                "event_id": original_event_id,
            }
        });
        self.send_event(room, user, "m.room.message", c)
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
            "PUT",
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
            "PUT",
            &format!("/_matrix/client/v3/rooms/{}/typing/{}", enc(room), enc(user)),
            user,
            Some(if on {
                json!({"typing": true, "timeout": 120000})
            } else {
                json!({"typing": false})
            }),
        );
    }

    pub fn joined(&self, room: &str, as_user: &str) -> R<Vec<String>> {
        let v = self.as_call(
            "GET",
            &format!("/_matrix/client/v3/rooms/{}/joined_members", enc(room)),
            as_user,
            None,
        )?;
        Ok(v["joined"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default())
    }

    pub fn room_state(&self, room: &str, as_user: &str) -> R<Vec<Value>> {
        let v = self.as_call(
            "GET",
            &format!("/_matrix/client/v3/rooms/{}/state", enc(room)),
            as_user,
            None,
        )?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// For rooms the owner made that no agent is in yet.
    pub fn room_state_as_owner(&self, room: &str) -> R<Vec<Value>> {
        let v = self.call(
            "GET",
            &format!("/_matrix/client/v3/rooms/{}/state", enc(room)),
            None,
            &self.admin_token,
            None,
        )?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// The owner is in every room the bridge manages.
    pub fn messages_as_owner(&self, room: &str, limit: usize) -> R<Vec<Value>> {
        let filter = enc(&json!({"types": ["m.room.message"]}).to_string());
        let v = self.call(
            "GET",
            &format!(
                "/_matrix/client/v3/rooms/{}/messages?dir=b&limit={limit}&filter={filter}",
                enc(room)
            ),
            None,
            &self.admin_token,
            None,
        )?;
        Ok(v["chunk"].as_array().cloned().unwrap_or_default())
    }

    pub fn joined_as_owner(&self, room: &str) -> R<Vec<String>> {
        let v = self.call(
            "GET",
            &format!("/_matrix/client/v3/rooms/{}/joined_members", enc(room)),
            None,
            &self.admin_token,
            None,
        )?;
        Ok(v["joined"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default())
    }

    /// For rooms the owner made: the bridge is not a member of those.
    pub fn invite_as_owner(&self, room: &str, target: &str) -> R<()> {
        match self.call(
            "POST",
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
            "POST",
            &format!("/_matrix/client/v3/rooms/{}/kick", enc(room)),
            None,
            &self.admin_token,
            Some(json!({"user_id": target, "reason": reason})),
        )
        .map(|_| ())
    }

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
        let auth = bearer(&self.as_token);
        let r = self.http.send("GET", &url, &[("authorization", &auth)], None)?;
        if !r.ok() {
            return Err(MxErr { status: r.status, errcode: String::new(), msg: r.text() });
        }
        Ok(r.body)
    }

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
        let auth = bearer(&self.as_token);
        let headers = [("authorization", auth.as_str()), ("content-type", content_type)];
        let r = self.http.send("POST", &url, &headers, Some(&bytes))?;
        let status = r.status;
        let v = r.json();
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

    /// History stays: an agent's user still in the room can invite the owner back.
    pub fn leave_and_forget_as_owner(&self, room: &str) -> R<()> {
        for verb in ["leave", "forget"] {
            self.call(
                "POST",
                &format!("/_matrix/client/v3/rooms/{}/{verb}", enc(room)),
                None,
                &self.admin_token,
                Some(json!({})),
            )?;
        }
        Ok(())
    }

    /// Synapse's admin-join API refuses invite-only rooms it has no member to invite from.
    pub fn join_as_owner(&self, room: &str) -> R<()> {
        self.call(
            "POST",
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
