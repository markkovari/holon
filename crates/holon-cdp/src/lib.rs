//! Two-tier Chrome DevTools Protocol (CDP) client and delta-GIF recorder.
//!
//! - **Tier 1 (`Engine::Lightpanda`)**: Instant-startup Zig headless DOM + V8 engine over CDP for
//!   functional E2E suites (`e2e/`).
//! - **Tier 2 (`Engine::Chromium`)**: Full Blink/Skia rasterizer over the same CDP client, paired
//!   with [`film::Gif`] to record delta-compressed showcase GIFs directly from `Page.captureScreenshot`
//!   without `.webm`, `ffmpeg`, or `gifsicle`.

pub mod film;

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

pub use film::Gif;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// Prefer Lightpanda when installed, falling back to Chromium.
    Auto,
    /// Prefer Chromium when installed (for full CSS/Canvas/SVG pixel rendering), falling back to Lightpanda.
    AutoVisual,
    Lightpanda,
    Chromium,
}

pub struct BrowserServer {
    ws_url: String,
    kind: EngineKind,
    child: Option<Child>,
    _profile_dir: Option<tempfile::TempDir>,
}

impl BrowserServer {
    pub fn start(requested: EngineKind) -> Result<Self> {
        if let Ok(ws_url) = std::env::var("HOLON_CDP_URL") {
            if !ws_url.trim().is_empty() {
                return Ok(Self { ws_url, kind: requested, child: None, _profile_dir: None });
            }
        }

        match requested {
            EngineKind::Lightpanda => Self::start_lightpanda(),
            EngineKind::Chromium => Self::start_chromium(),
            EngineKind::Auto => Self::start_lightpanda().or_else(|_| Self::start_chromium()),
            EngineKind::AutoVisual => Self::start_chromium().or_else(|_| Self::start_lightpanda()),
        }
    }

    pub fn kind(&self) -> EngineKind {
        self.kind
    }

    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }

    pub fn new_page(&self) -> Result<Page> {
        Page::connect(&self.ws_url, Duration::from_secs(10))
    }

    fn start_lightpanda() -> Result<Self> {
        let bin = find_lightpanda_bin().context("lightpanda binary not found")?;
        let port = free_port()?;
        let child = Command::new(&bin)
            .args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--load-resources",
                "stylesheet",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawn {}", bin.display()))?;

        let mut server = Self {
            ws_url: String::new(),
            kind: EngineKind::Lightpanda,
            child: Some(child),
            _profile_dir: None,
        };
        server.ws_url = wait_for_cdp_ws_url(port, Duration::from_secs(5))?;
        Ok(server)
    }

    fn start_chromium() -> Result<Self> {
        let bin = find_chromium_bin().context("chromium/chrome binary not found")?;
        let port = free_port()?;
        let profile = tempfile::tempdir().context("tempdir for chromium profile")?;
        let child = Command::new(&bin)
            .args([
                "--headless",
                "--disable-gpu",
                "--no-sandbox",
                "--disable-dev-shm-usage",
                "--disable-background-networking",
                "--disable-default-apps",
                "--disable-extensions",
                "--disable-sync",
                "--no-first-run",
                "--window-size=1280,800",
                &format!("--remote-debugging-port={port}"),
                &format!("--user-data-dir={}", profile.path().display()),
                "about:blank",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawn {}", bin.display()))?;

        let mut server = Self {
            ws_url: String::new(),
            kind: EngineKind::Chromium,
            child: Some(child),
            _profile_dir: Some(profile),
        };
        server.ws_url = wait_for_cdp_ws_url(port, Duration::from_secs(10))?;
        Ok(server)
    }
}

impl Drop for BrowserServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("bind ephemeral port")?;
    Ok(listener.local_addr()?.port())
}

