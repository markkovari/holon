//! A GPUI desktop console for autonomous agents.
//!
//! An agent has a name, a description, capabilities (text, optionally tied to
//! a WIT ref), a model, and triggers — HTTP through the lattice, cron
//! schedules, and events. The runtime (`agent-runtime`, embedded here) runs
//! each as a bounded tool-using loop with memory; this window shows what they
//! do, including runs nobody typed (a schedule firing, another agent calling),
//! and is where a human approves the sensitive tool calls an agent asks for.
//! Agents persist across launches. Each also gets an HTTP front door on the
//! lattice: one shared gateway component deployed under its name.
//!
//! "+ new agent" opens a separate setup window rather than repurposing the
//! conversation pane as a compose box.
//!
//! The text fields are deliberately minimal hand-rolled inputs (append-on-
//! type, backspace-on-delete, whole-window key capture) — GPUI ships no
//! built-in text field; its own `examples/input.rs` is ~750 lines of IME/
//! selection/clipboard handling, out of scope here.

mod fm;
mod lattice;

use gpui::{
    div, prelude::*, px, rgb, size, App, Application, Bounds, Context, Entity, FocusHandle,
    Focusable, KeyDownEvent, SharedString, Window, WindowBounds, WindowOptions,
};

use lattice::{spec_from_form, AgentRow, FormInput, Kind, Lattice, Message, View};

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
        self.lattice.update(cx, |l, cx| l.select(name, cx));
    }

    /// Opens a separate, small "new agent" window — a setup dialog, not the
    /// same conversation pane repurposed as a compose box. Shares the same
    /// `Lattice` entity, so the moment it spawns an agent, this window's
    /// sidebar (reactive via `cx.observe`) picks it up on its own.
    fn open_new_agent_window(&mut self, cx: &mut Context<Self>) {
        let lattice = self.lattice.clone();
        let bounds = Bounds::centered(None, size(px(560.0), px(720.0)), cx);
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
        let approvals = state.approvals.clone();
        let view = state.view;
        let detail = state.detail.clone();
        let message_field = format!("{}_", self.message);

        let selected_agent =
            selected.as_ref().and_then(|name| agents.iter().find(|a| &a.name == name));
        let header = match &selected_agent {
            Some(a) => {
                format!("{}  ·  {}{}", a.name, a.status, if a.paused { "  ·  paused" } else { "" })
            }
            None => "no agent selected".to_string(),
        };
        let description = selected_agent.map(|a| a.description.clone()).filter(|d| !d.is_empty());
        let facts = selected_agent.map(|a| {
            format!(
                "model: {}   ·   can: {}   ·   triggers: {}",
                a.model,
                a.capabilities.join(", "),
                if a.triggers.is_empty() {
                    "http only".to_string()
                } else {
                    format!("http, {}", a.triggers.join(", "))
                }
            )
        });
        let paused = selected_agent.is_some_and(|a| a.paused);
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
                            .when_some(facts, |main, f| {
                                main.child(div().text_xs().text_color(rgb(0x777777)).child(f))
                            })
                            .when(has_selection, |main| {
                                main.child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap_2()
                                        .child(tab_button(
                                            "tab-chat",
                                            "conversation",
                                            view == View::Chat,
                                            cx,
                                            |l, cx| l.set_view(View::Chat, cx),
                                        ))
                                        .child(tab_button(
                                            "tab-memory",
                                            "memory",
                                            view == View::Memory,
                                            cx,
                                            |l, cx| l.set_view(View::Memory, cx),
                                        ))
                                        .child(tab_button(
                                            "tab-spec",
                                            "spec",
                                            view == View::Spec,
                                            cx,
                                            |l, cx| l.set_view(View::Spec, cx),
                                        ))
                                        .child(div().flex_1())
                                        .child(action_button(
                                            "pause-resume",
                                            if paused { "resume" } else { "pause" },
                                            rgb(0x6a5a2d),
                                            cx,
                                            move |l, cx| {
                                                if let Some(n) = l.selected.clone() {
                                                    l.set_paused(&n, !paused, cx);
                                                }
                                            },
                                        ))
                                        .child(action_button(
                                            "delete",
                                            "delete",
                                            rgb(0x6a2d2d),
                                            cx,
                                            |l, cx| {
                                                if let Some(n) = l.selected.clone() {
                                                    l.delete_agent(n, cx);
                                                }
                                            },
                                        )),
                                )
                            })
                            .child(
                                div()
                                    .id("transcript")
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .flex_1()
                                    .overflow_scroll()
                                    .when(view == View::Chat, |t| {
                                        t.children(transcript.into_iter().map(render_message))
                                    })
                                    .when(view != View::Chat, |t| {
                                        t.children(detail.into_iter().map(|l| {
                                            div().text_sm().text_color(rgb(0xcccccc)).child(l)
                                        }))
                                    }),
                            )
                            .children(approvals.into_iter().map(|p| {
                                let id = p.id;
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .items_center()
                                    .p_2()
                                    .rounded_md()
                                    .bg(rgb(0x4a3d1a))
                                    .child(div().flex_1().child(format!(
                                        "{} wants to run {} {}",
                                        p.agent, p.tool, p.args
                                    )))
                                    .child(action_button(
                                        SharedString::from(format!("approve-{id}")),
                                        "approve",
                                        rgb(0x2d6a4f),
                                        cx,
                                        move |l, cx| l.resolve_approval(id, true, cx),
                                    ))
                                    .child(action_button(
                                        SharedString::from(format!("deny-{id}")),
                                        "deny",
                                        rgb(0x6a2d2d),
                                        cx,
                                        move |l, cx| l.resolve_approval(id, false, cx),
                                    ))
                            }))
                            .child(div().text_sm().text_color(rgb(0x999999)).child(status_line))
                            .when(has_selection && view == View::Chat, |main| {
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

fn tab_button(
    id: &'static str,
    label: &'static str,
    active: bool,
    cx: &Context<Console>,
    f: impl Fn(&mut Lattice, &mut Context<Lattice>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(if active { rgb(0x2a4a6a) } else { rgb(0x2a2a2a) })
        .child(label)
        .on_click(cx.listener(move |this, _, _, cx| this.lattice.update(cx, |l, cx| f(l, cx))))
}

fn action_button(
    id: impl Into<SharedString>,
    label: &'static str,
    color: gpui::Rgba,
    cx: &Context<Console>,
    f: impl Fn(&mut Lattice, &mut Context<Lattice>) + 'static,
) -> impl IntoElement {
    div()
        .id(id.into())
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(color)
        .child(label)
        .on_click(cx.listener(move |this, _, _, cx| this.lattice.update(cx, |l, cx| f(l, cx))))
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

/// The "+ new agent" window's fields, in tab order: (label, hint). Only the
/// first two are required; everything else has a working default.
const FIELDS: [(&str, &str); 8] = [
    ("name", "unique; lowercase words joined by dashes"),
    ("description", "what it is for — becomes its system prompt"),
    ("capabilities", "name [@ wit-ref] [| what it does]; … — e.g. http_get; write_file; summarize | write 3 lines; agent:other"),
    ("schedules", "cron :: task; … — e.g. */10 * * * * :: check the feed   (also @hourly, @every 5m)"),
    ("events", "topics it wakes on, comma-separated"),
    ("allowed hosts", "for http_get, comma-separated; empty = none"),
    ("model", "blank/local, anthropic:<model>, or openai:<base-url>|<model>"),
    ("auto-approve", "sensitive tools it may use without asking (http_get, write_file)"),
];

/// The "+ new agent" setup window: a small, separate dialog rather than the
/// main window's conversation pane repurposed as a compose box. Submitting
/// builds an `AgentSpec` (`lattice::spec_from_form` — the same checks the
/// runtime applies, so a bad cron expression is refused here, not discovered
/// as an agent that never fires), spawns it on the shared `Lattice` entity and
/// closes itself; the main window's sidebar picks the new agent up on its own.
struct NewAgentForm {
    lattice: Entity<Lattice>,
    focus_handle: FocusHandle,
    values: [String; 8],
    field: usize,
    error: Option<String>,
}

impl NewAgentForm {
    fn new(lattice: Entity<Lattice>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        Self { lattice, focus_handle, values: Default::default(), field: 0, error: None }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "backspace" => {
                self.values[self.field].pop();
            }
            "tab" => {
                let step = if event.keystroke.modifiers.shift { FIELDS.len() - 1 } else { 1 };
                self.field = (self.field + step) % FIELDS.len();
            }
            "enter" => self.create(window, cx),
            "escape" => window.remove_window(),
            _ => {
                if let Some(ch) = &event.keystroke.key_char {
                    if ch.chars().all(is_typable) {
                        self.values[self.field].push_str(ch);
                    }
                }
            }
        }
        cx.notify();
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let v = &self.values;
        let input = FormInput {
            name: v[0].clone(),
            description: v[1].clone(),
            capabilities: v[2].clone(),
            schedules: v[3].clone(),
            events: v[4].clone(),
            hosts: v[5].clone(),
            model: v[6].clone(),
            auto_approve: v[7].clone(),
        };
        let spec = match spec_from_form(&input) {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        if let Err(e) = self.lattice.read(cx).validate_name(&spec.name) {
            self.error = Some(e);
            cx.notify();
            return;
        }
        self.lattice.update(cx, |l, cx| l.spawn_agent(spec, cx));
        window.remove_window();
    }
}

impl Focusable for NewAgentForm {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn form_field(
    label: &'static str,
    hint: &'static str,
    value: String,
    active: bool,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(div().text_xs().text_color(rgb(0xcccccc)).child(label))
                .child(div().text_xs().text_color(rgb(0x777777)).child(hint)),
        )
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
        let error = self.error.clone();
        div()
            .id("new-agent-form")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xe0e0e0))
            .child(div().text_lg().child("new agent  ·  tab / shift-tab to move, enter to create"))
            .children(FIELDS.iter().enumerate().map(|(i, (label, hint))| {
                div()
                    .id(SharedString::from(format!("field-{i}")))
                    .child(form_field(label, hint, self.values[i].clone(), self.field == i))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.field = i;
                        cx.notify();
                    }))
            }))
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
                    .child("create")
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
