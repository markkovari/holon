//! Lucide SVGs (ISC, `assets/icons/LICENSE`), embedded so the binary needs nothing beside it.
//! GPUI draws an SVG as a mask tinted with the text colour, so one file serves every theme.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{svg, AssetSource, Rgba, SharedString, Svg};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const FILES: &[(&str, &[u8])] = &[
            $((concat!("icons/", $name, ".svg"),
               include_bytes!(concat!("../assets/icons/", $name, ".svg")).as_slice())),*
        ];
    };
}

icons![
    "asterisk",
    "bot",
    "brain",
    "calendar",
    "check",
    "chevron-down",
    "circle-alert",
    "circle-x",
    "clock",
    "code-xml",
    "cpu",
    "database",
    "eye",
    "file-pen",
    "file-text",
    "flask-conical",
    "folder",
    "git-fork",
    "globe",
    "hexagon",
    "loader-circle",
    "message-square",
    "mic",
    "moon",
    "plus",
    "search",
    "settings",
    "shield-check",
    "sparkle",
    "sparkles",
    "square",
    "sun",
    "terminal",
    "user-round",
    "utensils",
    "volume-2",
    "workflow",
    "wrench",
    "x",
    "zap",
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(FILES.iter().find(|(p, _)| *p == path).map(|(_, b)| Cow::Borrowed(*b)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(FILES
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}

pub fn icon(name: &str) -> Svg {
    svg().path(SharedString::from(format!("icons/{name}.svg")))
}

pub fn tool_icon(tool: &str) -> &'static str {
    match tool {
        "read_file" | "transcribe" => "file-text",
        "list_dir" => "folder",
        "write_file" => "file-pen",
        "run" => "terminal",
        "http_get" => "globe",
        "remember" | "recall" => "brain",
        "store_get" | "store_put" | "store_list" => "database",
        "emit_event" => "zap",
        "now" | "schedule_self" => "clock",
        "spawn_task" | "task_result" | "list_agents" => "git-fork",
        "speak" => "volume-2",
        "session_start" | "session_result" | "session_close" => "code-xml",
        t if t.starts_with("gcal_") => "calendar",
        t if t.starts_with("food_") => "utensils",
        t if t.starts_with("agent") => "bot",
        t if t.starts_with("session") => "code-xml",
        _ => "wrench",
    }
}

/// Deliberately not anybody's logo: just enough to tell a row of agents apart.
pub fn model_glyph(model: &str) -> (&'static str, Rgba) {
    let m = model.to_lowercase();
    let hex = |c: u32| gpui::rgb(c);
    if m.contains("anthropic") || m.contains("claude") {
        ("asterisk", hex(0xe0805f))
    } else if m.contains("openai") && !m.contains("deepseek") || m.contains("gpt") {
        ("hexagon", hex(0x3fbf9f))
    } else if m.contains("gemini") || m.contains("google") {
        ("sparkle", hex(0x5b8def))
    } else if m.contains("deepseek") {
        ("search", hex(0x6f7bff))
    } else if m.starts_with("local") || m.contains("qwen") || m.contains("mlx") {
        ("cpu", hex(0x9aa3b2))
    } else if m.starts_with("mock") {
        ("flask-conical", hex(0xb28cf0))
    } else {
        ("bot", hex(0x9aa3b2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_a_tool_or_a_model_can_be_drawn_as_is_embedded() {
        let have = |name: &str| FILES.iter().any(|(p, _)| *p == format!("icons/{name}.svg"));
        for t in [
            "read_file",
            "list_dir",
            "write_file",
            "run",
            "http_get",
            "remember",
            "recall",
            "store_get",
            "emit_event",
            "now",
            "spawn_task",
            "speak",
            "session_start",
            "gcal_add",
            "food_photo",
            "agent_save",
            "something_new",
        ] {
            assert!(have(tool_icon(t)), "{t} -> {}", tool_icon(t));
        }
        for m in [
            "anthropic/claude-haiku",
            "openai/gpt-5",
            "google/gemini-flash",
            "openai/deepseek-chat",
            "local/system",
            "mock/mock",
            "unknown",
        ] {
            assert!(have(model_glyph(m).0), "{m} -> {}", model_glyph(m).0);
        }
        for n in [
            "check",
            "clock",
            "square",
            "circle-x",
            "loader-circle",
            "chevron-down",
            "sparkles",
            "sun",
            "moon",
        ] {
            assert!(have(n), "{n}");
        }
    }

    #[test]
    fn a_deepseek_model_served_over_the_openai_dialect_is_still_deepseek() {
        assert_eq!(model_glyph("openai/deepseek-chat").0, "search");
        assert_eq!(model_glyph("openai/gpt-5").0, "hexagon");
    }

    #[test]
    fn the_assets_load_by_path() {
        let a = Assets;
        assert!(a.load("icons/check.svg").unwrap().is_some());
        assert!(a.load("icons/nope.svg").unwrap().is_none());
    }
}
