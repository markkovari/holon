use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::RwLock;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VoiceControlStatus {
    pub listening: bool,
    pub state: String,
    pub agent: String,
    pub device: String,
    pub last_heard: String,
    pub last_reply: String,
    pub active_agent: Option<String>,
}

pub struct ControlState {
    pub listening: AtomicBool,
    pub state_label: RwLock<String>,
    pub agent: RwLock<String>,
    pub device: String,
    pub last_heard: RwLock<String>,
    pub last_reply: RwLock<String>,
    pub active_agent: RwLock<Option<String>>,
}

impl ControlState {
    pub fn new(
        initial_listening: bool,
        agent: impl Into<String>,
        device: impl Into<String>,
    ) -> Self {
        Self {
            listening: AtomicBool::new(initial_listening),
            state_label: RwLock::new(if initial_listening {
                "WaitingWakeWord".to_string()
            } else {
                "Paused".to_string()
            }),
            agent: RwLock::new(agent.into()),
            device: device.into(),
            last_heard: RwLock::new(String::new()),
            last_reply: RwLock::new(String::new()),
            active_agent: RwLock::new(None),
        }
    }

    pub async fn snapshot(&self) -> VoiceControlStatus {
        VoiceControlStatus {
            listening: self.listening.load(Ordering::SeqCst),
            state: self.state_label.read().await.clone(),
            agent: self.agent.read().await.clone(),
            device: self.device.clone(),
            last_heard: self.last_heard.read().await.clone(),
            last_reply: self.last_reply.read().await.clone(),
            active_agent: self.active_agent.read().await.clone(),
        }
    }

    pub async fn toggle(&self) -> bool {
        let current = self.listening.load(Ordering::SeqCst);
        let next = !current;
        self.listening.store(next, Ordering::SeqCst);
        let mut label = self.state_label.write().await;
        *label = if next { "WaitingWakeWord".to_string() } else { "Paused".to_string() };
        if !next {
            let mut a = self.active_agent.write().await;
            *a = None;
        }
        next
    }

    pub async fn set_listening(&self, val: bool) {
        self.listening.store(val, Ordering::SeqCst);
        let mut label = self.state_label.write().await;
        *label = if val { "WaitingWakeWord".to_string() } else { "Paused".to_string() };
        if !val {
            let mut a = self.active_agent.write().await;
            *a = None;
        }
    }

    pub async fn set_agent(&self, new_agent: &str) {
        let mut a = self.agent.write().await;
        *a = new_agent.to_string();
    }

    pub async fn set_state_and_active_agent(&self, state_str: &str, active_agent: Option<&str>) {
        {
            let mut s = self.state_label.write().await;
            *s = state_str.to_string();
        }
        {
            let mut a = self.active_agent.write().await;
            *a = active_agent.map(|s| s.to_string());
        }
    }

    pub async fn update_turn(&self, heard: &str, reply: &str) {
        {
            let mut h = self.last_heard.write().await;
            *h = heard.to_string();
        }
        {
            let mut r = self.last_reply.write().await;
            *r = reply.to_string();
        }
    }
}

pub async fn run_control_server(
    port: u16,
    state: Arc<ControlState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr).await?;
    eprintln!("🎛️  [Control Server]: Listening on http://{addr}");

    loop {
        let (mut socket, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(_) => continue,
        };

        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let n = match socket.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };

            let req_str = String::from_utf8_lossy(&buf[..n]);
            let first_line = req_str.lines().next().unwrap_or("");
            let mut parts = first_line.split_whitespace();
            let _method = parts.next().unwrap_or("GET");
            let path = parts.next().unwrap_or("/");

            let (status_code, body) = if path == "/status" || path == "/" {
                let snap = state_clone.snapshot().await;
                let json = serde_json::to_string_pretty(&snap).unwrap_or_default();
                ("200 OK", json)
            } else if path == "/toggle" {
                let is_on = state_clone.toggle().await;
                let snap = state_clone.snapshot().await;
                let json = serde_json::to_string_pretty(&snap).unwrap_or_default();
                eprintln!(
                    "🎛️  [Control]: Toggled microphone -> {}",
                    if is_on { "ON" } else { "OFF" }
                );
                ("200 OK", json)
            } else if path == "/pause" {
                state_clone.set_listening(false).await;
                let snap = state_clone.snapshot().await;
                let json = serde_json::to_string_pretty(&snap).unwrap_or_default();
                eprintln!("🎛️  [Control]: Microphone paused");
                ("200 OK", json)
            } else if path == "/resume" {
                state_clone.set_listening(true).await;
                let snap = state_clone.snapshot().await;
                let json = serde_json::to_string_pretty(&snap).unwrap_or_default();
                eprintln!("🎛️  [Control]: Microphone resumed");
                ("200 OK", json)
            } else if path.starts_with("/agent") {
                let name =
                    if let Some(idx) = path.find("name=") { &path[idx + 5..] } else { "coach" };
                let clean_name = name.split('&').next().unwrap_or("coach");
                state_clone.set_agent(clean_name).await;
                let snap = state_clone.snapshot().await;
                let json = serde_json::to_string_pretty(&snap).unwrap_or_default();
                eprintln!("🎛️  [Control]: Switched default agent to '{clean_name}'");
                ("200 OK", json)
            } else {
                ("404 Not Found", "{\"error\": \"not found\"}".to_string())
            };

            let resp = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status_code,
                body.len(),
                body
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        });
    }
}
