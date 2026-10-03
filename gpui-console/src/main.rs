//! A GPUI desktop console for the lattice's dynamic agent CRUD.
//!
//! Proves the same claim `reconciler/tests/juan_live.rs` proves — that
//! platform-domain's HTTP API + comp-reconciler's convergence loop can take a
//! brand-new component from nonexistent to live-and-serving, with no
//! wasmCloud/wadm/Kubernetes — but driven from a GUI instead of a NATS
//! message, and showing every currently-deployed agent's status.
//!
//! Boots its own throwaway local dev lattice (`comp_reconciler::fleet::Fleet`)
//! on launch, exactly like the test does. Pointing this at an already-running,
//! persistent deployment instead of an ephemeral local one is a deliberate
//! follow-up, not done here.
//!
//! The name field below is a deliberately minimal hand-rolled text input
//! (append-on-type, backspace-on-delete, whole-window key capture) — GPUI
//! ships no built-in text field; its own `examples/input.rs` is a ~750-line
//! full IME/selection/clipboard-capable editor, which is out of scope for
//! one field in an MVP console.

mod lattice;

use gpui::{
    div, prelude::*, px, rgb, size, App, Application, Bounds, Context, Entity, FocusHandle,
    Focusable, KeyDownEvent, SharedString, Window, WindowBounds, WindowOptions,
};

use lattice::{AgentRow, Lattice, TriggerEvent};

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
            "enter" => self.spawn(cx),
            _ => {
                if let Some(ch) = &event.keystroke.key_char {
                    // A real message, not a slug: letters/digits plus the
                    // punctuation a sentence like "create an agent that
                    // tells a joke, call it juan" actually needs.
                    if ch.chars().all(|c| c.is_ascii_alphanumeric() || " -,.!'".contains(c)) {
                        self.message.push_str(ch);
                    }
                }
            }
        }
        cx.notify();
    }

    fn spawn(&mut self, cx: &mut Context<Self>) {
        let message = std::mem::take(&mut self.message);
        self.lattice.update(cx, |l, cx| l.spawn_agent(message, cx));
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
        let triggers = state.triggers.clone();
        let spawning = state.spawning;
        let message_field = format!("{}_", self.message);

        div()
            .id("root")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xe0e0e0))
            .p_4()
            .gap_3()
            .child(div().text_xl().child(
                "holon — lattice console (platform-domain + comp-reconciler, no wasmCloud/k8s)",
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().text_color(rgb(0x999999)).child("message:"))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .min_w(px(420.0))
                            .rounded_md()
                            .bg(rgb(0x111111))
                            .border_1()
                            .border_color(rgb(0x444444))
                            .child(message_field),
                    )
                    .child(
                        div()
                            .id("spawn")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(if spawning { rgb(0x555555) } else { rgb(0x2d6a4f) })
                            .cursor_pointer()
                            .child(if spawning {
                                "spawning…"
                            } else {
                                "+ spawn (or press enter)"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.spawn(cx))),
                    )
                    .child(
                        div()
                            .id("refresh")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(0x333366))
                            .cursor_pointer()
                            .child("refresh")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.lattice.update(cx, |l, cx| l.refresh(cx));
                            })),
                    ),
            )
            .child(div().text_sm().text_color(rgb(0x999999)).child(status_line))
            .child(
                div()
                    .id("panes")
                    .flex()
                    .flex_row()
                    .gap_3()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(
                        // Left: what was typed, and which agent it spawned —
                        // kept around after the status line moves on, so
                        // "what triggered this agent" is never lost.
                        div()
                            .id("trigger-list")
                            .flex()
                            .flex_col()
                            .gap_2()
                            .w(px(260.0))
                            .flex_shrink_0()
                            .overflow_scroll()
                            .child(div().text_sm().text_color(rgb(0x999999)).child("triggers"))
                            .children(triggers.into_iter().map(render_trigger_row)),
                    )
                    .child(
                        div()
                            .id("agent-list")
                            .flex()
                            .flex_col()
                            .gap_2()
                            .flex_1()
                            .overflow_scroll()
                            .child(div().text_sm().text_color(rgb(0x999999)).child("agents"))
                            .children(agents.into_iter().map(|a| render_agent_row(a, cx))),
                    ),
            )
    }
}

fn render_trigger_row(t: TriggerEvent) -> impl IntoElement {
    let status_color = if t.status == "live" {
        rgb(0x88cc88)
    } else if t.status.starts_with("failed") {
        rgb(0xcc8888)
    } else {
        rgb(0xcccc88)
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(rgb(0x252525))
        .child(div().text_sm().child(t.message))
        .child(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .text_xs()
                .child(div().text_color(rgb(0x999999)).child(format!("→ {}", t.agent_name)))
                .child(div().text_color(status_color).child(t.status)),
        )
}

fn render_agent_row(agent: AgentRow, cx: &Context<Console>) -> impl IntoElement {
    let id_for_click = agent.id.clone();
    div()
        .id(SharedString::from(agent.id.clone()))
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(rgb(0x2a2a2a))
        .child(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .child(div().child(format!("{}  ({})", agent.name, agent.id)))
                .child(div().text_color(rgb(0x88cc88)).child(agent.status.clone())),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .id(SharedString::from(format!("{}-ping", agent.id)))
                        .px_2()
                        .rounded_md()
                        .bg(rgb(0x444444))
                        .cursor_pointer()
                        .text_sm()
                        .child("ping")
                        .on_click({
                            let id = id_for_click.clone();
                            cx.listener(move |this, _, _, cx| {
                                let id = id.clone();
                                this.lattice.update(cx, |l, cx| l.ping_agent(id, cx));
                            })
                        }),
                )
                .child(div().text_sm().text_color(rgb(0xcccccc)).child(agent.last_output.clone())),
        )
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
        let bounds = Bounds::centered(None, size(px(1040.0), px(600.0)), cx);
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
