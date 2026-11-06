#![cfg_attr(target_family = "wasm", no_main)]

use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{px, size, App, Application, Bounds, WindowBounds, WindowHandle, WindowOptions};

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
    tour: bool,
    tour_skills: bool,
    /// The runtime to show (its URL and admin token); without it, the scripted demo.
    runtime: Option<(String, String)>,
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
        let actions: Rc<dyn Actions> = match &o.runtime {
            #[cfg(not(target_family = "wasm"))]
            Some((url, token)) => {
                let r = agent_board::remote::Remote::new(url, token);
                r.follow(tree.clone(), cx);
                Rc::new(r)
            }
            _ => demo::play(tree.clone(), o.auto, cx),
        };
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
                        let board = Board::new(tree, actions, mode, cx).with_element(
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
        if o.tour {
            cx.spawn(async move |cx| {
                let clock = cx.background_executor().clone();
                let wait = |ms| clock.timer(Duration::from_millis(ms));
                eprintln!("[tour] starting tour...");
                wait(400).await;
                let _ = window.update(cx, |board, window, cx| {
                    board.set_tab(Tab::Agents, window, cx);
                    board.show(Tab::Agents, "lead", cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                wait(600).await;
                // 1. Open Batch Queue Inspector
                let _ = window.update(cx, |board, _, cx| {
                    board.toggle_queue(cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[tour] toggle queue open");
                wait(2200).await;
                // 2. Close Batch Queue Inspector
                let _ = window.update(cx, |board, _, cx| {
                    board.toggle_queue(cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[tour] toggle queue closed");
                wait(700).await;
                // 3. Expand model reasoning & tool trace
                let _ = window.update(cx, |board, _, cx| {
                    board.expand_first_exchange(cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[tour] expand trace");
                wait(1600).await;
                // 4. Toggle memories in sidebar
                let _ = window.update(cx, |board, _, cx| {
                    board.toggle_memories(cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[tour] toggle memories collapse");
                wait(600).await;
                let _ = window.update(cx, |board, _, cx| {
                    board.toggle_memories(cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[tour] toggle memories expand");
                wait(800).await;
                // 5. Voice mic: active listening state
                let _ = window.update(cx, |board, _, cx| {
                    board.toggle_mic(cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[tour] voice mic listening");
                wait(1100).await;
                // 6. Voice mic turns off & transcribed text streams into input bar
                let prompt = "How did the enqueued batch requests work out?";
                let say = window.update(cx, |board, _, cx| {
                    board.toggle_mic(cx);
                    let say = board.say_field();
                    say.update(cx, |f, cx| f.clear(cx));
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                    say
                });
                eprintln!("[tour] voice mic off, typing prompt");
                if let Ok(say) = say {
                    for c in prompt.chars() {
                        say.update(cx, |f, cx| f.push_str(&c.to_string(), cx));
                        let _ = window.update(cx, |_, _, _| {
                            #[cfg(target_os = "macos")]
                            agent_board::capture::flush_window();
                        });
                        wait(35).await;
                    }
                }
                wait(400).await;
                // 7. Submit prompt -> response bubble appears and finishes
                let _ = window.update(cx, |board, _, cx| {
                    board.say_to(prompt.to_string(), cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[tour] prompt submitted, awaiting response");
                wait(2400).await;
            })
            .detach();
        }
        if o.tour_skills {
            cx.spawn(async move |cx| {
                let clock = cx.background_executor().clone();
                let wait = |ms| clock.timer(Duration::from_millis(ms));
                eprintln!("[skills-tour] starting skills workflow tour...");
                wait(400).await;

                // 1. Open Explore Tab on WIT Contract-First Scaffolder
                let _ = window.update(cx, |board, window, cx| {
                    board.set_tab(Tab::Explore, window, cx);
                    board.show(Tab::Explore, "skill:wit:contract_first_scaffold", cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[skills-tour] focus skill:wit:contract_first_scaffold");
                wait(2200).await;

                // 2. Click sub-skill bindings:generate_xtask
                let _ = window.update(cx, |board, _, cx| {
                    board.show(Tab::Explore, "skill:bindings:generate_xtask", cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[skills-tour] focus sub-skill bindings:generate_xtask");
                wait(1600).await;

                // 3. Switch to Autonomous Bug Repro & Repair DAG
                let _ = window.update(cx, |board, _, cx| {
                    board.show(Tab::Explore, "skill:bug:autonomous_repro_and_repair", cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[skills-tour] focus skill:bug:autonomous_repro_and_repair");
                wait(2000).await;

                // 4. Switch to Skill Curator & Hygiene DAG
                let _ = window.update(cx, |board, _, cx| {
                    board.show(Tab::Explore, "skill:curate_registry", cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[skills-tour] focus skill:curate_registry");
                wait(2000).await;

                // 5. Switch to Agents tab with lead agent
                let _ = window.update(cx, |board, window, cx| {
                    board.set_tab(Tab::Agents, window, cx);
                    board.show(Tab::Agents, "lead", cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[skills-tour] switch to Agents tab");
                wait(1000).await;

                // 6. Type workflow prompt for skill execution
                let prompt = "Execute skill wit:contract_first_scaffold for component kv-store";
                let say = window.update(cx, |board, _, cx| {
                    let say = board.say_field();
                    say.update(cx, |f, cx| f.clear(cx));
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                    say
                });
                if let Ok(say) = say {
                    for c in prompt.chars() {
                        say.update(cx, |f, cx| f.push_str(&c.to_string(), cx));
                        let _ = window.update(cx, |_, _, _| {
                            #[cfg(target_os = "macos")]
                            agent_board::capture::flush_window();
                        });
                        wait(28).await;
                    }
                }
                wait(400).await;

                // 7. Submit prompt -> live execution response
                let _ = window.update(cx, |board, _, cx| {
                    board.say_to(prompt.to_string(), cx);
                    #[cfg(target_os = "macos")]
                    agent_board::capture::flush_window();
                });
                eprintln!("[skills-tour] submitted workflow call, waiting for completion");
                wait(2600).await;
            })
            .detach();
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
        #[arg(long)]
        tour: bool,
        #[arg(long)]
        tour_skills: bool,
        #[arg(long, value_name = "GOAL")]
        goal: Option<String>,
        #[arg(long, value_parser = ["sessions", "agents", "explore", "map", "keys"])]
        tab: Option<String>,
        #[arg(long, value_name = "KEY", requires = "tab")]
        select: Option<String>,
        #[arg(long, value_name = "SERVER")]
        matrix: Option<String>,
        #[arg(long, value_name = "FILE", requires = "film")]
        gif: Option<String>,
        #[arg(long, value_parser = parse_size, default_value = "1320x900")]
        size: (f32, f32),
        /// A running agent-runtime to show and drive, e.g. http://127.0.0.1:18017.
        #[arg(long, env = "HOLON_RUNTIME_URL")]
        runtime: Option<String>,
        /// Its admin token, read from this file.
        #[arg(
            long,
            env = "HOLON_RUNTIME_TOKEN_FILE",
            default_value = "~/.holon-agents/admin-token"
        )]
        runtime_token_file: String,
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
        let is_tour = a.tour;
        let is_skills_tour = a.tour_skills;
        let o = Opts {
            light: a.mode == "light",
            tab: if is_tour {
                Some(Tab::Agents)
            } else if is_skills_tour {
                Some(Tab::Explore)
            } else {
                a.tab.as_deref().and_then(tab_named)
            },
            select: if is_tour {
                Some("lead".into())
            } else if is_skills_tour {
                Some("skill:wit:contract_first_scaffold".into())
            } else {
                a.select.clone()
            },
            open: a.open,
            form: a.form,
            goal: a.goal.clone(),
            auto: a
                .auto
                .map(Duration::from_secs_f32)
                .or((a.shoot.is_some() || a.gif.is_some() || a.tour || a.tour_skills)
                    .then(|| Duration::from_secs(14))),
            size: a.size,
            tour: a.tour,
            tour_skills: a.tour_skills,
            matrix: a.matrix.clone(),
            runtime: a
                .runtime
                .clone()
                .or_else(|| {
                    if a.shoot.is_none()
                        && a.gif.is_none()
                        && !a.tour
                        && !a.tour_skills
                        && a.auto.is_none()
                    {
                        let home = std::env::var("HOME").unwrap_or_default();
                        let f = a.runtime_token_file.replacen('~', &home, 1);
                        if std::path::Path::new(&f).exists() {
                            let probe = reqwest::blocking::Client::builder()
                                .timeout(Duration::from_millis(150))
                                .build()
                                .ok()
                                .and_then(|c| c.get("http://127.0.0.1:18017/status").send().ok());
                            if probe.is_some() {
                                return Some("http://127.0.0.1:18017".into());
                            }
                        }
                    }
                    None
                })
                .map(|url| {
                    let home = std::env::var("HOME").unwrap_or_default();
                    let f = a.runtime_token_file.replacen('~', &home, 1);
                    let token = std::fs::read_to_string(&f)
                        .unwrap_or_else(|e| panic!("the runtime's admin token, {f}: {e}"));
                    (url, token.trim().to_string())
                }),
        };
        let photographing = a.shoot.is_some() || a.gif.is_some();
        run(
            gpui_platform::application(),
            o,
            move |w| WindowOptions {
                kind: if photographing { WindowKind::PopUp } else { WindowKind::Normal },
                titlebar: Some(TitlebarOptions {
                    title: Some("Agents".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.0), px(18.0))),
                }),
                ..w
            },
            move |window, mode, cx| photograph(a, window, mode, cx),
        );
    }

    #[allow(unused_variables)]
    fn photograph(a: Args, window: WindowHandle<Board>, mode: Mode, cx: &mut App) {
        #[cfg(target_os = "macos")]
        if let (Some((start, count, step)), gif, dir) = (a.film, a.gif.clone(), a.shoot.clone()) {
            if let Some(d) = dir.as_ref() {
                std::fs::create_dir_all(d).ok();
            }
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_secs_f32(start)).await;
                let mut film: Option<agent_board::film::Gif> = None;
                for i in 0..count {
                    let at = std::time::Instant::now();
                    let _ = window.update(cx, |_, _, _| {
                        agent_board::capture::flush_window();
                    });
                    if let Some(d) = dir.as_ref() {
                        let path = format!("{d}/film-{i:03}.png");
                        if let Err(e) = agent_board::capture::own_window_png(&path) {
                            eprintln!("shot {path} failed: {e}");
                        }
                    }
                    if let Some(g) = gif.as_ref() {
                        let made =
                            agent_board::capture::own_window_rgba().and_then(|(rgba, w, h)| {
                                let (rgba, w, h) = agent_board::film::half(&rgba, w, h);
                                if film.is_none() {
                                    film = Some(agent_board::film::Gif::create(g, w, h)?);
                                }
                                film.as_mut().map_or(Ok(()), |f| f.push(rgba, at))
                            });
                        if let Err(e) = made {
                            eprintln!("film {g}: {e}");
                            std::process::exit(1);
                        }
                    }
                    cx.background_executor().timer(Duration::from_millis(step)).await;
                }
                if let Some(f) = film {
                    if let Err(e) = f.finish(std::time::Instant::now()) {
                        eprintln!("film finish: {e}");
                        std::process::exit(1);
                    }
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
                    let _ = window.update(cx, |_, _, _| {
                        agent_board::capture::flush_window();
                    });
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
