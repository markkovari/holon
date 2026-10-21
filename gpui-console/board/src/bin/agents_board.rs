#![cfg_attr(target_family = "wasm", no_main)]

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
    matrix: Option<String>,
}

fn tab_named(s: &str) -> Option<Tab> {
    match s {
        "explore" => Some(Tab::Explore),
        "agents" => Some(Tab::Agents),
        "map" => Some(Tab::Map),
        "sessions" => Some(Tab::Sessions),
        "keys" => Some(Tab::Keys),
        _ => None,
    }
}

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
                        let board = Board::new(tree, actions as Rc<dyn Actions>, mode, cx)
                            .with_element(
                                o.matrix.as_deref().map(agent_board::agents::Element::on),
                            );
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
            "matrix" => o.matrix = Some(v),
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

    #[derive(Parser)]
    #[command(name = "agents-board", about = "The agent board over a scripted run")]
    struct Args {
        #[arg(long, value_parser = ["dark", "light"], default_value = "dark")]
        mode: String,
        #[arg(long, value_name = "DIR")]
        shoot: Option<String>,
        #[arg(long, value_delimiter = ',', default_values_t = [4.0, 11.0, 24.0])]
        at: Vec<f32>,
        #[arg(long)]
        auto: Option<f32>,
        #[arg(long, value_parser = parse_film)]
        film: Option<(f32, usize, u64)>,
        #[arg(long)]
        open: bool,
        #[arg(long)]
        form: bool,
        #[arg(long, value_name = "GOAL")]
        goal: Option<String>,
        #[arg(long, value_parser = ["sessions", "agents", "explore", "map"])]
        tab: Option<String>,
        #[arg(long, value_name = "KEY", requires = "tab")]
        select: Option<String>,
        #[arg(long, value_name = "SERVER")]
        matrix: Option<String>,
        #[arg(long, value_name = "FILE", requires = "film")]
        gif: Option<String>,
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
            matrix: a.matrix.clone(),
        };
        let photographing = a.shoot.is_some() || a.gif.is_some();
        run(
            gpui_platform::application(),
            o,
            move |w| WindowOptions {
                // macOS stops drawing a covered window, so photographs are taken from a pop-up
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
