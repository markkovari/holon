use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    canvas, div, fill, img, point, prelude::*, px, size, App, Bounds, Context, EventEmitter,
    FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, PathBuilder, Pixels, Render, Rgba, ScrollWheelEvent,
    SharedString, StyledImage, Window,
};

use super::icons::icon;
use super::map::{self, Key, Kind, Map, Placed, Proof, Rect, Ref, Tab};
use super::theme::Theme;
use super::tree::Status;

const MIN_ZOOM: f32 = 0.06;
const MAX_ZOOM: f32 = 2.5;
const FIT_MAX: f32 = 1.1;
const READABLE: f32 = 0.62;
const OVERVIEW: f32 = 0.5;
const PANEL_W: f32 = 320.0;

type Drag = ((f32, f32), (f32, f32), bool);
type End = ((f32, f32), Side);

pub enum MapEvent {
    Open(Ref),
}

pub struct MapView {
    title: SharedString,
    empty: (SharedString, SharedString),
    map: Map,
    placed: Vec<Placed>,
    pan: (f32, f32),
    zoom: f32,
    selected: Option<(usize, usize)>,
    hover: Option<(usize, usize)>,
    bundles: Vec<(usize, usize, usize)>,
    within: Vec<usize>,
    cols: Vec<usize>,
    drag: Option<Drag>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    fitted: bool,
    pending: Option<(Key, Option<Key>)>,
    problem: Option<String>,
    theme: Theme,
    focus: FocusHandle,
    root: PathBuf,
}

impl EventEmitter<MapEvent> for MapView {}

impl Focusable for MapView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

pub fn hit(placed: &[Placed], x: f32, y: f32) -> Option<(usize, usize)> {
    placed.iter().enumerate().find_map(|(f, p)| {
        p.frame
            .contains(x, y)
            .then(|| p.items.iter().position(|r| r.contains(x, y)).map(|i| (f, i)))?
    })
}

pub fn frame_view(r: Rect, w: f32, h: f32) -> (f32, (f32, f32)) {
    let (zoom, pan) = fit(r, w, h);
    if zoom >= READABLE {
        return (zoom, pan);
    }
    (READABLE, (32.0 - r.x * READABLE, 32.0 - r.y * READABLE))
}

pub fn fit(r: Rect, w: f32, h: f32) -> (f32, (f32, f32)) {
    if r.w <= 0.0 || r.h <= 0.0 {
        return (1.0, (0.0, 0.0));
    }
    let zoom = ((w / r.w).min(h / r.h) * 0.9).clamp(MIN_ZOOM, FIT_MAX);
    let (cx, cy) = r.centre();
    (zoom, (w / 2.0 - cx * zoom, h / 2.0 - cy * zoom))
}

impl MapView {
    pub fn new(
        title: impl Into<SharedString>,
        empty: (impl Into<SharedString>, impl Into<SharedString>),
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            title: title.into(),
            empty: (empty.0.into(), empty.1.into()),
            map: Map::default(),
            placed: Vec::new(),
            pan: (0.0, 0.0),
            zoom: 1.0,
            selected: None,
            hover: None,
            bundles: Vec::new(),
            within: Vec::new(),
            cols: Vec::new(),
            drag: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
            fitted: false,
            pending: None,
            problem: None,
            theme,
            focus: cx.focus_handle(),
            root: crate::repo_root(),
        }
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    pub fn map(&self) -> &Map {
        &self.map
    }

    pub fn set_map(&mut self, map: Map, cx: &mut Context<Self>) {
        let keep = self.selected_item().map(|(f, i)| (f.key.clone(), i.key.clone()));
        self.placed = map::layout(&map);
        (self.bundles, self.within) = map::bundles(&map);
        self.cols = map::columns(map.frames.len(), &self.bundles);
        self.hover = None;
        self.map = map;
        self.selected = keep.and_then(|(fk, ik)| {
            let f = self.map.frames.iter().position(|f| f.key == fk)?;
            Some((f, self.map.frames[f].items.iter().position(|i| i.key == ik)?))
        });
        self.fitted = false;
        self.problem = None;
        cx.notify();
    }

    fn heaviest(&self) -> Vec<u32> {
        self.map
            .frames
            .iter()
            .map(|f| f.items.iter().map(|i| i.weight).max().unwrap_or(0))
            .collect()
    }

    pub fn set_problem(&mut self, problem: Option<String>, cx: &mut Context<Self>) {
        self.problem = problem;
        cx.notify();
    }

    fn selected_item(&self) -> Option<(&map::Frame, &map::Item)> {
        let (f, i) = self.selected?;
        let fr = self.map.frames.get(f)?;
        Some((fr, fr.items.get(i)?))
    }

    fn view_size(&self) -> (f32, f32) {
        let b = self.bounds.get();
        let panel = if self.selected.is_some() { PANEL_W } else { 0.0 };
        ((f32::from(b.size.width) - panel).max(1.0), f32::from(b.size.height).max(1.0))
    }

