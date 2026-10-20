//! `github-forge` — `git:forge` over GitHub's REST API.

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

use bindings::comp::secrets::reader as secrets;
use bindings::exports::git::forge::repo::{FileChange, ForgeError, Guest, Opened, Proposal};
use bindings::wasi::config::store as config;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};

struct Component;

struct Api {
    scheme: Scheme,
    authority: String,
    /// So a GitHub Enterprise install under `/api/v3` works.
    prefix: String,
    repo: String,
    token: String,
}

fn cfg(key: &str, default: &str) -> String {
    config::get(key).ok().flatten().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
}

impl Api {
    fn open() -> Result<Self, ForgeError> {
        let repo = cfg("forge-repo", "");
        // GitHub's 404 for a malformed path reads like "the repository does not exist".
        if repo.split('/').filter(|s| !s.is_empty()).count() != 2 {
            return Err(ForgeError::NotConfigured(format!(
                "forge-repo must be \"owner/name\", got {repo:?}"
            )));
        }
        let url = cfg("forge-api", "https://api.github.com");
        let (scheme, rest) = match url.split_once("://") {
            Some(("https", r)) => (Scheme::Https, r),
            Some(("http", r)) => (Scheme::Http, r),
            _ => {
                return Err(ForgeError::NotConfigured(format!(
                    "forge-api must start with http:// or https://, got {url:?}"
                )))
            }
        };
        let (authority, prefix) = match rest.split_once('/') {
            Some((a, p)) => (a.to_string(), format!("/{}", p.trim_end_matches('/'))),
            None => (rest.to_string(), String::new()),
        };
        let token = match secrets::get("forge-token") {
            Ok(Some(s)) => secrets::reveal(&s).unwrap_or_default(),
            _ => String::new(),
        };
        if token.is_empty() {
            // Never optional: there is no anonymous push.
            return Err(ForgeError::NotConfigured(
                "no `forge-token` secret — a forge cannot write anonymously".into(),
            ));
        }
        Ok(Self { scheme, authority, prefix, repo, token })
    }

    fn path(&self, tail: &str) -> String {
        format!("{}/repos/{}{tail}", self.prefix, self.repo)
    }
}

fn call(
    api: &Api,
    method: Method,
    path: &str,
    body: Option<String>,
) -> Result<(u16, String), ForgeError> {
    let headers = Fields::new();
    let set = |k: &str, v: &str| {
        let _ = headers.set(k, &[v.as_bytes().to_vec()]);
    };
    set("accept", "application/vnd.github+json");
    set("authorization", &format!("Bearer {}", api.token));
    // GitHub rejects requests without a user-agent and pins behaviour to an API version.
    set("user-agent", "comp-github-forge");
    set("x-github-api-version", "2022-11-28");
    if body.is_some() {
        set("content-type", "application/json");
    }

    let req = OutgoingRequest::new(headers);
    let net = |m: String| ForgeError::Unavailable(m);
    req.set_method(&method).map_err(|_| net("set method".into()))?;
    req.set_scheme(Some(&api.scheme)).map_err(|_| net("set scheme".into()))?;
    req.set_authority(Some(&api.authority)).map_err(|_| net("set authority".into()))?;
    req.set_path_with_query(Some(path)).map_err(|_| net("set path".into()))?;

    let out = req.body().map_err(|_| net("no request body".into()))?;
    {
        if let Some(b) = &body {
            let stream = out.write().map_err(|_| net("no request stream".into()))?;
            // blocking_write_and_flush caps at 4096 bytes a call.
            for chunk in b.as_bytes().chunks(4096) {
                stream
                    .blocking_write_and_flush(chunk)
                    .map_err(|e| net(format!("writing the body: {e:?}")))?;
            }
        }
    }
    OutgoingBody::finish(out, None).map_err(|_| net("finishing the body".into()))?;

    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(15_000_000_000));
    let _ = opts.set_first_byte_timeout(Some(60_000_000_000));

    let fut = bindings::wasi::http::outgoing_handler::handle(req, Some(opts))
        .map_err(|e| net(format!("sending: {e:?}")))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| net("no response".into()))?
        .map_err(|_| net("response already taken".into()))?
        .map_err(|e| net(format!("connecting: {e:?}")))?;

    let status = resp.status();
    let incoming = resp.consume().map_err(|_| net("no response body".into()))?;
    let stream = incoming.stream().map_err(|_| net("no response stream".into()))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // A failed read is not the end: truncated bytes turn half a reply into a wrong whole one.
            Err(e) => return Err(net(format!("reading the response: {e:?}"))),
        }
    }
    Ok((status, String::from_utf8_lossy(&buf).into_owned()))
}

