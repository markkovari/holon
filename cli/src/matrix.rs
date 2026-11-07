//! `holon matrix`: what the Matrix bridge component (`components/matrix-bridge`) needs before
//! it starts, and the move from the native bridge it replaced. Every file it writes holds
//! secrets, so each is 0600 and no value is ever printed.

use std::io::Read;
use std::path::{Path, PathBuf};

use agent_matrix::{setup, Config};
use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::selfhost::write_secret_file;

pub struct InitArgs {
    pub server_name: String,
    pub homeserver: String,
    pub owner_localpart: String,
    pub owner_password: String,
    pub runtime_url: String,
    pub runtime_token: String,
    /// What Synapse dials, e.g. the tailnet address of the host serving the component.
    pub appservice_url: String,
    pub dir: PathBuf,
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .context("reading /dev/urandom")?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// The owner's session token: the bridge accepts the owner's invites with it.
fn login(homeserver: &str, user: &str, password: &str) -> Result<String> {
    let url = format!("{}/_matrix/client/v3/login", homeserver.trim_end_matches('/'));
    let v: serde_json::Value = ureq::post(&url)
        .send_json(json!({
            "type": "m.login.password",
            "identifier": {"type": "m.id.user", "user": user},
            "password": password,
            "initial_device_display_name": "holon-bridge",
        }))
        .with_context(|| format!("logging the owner in at {homeserver}"))?
        .into_json()?;
    match v["access_token"].as_str() {
        Some(t) => Ok(t.to_string()),
        None => bail!("the homeserver answered the login without a token"),
    }
}

pub fn config_path(dir: &Path) -> PathBuf {
    dir.join("bridge.conf")
}

/// Writes the app's config file and Synapse's registration file; answers both paths.
pub fn init(a: InitArgs) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(&a.dir).with_context(|| a.dir.display().to_string())?;
    let cfg = Config {
        admin_token: login(&a.homeserver, &a.owner_localpart, &a.owner_password)?,
        owner: format!("@{}:{}", a.owner_localpart, a.server_name),
        homeserver: a.homeserver,
        server_name: a.server_name,
        as_token: random_hex(32)?,
        hs_token: random_hex(32)?,
        runtime_url: a.runtime_url,
        runtime_token: a.runtime_token,
        history_chars: agent_matrix::config::history_chars(),
        jev_key: None,
    };
    let conf = config_path(&a.dir);
    let reg = a.dir.join("holon-agents.yaml");
    write_secret_file(&conf, setup::config_file(&cfg).as_bytes())?;
    write_secret_file(&reg, setup::registration_yaml(&cfg, &a.appservice_url).as_bytes())?;
    Ok((conf, reg))
}

/// The native bridge's `bridge.json` as the component's config file: the same tokens, so
/// Synapse's registration stays as it is. A Jev key it read from `HOLON_JEV_KEY_FILE` comes along.
pub fn migrate(dir: &Path) -> Result<PathBuf> {
    let old = dir.join("bridge.json");
    let text = std::fs::read_to_string(&old).with_context(|| old.display().to_string())?;
    let mut cfg: Config = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a bridge config", old.display()))?;
    if let Ok(f) = std::env::var("HOLON_JEV_KEY_FILE") {
        cfg.jev_key = std::fs::read_to_string(&f).ok().map(|k| k.trim().to_string());
    }
    if let Ok(n) = std::env::var("HOLON_HISTORY_CHARS") {
        cfg.history_chars = n.parse().context("HOLON_HISTORY_CHARS")?;
    }
    let conf = config_path(dir);
    write_secret_file(&conf, setup::config_file(&cfg).as_bytes())?;
    Ok(conf)
}

/// Hands the native bridge's `state.json` to the running component, which takes it only while
/// it has none; then renames it, so it is never offered twice.
pub fn import_state(dir: &Path, url: &str) -> Result<()> {
    let state = dir.join("state.json");
    let body = std::fs::read(&state).with_context(|| state.display().to_string())?;
    let conf = std::fs::read_to_string(config_path(dir)).context("reading bridge.conf")?;
    let hs_token = conf
        .lines()
        .find_map(|l| l.strip_prefix("matrix-hs-token = "))
        .context("bridge.conf has no matrix-hs-token")?;
    let r = ureq::put(&format!("{}/internal/state", url.trim_end_matches('/')))
        .set("authorization", &format!("Bearer {hs_token}"))
        .send_bytes(&body);
    match r {
        Ok(_) => {}
        Err(ureq::Error::Status(409, r)) => bail!("{}", r.into_string().unwrap_or_default()),
        Err(e) => return Err(e).context("importing the state"),
    }
    std::fs::rename(&state, dir.join("state.json.imported"))?;
    Ok(())
}
