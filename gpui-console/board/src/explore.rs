//! One thing at a time, and what it is joined to: the Explore tab.
//!
//! The subject is in the middle. Above it is where it came from (the goal run that added it, the attempt
//! that wrote it), left what it builds on, right what builds on it, below what it led to and where it
//! appeared (a run's attempts, the sessions that touched it). A lane shows a few of its neighbours and
//! says how many more; any neighbour is a way on, and the trail of where a person has been is a way back.
//! A second level says, under each neighbour, a little of what is behind it, without drawing it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    canvas, div, img, point, prelude::*, px, Bounds, Context, EventEmitter, FontWeight,
    IntoElement, ObjectFit, PathBuilder, Pixels, Render, SharedString, StyledImage, Window,
};

use super::graph::{Graph, Lane, Side};
use super::icons::icon;
use super::map::{Key, Kind, Proof};
use super::theme::Theme;
use super::tree::Status;

/// How many of a lane's neighbours are shown before "+N more".
const SHOWN: usize = 6;
const CHIP_W: f32 = 210.0;

/// What Explore asks of the board: show this in the map, or this session on its own tab.
pub enum ExploreEvent {
    InMap(Key),
    Session(String),
}

pub struct ExploreView {
    graph: Graph,
    subject: Option<Key>,
    /// Where a person has been, oldest first; the subject is the last.
    trail: Vec<Key>,
    expanded: HashSet<&'static str>,
    deep: bool,
    theme: Theme,
    root: PathBuf,
    /// Where each chip and the subject were laid out, for the lines between them.
    placed: Rc<RefCell<HashMap<Key, Bounds<Pixels>>>>,
}

impl EventEmitter<ExploreEvent> for ExploreView {}

/// The colour a kind of thing is marked with, wherever it is listed.
pub fn tint(t: &Theme, kind: Kind) -> gpui::Rgba {
    match kind {
        Kind::App => t.badge(Status::Running).1,
        Kind::Probe => t.faint,
        Kind::Run | Kind::Session => t.badge(Status::NeedsYou).1,
        Kind::Attempt => t.badge(Status::Done).1,
        Kind::Missed | Kind::Lesson => t.badge(Status::Failed).1,
        Kind::Part => t.muted,
    }
}

/// What a kind is called in a sentence.
pub fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::App => "application",
        Kind::Part => "component",
        Kind::Probe => "probe",
        Kind::Run => "goal run",
        Kind::Attempt => "attempt that passed",
        Kind::Missed => "attempt that failed",
        Kind::Lesson => "lesson",
        Kind::Session => "session",
    }
}