fn status_error(what: &str, status: u16, body: &str) -> ForgeError {
    let snippet: String = body.chars().take(400).collect();
    match status {
        401 | 403 => {
            ForgeError::Rejected(format!("{what}: {status} — the token was refused: {snippet}"))
        }
        404 => ForgeError::Rejected(format!(
            "{what}: 404 — the repository, or the base branch, is not there (a token \
             without access reads as 404 here, not 403): {snippet}"
        )),
        // 422 on a ref create means it exists; GitHub says the same for an already-open PR.
        409 | 422 => ForgeError::Conflict(format!("{what}: {status}: {snippet}")),
        _ => ForgeError::Unavailable(format!("{what}: {status}: {snippet}")),
    }
}

fn ok_json(what: &str, r: (u16, String)) -> Result<serde_json::Value, ForgeError> {
    let (status, body) = r;
    if !(200..300).contains(&status) {
        return Err(status_error(what, status, &body));
    }
    serde_json::from_str(&body)
        .map_err(|e| ForgeError::Unavailable(format!("{what}: unreadable answer: {e}")))
}

fn sha_of(what: &str, v: &serde_json::Value) -> Result<String, ForgeError> {
    v["sha"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ForgeError::Unavailable(format!("{what}: no sha in the answer: {v}")))
}

/// Checked here; otherwise the failure is a 422 from the ref create, five calls in.
fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('/')
        && !name.starts_with('-')
        && !name.ends_with('/')
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
}

/// Inside the repository: "GitHub would likely refuse" is not a boundary.
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.split('/').any(|seg| seg.is_empty() || seg == ".." || seg == ".")
}

impl Component {
    fn base_sha(api: &Api, base: &str) -> Result<String, ForgeError> {
        let base = if base.is_empty() { cfg("forge-base", "main") } else { base.to_string() };
        let v = ok_json(
            "reading the base ref",
            call(api, Method::Get, &api.path(&format!("/git/ref/heads/{base}")), None)?,
        )?;
        v["object"]["sha"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| ForgeError::Unavailable(format!("no sha for base {base:?}: {v}")))
    }
}

impl Guest for Component {
    fn base_commit(base: String) -> Result<String, ForgeError> {
        let api = Api::open()?;
        Component::base_sha(&api, &base)
    }

