use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct AppSpec {
    pub name: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub artifact: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub static_dir: Option<toml::Value>,
    #[serde(default)]
    pub kv: Option<toml::Value>,
    #[serde(default)]
    pub root: Option<String>,
}

impl AppSpec {
    pub fn kv_as_string(&self) -> Option<String> {
        self.kv.as_ref().map(|v| match v {
            toml::Value::String(s) => s.clone(),
            toml::Value::Table(t) => {
                if let Some(toml::Value::String(n)) = t.get("name") {
                    n.clone()
                } else {
                    v.to_string()
                }
            },
            _ => v.to_string(),
        })
    }

    pub fn static_dir_as_string(&self) -> Option<String> {
        self.static_dir.as_ref().map(|v| match v {
            toml::Value::String(s) => s.clone(),
            toml::Value::Array(a) => {
                if let Some(toml::Value::String(s)) = a.first() {
                    s.clone()
                } else {
                    v.to_string()
                }
            }
            _ => v.to_string(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct App {
    pub name: String,
    pub root: String,
    pub artifact: String,
}

pub fn registered_apps(root_dir: &Path) -> Vec<AppSpec> {
    let mut apps = Vec::new();
    if let Ok(entries) = fs::read_dir(root_dir.join("apps")) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map_or(false, |e| e == "toml") {
                if let Ok(content) = fs::read_to_string(&p) {
                    match toml::from_str::<AppSpec>(&content) {
                        Ok(spec) => apps.push(spec),
                        Err(e) => eprintln!("Error parsing {:?}: {}", p, e),
                    }
                }
            }
        }
    }
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    apps
}

pub fn discover_apps(root_dir: &Path) -> Vec<App> {
    let mut out = Vec::new();
    let mut roots_seen = BTreeSet::new();

    // Collect all built or source component directory names under components/
    let component_names = crate::component::registered_components(root_dir);

    // 1. Read registered applications from apps/*.toml
    let specs = registered_apps(root_dir);
    for spec in specs {
        if let Some(art) = spec.artifact.as_ref() {
            let art_file = art.rsplit('/').next().unwrap_or(art).to_string();
            let stem = art_file
                .strip_suffix(".composed.wasm")
                .or_else(|| art_file.strip_suffix(".wasm"))
                .unwrap_or(&art_file)
                .replace('_', "-");

            let root_name = spec.root.clone().unwrap_or_else(|| {
                if component_names.contains(&format!("{}-domain", spec.name)) {
                    format!("{}-domain", spec.name)
                } else if component_names.contains(&format!("{}-domain", stem)) {
                    format!("{}-domain", stem)
                } else if component_names.contains(&stem) {
                    stem.clone()
                } else {
                    let norm_stem = stem.replace('-', "");
                    let norm_domain = format!("{}domain", norm_stem);
                    component_names
                        .iter()
                        .find(|c| {
                            let norm_c = c.replace('-', "");
                            norm_c == norm_stem || norm_c == norm_domain
                        })
                        .cloned()
                        .unwrap_or(stem)
                }
            });

            roots_seen.insert(root_name.clone());
            out.push(App {
                name: spec.name,
                root: root_name,
                artifact: art_file,
            });
        }
    }

    // 2. Discover HTTP application components dynamically from WIT exports
    let mut names_seen: BTreeSet<String> = out.iter().map(|a| a.name.clone()).collect();
    for comp in &component_names {
        if roots_seen.contains(comp) {
            continue;
        }
        if comp.ends_with("-probe")
            || comp.ends_with("-suite")
            || comp == &"adversary"
            || comp == &"contrast-audit"
            || comp == &"http-serve"
        {
            continue;
        }

        let app_name = comp
            .strip_suffix("-domain")
            .or_else(|| comp.strip_suffix("-app"))
            .unwrap_or(comp)
            .to_string();
        if names_seen.contains(&app_name) {
            continue;
        }

        let wit_dir = root_dir.join("components").join(comp).join("wit");
        let mut exports_http = false;
        if let Ok(entries) = std::fs::read_dir(&wit_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|ext| ext == "wit") {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        if content.contains("wasi:http/incoming-handler") {
                            exports_http = true;
                            break;
                        }
                    }
                }
            }
        }

        if exports_http || comp.ends_with("-app") {
            let art_file = format!("{}.composed.wasm", comp.replace('-', "_"));
            roots_seen.insert(comp.clone());
            names_seen.insert(app_name.clone());
            out.push(App {
                name: app_name,
                root: comp.clone(),
                artifact: art_file,
            });
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.root.cmp(&b.root)));
    out.dedup_by(|a, b| a.root == b.root && a.name == b.name);
    out
}
