//! A GPUI desktop console for the lattice's dynamic agent CRUD.
//!
//! Proves the same claim `reconciler/tests/juan_live.rs` proves — that
//! platform-domain's HTTP API + comp-reconciler's convergence loop can take a
//! brand-new component from nonexistent to live-and-serving, with no
//! wasmCloud/wadm/Kubernetes — but driven from a GUI instead of a NATS
//! message: agents in a sidebar on the left, each one's conversation in the
//! main panel, chat-app style. The main window is for EXISTING agents only;
//! "+ new agent" opens a separate setup window, same shape as a real chat
//! app's "new conversation" dialog, rather than repurposing the same
//! conversation pane as a compose box.
//!
//! Boots its own throwaway local dev lattice (`comp_reconciler::fleet::Fleet`)
//! on launch, exactly like the test does. Pointing this at an already-running,
//! persistent deployment instead of an ephemeral local one is a deliberate
//! follow-up, not done here — which also means an agent you create does NOT
//! survive quitting and relaunching the console; the whole lattice it lives
//! on gets torn down and a fresh one booted next time.
//!
//! The message field below is a deliberately minimal hand-rolled text input
//! (append-on-type, backspace-on-delete, whole-window key capture) — GPUI
//! ships no built-in text field; its own `examples/input.rs` is a ~750-line
//! full IME/selection/clipboard-capable editor, which is out of scope for
//! one field in an MVP console.

mod fm;
mod lattice;

use gpui::{
    div, prelude::*, px, rgb, size, App, Application, Bounds, Context, Entity, FocusHandle,
    Focusable, KeyDownEvent, SharedString, Window, WindowBounds, WindowOptions,
};

use lattice::{AgentRow, Kind, Lattice, Message};

/// Any printable ASCII character, plus space — covers punctuation like `?`
/// that an earlier, narrower whitelist (letters/digits plus a fixed handful
/// of punctuation marks) missed. A real message needs whatever punctuation
/// a sentence needs; guessing the full set in advance is the wrong approach.
fn is_typable(c: char) -> bool {
    c == ' ' || c.is_ascii_graphic()
}

struct Console {
    lattice: Entity<Lattice>,
    focus_handle: FocusHandle,
    message: String,
}

impl Console {
    fn new(lattice: Entity<Lattice>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&lattice, |_, _, cx| cx.notify()).detach();
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        Self { lattice, focus_handle, message: String::new() }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "backspace" => {
                self.message.pop();
            }
            "enter" => self.submit(cx),
            _ => {
                if let Some(ch) = &event.keystroke.key_char {
                    if ch.chars().all(is_typable) {
                        self.message.push_str(ch);
                    }
                }
            }
        }
        cx.notify();
    }

    /// Enter (or the send button): sends the typed text to whichever agent
    /// is selected. Does nothing when no agent is selected — this window is
    /// for existing agents only; creating one happens in its own window.
    fn submit(&mut self, cx: &mut Context<Self>) {
        let text = std::mem::take(&mut self.message).trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(name) = self.lattice.read(cx).selected.clone() else { return };
        self.lattice.update(cx, |l, cx| l.send_to_agent(name, text, cx));
    }

    fn select(&mut self, name: String, cx: &mut Context<Self>) {
        self.lattice.update(cx, |l, cx| {
            l.selected = Some(name);
            cx.notify();
        });
    }

    /// Opens a separate, small "new agent" window — a setup dialog, not the
    /// same conversation pane repurposed as a compose box. Shares the same
    /// `Lattice` entity, so the moment it spawns an agent, this window's
    /// sidebar (reactive via `cx.observe`) picks it up on its own.
    fn open_new_agent_window(&mut self, cx: &mut Context<Self>) {
        let lattice = self.lattice.clone();
        let bounds = Bounds::centered(None, size(px(460.0), px(300.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| NewAgentForm::new(lattice, window, cx)),
        )
        .ok();
    }
}

impl Focusable for Console {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Console {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.lattice.read(cx);
        let status_line = state.status_line.clone();
        let agents = state.agents.clone();
        let selected = state.selected.clone();
        let message_field = format!("{}_", self.message);

        let selected_agent =
            selected.as_ref().and_then(|name| agents.iter().find(|a| &a.name == name));
        let header = match &selected_agent {
            Some(a) => format!("{}  ·  {}", a.name, a.status),
            None => "no agent selected".to_string(),
        };
        let description = selected_agent.map(|a| a.description.clone()).filter(|d| !d.is_empty());
        let transcript: Vec<Message> =
            selected_agent.map(|a| a.messages.clone()).unwrap_or_default();
        let has_selection = selected_agent.is_some();

