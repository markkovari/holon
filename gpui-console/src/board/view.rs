//! The board as GPUI draws it: a header that says how the whole is doing, and under it each tree of
//! agents as cards joined by the line from whatever started them.
//!
//! ## Motion
//!
//! Everything that moves says something and nothing moves to be looked at.
//!
//! * A card that appears fades in and rises a few pixels, each level a beat after the one above,
//!   so a tree reads as being started from the top.
//! * A running badge turns; a card that needs a person breathes in the one colour the board keeps
//!   for asking, and nothing else on it does.
//! * A cost counts to its new value rather than jumping, and a badge whose state changed arrives
//!   rather than being swapped.
//!
//! Every one of these is a one-shot or a slow loop, ends in the state a still frame shows, and is
//! keyed by what it is about (a node, a status, a value), so a change restarts the right one and a
//! re-render restarts none.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    canvas, deferred, div, ease_in_out, ease_out_quint, percentage, point, prelude::*,
    pulsating_between, px, Animation, AnimationExt as _, App, BoxShadow, Context, Entity,
    FontWeight, Hsla, IntoElement, PathBuilder, Pixels, Render, Rgba, SharedString, Task,
    Transformation, Window,
};

use super::icons::{icon, model_glyph, tool_icon};
use super::theme::{Mode, Theme};
use super::tree::{AgentTree, Kind, Node, Pending, Status, Summary, Waiting};

// -- metrics ------------------------------------------------------------------------------------

/// A card's width, and the gap between two columns of them.
const CARD_W: f32 = 284.0;
const GAP: f32 = 36.0;
/// From a parent's bottom edge to the bar its children hang from.
const STEM: f32 = 44.0;
const LINE: f32 = 1.5;
/// The radius of a connector's corner.
const R: f32 = 14.0;
/// How far a nested card is pushed in, and the space above it.
const INDENT: f32 = 40.0;
const NEST_GAP: f32 = 14.0;
/// From the top of a card to the middle of its first row, where a nested connector arrives.
const HEADER_MID: f32 = 31.0;
/// Where a nested connector's line runs, from the parent's left edge.
const NEST_X: f32 = 27.0;
const DOT: f32 = 9.0;
/// More children than this are stacked, not laid out in a row.
const ROW_MAX: usize = 4;

