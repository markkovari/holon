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

use clap::Parser;
use gpui::{
    point, prelude::*, px, size, App, Application, Bounds, TitlebarOptions, WindowBounds,
    WindowOptions,
};

use gpui_console::board::demo;
use gpui_console::board::icons::Assets;
use gpui_console::board::theme::Mode;
use gpui_console::board::{AgentTree, Board};

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
    /// The window's size, WIDTHxHEIGHT.
    #[arg(long, value_parser = parse_size, default_value = "1010x900")]
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

fn main() {
    let a = Args::parse();
    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        let tree = cx.new(|_| AgentTree::default());
        let auto = a
            .auto
            .map(Duration::from_secs_f32)
            .or(a.shoot.as_ref().map(|_| Duration::from_secs(6)));
        let actions = demo::play(tree.clone(), auto, cx);
        let bounds = Bounds::centered(None, size(px(a.size.0), px(a.size.1)), cx);
        let mode = if a.mode == "light" { Mode::Light } else { Mode::Dark };
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
