#![cfg_attr(target_family = "wasm", no_main)]
//! `agents-board`: the agent board over a scripted run, with no lattice, model or key, natively or in a
//! browser.
//!
//!   cargo run --bin agents-board                       # watch it
//!   cargo run --bin agents-board -- --mode light
//!   cargo run --bin agents-board -- --shoot /tmp/shots --at 4,10,20,30   # photograph it
//!
//! `--shoot DIR` writes `DIR/board-<mode>-<seconds>.png` at each time in `--at` (seconds after
//! start) from inside the process, approves the approval itself after `--auto` seconds, and exits
//! after the last shot. `--film START:COUNT:STEP_MS` instead takes COUNT shots, STEP_MS apart,
//! from START seconds in, as `DIR/film-NN.png`: how a motion is looked at. With `--gif FILE` the
//! same frames are one GIF instead, at the size a person sees the window:
//!
//!   cargo run --bin agents-board -- --open --film 0:200:120 --gif docs/media/agents-board.gif
//!
//! In a browser (README.md says how it is built) the same choices are the page's query:
//! `index.html?tab=explore&select=component:record-store&mode=light&auto=6`.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    prelude::*, px, size, App, Application, Bounds, WindowBounds, WindowHandle, WindowOptions,
};

use agent_board::demo;
use agent_board::icons::Assets;
use agent_board::map::Tab;
use agent_board::theme::Mode;
use agent_board::{Actions, AgentTree, Board};

/// What to show, however it was asked for: flags natively, the page's query in a browser.
#[derive(Default)]
struct Opts {
    light: bool,
    tab: Option<Tab>,
    select: Option<String>,
    open: bool,
    form: bool,
    goal: Option<String>,
    auto: Option<Duration>,
    size: (f32, f32),
}

fn tab_named(s: &str) -> Option<Tab> {
    match s {
        "explore" => Some(Tab::Explore),
        "map" => Some(Tab::Map),
        "sessions" => Some(Tab::Sessions),
        _ => None,
    }
}

/// Opens the board and plays the story into it; `options` adds what only one platform has.
fn run(
    app: Application,
    o: Opts,
    options: impl FnOnce(WindowOptions) -> WindowOptions + 'static,
    then: impl FnOnce(WindowHandle<Board>, Mode, &mut App) + 'static,
) {
    app.with_assets(Assets).run(move |cx: &mut App| {
        #[cfg(target_family = "wasm")]
        cx.text_system().add_fonts(agent_board::web_fonts()).expect("the board's fonts");
        let tree = cx.new(|_| AgentTree::default());
        let actions = demo::play(tree.clone(), o.auto, cx);
        let bounds = Bounds::centered(None, size(px(o.size.0), px(o.size.1)), cx);
        let mode = if o.light { Mode::Light } else { Mode::Dark };
        let window = cx
            .open_window(
                options(WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                }),
                move |_, cx| {
                    cx.new(|cx| {
                        let board = Board::new(tree, actions as Rc<dyn Actions>, mode, cx);
                        // a page has no traffic lights to leave room for
                        if cfg!(target_family = "wasm") {
                            board.with_inset(18.0)
                        } else {
                            board
                        }
                    })
                },
            )
            .expect("open the window");
        cx.activate(true);
        if o.open {
            window.update(cx, |board, _, cx| board.set_open(true, cx)).expect("open the list");
        }
        if let Some(tab) = o.tab {
            let select = o.select.clone();
            window
                .update(cx, |board, window, cx| {
                    board.set_tab(tab, window, cx);
                    if let Some(k) = select.as_deref() {
                        board.show(tab, k, cx);
                    }
                })
                .expect("open the tab");
        }
        if o.form {
            window
                .update(cx, |board, window, cx| board.open_form(window, cx))
                .expect("open the form");
        }
        if let Some(goal) = o.goal.clone() {
            cx.spawn(async move |cx| {
                let clock = cx.background_executor().clone();
                let wait = |ms| clock.timer(Duration::from_millis(ms));
                wait(2_000).await;
                let field = window.update(cx, |board, window, cx| {
                    board.open_form(window, cx);
                    board.field()
                });
                let Ok(field) = field else { return };
                wait(500).await;
                for c in goal.chars() {
                    field.update(cx, |f, cx| f.push_str(&c.to_string(), cx));
                    wait(28).await;
                }
                wait(600).await;
                window.update(cx, |board, _, cx| board.submit(goal, cx)).ok();
            })
            .detach();
        }
        then(window, mode, cx);
    });
}