    fn propose(p: Proposal) -> Result<Opened, ForgeError> {
        let api = Api::open()?;

        if !valid_branch(&p.branch) {
            return Err(ForgeError::Rejected(format!("not a usable branch name: {:?}", p.branch)));
        }
        if p.changes.is_empty() {
            // A diff-less PR is an agent that reported success having done nothing (ADR-0081).
            return Err(ForgeError::Rejected("no changes — there is nothing to propose".into()));
        }
        if let Some(bad) = p.changes.iter().find(|c| !valid_path(&c.path)) {
            return Err(ForgeError::Rejected(format!(
                "path escapes the repository: {:?}",
                bad.path
            )));
        }

        let base_sha = Component::base_sha(&api, &p.base)?;

        // Base64 so a non-UTF-8 file or a lone `"` survives the JSON.
        let mut entries = Vec::with_capacity(p.changes.len());
        for FileChange { path, content } in &p.changes {
            let body = serde_json::json!({
                "content": B64.encode(content.as_bytes()),
                "encoding": "base64"
            })
            .to_string();
            let v = ok_json(
                &format!("writing {path}"),
                call(&api, Method::Post, &api.path("/git/blobs"), Some(body))?,
            )?;
            entries.push(serde_json::json!({
                "path": path,
                // Regular non-executable; guessing the mode from the path would be worse.
                "mode": "100644",
                "type": "blob",
                "sha": sha_of("blob", &v)?,
            }));
        }

        // Over the base tree, or this commit deletes every untouched file.
        let tree = ok_json(
            "writing the tree",
            call(
                &api,
                Method::Post,
                &api.path("/git/trees"),
                Some(serde_json::json!({ "base_tree": base_sha, "tree": entries }).to_string()),
            )?,
        )?;

        let commit = ok_json(
            "writing the commit",
            call(
                &api,
                Method::Post,
                &api.path("/git/commits"),
                Some(
                    serde_json::json!({
                        "message": p.message,
                        "tree": sha_of("tree", &tree)?,
                        "parents": [base_sha],
                    })
                    .to_string(),
                ),
            )?,
        )?;
        let commit_sha = sha_of("commit", &commit)?;

        // The branch last (see the module doc).
        ok_json(
            "creating the branch",
            call(
                &api,
                Method::Post,
                &api.path("/git/refs"),
                Some(
                    serde_json::json!({
                        "ref": format!("refs/heads/{}", p.branch),
                        "sha": commit_sha,
                    })
                    .to_string(),
                ),
            )?,
        )?;

        let base = if p.base.is_empty() { cfg("forge-base", "main") } else { p.base.clone() };
        let pr = ok_json(
            "opening the pull request",
            call(
                &api,
                Method::Post,
                &api.path("/pulls"),
                Some(
                    serde_json::json!({
                        "title": p.title,
                        "body": p.body,
                        "head": p.branch,
                        "base": base,
                    })
                    .to_string(),
                ),
            )?,
        )?;

        Ok(Opened {
            number: pr["number"].as_u64().unwrap_or(0) as u32,
            url: pr["html_url"].as_str().unwrap_or_default().to_string(),
            commit: commit_sha,
            branch: p.branch,
        })
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_name_git_would_refuse_is_refused_here() {
        for ok in ["swarm/attempt-3", "fix_the_thing", "v1.2-rc"] {
            assert!(valid_branch(ok), "{ok} is a legal branch name");
        }
        for bad in [
            "",
            "/leading",
            "-leading",
            "trailing/",
            "has..dots",
            "double//slash",
            "ref@{0}",
            "thing.lock",
            "has space",
            "has~tilde",
        ] {
            assert!(!valid_branch(bad), "{bad:?} must be refused before five calls are spent");
        }
    }

    #[test]
    fn a_path_cannot_leave_the_repository() {
        for ok in ["src/lib.rs", "a/b/c.txt", ".comp/goals/x.md"] {
            assert!(valid_path(ok), "{ok} is inside the repo");
        }
        for bad in ["", "/etc/passwd", "../outside", "a/../../b", "a//b", "./a"] {
            assert!(!valid_path(bad), "{bad:?} must not be written");
        }
    }

    /// GitHub answers 404 for a token that cannot see the repo, so the message must say so.
    #[test]
    fn a_404_says_it_might_be_permissions() {
        match status_error("reading the base ref", 404, "{}") {
            ForgeError::Rejected(m) => assert!(m.contains("404") && m.contains("not 403")),
            other => panic!("404 should be a rejection: {other:?}"),
        }
    }

    #[test]
    fn an_existing_branch_is_a_conflict_not_a_rejection() {
        assert!(matches!(
            status_error("creating the branch", 422, "Reference already exists"),
            ForgeError::Conflict(_)
        ));
    }
}