/// Who answers a person's yes or no, and how far a yes reaches.
pub trait Actions {
    fn answer(&self, id: u64, approve: bool, scope: Scope, cx: &mut App);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Once,
    Task,
    Session,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Once => "once",
            Scope::Task => "task",
            Scope::Session => "session",
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// -- what the numbers say -----------------------------------------------------------------------

/// `$0.29`; under a cent, three places, because "$0.00" says nothing was spent.
pub fn money(micros: u64) -> String {
    let usd = micros as f64 / 1e6;
    if micros == 0 {
        "$0.00".into()
    } else if usd < 0.01 {
        format!("${usd:.3}")
    } else {
        format!("${usd:.2}")
    }
}

/// A model as a person says it: `claude-sonnet-5-5` is `Sonnet 5.5`, `gpt-5-mini` is `GPT-5 Mini`,
/// `local/system` is `On-device`. The provider is the glyph's job, not the words'.
pub fn model_name(label: &str) -> String {
    let m = label.rsplit('/').next().unwrap_or(label);
    if label.starts_with("local/") && m == "system" {
        return "On-device".into();
    }
    if let Some(rest) = m.strip_prefix("claude-") {
        let mut parts = rest.split('-');
        let family = parts.next().unwrap_or("");
        let nums: Vec<&str> =
            parts.filter(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())).collect();
        let family = {
            let mut c = family.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        };
        return match nums.as_slice() {
            [] => family,
            [a] => format!("{family} {a}"),
            [a, b, ..] => format!("{family} {a}.{b}"),
        };
    }
    m.split('-')
        .map(|w| match w {
            "gpt" => "GPT".to_string(),
            "deepseek" => "DeepSeek".to_string(),
            "qwen" => "Qwen".to_string(),
            w if w.chars().all(|c| c.is_ascii_digit() || c == '.') => w.to_string(),
            w => {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .replacen("GPT ", "GPT-", 1)
}

/// `45s`, `1m 09s`, `2h 05m`.
pub fn clock(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// `12.4k tok`, for a model whose price is not known.
pub fn tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k tok", n as f64 / 1000.0)
    } else {
        format!("{n} tok")
    }
}

/// How much slower than real time everything moves, from `BOARD_SLOWMO` (default 1). For looking at a
/// motion frame by frame; never set in use.
fn slowmo() -> f32 {
    static F: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *F.get_or_init(|| {
        std::env::var("BOARD_SLOWMO")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|f: &f32| *f > 0.0)
            .unwrap_or(1.0)
    })
}

/// A duration in milliseconds, at the speed the board is running at.
fn ms(n: u64) -> Duration {
    Duration::from_millis((n as f32 * slowmo()) as u64)
}

fn lerp(a: u64, b: u64, t: f32) -> u64 {
    (a as f64 + (b as f64 - a as f64) * t as f64).round().max(0.0) as u64
}

fn with_alpha(c: Rgba, a: f32) -> Hsla {
    let h: Hsla = c.into();
    h.alpha(a)
}

/// An easing that waits `delay_ms` and then runs `ease` over `ms`: one animation of `delay + ms`,
/// because an animation has a duration and no delay of its own.
fn after(
    delay_ms: u64,
    ms: u64,
    ease: impl Fn(f32) -> f32 + 'static,
) -> (Duration, impl Fn(f32) -> f32) {
    let total = (delay_ms + ms) as f32;
    (self::ms(delay_ms + ms), move |t: f32| {
        ease((((t * total) - delay_ms as f32) / ms as f32).clamp(0.0, 1.0))
    })
}

// -- a snapshot of the tree, so that drawing holds no borrow ------------------------------------

struct Branch {
    node: Node,
    kids: Vec<Branch>,
    /// How many children the node has, shown or not.
    kid_count: usize,
    collapsed: bool,
    /// How many levels below the root.
    depth: usize,
}

fn branch(tree: &AgentTree, n: &Node, collapsed: &HashSet<String>, depth: usize) -> Branch {
    let hidden = collapsed.contains(&n.id);
    let kid_count = n.children.len();
    let kids = if hidden {
        Vec::new()
    } else {
        tree.children_of(&n.id).into_iter().map(|c| branch(tree, c, collapsed, depth + 1)).collect()
    };
    Branch { node: n.clone(), kids, kid_count, collapsed: hidden, depth }
}

// -- the view -----------------------------------------------------------------------------------

pub struct Board {
    tree: Entity<AgentTree>,
    actions: Rc<dyn Actions>,
    theme: Theme,
    collapsed: HashSet<String>,
    open: bool,
    /// Wall clock at the last tick: what "running for 45s" is measured against.
    now: u64,
    /// The cost each node last showed, so a new one is counted to from there.
    shown_cost: HashMap<String, (u64, u64)>,
    /// The width of each tree's row of children as it was and is: when a child joins, the parent
    /// glides to the new middle instead of jumping there.
    row_width: HashMap<String, (f32, f32)>,
    _tick: Task<()>,
}

impl Board {
    pub fn new(
        tree: Entity<AgentTree>,
        actions: Rc<dyn Actions>,
        mode: Mode,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&tree, |_, _, cx| cx.notify()).detach();
        // The one clock: once a second, so "45s" reads 46s without anything having happened.
        let tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(1000)).await;
            if this
                .update(cx, |b, cx| {
                    b.now = now_ms();
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        });
        Self {
            tree,
            actions,
            theme: Theme::of(mode),
            collapsed: HashSet::new(),
            open: false,
            now: now_ms(),
            shown_cost: HashMap::new(),
            row_width: HashMap::new(),
            _tick: tick,
        }
    }

    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.theme = Theme::of(mode);
        cx.notify();
    }

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.open = open;
        cx.notify();
    }

    fn toggle(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(id) {
            self.collapsed.insert(id.to_string());
        }
        cx.notify();
    }

    // -- pieces ---------------------------------------------------------------------------------

    /// The pill that says how a card stands.
    fn badge(&self, id: &str, s: Status) -> impl IntoElement {
        let t = &self.theme;
        let (bg, ink) = t.badge(s);
        let (label, glyph) = match s {
            Status::Running => ("Running", "loader-circle"),
            Status::NeedsYou => ("Needs you", "clock"),
            Status::Done => ("Done", "check"),
            Status::Failed => ("Failed", "circle-x"),
            Status::Stopped => ("Stopped", "square"),
        };
        let mark = icon(glyph).size(px(13.0)).flex_none().text_color(ink);
        let mark = if s == Status::Running {
            mark.with_animation(
                SharedString::from(format!("spin-{id}")),
                Animation::new(self::ms(900)).repeat(),
                |svg, t| svg.with_transformation(Transformation::rotate(percentage(t))),
            )
            .into_any_element()
        } else {
            mark.into_any_element()
        };
        let pill = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(5.0))
            .h(px(24.0))
            .px(px(8.0))
            .rounded(px(7.0))
            .bg(bg)
            .text_color(ink)
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .child(mark)
            .child(label);
        // A status that has just changed arrives; one that has not is simply there.
        let (dur, ease) = after(0, 260, ease_out_quint());
        pill.relative().with_animation(
            SharedString::from(format!("badge-{id}-{s:?}")),
            Animation::new(dur).with_easing(ease),
            |el, t| el.opacity(0.25 + 0.75 * t).top(px(3.0 * (1.0 - t))),
        )
    }

    /// The tile that says which model, drawn as its maker's glyph.
    fn tile(&self, model: &str, kind: Kind) -> impl IntoElement {
        let t = &self.theme;
        let (glyph, tint) = if kind == Kind::Session {
            ("code-xml", gpui::rgb(0x7aa2f7))
        } else {
            model_glyph(model)
        };
        div()
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .size(px(38.0))
            .rounded(px(10.0))
            .bg(t.tile)
            .border_1()
            .border_color(t.tile_border)
            .child(icon(glyph).size(px(19.0)).text_color(tint))
    }

    /// The tools used so far, as small tiles; the last few, and how many came before.
    fn chips(&self, tools: &[String]) -> impl IntoElement {
        let t = &self.theme;
        const SHOWN: usize = 5;
        let extra = tools.len().saturating_sub(SHOWN);
        let mut row = div().flex().flex_row().items_center().gap(px(4.0));
        for name in tools.iter().take(SHOWN) {
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.0))
                    .rounded(px(7.0))
                    .bg(t.tile)
                    .border_1()
                    .border_color(t.tile_border)
                    .child(icon(tool_icon(name)).size(px(13.0)).text_color(t.muted)),
            );
        }
        if extra > 0 {
            row =
                row.child(div().text_size(px(11.5)).text_color(t.faint).child(format!("+{extra}")));
        }
        row
    }

    /// The cost, counting to its new value.
    fn cost(&mut self, n: &Node) -> impl IntoElement {
        let t = self.theme;
        let target = n.cost_micros.unwrap_or(0);
        let from = match self.shown_cost.get(&n.id) {
            Some((_, to)) if *to == target => self.shown_cost[&n.id].0,
            Some((_, to)) => *to,
            None => target,
        };
        self.shown_cost.insert(n.id.clone(), (from, target));
        let unknown = n.cost_micros.is_none();
        let toks = n.tokens;
        div()
            .text_size(px(14.5))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.text)
            .with_animation(
                SharedString::from(format!("cost-{}-{target}", n.id)),
                Animation::new(self::ms(520)).with_easing(ease_out_quint()),
                move |el, p| {
                    let shown = lerp(from, target, p);
                    el.child(if unknown && toks == 0 {
                        "—".to_string()
                    } else if unknown {
                        tokens(toks)
                    } else {
                        money(shown)
                    })
                },
            )
    }

    /// One agent: who it is, what state it is in, what it was asked, what it is using and what it
    /// has cost.
    fn card(&mut self, b: &Branch, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let n = &b.node;
        let active = !n.status.is_over();
        let attention = n.status == Status::NeedsYou;
        let id = n.id.clone();
        let title = if n.kind == Kind::Session { "Coder".to_string() } else { n.name.clone() };
        let subtitle = if n.model.is_empty() { String::new() } else { model_name(&n.model) };
        let has_kids = b.kid_count > 0;
        let kid_count = b.kid_count;
        let elapsed = clock(n.elapsed_ms(self.now));
        let activity = if active { n.activity.clone() } else { String::new() };
        let task = n.task.clone();

        let top = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(self.tile(&n.model, n.kind))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .text_size(px(15.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div().text_size(px(12.5)).text_color(t.muted).truncate().child(subtitle),
                    ),
            )
            .child(self.badge(&id, n.status));

        let body = div()
            .mt(px(11.0))
            .text_size(px(14.0))
            .line_height(px(19.0))
            .text_color(if active { t.text } else { t.muted })
            .line_clamp(2)
            .child(task);

        // What a card says it is doing or concluded, in the row under its task: the tool it is on,
        // or once it is over, how it ended. The row is always there, so a card is the same height
        // all its life and what is under it never moves.
        let activity = if !activity.is_empty() {
            activity
        } else if let (true, Some(r)) = (n.status.is_over(), n.result.as_ref()) {
            r.lines().next().unwrap_or("").to_string()
        } else if active {
            "starting".to_string()
        } else {
            String::new()
        };
        let used = div()
            .mt(px(9.0))
            .h(px(24.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(self.chips(&n.tools))
            .when(!activity.is_empty(), |row| {
                row.child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(t.faint)
                        .child(activity.clone()),
                )
            });

        let footer = div()
            .mt(px(11.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(self.cost(n))
            .child(div().text_size(px(13.0)).text_color(t.faint).child("·"))
            .child(div().text_size(px(13.0)).text_color(t.muted).child(elapsed))
            .child(div().flex_1())
            .when(has_kids, |row| {
                let id = id.clone();
                let open = !b.collapsed;
                row.child(
                    div()
                        .id(SharedString::from(format!("fold-{id}")))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(3.0))
                        .cursor_pointer()
                        .text_size(px(13.0))
                        .text_color(t.muted)
                        .hover(|s| s.text_color(t.text))
                        .child(kid_count.to_string())
                        .child(
                            icon("chevron-down").size(px(14.0)).text_color(t.muted).with_animation(
                                SharedString::from(format!("chev-{id}-{open}")),
                                Animation::new(self::ms(220)).with_easing(ease_in_out),
                                move |svg, p| {
                                    // down when open, a quarter turn to the right when folded
                                    // a percentage cannot be negative: a quarter turn back is three
                                    // quarters forward
                                    let turn = if open { 1.0 - p } else { p } * 0.25;
                                    svg.with_transformation(Transformation::rotate(percentage(
                                        1.0 - turn,
                                    )))
                                },
                            ),
                        )
                        .on_click({
                            let key = id.clone();
                            cx.listener(move |this, _, _, cx| this.toggle(&key, cx))
                        }),
                )
            });

        let edge = if attention { t.attention() } else { t.card_border };
        let shell = div()
            .id(SharedString::from(format!("card-{id}")))
            .w(px(CARD_W))
            .flex_none()
            .px(px(15.0))
            .pt(px(14.0))
            .pb(px(13.0))
            .rounded(px(16.0))
            .bg(t.card)
            .border_1()
            .border_color(edge)
            .shadow(vec![BoxShadow {
                color: t.shadow.into(),
                offset: point(px(0.0), px(1.0)),
                blur_radius: px(if t.mode == Mode::Light { 10.0 } else { 0.0 }),
                spread_radius: px(0.0),
            }])
            .hover(|s| s.border_color(if attention { t.attention() } else { t.card_hover }))
            .flex()
            .flex_col()
            .child(top)
            .child(body)
            .child(used)
            .child(footer);

        // A card that needs a person breathes; nothing else on the board does.
        let shell = if attention {
            let glow = t.attention();
            shell
                .with_animation(
                    SharedString::from(format!("breathe-{id}")),
                    Animation::new(self::ms(2400))
                        .repeat()
                        .with_easing(pulsating_between(0.0, 1.0)),
                    move |el, p| {
                        el.shadow(vec![BoxShadow {
                            color: with_alpha(glow, 0.05 + 0.20 * p),
                            offset: point(px(0.0), px(0.0)),
                            blur_radius: px(10.0 + 14.0 * p),
                            spread_radius: px(0.0),
                        }])
                    },
                )
                .into_any_element()
        } else {
            shell.into_any_element()
        };

        // Arrives a beat after its parent, so a tree reads as being started from the top.
        let (dur, ease) = after(90 * b.depth as u64, 420, ease_out_quint());
        div().relative().flex_none().child(shell).with_animation(
            SharedString::from(format!("enter-{id}")),
            Animation::new(dur).with_easing(ease),
            |el, p| el.opacity(p).top(px(10.0 * (1.0 - p))),
        )
    }
}

// -- layout -------------------------------------------------------------------------------------

/// A hollow circle where a line leaves a card.
fn dot(color: Rgba, bg: Rgba) -> impl IntoElement {
    div().size(px(DOT)).rounded(px(DOT)).border(px(LINE)).border_color(color).bg(bg)
}

/// A point `(x, y)` from `origin`, in layout pixels.
fn at(origin: gpui::Point<Pixels>, x: f32, y: f32) -> gpui::Point<Pixels> {
    point(origin.x + px(x), origin.y + px(y))
}

/// The line from a parent down to a row of `n` children: a stem, a bar with rounded ends, and a
/// drop onto each card. Children are one card wide with a gap between, so where each hangs is
/// arithmetic.
fn bracket(n: usize, line: Rgba) -> gpui::Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let o = bounds.origin;
            let mid = STEM / 2.0;
            // where the parent is: the middle of the row as wide as it is right now
            let centre = f32::from(bounds.size.width) / 2.0;
            let x = |i: usize| i as f32 * (CARD_W + GAP) + CARD_W / 2.0;
            let mut b = PathBuilder::stroke(px(LINE));
            b.move_to(at(o, centre, 0.0));
            b.line_to(at(o, centre, if n == 1 { STEM } else { mid }));
            if n > 1 {
                let (first, last) = (x(0), x(n - 1));
                b.move_to(at(o, first, STEM));
                b.line_to(at(o, first, mid + R));
                b.curve_to(at(o, first + R, mid), at(o, first, mid));
                b.line_to(at(o, last - R, mid));
                b.curve_to(at(o, last, mid + R), at(o, last, mid));
                b.line_to(at(o, last, STEM));
                for i in 1..n - 1 {
                    b.move_to(at(o, x(i), mid));
                    b.line_to(at(o, x(i), STEM));
                }
            }
            if let Ok(path) = b.build() {
                window.paint_path(path, line);
            }
        },
    )
    .absolute()
    .top(px(0.0))
    .left(px(0.0))
    .w_full()
    .h(px(STEM))
}

