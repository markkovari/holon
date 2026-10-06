//! The whole path, for real: a Synapse in Docker (deployed by the same
//! `infra/matrix/deploy.sh` that goes to malna), the bridge, and an agent
//! runtime with scripted agents, in-process. The test plays the owner with plain
//! Matrix client calls — what Element does.
//!
//! Skips (and says so) when Docker is not available. Needs an image pull the
//! first time (matrixdotorg/synapse).

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_matrix::init::{init, InitArgs};
use agent_matrix::{serve::serve, Bridge, Config};
use agent_runtime::speech::SpeechConfig;
use agent_runtime::{AgentSpec, Capability, Config as RtConfig, ModelSpec, Runtime};
use serde_json::{json, Value};

const SERVER: &str = "localhost";
const OWNER_PW: &str = "owner-test-password-not-a-secret";

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn docker_ok() -> bool {
    Command::new("docker").args(["compose", "version"]).output().is_ok_and(|o| o.status.success())
        && Command::new("docker").arg("info").output().is_ok_and(|o| o.status.success())
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Tears the test Synapse down however the test ends.
struct Synapse {
    project: String,
    dir: PathBuf,
}

impl Drop for Synapse {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["compose", "-p", &self.project, "-f"])
            .arg(self.dir.join("compose.yaml"))
            .args(["down", "-v"])
            .output();
    }
}