    pub fn fit_all(&mut self, cx: &mut Context<Self>) {
        let (w, h) = self.view_size();
        (self.zoom, self.pan) = fit(map::bounds(&self.placed), w, h);
        self.fitted = true;
        cx.notify();
    }

    pub fn focus_frame(&mut self, f: usize, cx: &mut Context<Self>) {
        if let Some(p) = self.placed.get(f) {
            let (w, h) = self.view_size();
            let (zoom, pan) = frame_view(p.frame, w, h);
            (self.zoom, self.pan) = (zoom, pan);
            self.fitted = true;
            cx.notify();
        }
    }

    pub fn focus_key(&mut self, key: &str, frame: Option<&str>, cx: &mut Context<Self>) {
        let found = match frame {
            Some(fk) => self.map.frames.iter().position(|f| f.key == fk).and_then(|f| {
                self.map.frames[f].items.iter().position(|i| i.key == key).map(|i| (f, i))
            }),
            None => None,
        }
        .or_else(|| self.map.find(key));
        let Some((f, i)) = found else {
            self.pending = Some((key.to_string(), frame.map(str::to_string)));
            return;
        };
        self.selected = Some((f, i));
        if f32::from(self.bounds.get().size.width) < 1.0 {
            self.pending = Some((key.to_string(), frame.map(str::to_string)));
            return cx.notify();
        }
        let (w, h) = self.view_size();
        self.zoom = self.zoom.clamp(1.0, FIT_MAX);
        let (x, y) = self.placed[f].items[i].centre();
        self.pan = (w / 2.0 - x * self.zoom, h / 2.0 - y * self.zoom);
        self.fitted = true;
        cx.notify();
    }

    fn show_connections(&mut self, cx: &mut Context<Self>) {
        let Some((sf, si)) = self.selected else { return };
        let key = self.map.frames[sf].items[si].key.clone();
        let mut r = self.placed[sf].items[si];
        for (f, fr) in self.map.frames.iter().enumerate() {
            for (i, it) in fr.items.iter().enumerate() {
                if self
                    .map
                    .edges
                    .iter()
                    .any(|e| (e.0 == key && e.1 == it.key) || (e.1 == key && e.0 == it.key))
                {
                    r = r.union(&self.placed[f].items[i]);
                }
            }
        }
        let (w, h) = self.view_size();
        (self.zoom, self.pan) = fit(r, w, h);
        cx.notify();
    }

    fn zoom_about(&mut self, factor: f32, at: (f32, f32), cx: &mut Context<Self>) {
        let z = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let (wx, wy) = ((at.0 - self.pan.0) / self.zoom, (at.1 - self.pan.1) / self.zoom);
        self.zoom = z;
        self.pan = (at.0 - wx * z, at.1 - wy * z);
        cx.notify();
    }

    fn local(&self, p: gpui::Point<Pixels>) -> (f32, f32) {
        let o = self.bounds.get().origin;
        (f32::from(p.x - o.x), f32::from(p.y - o.y))
    }

