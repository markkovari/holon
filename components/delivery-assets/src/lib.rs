//! `delivery-assets` — multi-project SDLC dashboard served as `ui:assets/files@0.1.0`.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: ["../../wit/ui-assets"],
        world: "ui:assets/ui-assets",
        generate_all,
    });
}

pub const INDEX_HTML: &str = include_str!("dashboard.html");

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::bindings;
    use crate::INDEX_HTML;
    use bindings::exports::ui::assets::files::{Asset, Guest};

    struct Component;

    impl Guest for Component {
        fn get(path: String) -> Option<Asset> {
            match path.as_str() {
                "/" | "/index.html" | "/dashboard" => Some(Asset {
                    content_type: "text/html; charset=utf-8".to_string(),
                    body: INDEX_HTML.as_bytes().to_vec(),
                }),
                _ => None,
            }
        }
    }

    bindings::export!(Component with_types_in bindings);
}
