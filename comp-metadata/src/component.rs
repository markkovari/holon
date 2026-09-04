use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

pub fn registered_components(root_dir: &Path) -> BTreeSet<String> {
    let mut component_names = BTreeSet::new();
    if let Ok(entries) = fs::read_dir(root_dir.join("components")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    if name != "target" && path.join("Cargo.toml").is_file() {
                        component_names.insert(name.to_string());
                    }
                }
            }
        }
    }
    component_names
}