#[cfg(target_family = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    gpui_platform::web_init();
    // `?tab=components&select=component:x&mode=light&open&form&goal=...&auto=6`
    let query = web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default();
    let mut o = Opts { size: (1320.0, 900.0), ..Default::default() };
    for pair in query.trim_start_matches('?').split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v: String = js_sys::decode_uri_component(&v.replace('+', " "))
            .map(String::from)
            .unwrap_or_default();
        match k {
            "mode" => o.light = v == "light",
            "tab" => o.tab = tab_named(&v),
            "select" => o.select = Some(v),
            "open" => o.open = true,
            "form" => o.form = true,
            "goal" => o.goal = Some(v),
            "auto" => o.auto = v.parse().ok().map(Duration::from_secs_f32),
            _ => {}
        }
    }
    run(gpui_platform::single_threaded_web(), o, |w| w, |_, _, _| {});
}

#[cfg(not(target_family = "wasm"))]
use native::main;

#[cfg(not(target_family = "wasm"))]
mod native {
    use super::*;
    use clap::Parser;
    use gpui::{point, TitlebarOptions, WindowKind};

    /// `agents-board`'s flags.
    #[derive(Parser)]
    #[command(name = "agents-board", about = "The agent board over a scripted run")]
    struct Args {
        /// `dark` or `light`.
        #[arg(long, value_parser = ["dark", "light"], default_value = "dark")]
        mode: String,
        /// Write screenshots of the window into DIR, from inside the process, and exit.
        #[arg(long, value_name = "DIR")]
        shoot: Option<String>,
        /// With `--shoot`: the seconds after start to photograph at.
        #[arg(long, value_delimiter = ',', default_values_t = [4.0, 11.0, 24.0])]
        at: Vec<f32>,
        /// Approve the approval after this many seconds if nobody has (a `--shoot` does, after 6).
        #[arg(long)]
        auto: Option<f32>,
        /// With `--shoot`: START:COUNT:STEP_MS, a burst of frames to look at a motion.
        #[arg(long, value_parser = parse_film)]
        film: Option<(f32, usize, u64)>,
        /// Open the list of what is waiting for a person as soon as there is one.
        #[arg(long)]
        open: bool,
        /// Open the form that gives an agent a goal.
        #[arg(long)]
        form: bool,
        /// Two seconds in, type GOAL into the form and start it, as a person would.
        #[arg(long, value_name = "GOAL")]
        goal: Option<String>,
        /// Open on `explore` or `map` instead of the sessions.
        #[arg(long, value_parser = ["sessions", "explore", "map"])]
        tab: Option<String>,
        /// With `--tab`: look at KEY there (`component:rate-limiter`), as following a reference would.
        #[arg(long, value_name = "KEY", requires = "tab")]
        select: Option<String>,
        /// With `--film`: write the frames as one GIF at FILE, not as PNGs.
        #[arg(long, value_name = "FILE", requires = "film")]
        gif: Option<String>,
        /// The window's size, WIDTHxHEIGHT.
        #[arg(long, value_parser = parse_size, default_value = "1320x900")]
        size: (f32, f32),
    }

    fn parse_film(s: &str) -> Result<(f32, usize, u64), String> {
        let p: Vec<&str> = s.split(':').collect();
        match p.as_slice() {
            [t, n, step] => Ok((
                t.parse().map_err(|e| format!("start: {e}"))?,
                n.parse().map_err(|e| format!("count: {e}"))?,
                step.parse().map_err(|e| format!("step: {e}"))?,
            )),
            _ => Err("write it as START:COUNT:STEP_MS, for example 1.2:10:90".into()),
        }
    }