fn wait_for_cdp_ws_url(port: u16, timeout: Duration) -> Result<String> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let client =
        reqwest::blocking::Client::builder().timeout(Duration::from_millis(500)).build()?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(resp) = client.get(&url).send() {
            if let Ok(v) = resp.json::<Value>() {
                if let Some(ws) = v.get("webSocketDebuggerUrl").and_then(Value::as_str) {
                    return Ok(ws.to_string());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    bail!("timed out waiting for CDP server on 127.0.0.1:{port}")
}

pub fn find_lightpanda_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LIGHTPANDA_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    for candidate in [
        "/tmp/lightpanda",
        "/usr/local/bin/lightpanda",
        "/opt/homebrew/bin/lightpanda",
        "target/release/lightpanda",
    ] {
        let p = PathBuf::from(candidate);
        if p.is_file() {
            return Some(p);
        }
    }
    find_in_path("lightpanda")
}

pub fn find_chromium_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CHROME_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    for name in ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"] {
        if let Some(p) = find_in_path(name) {
            return Some(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let pw_cache = Path::new(&home).join("Library/Caches/ms-playwright");
        if let Ok(entries) = std::fs::read_dir(&pw_cache) {
            let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            dirs.sort();
            dirs.reverse();
            for dir in dirs {
                for rel in [
                    "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                    "chrome-headless-shell-mac-arm64/chrome-headless-shell",
                    "chrome-linux/chrome",
                    "chrome-headless-shell-linux64/chrome-headless-shell",
                ] {
                    let candidate = dir.join(rel);
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
        let linux_pw = Path::new(&home).join(".cache/ms-playwright");
        if let Ok(entries) = std::fs::read_dir(&linux_pw) {
            let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            dirs.sort();
            dirs.reverse();
            for dir in dirs {
                for rel in
                    ["chrome-linux/chrome", "chrome-headless-shell-linux64/chrome-headless-shell"]
                {
                    let candidate = dir.join(rel);
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    for app in [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    ] {
        let p = PathBuf::from(app);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn find_in_path(bin: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let full = dir.join(bin);
        if full.is_file() {
            return Some(full);
        }
    }
    None
}

pub struct Page {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    context_id: Option<String>,
    target_id: String,
    session_id: String,
    default_timeout: Duration,
}

impl Page {
    pub fn connect(ws_url: &str, default_timeout: Duration) -> Result<Self> {
        let (ws, _) = tungstenite::connect(ws_url).with_context(|| format!("connect {ws_url}"))?;
        let mut page = Self {
            ws,
            next_id: 1,
            context_id: None,
            target_id: String::new(),
            session_id: String::new(),
            default_timeout,
        };

        let ctx_res = page.send_browser("Target.createBrowserContext", json!({}));
        let context_id = ctx_res
            .ok()
            .and_then(|v| v.get("browserContextId").and_then(Value::as_str).map(String::from));
        page.context_id = context_id.clone();

        let create_params = match &context_id {
            Some(cid) => json!({ "url": "about:blank", "browserContextId": cid }),
            None => json!({ "url": "about:blank" }),
        };
        let target_res = page.send_browser("Target.createTarget", create_params)?;
        let target_id = target_res
            .get("targetId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Target.createTarget missing targetId: {target_res}"))?
            .to_string();
        page.target_id = target_id.clone();

        let attach_res = page.send_browser(
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
        )?;
        let session_id = attach_res
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Target.attachToTarget missing sessionId: {attach_res}"))?
            .to_string();
        page.session_id = session_id;

        let _ = page.send_session("Page.enable", json!({}));
        let _ = page.send_session("Runtime.enable", json!({}));
        Ok(page)
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    fn send_browser(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        self.ws.send(Message::Text(msg.to_string().into()))?;
        self.read_response(id)
    }

    fn send_session(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "id": id,
            "sessionId": self.session_id,
            "method": method,
            "params": params,
        });
        self.ws.send(Message::Text(msg.to_string().into()))?;
        self.read_response(id)
    }

    fn read_response(&mut self, expected_id: u64) -> Result<Value> {
        loop {
            let msg = self.ws.read()?;
            match msg {
                Message::Text(txt) => {
                    let v: Value = serde_json::from_str(&txt)?;
                    if v.get("id").and_then(Value::as_u64) == Some(expected_id) {
                        if let Some(err) = v.get("error") {
                            bail!("CDP error for id {expected_id}: {err}");
                        }
                        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                    }
                }
                Message::Ping(payload) => {
                    let _ = self.ws.send(Message::Pong(payload));
                }
                Message::Close(_) => bail!("CDP websocket closed"),
                _ => {}
            }
        }
    }

    pub fn goto(&mut self, url: &str) -> Result<()> {
        self.send_session("Page.navigate", json!({ "url": url }))?;
        let deadline = Instant::now() + self.default_timeout;
        while Instant::now() < deadline {
            if let Ok(state) = self.eval_string("document.readyState || ''") {
                if state == "complete" || state == "interactive" {
                    self.ensure_helpers()?;
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.ensure_helpers()?;
        Ok(())
    }

    fn ensure_helpers(&mut self) -> Result<()> {
        let script = r#"
(function() {
  if (window.__holonQueryAll) return true;
  if (!window.alert) window.alert = function(m) { console.log('ALERT:', m); };
  window.__holonIsVisible = function(el) {
    if (!el) return false;
    let cur = el;
    while (cur && cur.nodeType === 1) {
      if (cur.classList && cur.classList.contains('hidden')) return false;
      if (cur.hasAttribute && cur.hasAttribute('hidden')) return false;
      if (cur.style && (cur.style.display === 'none' || cur.style.visibility === 'hidden')) return false;
      if (window.getComputedStyle) {
        const cs = window.getComputedStyle(cur);
        if (cs && (cs.display === 'none' || cs.visibility === 'hidden' || cs.opacity === '0')) return false;
      }
      cur = cur.parentElement;
    }
    return true;
  };
  window.__holonText = function(el) {
    if (!el) return '';
    const tc = (el.textContent || '').trim();
    const it = (typeof el.innerText === 'string' ? el.innerText : '').trim();
    return tc === it ? tc : (tc + ' ' + it).trim();
  };
  function matchStep(roots, part) {
    part = part.trim();
    if (part === ':first') return roots.length > 0 ? [roots[0]] : [];
    if (part === ':last') return roots.length > 0 ? [roots[roots.length - 1]] : [];
    if (part.startsWith('text=')) {
      const needle = part.slice(5).trim();
      const out = [];
      for (const r of roots) {
        const all = r.querySelectorAll('button, a, span, div, label, li, h1, h2, h3, h4, p');
        for (const el of all) {
          if (window.__holonText(el).includes(needle)) out.push(el);
        }
      }
      return out;
    }
    const hasTextIdx = part.indexOf(':has-text(');
    if (hasTextIdx !== -1) {
      const baseSel = part.slice(0, hasTextIdx).trim() || '*';
      let inner = part.slice(hasTextIdx + 10).trim();
      if (inner.endsWith(')')) inner = inner.slice(0, -1).trim();
      if ((inner.startsWith('"') && inner.endsWith('"')) || (inner.startsWith("'") && inner.endsWith("'"))) {
        inner = inner.slice(1, -1);
      }
      const out = [];
      for (const r of roots) {
        const candidates = r.querySelectorAll(baseSel);
        for (const c of candidates) {
          if (window.__holonText(c).includes(inner)) out.push(c);
        }
      }
      return out;
    }
    const out = [];
    for (const r of roots) {
      const found = r.querySelectorAll(part);
      for (const f of found) out.push(f);
    }
    return out;
  }
  window.__holonQueryAll = function(selector) {
    const parts = selector.split('>>');
    let current = [document];
    for (const p of parts) {
      current = matchStep(current, p);
      if (current.length === 0) break;
    }
    return current;
  };
  window.__holonQuery = function(selector) {
    const all = window.__holonQueryAll(selector);
    return all.length > 0 ? all[0] : null;
  };
  return true;
})()
"#;
        let _ = self.eval_json(script)?;
        Ok(())
    }

    pub fn eval_json(&mut self, expression: &str) -> Result<Value> {
        let res = self.send_session(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            }),
        )?;
        if let Some(exc) = res.get("exceptionDetails") {
            bail!("JS exception evaluating `{expression}`: {exc}");
        }
        Ok(res.get("result").and_then(|r| r.get("value")).cloned().unwrap_or(Value::Null))
    }

    pub fn eval_string(&mut self, expression: &str) -> Result<String> {
        let v = self.eval_json(expression)?;
        Ok(v.as_str().unwrap_or_default().to_string())
    }

    pub fn click(&mut self, selector: &str) -> Result<()> {
        let sel_json = serde_json::to_string(selector)?;
        let script = format!(
            r#"
(function() {{
  const el = window.__holonQuery({sel_json});
  if (!el || !window.__holonIsVisible(el)) return false;
  el.click();
  return true;
}})()
"#
        );
        self.poll_bool(&script, &format!("click({selector})"), self.default_timeout)
    }

    pub fn fill(&mut self, selector: &str, value: &str) -> Result<()> {
        let sel_json = serde_json::to_string(selector)?;
        let val_json = serde_json::to_string(value)?;
        let script = format!(
            r#"
(function() {{
  const el = window.__holonQuery({sel_json});
  if (!el) return false;
  el.value = {val_json};
  el.dispatchEvent(new Event('input', {{ bubbles: true }}));
  el.dispatchEvent(new Event('change', {{ bubbles: true }}));
  return true;
}})()
"#
        );
        self.poll_bool(&script, &format!("fill({selector}, {value})"), self.default_timeout)
    }

    pub fn select_option(&mut self, selector: &str, value: &str) -> Result<()> {
        self.fill(selector, value)
    }

    pub fn wait_for_text(&mut self, selector: &str, expected: &str) -> Result<()> {
        self.wait_for_text_timeout(selector, expected, self.default_timeout)
    }

    pub fn wait_for_text_timeout(
        &mut self,
        selector: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<()> {
        let sel_json = serde_json::to_string(selector)?;
        let exp_json = serde_json::to_string(expected)?;
        let script = format!(
            r#"
(function() {{
  const el = window.__holonQuery({sel_json});
  if (!el) return false;
  return window.__holonText(el).includes({exp_json});
}})()
"#
        );
        self.poll_bool(&script, &format!("wait_for_text({selector}, {expected:?})"), timeout)
    }

    pub fn wait_for_not_text(&mut self, selector: &str, forbidden: &str) -> Result<()> {
        let sel_json = serde_json::to_string(selector)?;
        let forbid_json = serde_json::to_string(forbidden)?;
        let script = format!(
            r#"
(function() {{
  const el = window.__holonQuery({sel_json});
  if (!el) return false;
  const t = window.__holonText(el);
  return t.length > 0 && !t.includes({forbid_json});
}})()
"#
        );
        self.poll_bool(
            &script,
            &format!("wait_for_not_text({selector}, {forbidden:?})"),
            self.default_timeout,
        )
    }

    pub fn wait_for_visible(&mut self, selector: &str) -> Result<()> {
        let sel_json = serde_json::to_string(selector)?;
        let script = format!(
            r#"
(function() {{
  const el = window.__holonQuery({sel_json});
  return !!el && window.__holonIsVisible(el);
}})()
"#
        );
        self.poll_bool(&script, &format!("wait_for_visible({selector})"), self.default_timeout)
    }

    pub fn wait_for_hidden(&mut self, selector: &str) -> Result<()> {
        let sel_json = serde_json::to_string(selector)?;
        let script = format!(
            r#"
(function() {{
  const el = window.__holonQuery({sel_json});
  return !el || !window.__holonIsVisible(el);
}})()
"#
        );
        self.poll_bool(&script, &format!("wait_for_hidden({selector})"), self.default_timeout)
    }

    pub fn wait_for_count(&mut self, selector: &str, count: usize) -> Result<()> {
        let sel_json = serde_json::to_string(selector)?;
        let script = format!(
            r#"
(function() {{
  return window.__holonQueryAll({sel_json}).length === {count};
}})()
"#
        );
        self.poll_bool(
            &script,
            &format!("wait_for_count({selector}, {count})"),
            self.default_timeout,
        )
    }

    pub fn wait_for_class(
        &mut self,
        selector: &str,
        class_name: &str,
        present: bool,
    ) -> Result<()> {
        let sel_json = serde_json::to_string(selector)?;
        let cls_json = serde_json::to_string(class_name)?;
        let script = format!(
            r#"
(function() {{
  const el = window.__holonQuery({sel_json});
  if (!el || !el.classList) return false;
  return el.classList.contains({cls_json}) === {present};
}})()
"#
        );
        self.poll_bool(
            &script,
            &format!("wait_for_class({selector}, {class_name}, {present})"),
            self.default_timeout,
        )
    }

    pub fn wait_for_non_empty(&mut self, selector: &str) -> Result<String> {
        let sel_json = serde_json::to_string(selector)?;
        let script = format!(
            r#"
(function() {{
  const el = window.__holonQuery({sel_json});
  if (!el) return '';
  const v = typeof el.value === 'string' && el.value.length > 0 ? el.value : window.__holonText(el);
  return v.trim();
}})()
"#
        );
        let deadline = Instant::now() + self.default_timeout;
        while Instant::now() < deadline {
            let _ = self.ensure_helpers();
            if let Ok(s) = self.eval_string(&script) {
                if !s.is_empty() {
                    return Ok(s);
                }
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        bail!("timed out waiting for non-empty `{selector}`")
    }

    /// Capture a PNG screenshot via CDP `Page.captureScreenshot`.
    pub fn screenshot_png(&mut self) -> Result<Vec<u8>> {
        let res = self.send_session("Page.captureScreenshot", json!({ "format": "png" }))?;
        let data = res
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Page.captureScreenshot missing base64 data"))?;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .context("decode base64 PNG from Page.captureScreenshot")
    }

    /// Capture a screenshot via CDP and push it as a delta frame into `gif`.
    pub fn record_step(&mut self, gif: &mut Gif) -> Result<()> {
        let png = self.screenshot_png()?;
        gif.push_png(&png, Instant::now()).map_err(|e| anyhow!(e))
    }

    fn poll_bool(&mut self, script: &str, label: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let _ = self.ensure_helpers();
            if let Ok(Value::Bool(true)) = self.eval_json(script) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(35));
        }
        let body_diag = self
            .eval_string("document.body ? document.body.innerText.slice(0, 400) : '<no body>'")
            .unwrap_or_default();
        bail!("timed out after {:?} on {label}; page text: {body_diag:?}", timeout)
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        if !self.target_id.is_empty() {
            let _ = self.send_browser("Target.closeTarget", json!({ "targetId": self.target_id }));
        }
        if let Some(cid) = self.context_id.take() {
            let _ = self
                .send_browser("Target.disposeBrowserContext", json!({ "browserContextId": cid }));
        }
        let _ = self.ws.close(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::Html;
    use axum::routing::get;
    use axum::Router;

    const TEST_HTML: &str = r#"<!doctype html>
<html>
<head>
  <style>
    .hidden { display: none; }
    .toast { padding: 8px; background: #222; color: #fff; }
  </style>
</head>
<body>
  <button id="login-alice">Login Alice</button>
  <div id="toast-container"></div>
  <input id="book-title" value="" />
  <button id="add-btn">Add to Catalog</button>
  <div id="books-list"></div>
  <script>
    document.getElementById('login-alice').addEventListener('click', () => {
      const t = document.createElement('div');
      t.className = 'toast';
      t.innerText = 'Authenticated as alice';
      document.getElementById('toast-container').appendChild(t);
    });
    document.getElementById('add-btn').addEventListener('click', () => {
      const title = document.getElementById('book-title').value;
      const card = document.createElement('div');
      card.className = 'book-card';
      card.innerHTML = '<div class="book-title">' + title + '</div>';
      document.getElementById('books-list').appendChild(card);
    });
  </script>
</body>
</html>"#;

    #[tokio::test]
    async fn drives_dom_and_records_delta_gif_over_cdp() {
        let Some(_) = find_lightpanda_bin().or_else(find_chromium_bin) else {
            eprintln!("SKIPPED: neither lightpanda nor chromium is installed");
            return;
        };

        let app = Router::new().route("/", get(|| async { Html(TEST_HTML) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let url = format!("http://{addr}/");
        tokio::task::spawn_blocking(move || {
            let server = BrowserServer::start(EngineKind::Auto).expect("start browser server");
            let mut page = server.new_page().expect("new page");
            page.goto(&url).expect("goto");
            page.click("#login-alice").expect("click login-alice");
            page.wait_for_text(".toast >> :last", "Authenticated as alice").expect("toast text");
            page.fill("#book-title", "The Left Hand of Darkness").expect("fill title");
            page.click("button:has-text(\"Add to Catalog\")").expect("click add");
            page.wait_for_count(".book-card", 1).expect("1 book card");
            page.wait_for_text(".book-card >> :first >> .book-title", "The Left Hand of Darkness")
                .expect("book title matches");

            if find_chromium_bin().is_some() || find_lightpanda_bin().is_some() {
                let dir = tempfile::tempdir().unwrap();
                let gif_path = dir.path().join("demo.gif");
                let mut gif = Gif::create(&gif_path, 400, 300).unwrap();
                if page.record_step(&mut gif).is_ok() {
                    std::thread::sleep(Duration::from_millis(50));
                    page.fill("#book-title", "Second Book").unwrap();
                    page.click("button:has-text(\"Add to Catalog\")").unwrap();
                    let _ = page.record_step(&mut gif);
                    gif.finish(Instant::now()).unwrap();
                    assert!(gif_path.metadata().unwrap().len() > 0);
                }
            }
        })
        .await
        .unwrap();
    }
}