impl ExploreView {
    pub fn new(theme: Theme) -> Self {
        Self {
            graph: Graph::default(),
            subject: None,
            trail: Vec::new(),
            expanded: HashSet::new(),
            deep: false,
            theme,
            root: crate::repo_root(),
            placed: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn subject(&self) -> Option<&Key> {
        self.subject.as_ref()
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    pub fn set_graph(&mut self, graph: Graph, cx: &mut Context<Self>) {
        self.graph = graph;
        cx.notify();
    }

    /// Looks at `key`: onto the end of the trail, or back to where it already is on it.
    pub fn show(&mut self, key: &str, cx: &mut Context<Self>) {
        match self.trail.iter().position(|k| k == key) {
            Some(i) => self.trail.truncate(i + 1),
            None => self.trail.push(key.to_string()),
        }
        // a trail longer than a person can read back is only its last few steps
        if self.trail.len() > 6 {
            self.trail.drain(..self.trail.len() - 6);
        }
        self.subject = Some(key.to_string());
        self.expanded.clear();
        cx.notify();
    }

    fn node_tint(&self, kind: Kind) -> gpui::Rgba {
        tint(&self.theme, kind)
    }

    /// Records where `el` was laid out under `key`, so a line can be drawn to it.
    fn tracked(&self, key: Key, el: impl IntoElement) -> gpui::AnyElement {
        let placed = self.placed.clone();
        div()
            .relative()
            .child(el)
            .child(
                canvas(
                    move |b, _, _| {
                        placed.borrow_mut().insert(key.clone(), b);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .into_any_element()
    }

    /// One neighbour: what it is, and how much is behind it; a way on.
    fn chip(&self, key: &Key, side: Side, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let g = &self.graph;
        let n = g.nodes.get(key);
        let label = g.label(key);
        let kind = n.map_or(Kind::Part, |n| n.kind);
        let peek: Vec<String> = if self.deep {
            g.lanes(key)
                .iter()
                .flat_map(|l| l.keys.iter())
                .filter(|k| Some(*k) != self.subject.as_ref())
                .take(3)
                .map(|k| g.label(k))
                .collect()
        } else {
            Vec::new()
        };
        let to = key.clone();
        let chip = div()
            .id(SharedString::from(format!("chip-{side:?}-{key}")))
            .w(px(CHIP_W))
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(10.0))
            .bg(t.card)
            .border_1()
            .border_color(t.card_border)
            .hover(|s| s.border_color(t.line))
            .cursor_pointer()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div().size(px(7.0)).flex_none().rounded(px(4.0)).bg(self.node_tint(kind)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(13.0))
                            .line_height(px(18.0))
                            .text_color(t.text)
                            .truncate()
                            .child(label),
                    ),
            )
            .when(!peek.is_empty(), |c| {
                c.child(
                    div()
                        .pl(px(15.0))
                        .text_size(px(11.0))
                        .line_height(px(15.0))
                        .text_color(t.faint)
                        .truncate()
                        .child(format!("→ {}", peek.join(", "))),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| this.show(&to, cx)));
        chip.into_any_element()
    }

    /// A lane: what joins its neighbours to the subject, how many, the first few, and the rest on asking.
    fn lane(&self, lane: &Lane, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let open = self.expanded.contains(lane.title);
        let shown = if open { lane.keys.len() } else { lane.keys.len().min(SHOWN) };
        let rest = lane.keys.len() - shown;
        let horizontal = matches!(lane.side, Side::Above | Side::Below);
        let title = lane.title;
        let mut chips = div().flex().gap(px(8.0));
        chips = if horizontal {
            chips.flex_row().flex_wrap().justify_center()
        } else {
            chips.flex_col()
        };
        for k in lane.keys.iter().take(shown) {
            chips = chips.child(self.chip(k, lane.side, cx));
        }
        let more = (rest > 0 || open && lane.keys.len() > SHOWN).then(|| {
            div()
                .id(SharedString::from(format!("more-{title}")))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(8.0))
                .text_size(px(12.0))
                .text_color(t.muted)
                .cursor_pointer()
                .hover(|s| s.text_color(t.text).bg(t.row_hover))
                .child(if open { "fewer".to_string() } else { format!("+{rest} more") })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.expanded.remove(title) {
                        this.expanded.insert(title);
                    }
                    cx.notify();
                }))
        });
        let block = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .when(horizontal, |c| c.items_center())
            .when(lane.side == Side::Left, |c| c.items_end())
            .child(
                div()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.faint)
                    .child(format!("{title} · {}", lane.keys.len())),
            )
            .child(chips)
            .children(more);
        self.tracked(format!("lane:{title}"), block)
    }

    /// The subject: what it is, a few facts, and what proves it.
    fn subject_card(&self, key: &Key, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let n = self.graph.nodes.get(key);
        let (label, sub, kind) = n.map_or_else(
            || (self.graph.label(key), String::new(), Kind::Part),
            |n| (n.label.clone(), n.sub.clone(), n.kind),
        );
        let proof = n.map(|n| n.proof.clone()).unwrap_or_default();
        let mut proofs = div().flex().flex_col().gap(px(6.0));
        for (i, p) in proof.into_iter().enumerate() {
            let root = self.root.clone();
            proofs = match p {
                Proof::Media(path) => {
                    let full = root.join(&path);
                    proofs.child(
                        div()
                            .id(SharedString::from(format!("proof-media-{i}")))
                            .rounded(px(10.0))
                            .overflow_hidden()
                            .border_1()
                            .border_color(t.card_border)
                            .bg(t.bg)
                            .cursor_pointer()
                            .child(
                                img(full.clone())
                                    .w_full()
                                    .h(px(180.0))
                                    .object_fit(ObjectFit::Contain),
                            )
                            .on_click(move |_, _, cx| cx.open_with_system(&full)),
                    )
                }
                Proof::Check { id, passed, detail } => proofs.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(6.0))
                        .items_start()
                        .child(
                            icon(if passed { "check" } else { "circle-x" })
                                .size(px(13.0))
                                .flex_none()
                                .text_color(if passed {
                                    t.badge(Status::Done).1
                                } else {
                                    t.badge(Status::Failed).1
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w(px(0.0))
                                .child(div().text_size(px(12.5)).text_color(t.text).child(id))
                                .when(!detail.is_empty(), |c| {
                                    c.child(
                                        div()
                                            .font_family(crate::MONO)
                                            .text_size(px(10.5))
                                            .text_color(t.faint)
                                            .line_clamp(3)
                                            .child(detail),
                                    )
                                }),
                        ),
                ),
                Proof::Link { url, what } => proofs.child(
                    div()
                        .id(SharedString::from(format!("proof-link-{i}")))
                        .flex()
                        .flex_row()
                        .gap(px(6.0))
                        .items_center()
                        .cursor_pointer()
                        .child(icon("git-fork").size(px(13.0)).text_color(t.line))
                        .child(div().text_size(px(12.5)).text_color(t.text).truncate().child(what))
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                ),
                Proof::File { path, what } => {
                    let full = root.join(&path);
                    proofs.child(
                        div()
                            .id(SharedString::from(format!("proof-file-{i}")))
                            .flex()
                            .flex_row()
                            .gap(px(6.0))
                            .items_center()
                            .cursor_pointer()
                            .child(icon("file-text").size(px(13.0)).text_color(t.muted))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(t.muted)
                                    .truncate()
                                    .child(what),
                            )
                            .on_click(move |_, _, cx| cx.open_with_system(&full)),
                    )
                }
            };
        }
        let action = match kind {
            Kind::Session => Some((
                "Open the session",
                ExploreEvent::Session(key.strip_prefix("session:").unwrap_or(key).to_string()),
            )),
            Kind::App | Kind::Part | Kind::Probe => {
                Some(("Show in the whole system", ExploreEvent::InMap(key.clone())))
            }
            _ => None,
        };
        let card = div()
            .w(px(340.0))
            .p(px(18.0))
            .rounded(px(16.0))
            .bg(t.card)
            .border_1()
            .border_color(t.line)
            .shadow_md()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(7.0))
                    .child(div().size(px(8.0)).rounded(px(4.0)).bg(self.node_tint(kind)))
                    .child(div().text_size(px(11.5)).text_color(t.faint).child(kind_name(kind))),
            )
            .child(
                div()
                    .text_size(px(20.0))
                    .line_height(px(26.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text)
                    .line_clamp(2)
                    .child(label),
            )
            .when(!sub.is_empty(), |c| {
                c.child(
                    div()
                        .font_family(crate::MONO)
                        .text_size(px(11.5))
                        .text_color(t.muted)
                        .line_clamp(2)
                        .child(sub),
                )
            })
            .child(proofs)
            .children(action.map(|(what, ev)| {
                let ev = RefCell::new(Some(ev));
                div()
                    .id("subject-action")
                    .mt(px(4.0))
                    .h(px(28.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(t.tile_border)
                    .bg(t.tile)
                    .text_size(px(12.5))
                    .text_color(t.text)
                    .cursor_pointer()
                    .hover(|s| s.border_color(t.line))
                    .child(what)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        let e = match ev.borrow().as_ref() {
                            Some(ExploreEvent::InMap(k)) => ExploreEvent::InMap(k.clone()),
                            Some(ExploreEvent::Session(r)) => ExploreEvent::Session(r.clone()),
                            None => return,
                        };
                        cx.emit(e);
                    }))
            }));
        self.tracked(key.clone(), card)
    }

    /// Where a person has been: each step a way back.
    fn trail_el(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let mut row = div().flex().flex_row().items_center().gap(px(4.0)).min_w(px(0.0));
        let last = self.trail.len().saturating_sub(1);
        for (i, k) in self.trail.iter().enumerate() {
            if i > 0 {
                row = row.child(div().text_size(px(12.5)).text_color(t.faint).child("›"));
            }
            let to = k.clone();
            row = row.child(
                div()
                    .id(SharedString::from(format!("trail-{i}")))
                    .px(px(6.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .text_size(px(12.5))
                    .text_color(if i == last { t.text } else { t.muted })
                    .when(i != last, |c| c.cursor_pointer().hover(|s| s.bg(t.row_hover)))
                    .truncate()
                    .child(self.graph.label(k))
                    .on_click(cx.listener(move |this, _, _, cx| this.show(&to, cx))),
            );
        }
        row
    }
}

impl Render for ExploreView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let Some(subject) = self.subject.clone().filter(|k| self.graph.nodes.contains_key(k))
        else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(icon("search").size(px(24.0)).text_color(t.faint))
                .child(
                    div()
                        .text_size(px(15.0))
                        .text_color(t.muted)
                        .child("Pick something to look at"),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(t.faint)
                        .child("Search on the left, or start from something recent."),
                )
                .into_any_element();
        };
        self.placed.borrow_mut().clear();
        let lanes = self.graph.lanes(&subject);
        let on = |side: Side| lanes.iter().filter(|l| l.side == side).collect::<Vec<_>>();
        let column =
            |els: Vec<gpui::AnyElement>| div().flex().flex_col().gap(px(22.0)).children(els);
        // above and below, lanes side by side, so a line to one never crosses another
        let row = |els: Vec<gpui::AnyElement>| {
            div().flex().flex_row().items_start().justify_center().gap(px(40.0)).children(els)
        };
        let above: Vec<gpui::AnyElement> =
            on(Side::Above).into_iter().map(|l| self.lane(l, cx)).collect();
        let left: Vec<gpui::AnyElement> =
            on(Side::Left).into_iter().map(|l| self.lane(l, cx)).collect();
        let right: Vec<gpui::AnyElement> =
            on(Side::Right).into_iter().map(|l| self.lane(l, cx)).collect();
        let below: Vec<gpui::AnyElement> =
            on(Side::Below).into_iter().map(|l| self.lane(l, cx)).collect();
        let card = self.subject_card(&subject, cx);

        // one line per lane, from the lane's edge that faces the subject to the subject's edge that faces it
        let joins: Vec<(Key, Side)> =
            lanes.iter().map(|l| (format!("lane:{}", l.title), l.side)).collect();
        let placed = self.placed.clone();
        let line: gpui::Hsla = t.line.into();
        let wires = canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let at = placed.borrow();
                let Some(s) = at.get(&subject) else { return };
                let (sx, sy, sw, sh) = (
                    f32::from(s.origin.x),
                    f32::from(s.origin.y),
                    f32::from(s.size.width),
                    f32::from(s.size.height),
                );
                for (k, side) in &joins {
                    let Some(b) = at.get(k) else { continue };
                    let (bx, by, bw, bh) = (
                        f32::from(b.origin.x),
                        f32::from(b.origin.y),
                        f32::from(b.size.width),
                        f32::from(b.size.height),
                    );
                    // from the neighbour's edge that faces the subject, to the subject's edge that faces it
                    let (from, to, c1, c2) = match side {
                        Side::Left => {
                            let (a, z) = ((bx + bw, by + bh / 2.0), (sx, sy + sh / 2.0));
                            let d = (z.0 - a.0) / 2.0;
                            (a, z, (a.0 + d, a.1), (z.0 - d, z.1))
                        }
                        Side::Right => {
                            let (a, z) = ((bx, by + bh / 2.0), (sx + sw, sy + sh / 2.0));
                            let d = (a.0 - z.0) / 2.0;
                            (a, z, (a.0 - d, a.1), (z.0 + d, z.1))
                        }
                        Side::Above => {
                            let (a, z) = ((bx + bw / 2.0, by + bh), (sx + sw / 2.0, sy));
                            let d = (z.1 - a.1) / 2.0;
                            (a, z, (a.0, a.1 + d), (z.0, z.1 - d))
                        }
                        Side::Below => {
                            let (a, z) = ((bx + bw / 2.0, by), (sx + sw / 2.0, sy + sh));
                            let d = (a.1 - z.1) / 2.0;
                            (a, z, (a.0, a.1 - d), (z.0, z.1 + d))
                        }
                    };
                    let p = |q: (f32, f32)| point(px(q.0), px(q.1));
                    let mut path = PathBuilder::stroke(px(2.0));
                    path.move_to(p(from));
                    path.cubic_bezier_to(p(to), p(c1), p(c2));
                    if let Ok(path) = path.build() {
                        window.paint_path(path, line.alpha(0.55));
                    }
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let deep = self.deep;
        let toggle = |id: &'static str, label: &'static str, on: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(6.0))
                .text_size(px(12.0))
                .text_color(if on { t.text } else { t.muted })
                .when(on, |c| c.bg(t.tile))
                .cursor_pointer()
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.deep = id == "depth-2";
                    cx.notify();
                }))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(28.0))
                    .py(px(10.0))
                    .border_b_1()
                    .border_color(t.card_border)
                    .child(div().flex_1().min_w(px(0.0)).child(self.trail_el(cx)))
                    .child(div().text_size(px(12.0)).text_color(t.faint).child("depth"))
                    .child(toggle("depth-1", "1", !deep, cx))
                    .child(toggle("depth-2", "2", deep, cx)),
            )
            .child(
                div()
                    .id("explore")
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .child(wires)
                    .child(
                        div()
                            .min_h_full()
                            .py(px(36.0))
                            .px(px(24.0))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(44.0))
                            .when(!above.is_empty(), |c| c.child(row(above)))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(84.0))
                                    .child(column(left).items_end().w(px(CHIP_W)))
                                    .child(card)
                                    .child(column(right).w(px(CHIP_W))),
                            )
                            .when(!below.is_empty(), |c| c.child(row(below))),
                    ),
            )
            .into_any_element()
    }
}
