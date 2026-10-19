//! `agents-board`: the agent board over a scripted run, with no lattice, model or key.
//!
//!   cargo run --bin agents-board                       # watch it
//!   cargo run --bin agents-board -- --mode light
//!   cargo run --bin agents-board -- --shoot /tmp/shots --at 4,10,20,30   # photograph it
//!
//! `--shoot DIR` writes `DIR/board-<mode>-<seconds>.png` at each time in `--at` (seconds after
//! start) from inside the process, approves the approval itself after `--auto` seconds, and exits
//! after the last shot. `--film START:COUNT:STEP_MS` instead takes COUNT shots, STEP_MS apart,
//! from START seconds in, as `DIR/film-NN.png`: how a motion is looked at.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    point, prelude::*, px, size, App, Application, Bounds, TitlebarOptions, WindowBounds,
    WindowOptions,
};

use gpui_console::board::demo;
use gpui_console::board::icons::Assets;
use gpui_console::board::theme::Mode;
use gpui_console::board::{AgentTree, Board};

struct Args {
    mode: Mode,
    shoot: Option<String>,
    at: Vec<f32>,
    auto: Option<f32>,
    film: Option<(f32, usize, u64)>,
    /// Open the list of what is waiting for a person as soon as there is one.
    open: bool,
    width: f32,
    height: f32,
}

fn args() -> Args {
    let mut a = Args {
        mode: Mode::Dark,
        shoot: None,
        at: vec![4.0, 11.0, 24.0],
        auto: None,
        film: None,
        open: false,
        width: 1010.0,
        height: 900.0,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = |it: &mut std::iter::Skip<std::env::Args>| it.next().unwrap_or_default();
        match k.as_str() {
            "--mode" => a.mode = if v(&mut it) == "light" { Mode::Light } else { Mode::Dark },
            "--shoot" => a.shoot = Some(v(&mut it)),
            "--at" => a.at = v(&mut it).split(',').filter_map(|s| s.trim().parse().ok()).collect(),
            "--auto" => a.auto = v(&mut it).parse().ok(),
            "--open" => a.open = true,
            "--film" => {
                let spec = v(&mut it);
                let p: Vec<&str> = spec.split(':').collect();
                if let [t, n, step] = p.as_slice() {
                    a.film = Some((
                        t.parse().unwrap_or(0.0),
                        n.parse().unwrap_or(8),
                        step.parse().unwrap_or(60),
                    ));
                }
            }
            "--size" => {
                let s = v(&mut it);
                if let Some((w, h)) = s.split_once('x') {
                    a.width = w.parse().unwrap_or(a.width);
                    a.height = h.parse().unwrap_or(a.height);
                }
            }
            other => eprintln!("agents-board: ignoring {other}"),
        }
    }
    a
}

fn main() {
    let a = args();
    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        let tree = cx.new(|_| AgentTree::default());
        let auto = a
            .auto
            .map(Duration::from_secs_f32)
            .or(a.shoot.as_ref().map(|_| Duration::from_secs(6)));
        let actions = demo::play(tree.clone(), auto, cx);
        let bounds = Bounds::centered(None, size(px(a.width), px(a.height)), cx);
        let mode = a.mode;
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Agents".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(16.0), px(18.0))),
                    }),
                    ..Default::default()
                },
                move |_, cx| {
                    cx.new(|cx| {
                        Board::new(tree, actions as Rc<dyn gpui_console::board::Actions>, mode, cx)
                    })
                },
            )
            .expect("open the window");
        cx.activate(true);
        if a.open {
            window.update(cx, |board, _, cx| board.set_open(true, cx)).expect("open the list");
        }

        #[cfg(target_os = "macos")]
        if let (Some(dir), Some((start, count, step))) = (a.shoot.clone(), a.film) {
            std::fs::create_dir_all(&dir).ok();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_secs_f32(start)).await;
                for i in 0..count {
                    let path = format!("{dir}/film-{i:02}.png");
                    if let Err(e) = gpui_console::capture::own_window_png(&path) {
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
                    match gpui_console::capture::own_window_png(&path) {
                        Ok((w, h)) => eprintln!("shot {path} ({w}x{h})"),
                        Err(e) => eprintln!("shot {path} failed: {e}"),
                    }
                }
                std::process::exit(0);
            })
            .detach();
        }
    });
}