        div()
            .id("root")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xe0e0e0))
            .child(div().p_3().text_xl().child(
                "holon — lattice console (platform-domain + comp-reconciler, no wasmCloud/k8s)",
            ))
            .child(
                div()
                    .id("panes")
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(
                        // Left: every existing agent, newest first, plus
                        // "+ new agent" — which opens a separate setup
                        // window rather than turning this list/panel into a
                        // compose box. Click an agent to view (and continue)
                        // its conversation in the main panel.
                        div()
                            .id("sidebar")
                            .flex()
                            .flex_col()
                            .gap_1()
                            .w(px(240.0))
                            .flex_shrink_0()
                            .p_2()
                            .border_r_1()
                            .border_color(rgb(0x333333))
                            .child(
                                div()
                                    .id("new-agent")
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .bg(rgb(0x2d6a4f))
                                    .child("+ new agent")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.open_new_agent_window(cx)
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .id("agent-sidebar-list")
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .flex_1()
                                    .overflow_scroll()
                                    .children(
                                        agents.iter().map(|a| {
                                            render_sidebar_row(a, selected.as_deref(), cx)
                                        }),
                                    ),
                            ),
                    )
                    .child(
                        // Main: the selected agent's conversation. With
                        // nothing selected, just a prompt — no compose box
                        // here; creating an agent is its own window.
                        div()
                            .id("main")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.0))
                            .p_3()
                            .gap_2()
                            .child(div().text_lg().child(header))
                            .when_some(description, |main, d| {
                                main.child(div().text_sm().text_color(rgb(0x999999)).child(d))
                            })
                            .child(
                                div()
                                    .id("transcript")
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .flex_1()
                                    .overflow_scroll()
                                    .children(transcript.into_iter().map(render_message)),
                            )
                            .child(div().text_sm().text_color(rgb(0x999999)).child(status_line))
                            .when(has_selection, |main| {
                                main.child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            div()
                                                .px_2()
                                                .py_1()
                                                .flex_1()
                                                .rounded_md()
                                                .bg(rgb(0x111111))
                                                .border_1()
                                                .border_color(rgb(0x444444))
                                                .child(message_field),
                                        )
                                        .child(
                                            div()
                                                .id("submit")
                                                .px_3()
                                                .py_1()
                                                .rounded_md()
                                                .bg(rgb(0x2d6a4f))
                                                .cursor_pointer()
                                                .child("send")
                                                .on_click(
                                                    cx.listener(|this, _, _, cx| this.submit(cx)),
                                                ),
                                        ),
                                )
                            }),
                    ),
            )
    }
}

fn status_color(status: &str) -> gpui::Rgba {
    if status == "live" {
        rgb(0x88cc88)
    } else if status.starts_with("failed") {
        rgb(0xcc8888)
    } else {
        rgb(0xcccc88)
    }
}

fn render_sidebar_row(
    agent: &AgentRow,
    selected: Option<&str>,
    cx: &Context<Console>,
) -> impl IntoElement {
    let name = agent.name.clone();
    let is_selected = selected == Some(agent.name.as_str());
    div()
        .id(SharedString::from(format!("agent-{}", agent.name)))
        .flex()
        .flex_col()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(if is_selected { rgb(0x2a4a6a) } else { rgb(0x252525) })
        .child(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .items_center()
                .child(div().child(agent.name.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(status_color(&agent.status))
                        .child(agent.status.clone()),
                ),
        )
        .when(!agent.description.is_empty(), |row| {
            row.child(
                div()
                    .text_xs()
                    .text_color(rgb(0x888888))
                    .overflow_hidden()
                    .child(agent.description.chars().take(40).collect::<String>()),
            )
        })
        .on_click(cx.listener(move |this, _, _, cx| this.select(name.clone(), cx)))
}

fn render_message(m: Message) -> impl IntoElement {
    // System entries (status checkpoints) are a centered, muted line with no
    // bubble — distinct from an actual message from either side, same idea
    // as a chat app's inline "X joined" line.
    if m.kind == Kind::System {
        return div()
            .text_xs()
            .text_color(rgb(0x888888))
            .child(format!("· {} ·", m.text))
            .into_any_element();
    }
    let (bg, label) =
        if m.kind == Kind::User { (rgb(0x2d4a6a), "you") } else { (rgb(0x2d4a2d), "agent") };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(bg)
        .max_w(px(520.0))
        .when(m.kind == Kind::User, |d| d.ml_auto())
        .child(div().text_xs().text_color(rgb(0x999999)).child(label))
        .child(div().child(m.text))
        .into_any_element()
}

/// Which of the "+ new agent" window's two fields is currently receiving
/// keystrokes. Tab toggles between them; clicking a field also focuses it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Description,
}

