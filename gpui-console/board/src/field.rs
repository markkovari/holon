//! A text field, because GPUI ships none.
//!
//! Typing appends, backspace deletes (with option a word, with command the line), command-v pastes,
//! enter submits (shift-enter starts a new line) and escape cancels; a caret breathes at the end. That
//! is all a goal needs and nothing a code editor would: no selection, no cursor in the middle, no IME
//! composition. The console's own inputs are the same size of thing for the same reason, and a field
//! that did more would be a thing to maintain for no reader of this board.

use std::time::Duration;

use gpui::{
    div, ease_in_out, prelude::*, px, Animation, AnimationExt as _, App, Context, EventEmitter,
    FocusHandle, Focusable, IntoElement, KeyDownEvent, Render, SharedString, Window,
};

use super::theme::Theme;

/// What a field says to whoever made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldEvent {
    Submit(String),
    Cancel,
    /// The text, after any key that changed it: what a search follows as it is typed.
    Changed(String),
}

pub struct TextField {
    text: String,
    placeholder: SharedString,
    focus: FocusHandle,
    theme: Theme,
    /// One line, and small: a search, not a goal.
    compact: bool,
}

impl EventEmitter<FieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Whether `s` is something to type: anything that is not a control character.
fn typable(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| !c.is_control())
}

/// `text` after a backspace: one character, or with `word` the last word, or with `line` all of it.
pub fn backspace(text: &str, word: bool, line: bool) -> String {
    if line {
        return String::new();
    }
    let mut t = text.to_string();
    if word {
        while t.ends_with(char::is_whitespace) {
            t.pop();
        }
        while t.chars().last().is_some_and(|c| !c.is_whitespace()) {
            t.pop();
        }
    } else {
        t.pop();
    }
    t
}

impl TextField {
    pub fn new(placeholder: impl Into<SharedString>, theme: Theme, cx: &mut Context<Self>) -> Self {
        Self {
            text: String::new(),
            placeholder: placeholder.into(),
            focus: cx.focus_handle(),
            theme,
            compact: false,
        }
    }

    /// A field of one line, for a search: enter takes the text as it is, shift-enter adds no line.
    pub fn compact(
        placeholder: impl Into<SharedString>,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Self {
        Self { compact: true, ..Self::new(placeholder, theme, cx) }
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Types `s` at the end, as if it had come from the keyboard.
    pub fn push_str(&mut self, s: &str, cx: &mut Context<Self>) {
        self.text.push_str(s);
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.text.clear();
        cx.emit(FieldEvent::Changed(String::new()));
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let m = &k.modifiers;
        let before = self.text.len();
        match k.key.as_str() {
            "enter" if m.shift && !self.compact => self.text.push('\n'),
            "enter" => {
                let goal = self.text.trim().to_string();
                if !goal.is_empty() {
                    cx.emit(FieldEvent::Submit(goal));
                }
            }
            "escape" => cx.emit(FieldEvent::Cancel),
            "backspace" => self.text = backspace(&self.text, m.alt, m.platform),
            "v" if m.platform => {
                if let Some(pasted) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.text.push_str(&pasted);
                }
            }
            _ if m.platform || m.control => return,
            _ => match &k.key_char {
                Some(c) if typable(c) => self.text.push_str(c),
                _ => return,
            },
        }
        // a key the field used is not also the window's: escape here closes the form, not the board
        cx.stop_propagation();
        if self.text.len() != before {
            cx.emit(FieldEvent::Changed(self.text.clone()));
        }
        cx.notify();
    }
}

impl Render for TextField {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let empty = self.text.is_empty();
        let shown = if empty { self.placeholder.to_string() } else { self.text.clone() };
        let focused = self.focus.is_focused(_window);
        let caret = (!empty || focused).then(|| {
            div()
                .mt(px(2.0))
                .w(px(1.5))
                .h(px(if self.compact { 15.0 } else { 18.0 }))
                .flex_none()
                .rounded(px(1.0))
                .bg(t.line)
                .with_animation(
                    "caret",
                    Animation::new(Duration::from_millis(1100)).repeat().with_easing(|p| {
                        let b = if p < 0.5 { p * 2.0 } else { (1.0 - p) * 2.0 };
                        0.15 + 0.85 * ease_in_out(b)
                    }),
                    |el, o| el.opacity(o),
                )
        });
        // the caret is where the next character goes: before the placeholder, after the text
        let (before, after) = if empty { (caret, None) } else { (None, caret) };
        div()
            .id("field")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .w_full()
            .min_h(px(if self.compact { 34.0 } else { 70.0 }))
            .px(px(if self.compact { 10.0 } else { 14.0 }))
            .py(px(if self.compact { 7.0 } else { 12.0 }))
            .rounded(px(if self.compact { 9.0 } else { 12.0 }))
            .bg(t.bg)
            .border_1()
            .border_color(if focused { t.line } else { t.tile_border })
            .flex()
            .flex_row()
            .items_start()
            .gap(px(2.0))
            .children(before)
            .child(
                div()
                    .min_w(px(0.0))
                    .text_size(px(if self.compact { 13.5 } else { 15.0 }))
                    .line_height(px(if self.compact { 19.0 } else { 21.0 }))
                    .when(self.compact, |c| c.truncate())
                    .text_color(if empty { t.faint } else { t.text })
                    .child(shown),
            )
            .children(after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backspace_takes_a_character_a_word_or_the_line() {
        assert_eq!(backspace("add rate limiting", false, false), "add rate limitin");
        assert_eq!(backspace("add rate limiting", true, false), "add rate ");
        assert_eq!(backspace("add rate limiting  ", true, false), "add rate ");
        assert_eq!(backspace("add rate limiting", false, true), "");
        assert_eq!(backspace("", false, false), "");
    }

    #[test]
    fn only_printable_text_is_typed() {
        assert!(typable("a") && typable("é") && typable(" "));
        assert!(!typable("") && !typable("\u{7f}") && !typable("\t"));
    }
}