fn deploy(project: &str, dir: &PathBuf, port: u16, registration: Option<&PathBuf>) {
    let mut cmd = Command::new("bash");
    cmd.arg(repo().join("infra/matrix/deploy.sh"))
        .arg("local")
        .env("LOCAL_DIR", dir)
        .env("SYNAPSE_PORT", port.to_string())
        .env("COMPOSE_PROJECT_NAME", project)
        .env("SERVER_NAME", SERVER);
    if let Some(r) = registration {
        cmd.env("APPSERVICE_REG", r);
    }
    let out = cmd.output().expect("running deploy.sh");
    assert!(
        out.status.success(),
        "deploy.sh failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn wait_for<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// What Element does, minus the UI.
struct Owner {
    base: String,
    token: String,
    user: String,
    http: reqwest::blocking::Client,
}

impl Owner {
    fn login(base: &str) -> Self {
        let http = reqwest::blocking::Client::new();
        let v: Value = http
            .post(format!("{base}/_matrix/client/v3/login"))
            .json(&json!({"type": "m.login.password", "identifier": {"type": "m.id.user", "user": "mark"}, "password": OWNER_PW}))
            .send()
            .unwrap()
            .json()
            .unwrap();
        Self {
            base: base.into(),
            token: v["access_token"].as_str().expect("owner login").into(),
            user: format!("@mark:{SERVER}"),
            http,
        }
    }

    fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let mut r =
            self.http.request(method, format!("{}{path}", self.base)).bearer_auth(&self.token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        r.send().unwrap().json().unwrap_or(Value::Null)
    }

    fn send(&self, room: &str, content: Value) -> String {
        let txn = format!(
            "t{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        );
        let v = self.call(
            reqwest::Method::PUT,
            &format!("/_matrix/client/v3/rooms/{}/send/m.room.message/{txn}", enc(room)),
            Some(content),
        );
        v["event_id"].as_str().unwrap_or_else(|| panic!("send failed: {v}")).to_string()
    }

    fn say(&self, room: &str, body: &str) -> String {
        self.send(room, json!({"msgtype": "m.text", "body": body}))
    }

    fn say_to(&self, room: &str, body: &str, agent: &str) -> String {
        self.send(
            room,
            json!({"msgtype": "m.text", "body": body, "m.mentions": {"user_ids": [format!("@agent-{agent}:{SERVER}")]}}),
        )
    }

    /// Newest first.
    fn events(&self, room: &str) -> Vec<Value> {
        let v = self.call(
            reqwest::Method::GET,
            &format!("/_matrix/client/v3/rooms/{}/messages?dir=b&limit=60", enc(room)),
            None,
        );
        v["chunk"].as_array().cloned().unwrap_or_default()
    }

    /// Text messages from `sender`, oldest first.
    fn texts_from(&self, room: &str, sender: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .events(room)
            .iter()
            .filter(|e| e["type"] == "m.room.message" && e["sender"] == sender)
            .filter_map(|e| e["content"]["body"].as_str().map(String::from))
            .collect();
        v.reverse();
        v
    }

    fn joined(&self, room: &str) -> Vec<String> {
        let v = self.call(
            reqwest::Method::GET,
            &format!("/_matrix/client/v3/rooms/{}/joined_members", enc(room)),
            None,
        );
        v["joined"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default()
    }

    /// Creates a room the way Element would (the owner is its creator and admin).
    fn create_room(&self, body: Value) -> String {
        let v = self.call(reqwest::Method::POST, "/_matrix/client/v3/createRoom", Some(body));
        v["room_id"].as_str().unwrap_or_else(|| panic!("createRoom failed: {v}")).to_string()
    }

    fn set_state(&self, room: &str, ty: &str, key: &str, content: Value) {
        let v = self.call(
            reqwest::Method::PUT,
            &format!("/_matrix/client/v3/rooms/{}/state/{ty}/{}", enc(room), enc(key)),
            Some(content),
        );
        assert!(v.get("errcode").is_none(), "setting {ty} failed: {v}");
    }

    fn invite(&self, room: &str, user: &str) {
        let v = self.call(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/invite", enc(room)),
            Some(json!({"user_id": user})),
        );
        assert!(v.get("errcode").is_none(), "invite failed: {v}");
    }

    fn kick(&self, room: &str, user: &str) {
        let v = self.call(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/kick", enc(room)),
            Some(json!({"user_id": user, "reason": "test"})),
        );
        assert!(v.get("errcode").is_none(), "kick failed: {v}");
    }

    fn upload(&self, bytes: Vec<u8>, content_type: &str, filename: &str) -> String {
        let v: Value = self
            .http
            .post(format!("{}/_matrix/media/v3/upload?filename={filename}", self.base))
            .bearer_auth(&self.token)
            .header("content-type", content_type)
            .body(bytes)
            .send()
            .unwrap()
            .json()
            .unwrap();
        v["content_uri"].as_str().unwrap_or_else(|| panic!("upload failed: {v}")).to_string()
    }

    fn download(&self, mxc: &str) -> Vec<u8> {
        let rest = mxc.strip_prefix("mxc://").unwrap();
        self.http
            .get(format!("{}/_matrix/client/v1/media/download/{rest}", self.base))
            .bearer_auth(&self.token)
            .send()
            .unwrap()
            .bytes()
            .unwrap()
            .to_vec()
    }

    /// Sends an audio file as a voice message, the way Element's microphone button does.
    fn send_voice(&self, room: &str, bytes: Vec<u8>, ms: u64) -> String {
        let size = bytes.len();
        let uri = self.upload(bytes, "audio/ogg", "voice-message.ogg");
        self.send(
            room,
            json!({
                "msgtype": "m.audio", "body": "Voice message", "url": uri,
                "info": {"mimetype": "audio/ogg", "size": size, "duration": ms},
                "org.matrix.msc3245.voice": {},
            }),
        )
    }

    /// Voice messages from `sender`, newest first.
    fn voice_from(&self, room: &str, sender: &str) -> Vec<Value> {
        self.events(room)
            .into_iter()
            .filter(|e| {
                e["type"] == "m.room.message"
                    && e["sender"] == sender
                    && e["content"]["msgtype"] == "m.audio"
            })
            .collect()
    }

    /// A Space with a room inside it, linked both ways, as Element does it.
    fn space_with_room(&self, space_name: &str, room_name: &str) -> (String, String) {
        let space = self.create_room(json!({"name": space_name, "topic": "what we eat", "preset": "private_chat", "creation_content": {"type": "m.space"}}));
        let room = self.add_room_to(&space, room_name);
        (space, room)
    }

    fn add_room_to(&self, space: &str, name: &str) -> String {
        let room = self.create_room(json!({"name": name, "preset": "private_chat"}));
        self.set_state(space, "m.space.child", &room, json!({"via": [SERVER]}));
        self.set_state(&room, "m.space.parent", space, json!({"via": [SERVER], "canonical": true}));
        room
    }

    fn state(&self, room: &str, ty: &str, key: &str) -> Value {
        self.call(
            reqwest::Method::GET,
            &format!("/_matrix/client/v3/rooms/{}/state/{ty}/{}", enc(room), enc(key)),
            None,
        )
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn agent(name: &str, desc: &str, replies: Vec<String>) -> AgentSpec {
    let mut s = AgentSpec::new(name, desc);
    s.model = ModelSpec::Mock { replies };
    s
}

fn ghost(a: &str) -> String {
    format!("@agent-{a}:{SERVER}")
}

/// Everything the scenarios share: a real Synapse with the bridge's appservice loaded, an
/// agent runtime with scripted agents, the bridge, and the owner logged in.
struct World {
    rt: Arc<Runtime>,
    bridge: Arc<Bridge>,
    owner: Owner,
    base: String,
    container: String,
    _work: tempfile::TempDir,
    _down: Synapse,
    _lock: std::sync::MutexGuard<'static, ()>,
}

/// One Synapse at a time: the scenarios are heavy and share Docker.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn world() -> Option<World> {
    world_with(SpeechConfig::default())
}

fn world_with(speech: SpeechConfig) -> Option<World> {
    if !docker_ok() {
        eprintln!("skipping: docker is not available");
        return None;
    }
    let lock = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let work = tempfile::Builder::new().prefix("agent-matrix-e2e-").tempdir().unwrap();
    let project = format!("holonmx{}", std::process::id());
    let syn_dir = work.path().join("synapse");
    let (syn_port, bridge_port) = (free_port(), free_port());
    let base = format!("http://127.0.0.1:{syn_port}");
    let down = Synapse { project: project.clone(), dir: syn_dir.clone() };

    // ---- an agent runtime with scripted agents -----------------------------
    let mut rt_cfg = RtConfig::new(work.path().join("runtime"));
    rt_cfg.speech = speech;
    let rt = Runtime::new(rt_cfg).unwrap();
    let rt_addr =
        agent_runtime::server::serve(rt.clone(), "127.0.0.1:0", "rt-admin-token".into()).unwrap();
    rt.create_agent(agent("rower", "logs rowing", vec!["Your last row was 12,108m.".into(); 8]))
        .unwrap();
    rt.create_agent(agent("coach", "coaches", vec!["Keep the pace steady.".into(); 8])).unwrap();
    rt.create_agent(agent("chef", "plans meals", vec!["Pasta tonight.".into(); 8])).unwrap();
    let mut scout = agent(
        "scout",
        "wants to write a file",
        vec![
            r#"{"tool":"write_file","args":{"path":"a.txt","content":"hi"}}"#.into(),
            "wrote it".into(),
        ],
    );
    scout.capabilities.push(Capability::named("write_file")); // sensitive, not auto-approved
    rt.create_agent(scout).unwrap();
    let token_file = work.path().join("rt-token");
    std::fs::write(&token_file, "rt-admin-token").unwrap();

    // ---- Synapse, then the owner, then the bridge's setup --------------------
    deploy(&project, &syn_dir, syn_port, None);
    let container = format!("{project}-synapse-1");
    let reg = Command::new("docker")
        .args([
            "exec",
            &container,
            "register_new_matrix_user",
            "-c",
            "/data/homeserver.yaml",
            "--admin",
            "-u",
            "mark",
            "-p",
            OWNER_PW,
            "http://localhost:8008",
        ])
        .output()
        .unwrap();
    assert!(reg.status.success(), "creating the owner: {}", String::from_utf8_lossy(&reg.stderr));

    let (config_path, reg_path) = init(InitArgs {
        server_name: SERVER.into(),
        homeserver: base.clone(),
        owner_localpart: "mark".into(),
        owner_password: OWNER_PW.into(),
        runtime_url: format!("http://{rt_addr}"),
        runtime_token: "rt-admin-token".into(),
        listen: format!("0.0.0.0:{bridge_port}"),
        appservice_url: format!("http://host.docker.internal:{bridge_port}"),
        dir: work.path().join("bridge"),
    })
    .expect("agent-matrix init");
    // the config and registration hold secrets: both must be private
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [&config_path, &reg_path] {
            assert_eq!(std::fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
    deploy(&project, &syn_dir, syn_port, Some(&reg_path)); // now Synapse knows the appservice

    let cfg = Config::load(&config_path).unwrap();
    let bridge: Arc<Bridge> = Bridge::new(cfg);
    bridge
        .ensure_bridge_user()
        .expect("the appservice registration is loaded, so the bridge user registers");
    serve(bridge.clone()).unwrap();
    bridge.reconcile().expect("first reconcile");
    bridge.spawn_loops();
    let owner = Owner::login(&base);
    Some(World { rt, bridge, owner, base, container, _work: work, _down: down, _lock: lock })
}

#[test]
fn agents_are_matrix_users_projects_are_spaces_and_approvals_are_polls() {
    let Some(World { rt, bridge, owner, base, container, _work, _down, _lock }) = world() else {
        return;
    };
    // ---- 1. every agent is a user with a room the owner is already in ---------
    let dm = |a: &str| {
        bridge.state.read(|d| d.dms.get(a).cloned()).unwrap_or_else(|| panic!("no room for {a}"))
    };
    let rower_room = dm("rower");
    let joined = owner.joined(&rower_room);
    assert!(
        joined.contains(&owner.user) && joined.contains(&ghost("rower")),
        "owner and agent are both in: {joined:?}"
    );
    let control = bridge.state.read(|d| d.control_room.clone()).expect("control room");
    assert!(
        owner.joined(&control).contains(&owner.user),
        "the owner was placed in the control room, no invite to accept"
    );

    // ---- 2. chatting with an agent ----------------------------------------
    owner.say(&rower_room, "how was my last row?");
    let a = wait_for("rower's answer", 40, || {
        Some(owner.texts_from(&rower_room, &ghost("rower"))).filter(|t| !t.is_empty())
    });
    assert_eq!(a[0], "Your last row was 12,108m.");
    // it was a real run in the runtime, and its input was the message
    let runs = rt.store().runs("rower", 5);
    assert_eq!(runs[0].input, "how was my last row?");

    // ---- 3. a project is a Space, made from the control room ------------------
    owner.say(&control, "!project new rowing weekly training");
    let rooms = wait_for("the project's rooms", 40, || {
        bridge.state.read(|d| d.projects.get("rowing").cloned())
    });
    assert_eq!(
        owner.state(&rooms.space, "m.room.create", "")["type"],
        "m.space",
        "it is a real Space"
    );
    assert!(
        owner.state(&rooms.space, "m.space.child", &rooms.general).get("via").is_some(),
        "general is a child of the space"
    );
    assert!(
        owner.state(&rooms.space, "m.space.child", &rooms.feed).get("via").is_some(),
        "so is the feed"
    );
    assert!(owner.joined(&rooms.general).contains(&owner.user));
    let reply = wait_for("the command reply", 20, || {
        Some(owner.texts_from(&control, &bridge.cfg.bridge_user()))
            .filter(|t| t.iter().any(|x| x.contains("created")))
    });
    assert!(reply.iter().any(|x| x.contains("Project rowing created")));

    // ---- 4. adding agents: the registry changes, the rooms follow ---------------
    owner.say(&control, "!project add rowing rower coach");
    wait_for("both agents in the project room", 40, || {
        let j = owner.joined(&rooms.general);
        (j.contains(&ghost("rower")) && j.contains(&ghost("coach"))).then_some(())
    });
    assert_eq!(rt.store().get_project("rowing").unwrap().agents, ["rower", "coach"]);

    // ---- 5. who answers in a project room ---------------------------------------
    owner.say(&rooms.general, "good morning everyone"); // no mention, no lead: silence
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        owner.texts_from(&rooms.general, &ghost("rower")).is_empty()
            && owner.texts_from(&rooms.general, &ghost("coach")).is_empty(),
        "nobody answers an unaddressed message"
    );
    owner.say_to(&rooms.general, "rower: what did I do?", "rower");
    wait_for("rower's answer in the project", 40, || {
        Some(owner.texts_from(&rooms.general, &ghost("rower"))).filter(|t| !t.is_empty())
    });
    assert!(
        owner.texts_from(&rooms.general, &ghost("coach")).is_empty(),
        "only the mentioned agent answers"
    );
    owner.say(&control, "!project lead rowing coach");
    wait_for("the lead to be set", 20, || {
        rt.store().get_project("rowing").filter(|p| p.lead.as_deref() == Some("coach"))
    });
    owner.say(&rooms.general, "any advice?"); // no mention: the lead answers
    let advice = wait_for("the lead's answer", 40, || {
        Some(owner.texts_from(&rooms.general, &ghost("coach"))).filter(|t| !t.is_empty())
    });
    assert_eq!(advice[0], "Keep the pace steady.");

    // ---- 6. membership is mutual: invite from Matrix adds to the project ----------
    let invite = owner.call(
        reqwest::Method::POST,
        &format!("/_matrix/client/v3/rooms/{}/invite", enc(&rooms.general)),
        Some(json!({"user_id": ghost("chef")})),
    );
    assert!(invite.get("errcode").is_none(), "inviting an agent's user works: {invite}");
    wait_for("chef to join the project", 40, || {
        rt.store().get_project("rowing").filter(|p| p.agents.iter().any(|a| a == "chef"))
    });
    wait_for("chef to join the room", 40, || {
        owner.joined(&rooms.general).contains(&ghost("chef")).then_some(())
    });
    // ...and the grant came with it: chef can now use the project's store and topics
    let eff = agent_runtime::projects::effective(
        &rt.store().get("chef").unwrap(),
        &rt.store().list_projects(),
    );
    assert_eq!(eff.store.write, ["project.rowing"]);
    // kicking it takes the membership away again
    let kick = owner.call(
        reqwest::Method::POST,
        &format!("/_matrix/client/v3/rooms/{}/kick", enc(&rooms.general)),
        Some(json!({"user_id": ghost("chef"), "reason": "test"})),
    );
    assert!(kick.get("errcode").is_none(), "{kick}");
    wait_for("chef to leave the project", 40, || {
        rt.store().get_project("rowing").filter(|p| !p.agents.iter().any(|a| a == "chef"))
    });

    // ---- 7. an approval is a poll, and the vote answers it -------------------------
    let scout_room = dm("scout");
    owner.say(&scout_room, "write the file");
    let poll = wait_for("the approval poll", 40, || {
        owner.events(&scout_room).into_iter().find(|e| {
            e["type"] == "org.matrix.msc3381.poll.start" && e["sender"] == ghost("scout").as_str()
        })
    });
    let text = poll["content"]["org.matrix.msc3381.poll.start"]["question"]
        ["org.matrix.msc1767.text"]
        .as_str()
        .unwrap();
    assert!(text.contains("scout wants to run write_file"), "{text}");
    assert!(rt.pending_approvals().len() == 1, "the run is parked on the approval");
    let txn = format!("v{}", std::process::id());
    owner.call(
        reqwest::Method::PUT,
        &format!(
            "/_matrix/client/v3/rooms/{}/send/org.matrix.msc3381.poll.response/{txn}",
            enc(&scout_room)
        ),
        Some(json!({
            "m.relates_to": {"rel_type": "m.reference", "event_id": poll["event_id"]},
            "org.matrix.msc3381.poll.response": {"answers": ["approve"]},
        })),
    );
    let done = wait_for("the run to finish after the vote", 40, || {
        Some(owner.texts_from(&scout_room, &ghost("scout"))).filter(|t| !t.is_empty())
    });
    assert_eq!(done[0], "wrote it");
    assert!(
        rt.store().workspace("scout").unwrap().join("a.txt").exists(),
        "the approved write happened"
    );

    // ---- 8. only the owner can talk to the agents ---------------------------------
    // A second account that IS in the agent's room (the owner invited it) is ignored.
    let reg = Command::new("docker")
        .args([
            "exec",
            &container,
            "register_new_matrix_user",
            "-c",
            "/data/homeserver.yaml",
            "--no-admin",
            "-u",
            "guest",
            "-p",
            OWNER_PW,
            "http://localhost:8008",
        ])
        .output()
        .unwrap();
    assert!(reg.status.success(), "{}", String::from_utf8_lossy(&reg.stderr));
    owner.call(
        reqwest::Method::POST,
        &format!("/_matrix/client/v3/rooms/{}/invite", enc(&rower_room)),
        Some(json!({"user_id": format!("@guest:{SERVER}")})),
    );
    let http = reqwest::blocking::Client::new();
    let guest: Value = http
        .post(format!("{base}/_matrix/client/v3/login"))
        .json(&json!({"type": "m.login.password", "identifier": {"type": "m.id.user", "user": "guest"}, "password": OWNER_PW}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    let gt = guest["access_token"].as_str().unwrap();
    let joined = http
        .post(format!("{base}/_matrix/client/v3/join/{}", enc(&rower_room)))
        .bearer_auth(gt)
        .json(&json!({}))
        .send()
        .unwrap();
    assert!(joined.status().is_success(), "the guest joins the room it was invited to");
    let before = rt.store().runs("rower", 50).len();
    let sent = http
        .put(format!("{base}/_matrix/client/v3/rooms/{}/send/m.room.message/g1", enc(&rower_room)))
        .bearer_auth(gt)
        .json(&json!({"msgtype": "m.text", "body": "hello from a stranger"}))
        .send()
        .unwrap();
    assert!(sent.status().is_success());
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(
        rt.store().runs("rower", 50).len(),
        before,
        "a message from anyone but the owner starts no run"
    );
}

#[test]
fn a_space_you_make_yourself_becomes_a_project_when_you_invite_an_agent_into_it() {
    let Some(World { rt, bridge, owner, _work, _down, _lock, .. }) = world() else { return };
    let control = bridge.state.read(|d| d.control_room.clone()).expect("control room");
    let notes = || owner.texts_from(&control, &bridge.cfg.bridge_user());

    // 1. You make a Space and a room in it, in your own client. Nothing is a project yet.
    let (space, room) = owner.space_with_room("Nutrition & Meals", "meal planning");
    assert!(rt.store().get_project("nutrition-meals").is_none());

    // 2. Inviting an agent into the SPACE makes the Space a project, named after it.
    owner.invite(&space, &ghost("chef"));
    let project =
        wait_for("the Space to be adopted", 40, || rt.store().get_project("nutrition-meals"));
    assert_eq!(project.agents, ["chef"]);
    assert_eq!(
        project.description, "what we eat",
        "the Space's topic is the project's description"
    );
    wait_for("the notice in the control room", 20, || {
        notes().into_iter().find(|n| n.contains("Adopted your Space"))
    });
    // the agent is in the Space AND in the room inside it, and the room is a project room
    wait_for("chef in the space and its room", 40, || {
        (owner.joined(&space).contains(&ghost("chef"))
            && owner.joined(&room).contains(&ghost("chef")))
        .then_some(())
    });
    assert!(
        matches!(bridge.state.kind_of(&room), Some(agent_matrix::state::RoomKind::ProjectRoom(p)) if p == "nutrition-meals")
    );

    // 3. Inside the Space the usual rules hold: unaddressed is silent, a mention answers.
    owner.say(&room, "hello"); // no mention, no lead
    std::thread::sleep(Duration::from_secs(4));
    assert!(owner.texts_from(&room, &ghost("chef")).is_empty());
    owner.say_to(&room, "chef: what is for dinner?", "chef");
    let a = wait_for("chef's answer", 40, || {
        Some(owner.texts_from(&room, &ghost("chef"))).filter(|t| !t.is_empty())
    });
    assert_eq!(a[0], "Pasta tonight.");

    // 4. Inviting another agent to just the ROOM also puts it in the project (and the Space).
    owner.invite(&room, &ghost("coach"));
    wait_for("coach to join the project", 40, || {
        rt.store().get_project("nutrition-meals").filter(|p| p.agents.iter().any(|a| a == "coach"))
    });
    wait_for("coach in the space too", 40, || {
        owner.joined(&space).contains(&ghost("coach")).then_some(())
    });

    // 5. A room you add to the Space later is picked up, with the members already in it.
    let later = owner.add_room_to(&space, "shopping");
    wait_for("members in the new room", 40, || {
        let j = owner.joined(&later);
        (j.contains(&ghost("chef")) && j.contains(&ghost("coach"))).then_some(())
    });

    // 6. Removing an agent from the Space removes it from the project and every room in it.
    owner.kick(&space, &ghost("coach"));
    wait_for("coach to leave the project", 40, || {
        rt.store().get_project("nutrition-meals").filter(|p| !p.agents.iter().any(|a| a == "coach"))
    });
    wait_for("coach out of the rooms", 40, || {
        (!owner.joined(&room).contains(&ghost("coach"))
            && !owner.joined(&later).contains(&ghost("coach")))
        .then_some(())
    });

    // 7. A Space whose name has nothing usable is refused with a note, not half-adopted.
    let (bad, _) = owner.space_with_room("!!!", "x");
    owner.invite(&bad, &ghost("scout"));
    wait_for("the refusal note", 40, || {
        notes().into_iter().find(|n| n.contains("can't make a project"))
    });
    assert!(rt.store().list_projects().iter().all(|p| !p.agents.iter().any(|a| a == "scout")));
}

#[test]
fn you_can_talk_to_an_agent_and_it_talks_back() {
    let real = cfg!(target_os = "macos")
        && ["say", "ffmpeg", "swiftc"].iter().all(|c| {
            Command::new(c)
                .arg(if *c == "say" {
                    "-v?"
                } else if *c == "ffmpeg" {
                    "-version"
                } else {
                    "--version"
                })
                .output()
                .is_ok_and(|o| o.status.success())
        });
    if !real {
        eprintln!("skipping: needs macOS with say, ffmpeg and the Swift toolchain");
        return;
    }
    // the same on-device recognizer the runtime ships, built once for this test
    let tools = tempfile::Builder::new().prefix("agent-matrix-stt-").tempdir().unwrap();
    let stt = tools.path().join("holon-stt");
    let build = Command::new("swiftc")
        .args(["-parse-as-library", "-O", "-o"])
        .arg(&stt)
        .arg(repo().join("agent-runtime/speech/holon-stt.swift"))
        .output()
        .unwrap();
    if !build.status.success() {
        eprintln!("skipping: holon-stt did not build (needs the macOS 26 SDK)");
        return;
    }
    let Some(World { rt, bridge, owner, _work, _down, _lock, .. }) =
        world_with(SpeechConfig { stt: Some(stt), ..Default::default() })
    else {
        return;
    };
    let room = bridge.state.read(|d| d.dms.get("rower").cloned()).expect("rower's room");
    // You hold the microphone and say something. (Spoken with a real voice, as Opus in Ogg.)
    let said = rt.speech().speak("What was my last row, please?", None).expect("speaking");
    owner.send_voice(&room, said.audio, said.duration_ms);

    // 1. the bridge shows what it heard...
    let heard = wait_for("the transcript", 60, || {
        owner.texts_from(&room, &ghost("rower")).into_iter().find(|t| t.starts_with("🎙️"))
    });
    assert!(heard.to_lowercase().contains("last row"), "heard: {heard}");
    // ...2. the agent ran on that transcript...
    wait_for("the run", 60, || rt.store().runs("rower", 3).into_iter().next());
    assert!(
        rt.store().runs("rower", 1)[0].input.to_lowercase().contains("last row"),
        "the agent was given the words, not the audio"
    );
    // ...3. and answered in text AND in a voice message
    let text = wait_for("the written answer", 60, || {
        owner.texts_from(&room, &ghost("rower")).into_iter().find(|t| t.contains("12,108m"))
    });
    assert_eq!(text, "Your last row was 12,108m.");
    let voice = wait_for("the voice answer", 60, || {
        owner.voice_from(&room, &ghost("rower")).into_iter().next()
    });
    let c = &voice["content"];
    assert!(
        c.get("org.matrix.msc3245.voice").is_some(),
        "it is a voice message, so a client shows it as one"
    );
    assert_eq!(c["info"]["mimetype"], "audio/ogg");
    assert!(
        c["org.matrix.msc1767.audio"]["waveform"].as_array().is_some_and(|w| w.len() == 100),
        "with a waveform"
    );
    assert!(c["info"]["duration"].as_u64().unwrap() > 500);

    // The spoken answer really says it: download it and listen.
    let audio = owner.download(c["url"].as_str().unwrap());
    assert_eq!(&audio[..4], b"OggS");
    let back = rt.speech().transcribe(&audio, None).expect("transcribing the reply").to_lowercase();
    assert!(back.contains("last row"), "the voice reply says the answer: {back}");

    // Without speech set up, a voice message gets a clear note instead of silence
    // (covered by the unit-level wording; the runtime used here has it configured).
}