    fn parse_size(s: &str) -> Result<(f32, f32), String> {
        let (w, h) = s.split_once('x').ok_or("write it as WIDTHxHEIGHT")?;
        Ok((
            w.parse().map_err(|e| format!("width: {e}"))?,
            h.parse().map_err(|e| format!("height: {e}"))?,
        ))
    }

    pub fn main() {
        let a = Args::parse();
        let o = Opts {
            light: a.mode == "light",
            tab: a.tab.as_deref().and_then(tab_named),
            select: a.select.clone(),
            open: a.open,
            form: a.form,
            goal: a.goal.clone(),
            auto: a
                .auto
                .map(Duration::from_secs_f32)
                .or((a.shoot.is_some() || a.gif.is_some()).then(|| Duration::from_secs(6))),
            size: a.size,
        };
        let photographing = a.shoot.is_some() || a.gif.is_some();
        run(
            gpui_platform::application(),
            o,
            move |w| WindowOptions {
                // macOS stops drawing a window something covers, and a photograph of it would be its
                // first frame: while photographing, it is above everything
                kind: if photographing { WindowKind::PopUp } else { WindowKind::Normal },
                titlebar: Some(TitlebarOptions {
                    title: Some("Agents".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.0), px(18.0))),
                }),
                ..w
            },
            move |_window, mode, cx| photograph(a, mode, cx),
        );
    }

    /// `--shoot` and `--film`: the window, from inside, at the times asked for.
    #[allow(unused_variables)]
    fn photograph(a: Args, mode: Mode, cx: &mut App) {
        #[cfg(target_os = "macos")]
        if let (Some(gif), Some((start, count, step))) = (a.gif.clone(), a.film) {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_secs_f32(start)).await;
                let mut film: Option<agent_board::film::Gif> = None;
                for _ in 0..count {
                    let at = std::time::Instant::now();
                    let made = agent_board::capture::own_window_rgba().and_then(|(rgba, w, h)| {
                        let (rgba, w, h) = agent_board::film::half(&rgba, w, h);
                        if film.is_none() {
                            film = Some(agent_board::film::Gif::create(&gif, w, h)?);
                        }
                        film.as_mut().map_or(Ok(()), |f| f.push(rgba, at))
                    });
                    if let Err(e) = made {
                        eprintln!("film {gif}: {e}");
                        std::process::exit(1);
                    }
                    cx.background_executor().timer(Duration::from_millis(step)).await;
                }
                if let Some(f) = film {
                    if let Err(e) = f.finish(std::time::Instant::now()) {
                        eprintln!("film {gif}: {e}");
                        std::process::exit(1);
                    }
                }
                eprintln!("film: {gif}, {count} frames from {start}s every {step}ms");
                std::process::exit(0);
            })
            .detach();
        } else if let (Some(dir), Some((start, count, step))) = (a.shoot.clone(), a.film) {
            std::fs::create_dir_all(&dir).ok();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_secs_f32(start)).await;
                for i in 0..count {
                    let path = format!("{dir}/film-{i:02}.png");
                    if let Err(e) = agent_board::capture::own_window_png(&path) {
                        eprintln!("shot {path} failed: {e}");
                    }
                    cx.background_executor().timer(Duration::from_millis(step)).await;
                }
                eprintln!("film: {count} frames from {start}s every {step}ms");
                std::process::exit(0);
            })
            .detach();
        } else if let Some(dir) = a.shoot.clone() {
            let times = a.at.clone();
            std::fs::create_dir_all(&dir).ok();
            cx.spawn(async move |cx| {
                let mut last = 0.0f32;
                for t in times {
                    cx.background_executor()
                        .timer(Duration::from_secs_f32((t - last).max(0.0)))
                        .await;
                    last = t;
                    let path = format!(
                        "{dir}/board-{}-{:04.1}.png",
                        if mode == Mode::Dark { "dark" } else { "light" },
                        t
                    );
                    match agent_board::capture::own_window_png(&path) {
                        Ok((w, h)) => eprintln!("shot {path} ({w}x{h})"),
                        Err(e) => eprintln!("shot {path} failed: {e}"),
                    }
                }
                std::process::exit(0);
            })
            .detach();
        }
    }
}