/// The line down the left of a stack, as it reaches one child: it comes from the card above (or the
/// parent's dot), turns into this card at the middle of its first row and, unless this is the last
/// child, goes on down past the card to the next. It is sized by the child it belongs to, so it is as
/// tall as that child is, whatever is stacked under it.
fn elbow(goes_on: bool, line: Rgba) -> gpui::Canvas<()> {
    let w = INDENT - NEST_X;
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let o = bounds.origin;
            let h: f32 = bounds.size.height.into();
            let turn = NEST_GAP + HEADER_MID;
            let r = 11.0;
            let mut b = PathBuilder::stroke(px(LINE));
            b.move_to(at(o, 0.5, 0.0));
            b.line_to(at(o, 0.5, turn - r));
            b.curve_to(at(o, 0.5 + r, turn), at(o, 0.5, turn));
            b.line_to(at(o, w, turn));
            if goes_on {
                b.move_to(at(o, 0.5, turn - r));
                b.line_to(at(o, 0.5, h));
            }
            if let Ok(path) = b.build() {
                window.paint_path(path, line);
            }
        },
    )
    .absolute()
    .left(px(-w))
    .top(px(-NEST_GAP))
    .bottom(px(-NEST_GAP))
    .w(px(w + 1.0))
}

impl Board {
    /// One tree: the root, and everything it started.
    fn block(&mut self, b: &Branch, cx: &mut Context<Self>) -> gpui::AnyElement {
        if b.kids.is_empty() {
            return div()
                .flex()
                .flex_col()
                .items_center()
                .child(self.card(b, cx))
                .into_any_element();
        }
        if b.kids.len() > ROW_MAX {
            return div().flex().flex_col().child(self.stack(b, cx)).into_any_element();
        }
        let n = b.kids.len();
        let row_w = n as f32 * CARD_W + (n - 1) as f32 * GAP;
        let (line, bg) = (self.theme.line, self.theme.bg);
        let from = match self.row_width.get(&b.node.id) {
            Some((_, to)) if *to == row_w => self.row_width[&b.node.id].0,
            Some((_, to)) => *to,
            None => row_w,
        };
        self.row_width.insert(b.node.id.clone(), (from, row_w));
        let children: Vec<gpui::AnyElement> = b
            .kids
            .iter()
            .map(|k| {
                let sub = if k.kids.is_empty() {
                    self.card(k, cx).into_any_element()
                } else {
                    self.stack(k, cx)
                };
                div().w(px(CARD_W)).flex_none().child(sub).into_any_element()
            })
            .collect();
        let card = self.card(b, cx);
        // The bracket is one stroke, so its corners and its joins are the same line and not
        // several boxes that nearly meet.
        let (dur, ease) = after(60, 420, ease_out_quint());
        let bracket = bracket(n, line).with_animation(
            SharedString::from(format!("bracket-{}", b.node.id)),
            Animation::new(dur).with_easing(ease),
            |el, p| el.opacity(p),
        );
        div()
            .flex()
            .flex_col()
            .items_center()
            .child(card)
            .child(
                div()
                    .relative()
                    .pt(px(STEM))
                    .child(bracket)
                    .child(
                        // the dot under the parent: at the middle of the row, as wide as it is now
                        div()
                            .absolute()
                            .top(px(-DOT / 2.0))
                            .w_full()
                            .flex()
                            .justify_center()
                            .child(dot(line, bg)),
                    )
                    .child(div().flex().flex_row().gap(px(GAP)).children(children))
                    .with_animation(
                        SharedString::from(format!("glide-{}-{}", b.node.id, row_w as i32)),
                        Animation::new(ms(380)).with_easing(ease_out_quint()),
                        move |el, t| el.w(px(from + (row_w - from) * t)),
                    ),
            )
            .into_any_element()
    }