/// The "+ new agent" setup window: a small, separate dialog rather than the
/// main window's conversation pane repurposed as a compose box. A name
/// (unique, validated up front — the same rule `Lattice::validate_name`
/// enforces again before actually spawning, so this is a UX convenience, not
/// the only check) and a description (the agent's purpose — becomes its
/// canned reply and the first line of its conversation). Submitting spawns
/// the agent on the shared `Lattice` entity and closes itself — the main
/// window's sidebar picks the new agent up on its own via `cx.observe`,
/// since both windows' views read the same entity.
struct NewAgentForm {
    lattice: Entity<Lattice>,
    focus_handle: FocusHandle,
    name: String,
    description: String,
    field: Field,
    error: Option<String>,
}

impl NewAgentForm {
    fn new(lattice: Entity<Lattice>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        Self {
            lattice,
            focus_handle,
            name: String::new(),
            description: String::new(),
            field: Field::Name,
            error: None,
        }
    }

    fn active_field(&mut self) -> &mut String {
        match self.field {
            Field::Name => &mut self.name,
            Field::Description => &mut self.description,
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "backspace" => {
                self.active_field().pop();
            }
            "tab" => {
                self.field =
                    if self.field == Field::Name { Field::Description } else { Field::Name };
            }
            "enter" => self.create(window, cx),
            "escape" => window.remove_window(),
            _ => {
                if let Some(ch) = &event.keystroke.key_char {
                    if ch.chars().all(is_typable) {
                        self.active_field().push_str(ch);
                    }
                }
            }
        }
        cx.notify();
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // `extract_name`'s old job (guessing a name out of free text) is
        // gone — the name is its own field now, and sanitized the same way
        // that guess used to be, so e.g. "Natasha" still becomes "natasha".
        let name: String = self
            .name
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>()
            .to_lowercase();
        if let Err(e) = self.lattice.read(cx).validate_name(&name) {
            self.error = Some(e);
            cx.notify();
            return;
        }
        let description = std::mem::take(&mut self.description);
        self.lattice.update(cx, |l, cx| l.spawn_agent(name, description, cx));
        window.remove_window();
    }
}

impl Focusable for NewAgentForm {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn form_field(label: &'static str, value: String, active: bool) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().text_color(rgb(0x999999)).child(label))
        .child(
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(0x111111))
                .border_1()
                .border_color(if active { rgb(0x2d6a4f) } else { rgb(0x444444) })
                .child(format!("{value}{}", if active { "_" } else { "" })),
        )
}

impl Render for NewAgentForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.name.clone();
        let description = self.description.clone();
        let field = self.field;
        let error = self.error.clone();
        div()
            .id("new-agent-form")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xe0e0e0))
            .child(div().text_lg().child("new agent"))
            .child(
                div()
                    .id("field-name")
                    .child(form_field(
                        "name (unique) — tab to switch fields",
                        name,
                        field == Field::Name,
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.field = Field::Name;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("field-description")
                    .child(form_field(
                        "description — what it's for; becomes its reply",
                        description,
                        field == Field::Description,
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.field = Field::Description;
                        cx.notify();
                    })),
            )
            .when_some(error, |form, e| {
                form.child(div().text_sm().text_color(rgb(0xcc8888)).child(e))
            })
            .child(
                div()
                    .id("create")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(0x2d6a4f))
                    .cursor_pointer()
                    .child("create (or press enter)")
                    .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
            )
    }
}

fn main() {
    eprintln!(
        "booting local dev lattice (comp-host + comp-reconciler + platform-domain + comp-ingress)…"
    );
    let boot = lattice::boot();
    eprintln!("lattice ready: {}", boot.base_url);

    Application::new().run(move |cx: &mut App| {
        let lattice = cx.new(|cx| {
            let mut l = Lattice::new(boot, cx);
            l.refresh(cx);
            l
        });

        let auto_refresh_handle = lattice.clone();
        let bounds = Bounds::centered(None, size(px(1040.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Console::new(lattice, window, cx)),
        )
        .unwrap();
        cx.activate(true);

        // Auto-refresh the agent list every few seconds so convergence (and a
        // just-spawned agent going live) shows up without a manual click.
        cx.spawn(async move |cx| loop {
            cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
            if auto_refresh_handle.update(cx, |l, cx| l.refresh(cx)).is_err() {
                break;
            }
        })
        .detach();
    });
}
