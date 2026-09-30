use anyhow::Result;
use wasmcloud_provider_sdk::{
    get_connection, run_provider, serve_provider_exports, Context, Provider,
};
use tracing::info;
use std::process::Command;
use wadm_types::{Manifest, Component, Trait};
use std::collections::BTreeMap;
wit_bindgen_wrpc::generate!({
    world: "builder-provider",
    path: "../../wit/system"
});

use exports::holon::system::builder::{BuildRequest, BuildResult, Handler};

#[derive(Default, Clone)]
struct BuilderProvider {}

impl Provider for BuilderProvider {}

impl Handler<Option<Context>> for BuilderProvider {
    async fn build_and_deploy(&self, _cx: Option<Context>, req: BuildRequest) -> anyhow::Result<Result<BuildResult, String>> {
        info!("Received build request for component: {}", req.component_name);

        let component_dir = format!("../../components/{}", req.component_name);

        // 1. Write the new .wit files
        for wit_file in req.wit_files {
            let path = format!("{}/wit/{}", component_dir, wit_file.path);
            std::fs::write(&path, &wit_file.content)?;
            info!("Wrote {}", path);
        }

        // 2. Write the new .rs files
        for rs_file in req.rs_files {
            let path = format!("{}/src/{}", component_dir, rs_file.path);
            std::fs::write(&path, &rs_file.content)?;
            info!("Wrote {}", path);
        }

        // 3. Compile the component
        info!("Running cargo component build for {}", req.component_name);
        let status = Command::new("cargo")
            .arg("component")
            .arg("build")
            .arg("--release")
            .current_dir(&component_dir)
            .status()?;
        
        if !status.success() {
            return Ok(Ok(BuildResult {
                success: false,
                message: "Compilation failed".to_string(),
                new_version: "".to_string(),
            }));
        }

        // 4. Push artifact to OCI registry (GHCR) using `wash oci push`
        info!("Pushing artifact to GHCR");
        let new_version = "0.2.0".to_string(); // In reality, parse Cargo.toml and bump version
        let github_user = std::env::var("GH_USER").unwrap_or_else(|_| "markkovari".to_string());
        let image_ref = format!("ghcr.io/{}/holon-{}:{}", github_user, req.component_name, new_version);
        
        info!("Pushing artifact to GHCR: {}", image_ref);
        let push_status = Command::new("wash")
            .arg("oci")
            .arg("push")
            .arg(&image_ref)
            .arg(format!("{}/target/wasm32-wasip2/release/{}.wasm", component_dir, req.component_name.replace('-', "_")))
            .status()?;
            
        if !push_status.success() {
            return Ok(Ok(BuildResult {
                success: false,
                message: "Failed to push to GHCR".to_string(),
                new_version: "".to_string(),
            }));
        }
        
        // 5. Update WADM Manifest via NATS
        info!("Creating/Updating WADM manifest via NATS");
        
        let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "127.0.0.1:4222".to_string());
        let nats = async_nats::connect(&nats_url).await?;

        // WADM default lattice is "default"
        let lattice = "default";
        
        let manifest_value = serde_json::json!({
            "apiVersion": "core.oam.dev/v1beta1",
            "kind": "Application",
            "metadata": {
                "name": req.component_name,
                "annotations": {
                    "description": "Auto-generated agent component",
                    "version": new_version
                }
            },
            "spec": {
                "components": [
                    {
                        "name": "nats-messaging-provider",
                        "type": "capability",
                        "properties": {
                            "image": "ghcr.io/wasmcloud/messaging-nats:0.22.0"
                        }
                    },
                    {
                        "name": req.component_name,
                        "type": "component",
                        "properties": {
                            "image": image_ref
                        },
                        "traits": [
                            {
                                "type": "link",
                                "properties": {
                                    "target": "nats-messaging-provider",
                                    "namespace": "wasmcloud",
                                    "package": "messaging",
                                    "interfaces": ["consumer", "handler"],
                                    "source_config": [{"name": "default-messaging"}]
                                }
                            }
                        ]
                    }
                ]
            }
        });

        let manifest: wadm_types::Manifest = serde_json::from_value(manifest_value).expect("Failed to build wadm_types::Manifest");
        let wadm_manifest = serde_yaml::to_string(&manifest).unwrap();

        // Put the updated model back to WADM
        let put_subject = format!("wadm.api.{lattice}.model.put");
        nats.request(put_subject, wadm_manifest.into()).await?;
        
        // Deploy the new version
        let deploy_subject = format!("wadm.api.{lattice}.model.deploy.{}", req.component_name);
        nats.request(deploy_subject, "".into()).await?;

        Ok(Ok(BuildResult {
            success: true,
            message: format!("Successfully built, pushed, and deployed {} version {}", req.component_name, new_version),
            new_version,
        }))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let provider = BuilderProvider::default();
    let shutdown = run_provider(provider.clone(), "builder-provider").await?;
    let connection = get_connection();
    let wrpc = connection.get_wrpc_client(connection.provider_key()).await?;
    serve_provider_exports(&wrpc, provider, shutdown, serve).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_patch_wadm_manifest() {
        let manifest = r#"
apiVersion: core.oam.dev/v1beta1
kind: Application
metadata:
  name: chat-agent
spec:
  components:
    - name: chat-agent
      type: component
      properties:
        image: localhost:5000/holon/chat-agent:0.1.0
"#;
        
        let expected_image = "ghcr.io/markkovari/holon-chat-agent:0.2.0";
        
        let patched = patch_wadm_manifest(manifest.as_bytes(), "chat-agent", expected_image).unwrap();
        
        // Ensure the patched manifest parses correctly and contains the new image reference
        let model: serde_yaml::Value = serde_yaml::from_str(&patched).unwrap();
        
        let components = model["spec"]["components"].as_sequence().unwrap();
        let agent_component = components.iter().find(|c| c["name"].as_str() == Some("chat-agent")).unwrap();
        let image = agent_component["properties"]["image"].as_str().unwrap();
        
        assert_eq!(image, expected_image);
    }
}