    /// A card with its children stacked under it, each tucked in and joined to the line down the
    /// left: how everything below the first level is drawn, and the first level when it is too wide
    /// for a row.
    fn stack(&mut self, b: &Branch, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (line, bg) = (self.theme.line, self.theme.bg);
        let last = b.kids.len().saturating_sub(1);
        let kids: Vec<gpui::AnyElement> = b
            .kids
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let (dur, ease) = after(90 * k.depth as u64, 420, ease_out_quint());
                let elbow = elbow(i < last, line).with_animation(
                    SharedString::from(format!("elbow-{}", k.node.id)),
                    Animation::new(dur).with_easing(ease),
                    |el, p| el.opacity(p),
                );
                let sub = if k.kids.is_empty() {
                    self.card(k, cx).into_any_element()
                } else {
                    self.stack(k, cx)
                };
                div().relative().child(elbow).child(sub).into_any_element()
            })
            .collect();
        let card = self.card(b, cx);
        div()
            .flex()
            .flex_col()
            .child(card)
            .child(
                div()
                    .relative()
                    .ml(px(INDENT))
                    .mt(px(NEST_GAP))
                    .flex()
                    .flex_col()
                    .gap(px(NEST_GAP))
                    .child(
                        div()
                            .absolute()
                            .left(px(NEST_X - INDENT - DOT / 2.0))
                            .top(px(-NEST_GAP - DOT / 2.0))
                            .child(dot(line, bg)),
                    )
                    .children(kids),
            )
            .into_any_element()
    }

    // -- the header -----------------------------------------------------------------------------

    /// A button that is a word.
    fn chip_button(
        &self,
        id: SharedString,
        label: &str,
        bg: Rgba,
        ink: Rgba,
        on: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .h(px(28.0))
            .px(px(11.0))
            .rounded(px(8.0))
            .bg(bg)
            .text_color(ink)
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .hover(|s| s.opacity(0.85))
            .child(label.to_string())
            .on_click(cx.listener(move |this, _, _, cx| on(this, cx)))
    }

    fn waiting_row(&self, w: &Waiting, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let Pending { id, tool, args, session } = w.pending.clone();
        let what: String = ["path", "command", "url", "message", "text", "key"]
            .iter()
            .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(70)
            .collect();
        let actions = self.actions.clone();
        let answer = move |approve: bool, scope: Scope| {
            let a = actions.clone();
            move |_: &mut Self, cx: &mut Context<Self>| a.answer(id, approve, scope, cx)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(10.0))
            .rounded(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .child(format!("{} wants to {tool}", w.agent)),
                    )
                    .when(!w.via.is_empty(), |c| {
                        c.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(t.faint)
                                .child(format!("for {}", w.via.join(" › "))),
                        )
                    })
                    .when(!what.is_empty(), |c| {
                        c.child(
                            div()
                                .font_family("Menlo")
                                .text_size(px(11.5))
                                .text_color(t.muted)
                                .truncate()
                                .child(what.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .child(self.chip_button(
                        SharedString::from(format!("yes-{id}")),
                        "Approve",
                        t.approve,
                        t.approve_ink,
                        answer(true, Scope::Once),
                        cx,
                    ))
                    .when(session, |r| {
                        r.child(self.chip_button(
                            SharedString::from(format!("yes-task-{id}")),
                            "This task",
                            t.tile,
                            t.text,
                            answer(true, Scope::Task),
                            cx,
                        ))
                        .child(self.chip_button(
                            SharedString::from(format!("yes-session-{id}")),
                            "Whole session",
                            t.tile,
                            t.text,
                            answer(true, Scope::Session),
                            cx,
                        ))
                    })
                    .child(div().flex_1())
                    .child(self.chip_button(
                        SharedString::from(format!("no-{id}")),
                        "Deny",
                        t.deny,
                        t.deny_ink,
                        answer(false, Scope::Once),
                        cx,
                    )),
            )
    }

    fn header(&mut self, s: &Summary, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let waiting = s.waiting.clone();
        let running_ink = t.badge(Status::Running).1;
        let (ask_bg, ask_ink) = t.badge(Status::NeedsYou);
        let total = {
            let id = "total".to_string();
            let target = s.cost_micros;
            let from = match self.shown_cost.get(&id) {
                Some((_, to)) if *to == target => self.shown_cost[&id].0,
                Some((_, to)) => *to,
                None => target,
            };
            self.shown_cost.insert(id, (from, target));
            div()
                .text_size(px(17.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text)
                .with_animation(
                    SharedString::from(format!("cost-total-{target}")),
                    Animation::new(self::ms(520)).with_easing(ease_out_quint()),
                    move |el, p| el.child(money(lerp(from, target, p))),
                )
        };
        let mode_glyph = if t.mode == Mode::Dark { "sun" } else { "moon" };
        let popover = (self.open && !waiting.is_empty()).then(|| {
            let rows: Vec<gpui::AnyElement> =
                waiting.iter().map(|w| self.waiting_row(w, cx).into_any_element()).collect();
            deferred(
                div()
                    .id("waiting")
                    .absolute()
                    .top(px(34.0))
                    .right(px(0.0))
                    .w(px(400.0))
                    .p(px(6.0))
                    .rounded(px(14.0))
                    .bg(t.popover)
                    .border_1()
                    .border_color(t.popover_border)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| this.set_open(false, cx)))
                    .children(rows)
                    .with_animation(
                        "popover-in",
                        Animation::new(self::ms(160)).with_easing(ease_out_quint()),
                        |el, p| el.opacity(p).top(px(28.0 + 6.0 * p)),
                    ),
            )
            .with_priority(1)
        });
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px(px(40.0))
            .pt(px(34.0))
            .pb(px(26.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(icon("sparkles").size(px(20.0)).text_color(running_ink))
                    .child(
                        div()
                            .text_size(px(24.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.text)
                            .child("Agents"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(18.0))
                    .child(div().text_size(px(14.5)).text_color(t.muted).child(format!(
                        "{} agent{}",
                        s.agents,
                        if s.agents == 1 { "" } else { "s" }
                    )))
                    .when(s.running > 0, |r| {
                        r.child(
                            div()
                                .text_size(px(14.5))
                                .text_color(running_ink)
                                .child(format!("{} running", s.running)),
                        )
                    })
                    .when(!waiting.is_empty(), |r| {
                        r.child(
                            div()
                                .relative()
                                .child(
                                    div()
                                        .id("needs-you")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(5.0))
                                        .h(px(28.0))
                                        .px(px(11.0))
                                        .rounded(px(9.0))
                                        .bg(ask_bg)
                                        .text_color(ask_ink)
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .cursor_pointer()
                                        .hover(|s| s.opacity(0.9))
                                        .child(format!("{} needs you", waiting.len()))
                                        .child(
                                            icon("chevron-down").size(px(14.0)).text_color(ask_ink),
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let open = !this.open;
                                            this.set_open(open, cx)
                                        })),
                                )
                                .children(popover),
                        )
                    })
                    .child(total)
                    .child(
                        div()
                            .id("theme")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(30.0))
                            .rounded(px(9.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(t.tile))
                            .child(icon(mode_glyph).size(px(16.0)).text_color(t.muted))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let m = this.theme.mode.toggled();
                                this.set_mode(m, cx)
                            })),
                    ),
            )
    }
}

