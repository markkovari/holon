use std::rc::Rc;

use gpui::{
    div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement,
    Render, SharedString, Window,
};

use super::agents::AgentInfo;
use super::field::{FieldEvent, TextField};
use super::icons::icon;
use super::theme::Theme;
use super::view::Actions;

#[derive(Clone, Debug, PartialEq)]
pub struct KeyRow {
    pub name: String,
    pub reference: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewKey {
    pub name: String,
    pub host: String,
    pub key: String,
    pub header: String,
}

pub struct KeysView {
    actions: Rc<dyn Actions>,
    theme: Theme,
    rows: Vec<KeyRow>,
    roster: Vec<AgentInfo>,
    fields: [Entity<TextField>; 4],
    problem: Option<String>,
    arming: Option<String>,
}

impl KeysView {
    pub fn new(actions: Rc<dyn Actions>, theme: Theme, cx: &mut Context<Self>) -> Self {
        let mut make = |label: &str, masked: bool| {
            let label = label.to_string();
            let f = cx.new(|cx| {
                if masked {
                    TextField::masked(label, theme, cx)
                } else {
                    TextField::compact(label, theme, cx)
                }
            });
            cx.subscribe(&f, |this, _, ev: &FieldEvent, cx| {
                if let FieldEvent::Submit(_) = ev {
                    this.add(cx)
                }
            })
            .detach();
            f
        };
        let fields = [
            make("Name, e.g. github", false),
            make("Host, e.g. api.github.com", false),
            make("The key", true),
            make("Header, if not Authorization: Bearer", false),
        ];
        Self {
            actions,
            theme,
            rows: Vec::new(),
            roster: Vec::new(),
            fields,
            problem: None,
            arming: None,
        }
    }

    pub fn count(&self) -> usize {
        self.rows.len()
    }

    pub fn first_field(&self, cx: &App) -> FocusHandle {
        self.fields[0].focus_handle(cx)
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        for f in &self.fields {
            f.update(cx, |f, cx| f.set_theme(theme, cx));
        }
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.roster = self.actions.roster(cx);
        match self.actions.keys(cx) {
            Ok(rows) => self.rows = rows,
            Err(e) => {
                self.rows.clear();
                self.problem = Some(e);
            }
        }
        cx.notify();
    }

    fn done(&mut self, r: Result<(), String>, cx: &mut Context<Self>) {
        self.arming = None;
        self.problem = r.err();
        if self.problem.is_none() {
            self.refresh(cx);
        }
        cx.notify();
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let text = |i: usize, cx: &App| self.fields[i].read(cx).text().trim().to_string();
        let k =
            NewKey { name: text(0, cx), host: text(1, cx), key: text(2, cx), header: text(3, cx) };
        let actions = self.actions.clone();
        let r = actions.put_key(&k, cx);
        if r.is_ok() {
            for f in &self.fields {
                f.update(cx, |f, cx| f.clear(cx));
            }
        }
        self.done(r, cx);
    }

    fn button(
        &self,
        id: String,
        label: &str,
        strong: bool,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        let t = self.theme;
        div()
            .id(SharedString::from(id))
            .h(px(30.0))
            .px(px(12.0))
            .flex()
            .flex_none()
            .items_center()
            .rounded(px(8.0))
            .text_size(px(13.0))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .when(strong, |el| el.bg(t.line).text_color(t.text))
            .when(!strong, |el| el.bg(t.tile).text_color(t.muted).hover(|s| s.text_color(t.text)))
            .child(label.to_string())
            .on_click(cx.listener(move |this, _, _, cx| on(this, cx)))
    }

    fn card(&self) -> gpui::Div {
        let t = self.theme;
        div()
            .p(px(16.0))
            .rounded(px(12.0))
            .bg(t.card)
            .border_1()
            .border_color(t.card_border)
            .flex()
            .flex_col()
            .gap(px(10.0))
    }

    fn row(&self, k: &KeyRow, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let arming = self.arming.as_deref() == Some(k.name.as_str());
        let mut agents = div().flex().flex_row().flex_wrap().gap(px(6.0));
        for a in &self.roster {
            let on = a.api_keys.contains(&k.name);
            let (key, agent) = (k.name.clone(), a.name.clone());
            agents = agents.child(
                div()
                    .id(SharedString::from(format!("grant-{}-{}", k.name, a.name)))
                    .h(px(26.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .rounded(px(13.0))
                    .border_1()
                    .text_size(px(12.5))
                    .cursor_pointer()
                    .when(on, |el| el.bg(t.line).border_color(t.line).text_color(t.text))
                    .when(!on, |el| {
                        el.border_color(t.tile_border)
                            .text_color(t.muted)
                            .hover(|s| s.border_color(t.line).text_color(t.text))
                    })
                    .when(on, |el| el.child(icon("check").size(px(12.0)).text_color(t.text)))
                    .child(a.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let actions = this.actions.clone();
                        let r = actions.grant(&key, &agent, !on, cx);
                        this.done(r, cx);
                    })),
            );
        }
        let name = k.name.clone();
        self.card()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(icon("key-round").size(px(16.0)).text_color(t.line))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(14.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(k.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_family(crate::MONO)
                                    .text_color(t.faint)
                                    .child(k.reference.clone()),
                            ),
                    )
                    .child(self.button(
                        format!("remove-{}", k.name),
                        if arming { "Click again to remove" } else { "Remove" },
                        false,
                        cx,
                        move |this, cx| {
                            if this.arming.as_deref() == Some(name.as_str()) {
                                let actions = this.actions.clone();
                                let r = actions.remove_key(&name, cx);
                                this.done(r, cx);
                            } else {
                                this.arming = Some(name.clone());
                                cx.notify();
                            }
                        },
                    )),
            )
            .child(div().text_size(px(12.5)).text_color(t.faint).child(if self.roster.is_empty() {
                "No agents yet."
            } else {
                "Switch it on for the agents that may use it:"
            }))
            .child(agents)
    }
}

impl Render for KeysView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let rows: Vec<gpui::AnyElement> =
            self.rows.clone().iter().map(|k| self.row(k, cx).into_any_element()).collect();
        let pair = |a: &Entity<TextField>, b: &Entity<TextField>| {
            div()
                .flex()
                .flex_row()
                .gap(px(10.0))
                .child(div().flex_1().child(a.clone()))
                .child(div().flex_1().child(b.clone()))
        };
        let add = self
            .card()
            .child(div().text_size(px(14.5)).font_weight(FontWeight::MEDIUM).child("Add a key"))
            .child(pair(&self.fields[0], &self.fields[1]))
            .child(pair(&self.fields[2], &self.fields[3]))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .child(self.button("add-key".into(), "Add", true, cx, |this, cx| this.add(cx)))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(t.faint)
                            .child("It goes to the vault encrypted and is never shown again."),
                    ),
            );
        div()
            .id("keys")
            .size_full()
            .overflow_y_scroll()
            .px(px(40.0))
            .py(px(24.0))
            .flex()
            .flex_col()
            .gap(px(14.0))
            .max_w(px(820.0))
            .child(div().text_size(px(13.5)).text_color(t.muted).child(
                "An agent gets only the keys you switch on here, and each key is sent only to its own host. The agent sees [api key] in its place.",
            ))
            .children(self.problem.clone().map(|p| {
                div()
                    .px(px(12.0))
                    .py(px(9.0))
                    .rounded(px(9.0))
                    .bg(t.deny)
                    .text_color(t.deny_ink)
                    .text_size(px(13.0))
                    .child(p)
            }))
            .child(add)
            .children(rows)
    }
}