    fn to_screen(&self, r: &Rect) -> Rect {
        Rect {
            x: r.x * self.zoom + self.pan.0,
            y: r.y * self.zoom + self.pan.1,
            w: r.w * self.zoom,
            h: r.h * self.zoom,
        }
    }

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.drag = Some((self.local(ev.position), self.pan, false));
        cx.notify();
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((start, pan, moved)) = self.drag else {
            let (x, y) = self.local(ev.position);
            let over = if self.zoom < OVERVIEW {
                None
            } else {
                hit(&self.placed, (x - self.pan.0) / self.zoom, (y - self.pan.1) / self.zoom)
            };
            if over != self.hover {
                self.hover = over;
                cx.notify();
            }
            return;
        };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let at = self.local(ev.position);
        let (dx, dy) = (at.0 - start.0, at.1 - start.1);
        let moved = moved || dx.abs() + dy.abs() > 3.0;
        if moved {
            self.pan = (pan.0 + dx, pan.1 + dy);
            cx.notify();
        }
        self.drag = Some((start, pan, moved));
    }

    fn on_up(&mut self, ev: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((_, _, moved)) = self.drag.take() else { return };
        if !moved {
            let (x, y) = self.local(ev.position);
            let (w, _) = self.view_size();
            if self.selected.is_some() && x > w {
                return; // on the panel
            }
            let world = ((x - self.pan.0) / self.zoom, (y - self.pan.1) / self.zoom);
            if self.zoom < OVERVIEW {
                if let Some(f) = self.placed.iter().position(|p| p.frame.contains(world.0, world.1))
                {
                    return self.focus_frame(f, cx);
                }
            }
            self.selected = hit(&self.placed, world.0, world.1);
        }
        cx.notify();
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let d = ev.delta.pixel_delta(px(16.0));
        let (dx, dy) = (f32::from(d.x), f32::from(d.y));
        if ev.modifiers.platform || ev.modifiers.control {
            self.zoom_about((dy * 0.004).exp(), self.local(ev.position), cx);
        } else {
            self.pan = (self.pan.0 + dx, self.pan.1 + dy);
            cx.notify();
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (w, h) = self.view_size();
        match ev.keystroke.key.as_str() {
            "escape" => self.selected = None,
            "0" => return self.fit_all(cx),
            "=" | "+" => return self.zoom_about(1.25, (w / 2.0, h / 2.0), cx),
            "-" => return self.zoom_about(0.8, (w / 2.0, h / 2.0), cx),
            _ => return,
        }
        cx.notify();
    }

    fn tint(&self, kind: Kind) -> Rgba {
        let t = &self.theme;
        match kind {
            Kind::App => t.badge(Status::Running).1,
            Kind::Probe => t.faint,
            Kind::Run => t.badge(Status::NeedsYou).1,
            Kind::Attempt => t.badge(Status::Done).1,
            Kind::Missed => t.badge(Status::Failed).1,
            Kind::Session => t.badge(Status::NeedsYou).1,
            Kind::Agent => t.line,
            Kind::Lesson => t.badge(Status::Failed).1,
            Kind::Part => t.muted,
        }
    }

    fn item_el(&self, item: &map::Item, r: Rect, state: ItemState, heat: f32) -> gpui::AnyElement {
        let t = self.theme;
        let z = self.zoom;
        let tint = self.tint(item.kind);
        let warm: gpui::Hsla = {
            let h: gpui::Hsla = t.line.into();
            h.alpha(0.08 + 0.32 * heat)
        };
        let edge = match state {
            ItemState::Selected | ItemState::Joined => t.line,
            ItemState::Plain => t.card_border,
        };
        let base = div()
            .absolute()
            .left(px(r.x))
            .top(px(r.y))
            .w(px(r.w))
            .h(px(r.h))
            .rounded(px(9.0 * z))
            .bg(t.card)
            .border(px(if state == ItemState::Selected { 2.0 } else { 1.0 }))
            .border_color(edge);
        if z < READABLE {
            let tile = base
                .when(heat > 0.0, |el| el.bg(warm))
                .child(div().absolute().left_0().top_0().bottom_0().w(px(3.0)).bg(tint));
            return if r.w >= 56.0 {
                tile.flex()
                    .items_center()
                    .pl(px(7.0))
                    .pr(px(3.0))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(t.muted)
                            .truncate()
                            .child(item.label.clone()),
                    )
                    .into_any_element()
            } else {
                tile.into_any_element()
            };
        }
        base.flex()
            .flex_row()
            .items_center()
            .gap(px(8.0 * z))
            .px(px(10.0 * z))
            .child(div().size(px(7.0 * z)).flex_none().rounded(px(4.0 * z)).bg(tint))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .text_size(px(12.5 * z))
                            .line_height(px(16.0 * z))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.text)
                            .truncate()
                            .child(item.label.clone()),
                    )
                    .when(!item.sub.is_empty(), |c| {
                        c.child(
                            div()
                                .text_size(px(10.5 * z))
                                .line_height(px(13.0 * z))
                                .text_color(t.faint)
                                .truncate()
                                .child(item.sub.clone()),
                        )
                    }),
            )
            .when(item.proof.iter().any(|p| matches!(p, Proof::Media(_))), |row| {
                row.child(icon("eye").size(px(12.0 * z)).flex_none().text_color(t.line))
            })
            .when(item.weight > 0, |row| {
                row.child(
                    div()
                        .flex_none()
                        .text_size(px(11.0 * z))
                        .text_color(t.muted)
                        .child(item.weight.to_string()),
                )
            })
            .into_any_element()
    }

    fn frame_el(&self, f: &map::Frame, r: Rect, items: Vec<gpui::AnyElement>) -> gpui::AnyElement {
        let t = self.theme;
        let z = self.zoom;
        let readable = z >= READABLE;
        div()
            .absolute()
            .left(px(r.x))
            .top(px(r.y))
            .w(px(r.w))
            .h(px(r.h))
            .rounded(px(16.0 * z))
            .bg(t.sidebar)
            .border_1()
            .border_color(t.card_border)
            .child(
                div()
                    .absolute()
                    .left(px(if readable { map::PAD * z } else { 2.0 }))
                    .top(px(if readable { 12.0 * z } else { -22.0 }))
                    .right(px(if readable { map::PAD * z } else { 0.0 }))
                    .flex()
                    .when(readable, |c| c.flex_col().gap(px(2.0 * z)))
                    .when(!readable, |c| c.flex_row().items_baseline().gap(px(8.0)))
                    .child(
                        div()
                            .text_size(px(if readable { 15.0 * z } else { 13.0 }))
                            .line_height(px(if readable { 20.0 * z } else { 17.0 }))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .truncate()
                            .child(f.title.clone()),
                    )
                    .when(readable || r.w > 220.0, |c| {
                        c.child(
                            div()
                                .text_size(px(if readable { 12.0 * z } else { 11.5 }))
                                .line_height(px(if readable { 16.0 * z } else { 15.0 }))
                                .text_color(t.faint)
                                .truncate()
                                .child(f.sub.clone()),
                        )
                    }),
            )
            .children(items)
            .into_any_element()
    }

    fn panel(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let t = self.theme;
        let (frame, item) = self.selected_item()?;
        let key = item.key.clone();
        let label = |k: &str| k.split_once(':').map_or(k, |(_, n)| n).to_string();
        let uses: Vec<(String, String)> = self
            .map
            .edges
            .iter()
            .filter(|e| e.0 == key)
            .map(|e| (e.1.clone(), e.2.clone()))
            .collect();
        let used_by: Vec<(String, String)> = self
            .map
            .edges
            .iter()
            .filter(|e| e.1 == key)
            .map(|e| (e.0.clone(), e.2.clone()))
            .collect();
        let heading = |s: &str| {
            div()
                .mt(px(14.0))
                .mb(px(4.0))
                .text_size(px(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.faint)
                .child(s.to_string())
        };
        let row = |id: String, icon_name: &'static str, text: String, sub: String| {
            div()
                .id(SharedString::from(id))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .rounded(px(7.0))
                .cursor_pointer()
                .hover(|s| s.bg(t.row_hover))
                .child(icon(icon_name).size(px(13.0)).flex_none().text_color(t.muted))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.0))
                        .child(div().text_size(px(13.0)).text_color(t.text).truncate().child(text))
                        .when(!sub.is_empty(), |c| {
                            c.child(
                                div().text_size(px(11.0)).text_color(t.faint).truncate().child(sub),
                            )
                        }),
                )
        };
        let mut list = div().flex().flex_col();
        if !item.proof.is_empty() {
            list = list.child(heading("Proof"));
            for (n, p) in item.proof.iter().enumerate() {
                let root = self.root.clone();
                list = match p.clone() {
                    Proof::Media(path) => {
                        let full = root.join(&path);
                        list.child(
                            div()
                                .id(SharedString::from(format!("media-{n}")))
                                .mt(px(4.0))
                                .mb(px(6.0))
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .cursor_pointer()
                                .child(
                                    div()
                                        .rounded(px(10.0))
                                        .overflow_hidden()
                                        .border_1()
                                        .border_color(t.card_border)
                                        .bg(t.bg)
                                        .child(
                                            img(full.clone())
                                                .w_full()
                                                .h(px(168.0))
                                                .object_fit(ObjectFit::Contain),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(t.faint)
                                        .truncate()
                                        .child(path),
                                )
                                .on_click(move |_, _, cx| cx.open_with_system(&full)),
                        )
                    }
                    Proof::File { path, what } => {
                        let full = root.join(&path);
                        list.child(
                            row(format!("file-{n}"), "file-text", path, what)
                                .on_click(move |_, _, cx| cx.open_with_system(&full)),
                        )
                    }
                    Proof::Link { url, what } => list.child(
                        row(format!("link-{n}"), "git-fork", url.clone(), what)
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
                    Proof::Check { id, passed, detail } => list.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap(px(8.0))
                            .px(px(8.0))
                            .py(px(5.0))
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
                                    .child(div().text_size(px(13.0)).text_color(t.text).child(id))
                                    .when(!detail.is_empty(), |c| {
                                        c.child(
                                            div()
                                                .font_family(crate::MONO)
                                                .text_size(px(10.5))
                                                .text_color(t.faint)
                                                .line_clamp(4)
                                                .child(detail),
                                        )
                                    }),
                            ),
                    ),
                };
            }
        }
        {
            let r =
                Ref { tab: Tab::Explore, key: key.clone(), frame: None, label: item.label.clone() };
            list = list.child(
                row(
                    "explore-it".into(),
                    "search",
                    "Explore it".into(),
                    "where it came from, and what it is joined to".into(),
                )
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(MapEvent::Open(r.clone())))),
            );
        }
        for (title, rows) in [("Uses", uses), ("Used by", used_by)] {
            if rows.is_empty() {
                continue;
            }
            list = list.child(heading(&format!("{title} · {}", rows.len())));
            for (n, (k, why)) in rows.into_iter().take(40).enumerate() {
                let to = k.clone();
                list = list
                    .child(row(format!("{title}-{n}"), "hexagon", label(&k), why).on_click(
                        cx.listener(move |this, _, _, cx| this.focus_key(&to, None, cx)),
                    ));
            }
        }
        Some(
            div()
                .id("panel")
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .w(px(PANEL_W))
                .bg(t.popover)
                .border_l_1()
                .border_color(t.card_border)
                .occlude()
                .overflow_y_scroll()
                .p(px(16.0))
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().size(px(9.0)).rounded(px(5.0)).bg(self.tint(item.kind)))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .text_size(px(16.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.text)
                                .truncate()
                                .child(item.label.clone()),
                        )
                        .child(
                            div()
                                .id("panel-close")
                                .cursor_pointer()
                                .child(icon("x").size(px(15.0)).text_color(t.muted))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.selected = None;
                                    cx.notify();
                                })),
                        ),
                )
                .when(!item.sub.is_empty(), |c| {
                    c.child(
                        div()
                            .mt(px(4.0))
                            .font_family(crate::MONO)
                            .text_size(px(11.5))
                            .text_color(t.muted)
                            .child(item.sub.clone()),
                    )
                })
                .child(
                    div()
                        .mt(px(4.0))
                        .text_size(px(12.0))
                        .text_color(t.faint)
                        .child(format!("in {}", frame.title)),
                )
                .when(self.map.edges.iter().any(|e| e.0 == key || e.1 == key), |c| {
                    c.child(
                        div()
                            .id("show-connections")
                            .mt(px(10.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .gap(px(6.0))
                            .h(px(28.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(t.tile_border)
                            .bg(t.tile)
                            .cursor_pointer()
                            .hover(|s| s.border_color(t.line))
                            .text_size(px(12.5))
                            .text_color(t.text)
                            .child(icon("git-fork").size(px(13.0)).text_color(t.line))
                            .child("Show its connections")
                            .on_click(cx.listener(|this, _, _, cx| this.show_connections(cx))),
                    )
                })
                .child(list)
                .into_any_element(),
        )
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let button = |id: &'static str, child: gpui::AnyElement| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .h(px(28.0))
                .min_w(px(28.0))
                .px(px(8.0))
                .rounded(px(8.0))
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(t.text)
                .hover(|s| s.bg(t.row_hover))
                .child(child)
        };
        div()
            .absolute()
            .bottom(px(16.0))
            .left(px(16.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .p(px(3.0))
            .rounded(px(10.0))
            .bg(t.popover)
            .border_1()
            .border_color(t.popover_border)
            .shadow_md()
            .occlude()
            .child(button("zoom-out", div().child("−").into_any_element()).on_click(cx.listener(
                |this, _, _, cx| {
                    let (w, h) = this.view_size();
                    this.zoom_about(0.8, (w / 2.0, h / 2.0), cx)
                },
            )))
            .child(
                div()
                    .w(px(44.0))
                    .text_size(px(12.0))
                    .text_color(t.muted)
                    .flex()
                    .justify_center()
                    .child(format!("{:.0}%", self.zoom * 100.0)),
            )
            .child(button("zoom-in", div().child("+").into_any_element()).on_click(cx.listener(
                |this, _, _, cx| {
                    let (w, h) = this.view_size();
                    this.zoom_about(1.25, (w / 2.0, h / 2.0), cx)
                },
            )))
            .child(
                button("fit", div().child("Fit").into_any_element())
                    .on_click(cx.listener(|this, _, _, cx| this.fit_all(cx))),
            )
    }
}

fn control(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    let (mx, my) = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    (mx - dy / 6.0, my + dx / 6.0)
}

struct Stroke {
    from: (f32, f32),
    c1: (f32, f32),
    c2: (f32, f32),
    to: (f32, f32),
    width: f32,
    strength: f32,
}

fn cubic_at(a: (f32, f32), c1: (f32, f32), c2: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (k0 * a.0 + k1 * c1.0 + k2 * c2.0 + k3 * b.0, k0 * a.1 + k1 * c1.1 + k2 * c2.1 + k3 * b.1)
}

type Side = (f32, f32);
const RIGHT: Side = (1.0, 0.0);
const LEFT: Side = (-1.0, 0.0);
const DOWN: Side = (0.0, 1.0);
const UP: Side = (0.0, -1.0);

fn on_side(r: &Rect, side: Side, t: f32) -> (f32, f32) {
    match side {
        RIGHT => (r.x + r.w, r.y + r.h * t),
        LEFT => (r.x, r.y + r.h * t),
        DOWN => (r.x + r.w * t, r.y + r.h),
        _ => (r.x + r.w * t, r.y),
    }
}

impl MapView {
    fn bundle_ends(&self, rects: &[Rect]) -> Vec<(End, End)> {
        let sides: Vec<(Side, Side)> = self
            .bundles
            .iter()
            .map(|&(a, b, _)| {
                let (ca, cb) = (
                    self.cols.get(a).copied().unwrap_or(0),
                    self.cols.get(b).copied().unwrap_or(0),
                );
                if ca.abs_diff(cb) > 1 {
                    (UP, UP) // over the column between: out of the top, into the top
                } else if cb > ca {
                    (RIGHT, LEFT)
                } else if cb < ca {
                    (LEFT, RIGHT)
                } else if rects[a].y < rects[b].y {
                    (DOWN, UP)
                } else {
                    (UP, DOWN)
                }
            })
            .collect();
        type Touching = Vec<(usize, bool, f32)>;
        let mut at: HashMap<(usize, (i8, i8)), Touching> = HashMap::new();
        for (k, &(a, b, _)) in self.bundles.iter().enumerate() {
            let key = |s: Side| (s.0 as i8, s.1 as i8);
            let (sa, sb) = sides[k];
            let far = |r: &Rect, s: Side| if s.0 != 0.0 { r.centre().1 } else { r.centre().0 };
            at.entry((a, key(sa))).or_default().push((k, true, far(&rects[b], sa)));
            at.entry((b, key(sb))).or_default().push((k, false, far(&rects[a], sb)));
        }
        let mut ends = vec![(((0.0, 0.0), RIGHT), ((0.0, 0.0), LEFT)); self.bundles.len()];
        for ((f, side), mut list) in at {
            list.sort_by(|x, y| x.2.total_cmp(&y.2));
            let m = list.len() as f32;
            let side = (side.0 as f32, side.1 as f32);
            for (j, (k, leaving, _)) in list.into_iter().enumerate() {
                let p = on_side(&rects[f], side, (j as f32 + 1.0) / (m + 1.0));
                if leaving {
                    ends[k].0 = (p, side);
                } else {
                    ends[k].1 = (p, side);
                }
            }
        }
        ends
    }
}

impl MapView {
    fn card_el(&self, f: &map::Frame, r: Rect, inside: usize) -> gpui::AnyElement {
        let t = self.theme;
        let base = div()
            .absolute()
            .left(px(r.x))
            .top(px(r.y))
            .w(px(r.w))
            .h(px(r.h))
            .overflow_hidden()
            .rounded(px(18.0))
            .bg(t.card)
            .border_1()
            .border_color(t.card_border);
        let title = div()
            .text_size(px(16.0))
            .line_height(px(21.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.text)
            .line_clamp(2)
            .child(f.title.clone());
        if r.h < 150.0 || r.w < 170.0 {
            return base
                .px(px(14.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .child(div().flex_1().min_w(px(0.0)).child(title))
                .child(
                    div()
                        .text_size(px(20.0))
                        .line_height(px(26.0))
                        .text_color(t.text)
                        .child(f.items.len().to_string()),
                )
                .into_any_element();
        }
        let mut top: Vec<&map::Item> = f.items.iter().collect();
        top.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.label.cmp(&b.label)));
        let room = ((r.h - 150.0) / 22.0).floor().clamp(0.0, 6.0) as usize;
        base.p(px(18.0))
            .flex()
            .flex_col()
            .child(title)
            .child(
                div()
                    .mt(px(6.0))
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(32.0))
                            .line_height(px(38.0))
                            .text_color(t.text)
                            .child(f.items.len().to_string()),
                    )
                    .when(inside > 0, |c| {
                        c.child(
                            div()
                                .text_size(px(12.0))
                                .line_height(px(16.0))
                                .text_color(t.faint)
                                .child(format!("{inside} connections inside")),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .line_height(px(18.0))
                    .text_color(t.muted)
                    .truncate()
                    .child(f.sub.clone()),
            )
            .child(div().mt(px(10.0)).flex().flex_col().gap(px(2.0)).children(
                top.into_iter().take(room).map(|i| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .h(px(20.0))
                        .child(
                            div().size(px(6.0)).flex_none().rounded(px(3.0)).bg(self.tint(i.kind)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .text_size(px(12.5))
                                .line_height(px(18.0))
                                .text_color(t.text)
                                .truncate()
                                .child(i.label.clone()),
                        )
                        .when(i.weight > 0, |c| {
                            c.child(
                                div()
                                    .text_size(px(11.5))
                                    .line_height(px(16.0))
                                    .text_color(t.faint)
                                    .child(i.weight.to_string()),
                            )
                        })
                }),
            ))
            .into_any_element()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemState {
    Plain,
    Selected,
    Joined,
}

impl Render for MapView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let b = self.bounds.get();
        if !self.fitted && f32::from(b.size.width) > 1.0 && !self.placed.is_empty() {
            let (w, h) = self.view_size();
            (self.zoom, self.pan) = fit(map::bounds(&self.placed), w, h);
            self.fitted = true;
        }
        if f32::from(b.size.width) > 1.0 && self.fitted {
            if let Some((k, f)) = self.pending.take() {
                self.focus_key(&k, f.as_deref(), cx);
            }
        }
        let (vw, vh) = (f32::from(b.size.width), f32::from(b.size.height));
        let view = Rect { x: -40.0, y: -40.0, w: vw + 80.0, h: vh + 80.0 };
        let overview = self.zoom < OVERVIEW;
        let lit = self.selected.or(self.hover);
        let sel_key =
            lit.and_then(|(f, i)| Some(self.map.frames.get(f)?.items.get(i)?.key.clone()));
        let joined: HashSet<Key> = sel_key.as_ref().map_or_else(HashSet::new, |k| {
            self.map
                .edges
                .iter()
                .filter_map(|e| {
                    if &e.0 == k {
                        Some(e.1.clone())
                    } else if &e.1 == k {
                        Some(e.0.clone())
                    } else {
                        None
                    }
                })
                .collect()
        });

        let mut lines: Vec<Stroke> = Vec::new();
        let mut pills: Vec<((f32, f32), usize)> = Vec::new();
        if overview {
            let rects: Vec<Rect> = self.placed.iter().map(|p| self.to_screen(&p.frame)).collect();
            let ends = self.bundle_ends(&rects);
            let most = self.bundles.iter().map(|b| b.2).max().unwrap_or(1) as f32;
            let top = rects.iter().map(|r| r.y).fold(f32::MAX, f32::min);
            for (k, &(a, b, n)) in self.bundles.iter().enumerate() {
                let ((from, dir_a), (to, dir_b)) = ends[k];
                let reach = ((to.0 - from.0).abs().max((to.1 - from.1).abs()) / 2.0).max(60.0);
                let mut c1 = (from.0 + dir_a.0 * reach, from.1 + dir_a.1 * reach);
                let mut c2 = (to.0 + dir_b.0 * reach, to.1 + dir_b.1 * reach);
                let skip =
                    self.cols.get(a).zip(self.cols.get(b)).is_some_and(|(x, y)| x.abs_diff(*y) > 1);
                if skip {
                    let over = top - 90.0;
                    c1 = (from.0 + (to.0 - from.0) * 0.25, over);
                    c2 = (to.0 - (to.0 - from.0) * 0.25, over);
                }
                let share = (n as f32 / most).sqrt();
                lines.push(Stroke {
                    from,
                    c1,
                    c2,
                    to,
                    width: 2.5 + 13.0 * share,
                    strength: 0.38 + 0.5 * share,
                });
                let free = |p: (f32, f32)| {
                    let r = Rect { x: p.0 - 26.0, y: p.1 - 14.0, w: 52.0, h: 28.0 };
                    !rects.iter().any(|c| c.overlaps(&r))
                        && !pills.iter().any(|(q, _): &((f32, f32), usize)| {
                            (q.0 - p.0).abs() < 52.0 && (q.1 - p.1).abs() < 26.0
                        })
                };
                if let Some(p) = [0.5, 0.4, 0.6, 0.3, 0.7, 0.2, 0.8]
                    .iter()
                    .map(|t| cubic_at(from, c1, c2, to, *t))
                    .find(|p| free(*p))
                {
                    pills.push((p, n));
                }
            }
        } else if let Some((sf, si)) = lit {
            let from = self.to_screen(&self.placed[sf].items[si]).centre();
            let strength = if self.selected.is_some() { 0.9 } else { 0.55 };
            for (f, fr) in self.map.frames.iter().enumerate() {
                for (i, it) in fr.items.iter().enumerate() {
                    if joined.contains(&it.key) {
                        let to = self.to_screen(&self.placed[f].items[i]).centre();
                        let c = control(from, to);
                        let third = |p: (f32, f32)| {
                            (p.0 + (c.0 - p.0) * 2.0 / 3.0, p.1 + (c.1 - p.1) * 2.0 / 3.0)
                        };
                        lines.push(Stroke {
                            from,
                            c1: third(from),
                            c2: third(to),
                            to,
                            width: 1.5,
                            strength,
                        });
                    }
                }
            }
        }

        let heaviest = self.heaviest();
        let mut frames = Vec::new();
        for (f, fr) in self.map.frames.iter().enumerate() {
            let r = self.to_screen(&self.placed[f].frame);
            if !r.overlaps(&view) {
                continue;
            }
            if overview {
                frames.push(self.card_el(fr, r, self.within.get(f).copied().unwrap_or(0)));
                continue;
            }
            let items: Vec<gpui::AnyElement> = fr
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, it)| {
                    let s = self.to_screen(&self.placed[f].items[i]);
                    if !s.overlaps(&view) {
                        return None;
                    }
                    let local = Rect { x: s.x - r.x, y: s.y - r.y, ..s };
                    let state = if self.selected == Some((f, i)) {
                        ItemState::Selected
                    } else if lit == Some((f, i)) || joined.contains(&it.key) {
                        ItemState::Joined
                    } else {
                        ItemState::Plain
                    };
                    let heat =
                        if heaviest[f] > 0 { it.weight as f32 / heaviest[f] as f32 } else { 0.0 };
                    Some(self.item_el(it, local, state, heat))
                })
                .collect();
            frames.push(self.frame_el(fr, r, items));
        }

        let bounds = self.bounds.clone();
        let me = cx.entity_id();
        let (dot, line) = (t.card_border, t.line);
        let (pan, zoom) = (self.pan, self.zoom);
        let backdrop = canvas(
            // fitting needs the laid-out size, so a new size draws again
            move |b, _, cx| {
                if bounds.get() != b {
                    bounds.set(b);
                    cx.notify(me);
                }
            },
            move |b, _, window, _| {
                let o = b.origin;
                let step = 32.0 * zoom * if zoom < 0.25 { 4.0 } else { 1.0 };
                let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
                let (mut x, y0) = (pan.0.rem_euclid(step), pan.1.rem_euclid(step));
                while x < w {
                    let mut y = y0;
                    while y < h {
                        window.paint_quad(fill(
                            Bounds::new(
                                point(o.x + px(x - 0.75), o.y + px(y - 0.75)),
                                size(px(1.5), px(1.5)),
                            ),
                            dot,
                        ));
                        y += step;
                    }
                    x += step;
                }
                let ink: gpui::Hsla = line.into();
                let at = |p: (f32, f32)| point(o.x + px(p.0), o.y + px(p.1));
                for l in &lines {
                    let mut p = PathBuilder::stroke(px(l.width));
                    p.move_to(at(l.from));
                    p.cubic_bezier_to(at(l.to), at(l.c1), at(l.c2));
                    if let Ok(path) = p.build() {
                        window.paint_path(path, ink.alpha(l.strength));
                    }
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let empty = self.map.frames.is_empty();
        let message = self
            .problem
            .clone()
            .map(|p| (SharedString::from("The store did not answer"), SharedString::from(p)))
            .or_else(|| empty.then(|| self.empty.clone()));
        let panel = self.panel(cx);
        div()
            .id(SharedString::from(format!("map-{}", self.title)))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(t.bg)
            .cursor_grab()
            .child(backdrop)
            .children(frames)
            .children(pills.into_iter().map(|((x, y), n)| {
                div()
                    .absolute()
                    .left(px(x - 22.0))
                    .top(px(y - 11.0))
                    .w(px(44.0))
                    .h(px(22.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(11.0))
                    .bg(t.popover)
                    .border_1()
                    .border_color(t.popover_border)
                    .text_size(px(11.5))
                    .text_color(t.muted)
                    .child(n.to_string())
            }))
            .when_some(message, |c, (head, how)| {
                c.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(8.0))
                        .child(div().text_size(px(15.0)).text_color(t.muted).child(head))
                        .child(
                            div()
                                .max_w(px(520.0))
                                .text_size(px(13.0))
                                .text_color(t.faint)
                                .child(how),
                        ),
                )
            })
            .when(!empty, |c| c.child(self.toolbar(cx)))
            .children(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_hits_the_item_under_it_and_nothing_between() {
        let placed = vec![Placed {
            frame: Rect { x: 0.0, y: 0.0, w: 200.0, h: 120.0 },
            items: vec![
                Rect { x: 10.0, y: 50.0, w: 80.0, h: 40.0 },
                Rect { x: 100.0, y: 50.0, w: 80.0, h: 40.0 },
            ],
        }];
        assert_eq!(hit(&placed, 20.0, 60.0), Some((0, 0)));
        assert_eq!(hit(&placed, 150.0, 60.0), Some((0, 1)));
        assert_eq!(hit(&placed, 95.0, 60.0), None, "the gap between two items");
        assert_eq!(hit(&placed, 20.0, 10.0), None, "a frame's header");
        assert_eq!(hit(&placed, 500.0, 60.0), None);
    }

    #[test]
    fn fitting_centres_the_whole_and_leaves_a_margin() {
        let (z, (px_, py)) = fit(Rect { x: 0.0, y: 0.0, w: 1000.0, h: 500.0 }, 500.0, 500.0);
        assert!((z - 0.45).abs() < 1e-6);
        assert!(
            (px_ - (250.0 - 500.0 * z)).abs() < 1e-4 && (py - (250.0 - 250.0 * z)).abs() < 1e-4
        );
        assert_eq!(fit(Rect::default(), 10.0, 10.0).0, 1.0, "nothing to fit");
        let (z, _) = fit(Rect { x: 0.0, y: 0.0, w: 100.0, h: 50.0 }, 1000.0, 1000.0);
        assert_eq!(z, FIT_MAX, "a small map is shown, not blown up");
        let (z, (x, y)) =
            frame_view(Rect { x: 100.0, y: 50.0, w: 4000.0, h: 3000.0 }, 1000.0, 700.0);
        assert_eq!((z, x, y), (READABLE, 32.0 - 100.0 * READABLE, 32.0 - 50.0 * READABLE));
    }
}