impl Render for Board {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let now = self.now;
        let (summary, branches) = {
            let tree = self.tree.read(cx);
            (
                tree.summary(now),
                tree.visible_roots(now)
                    .into_iter()
                    .map(|r| branch(tree, r, &self.collapsed, 0))
                    .collect::<Vec<_>>(),
            )
        };
        let header = self.header(&summary, cx);
        let trees: Vec<gpui::AnyElement> = branches.iter().map(|b| self.block(b, cx)).collect();
        let empty = trees.is_empty();
        div().size_full().flex().flex_col().bg(t.bg).text_color(t.text).child(header).child(
            div()
                .id("canvas")
                .flex_1()
                .min_h(px(0.0))
                .overflow_scroll()
                .px(px(40.0))
                .pb(px(48.0))
                .flex()
                .flex_col()
                .gap(px(56.0))
                .when(empty, |c| {
                    c.child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(10.0))
                            .pt(px(120.0))
                            .text_color(t.muted)
                            .child(icon("sparkles").size(px(26.0)).text_color(t.faint))
                            .child(div().text_size(px(15.0)).child("No agents are running"))
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(t.faint)
                                    .child("An agent that starts a coding session shows up here."),
                            ),
                    )
                })
                .children(trees),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_is_named_the_way_a_person_says_it() {
        for (label, want) in [
            ("anthropic/claude-sonnet-5-5", "Sonnet 5.5"),
            ("anthropic/claude-opus-5-5", "Opus 5.5"),
            ("anthropic/claude-haiku-4-5-20251001", "Haiku 4.5"),
            ("claude-sonnet-5", "Sonnet 5"),
            ("openai/gpt-5", "GPT-5"),
            ("openai/gpt-5-mini", "GPT-5 Mini"),
            ("openai/deepseek-chat", "DeepSeek Chat"),
            ("local/system", "On-device"),
            ("mock/mock", "Mock"),
        ] {
            assert_eq!(model_name(label), want, "{label}");
        }
    }

    #[test]
    fn money_is_never_a_misleading_zero_and_time_reads_as_a_clock() {
        assert_eq!(money(0), "$0.00");
        assert_eq!(money(2_500), "$0.003");
        assert_eq!(money(290_000), "$0.29");
        assert_eq!(money(2_150_000), "$2.15");
        assert_eq!(clock(45_000), "45s");
        assert_eq!(clock(69_000), "1m 09s");
        assert_eq!(clock(7_500_000), "2h 05m");
        assert_eq!(tokens(12_400), "12.4k tok");
        assert_eq!(tokens(800), "800 tok");
    }

    #[test]
    fn a_delay_waits_then_runs_the_easing_over_the_rest() {
        let (dur, f) = after(100, 300, |t| t);
        assert_eq!(dur, Duration::from_millis(400));
        assert_eq!(f(0.0), 0.0);
        assert_eq!(f(0.25), 0.0, "still waiting at 100ms");
        assert!((f(0.5) - 1.0 / 3.0).abs() < 1e-5);
        assert_eq!(f(1.0), 1.0);
    }
}
