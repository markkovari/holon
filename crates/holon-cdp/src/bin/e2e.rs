//! Native two-tier CDP runner for the ten showcase E2E apps (`e2e/run_all.sh`) and optional
//! delta-GIF recording (`--record-gif-dir`).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use holon_cdp::{free_port, http_get_ok, BrowserServer, EngineKind, Gif, Page};

const DEFAULT_APPS: &[&str] = &[
    "book-lending-domain",
    "ecommerce-fulfillment-domain",
    "moderation-domain",
    "photosocial-domain",
    "pipeline-domain",
    "real-estate-escrow-domain",
    "smart-home-domain",
    "support-domain",
    "ticket-triage-domain",
    "volunteer-shift-domain",
];

#[derive(Parser)]
#[command(
    name = "holon-e2e",
    about = "Run showcase E2E suites over CDP (Lightpanda for fast E2E, Chromium for visual GIF recording)."
)]
struct Args {
    /// Browser engine: `auto` (Lightpanda -> Chromium), `lightpanda`, or `chromium`.
    #[arg(long, default_value = "auto")]
    engine: String,

    /// Optional directory to record delta-compressed `.gif` files for each app run.
    #[arg(long)]
    record_gif_dir: Option<PathBuf>,

    /// Width of recorded GIFs in pixels.
    #[arg(long, default_value_t = 800)]
    gif_width: usize,

    /// Height of recorded GIFs in pixels.
    #[arg(long, default_value_t = 500)]
    gif_height: usize,

    /// Specific app domains to run (defaults to all 10 showcase domains).
    apps: Vec<String>,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct HostedApp {
    url: String,
    child: Child,
    run_dir: tempfile::TempDir,
}

impl HostedApp {
    fn start(root: &Path, app: &str, artifact: &Path) -> Result<Self> {
        let name = app.strip_suffix("-domain").unwrap_or(app);
        let port = free_port()?;
        let run_dir = tempfile::tempdir().context("tempdir for comp-host")?;
        let log_path = run_dir.path().join("host.log");
        let log_file = std::fs::File::create(&log_path)?;
        let log_err = log_file.try_clone()?;
        let host_bin = root.join("target/release/comp-host");
        if !host_bin.is_file() {
            bail!("missing {} — build comp-host first", host_bin.display());
        }

        let child = Command::new(&host_bin)
            .current_dir(run_dir.path())
            .args([
                "--app",
                app,
                "--component",
                &artifact.to_string_lossy(),
                "--addr",
                &format!("127.0.0.1:{port}"),
                "--kv",
                "sqlite",
                "--config",
                &format!("default-tenant={name}"),
                "--config-file",
                &root.join("e2e/e2e.conf").to_string_lossy(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_err))
            .spawn()
            .with_context(|| format!("spawn comp-host for {app}"))?;

        let url = format!("http://127.0.0.1:{port}");
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if http_get_ok(port, "/", Duration::from_millis(400)) {
                return Ok(Self { url, child, run_dir });
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        bail!("comp-host for {app} never answered on {url}:\n{log}")
    }

    fn log_tail(&self) -> String {
        let log_path = self.run_dir.path().join("host.log");
        let text = std::fs::read_to_string(log_path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(20);
        lines[start..].join("\n")
    }
}

impl Drop for HostedApp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn snap(page: &mut Page, gif: &mut Option<Gif>) {
    if let Some(g) = gif.as_mut() {
        let _ = page.record_step(g);
    }
}

fn run_spec(name: &str, page: &mut Page, base_url: &str, gif: &mut Option<Gif>) -> Result<()> {
    match name {
        "book-lending" => spec_book_lending(page, base_url, gif),
        "ecommerce-fulfillment" => spec_ecommerce_fulfillment(page, base_url, gif),
        "moderation" => spec_moderation(page, base_url, gif),
        "photosocial" => spec_photosocial(page, base_url, gif),
        "pipeline" => spec_pipeline(page, base_url, gif),
        "real-estate-escrow" => spec_real_estate_escrow(page, base_url, gif),
        "smart-home" => spec_smart_home(page, base_url, gif),
        "support" => spec_support(page, base_url, gif),
        "ticket-triage" => spec_ticket_triage(page, base_url, gif),
        "volunteer-shift" => spec_volunteer_shift(page, base_url, gif),
        other => bail!("unknown E2E spec '{other}'"),
    }
}

fn spec_book_lending(page: &mut Page, base_url: &str, gif: &mut Option<Gif>) -> Result<()> {
    page.goto(&format!("{base_url}/"))?;
    snap(page, gif);
    page.click("#login-alice")?;
    page.wait_for_text(".toast >> :last", "Authenticated as alice")?;
    page.wait_for_hidden("#auth-overlay")?;
    snap(page, gif);

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-overlay")?;
    page.click("#login-lib")?;
    page.wait_for_text(".toast >> :last", "Authenticated as lib")?;
    page.wait_for_hidden("#auth-overlay")?;

    page.fill("#book-title", "The Left Hand of Darkness")?;
    page.click("button:has-text(\"Add to Catalog\")")?;
    page.wait_for_text(".toast >> :last", "Book added successfully!")?;
    page.wait_for_count(".book-card", 1)?;
    page.wait_for_text(".book-card >> :first >> .book-title", "The Left Hand of Darkness")?;
    page.wait_for_text(".book-card >> :first >> .book-callnum", "BK-")?;
    snap(page, gif);

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-overlay")?;
    page.click("#login-alice")?;
    page.wait_for_text(".toast >> :last", "Authenticated as alice")?;
    page.wait_for_hidden("#auth-overlay")?;
    page.wait_for_count(".book-card", 1)?;
    page.click(".book-card >> :first >> .borrow-btn")?;
    page.wait_for_text(".toast >> :last", "Book borrowed!")?;
    page.wait_for_visible(".status-borrowed")?;
    snap(page, gif);

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-overlay")?;
    page.click("#login-bob")?;
    page.wait_for_text(".toast >> :last", "Authenticated as bob")?;
    page.wait_for_hidden("#auth-overlay")?;
    page.wait_for_hidden(".borrow-btn")?;
    page.wait_for_visible(".return-btn")?;
    page.click(".return-btn")?;
    page.wait_for_text(".toast >> :last", "Access Denied")?;
    snap(page, gif);

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-overlay")?;
    page.click("#login-alice")?;
    page.wait_for_text(".toast >> :last", "Authenticated as alice")?;
    page.wait_for_hidden("#auth-overlay")?;
    page.click(".return-btn")?;
    page.wait_for_text(".toast >> :last", "Book returned successfully!")?;
    page.wait_for_visible(".status-available")?;
    page.wait_for_visible(".borrow-btn")?;

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-overlay")?;
    page.click("#login-bob")?;
    page.wait_for_text(".toast >> :last", "Authenticated as bob")?;
    page.wait_for_hidden("#auth-overlay")?;
    page.click(".borrow-btn")?;
    page.wait_for_text(".toast >> :last", "Book borrowed!")?;

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-overlay")?;
    page.click("#login-lib")?;
    page.wait_for_text(".toast >> :last", "Authenticated as lib")?;
    page.wait_for_hidden("#auth-overlay")?;
    page.click(".return-btn")?;
    page.wait_for_text(".toast >> :last", "Book returned successfully!")?;
    page.wait_for_visible(".status-available")?;
    snap(page, gif);
    Ok(())
}

fn spec_ecommerce_fulfillment(
    page: &mut Page,
    base_url: &str,
    gif: &mut Option<Gif>,
) -> Result<()> {
    page.goto(&format!("{base_url}/"))?;
    snap(page, gif);
    page.select_option("#role-select", "customer")?;
    page.click("#login-btn")?;
    page.wait_for_text("#toast-container", "Authenticated as customer")?;
    page.wait_for_visible("#customer-section")?;
    snap(page, gif);

    page.fill("#qty-input", "2")?;
    page.click("#buy-btn")?;
    page.wait_for_text("#toast-container", "Order")?;
    page.wait_for_text("#toast-container", "placed successfully!")?;
    snap(page, gif);

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-view")?;

    page.select_option("#role-select", "fulfillment")?;
    page.click("#login-btn")?;
    page.wait_for_text("#toast-container", "Authenticated as fulfillment")?;
    page.wait_for_visible("#fulfillment-section")?;

    page.wait_for_visible(".order-card >> :first")?;
    page.wait_for_text(".order-card >> :first >> .status", "paid")?;
    snap(page, gif);

    page.click(".order-card >> :first >> .fulfill-btn")?;
    page.wait_for_text("#toast-container", "shipped!")?;
    page.wait_for_text(".order-card >> :first >> .status", "shipped")?;
    page.wait_for_count(".order-card >> :first >> .fulfill-btn", 0)?;
    snap(page, gif);
    Ok(())
}

fn spec_moderation(page: &mut Page, base_url: &str, gif: &mut Option<Gif>) -> Result<()> {
    page.goto(base_url)?;
    snap(page, gif);
    let test_text = "CDP Test Content 42";
    page.fill("#intakeText", test_text)?;
    page.click("button:has-text(\"Submit Content\")")?;

    let item_sel = format!(".queue-item:has-text(\"{test_text}\")");
    page.wait_for_visible(&item_sel)?;
    snap(page, gif);
    page.click(&item_sel)?;

    page.wait_for_text("#reviewPanel", test_text)?;
    snap(page, gif);
    page.click("button:has-text(\"Run Automated Review\")")?;
    page.wait_for_hidden(&item_sel)?;
    snap(page, gif);
    Ok(())
}

fn spec_photosocial(page: &mut Page, base_url: &str, gif: &mut Option<Gif>) -> Result<()> {
    page.goto(base_url)?;
    page.wait_for_not_text("#userName", "Guest")?;
    page.wait_for_text("#userName", "usr_")?;
    snap(page, gif);

    page.click("#uploadBtn")?;
    page.wait_for_visible("#uploadModal")?;
    let test_title = "E2E Automated Photo 42";
    page.fill("#uploadTitle", test_title)?;
    page.fill("#uploadDesc", "This is an automated E2E photo description.")?;
    snap(page, gif);
    page.click("#uploadSubmitBtn")?;
    page.wait_for_hidden("#uploadModal")?;

    let card_sel = format!(".photo-card:has-text(\"{test_title}\") >> :first");
    page.wait_for_visible(&card_sel)?;
    snap(page, gif);

    page.click(&format!("{card_sel} >> .vote-btn >> :first"))?;
    page.wait_for_text(&format!("{card_sel} >> .score-count"), "1")?;

    page.click(&format!("{card_sel} >> .photo-title"))?;
    page.wait_for_visible("#photoModal")?;
    let _ = page.wait_for_non_empty("#modalAiNarrative")?;
    snap(page, gif);

    page.fill(".rating-slider >> :first", "9.5")?;
    page.click("button:has-text(\"Submit Ratings\")")?;
    page.wait_for_hidden("#photoModal")?;
    snap(page, gif);
    Ok(())
}

fn spec_pipeline(page: &mut Page, base_url: &str, gif: &mut Option<Gif>) -> Result<()> {
    page.goto(base_url)?;
    page.wait_for_text("#statPending", "0")?;
    snap(page, gif);

    page.click("#enqueueBtn")?;
    page.wait_for_visible("#enqueueModal")?;
    page.fill("#eventTopic", "test.event.up")?;
    page.click("button:has-text(\"Submit Event\")")?;
    page.wait_for_hidden("#enqueueModal")?;

    page.wait_for_text("#eventLog", "test.event.up")?;
    page.wait_for_text("#eventLog", "acked")?;
    snap(page, gif);

    page.click(".slider")?;
    page.wait_for_text("#sinkStatusText", "DOWN")?;

    page.click("#enqueueBtn")?;
    page.fill("#eventTopic", "test.event.down")?;
    page.click("button:has-text(\"Submit Event\")")?;

    page.wait_for_text("#eventLog", "test.event.down")?;
    page.wait_for_text_timeout("#eventLog", "dead", Duration::from_secs(15))?;
    page.wait_for_text("#deadLettersList", "test.event.down")?;
    snap(page, gif);

    page.click(".slider")?;
    page.wait_for_text("#sinkStatusText", "UP")?;
    page.click("button:has-text(\"↻ Replay Event\")")?;
    page.wait_for_text_timeout("#eventLog >> :first", "acked", Duration::from_secs(10))?;
    snap(page, gif);
    Ok(())
}

fn spec_real_estate_escrow(page: &mut Page, base_url: &str, gif: &mut Option<Gif>) -> Result<()> {
    page.goto(base_url)?;
    page.wait_for_visible("#authOverlay")?;
    snap(page, gif);

    page.fill("#authEmail", "agent@holon.test")?;
    page.fill("#authPassword", "agent1234")?;
    page.click("button:has-text(\"Register\")")?;
    page.wait_for_hidden("#authOverlay")?;
    page.wait_for_text("#userName", "usr_")?;
    snap(page, gif);

    page.click("#createTxBtn")?;
    page.wait_for_visible("#createModal")?;
    let test_title = "E2E Escrow 42";
    page.fill("#txName", test_title)?;
    page.click("#submitTxBtn")?;
    page.wait_for_hidden("#createModal")?;

    page.wait_for_visible(&format!(".tx-card:has-text(\"{test_title}\") >> :first"))?;
    snap(page, gif);
    Ok(())
}

fn spec_smart_home(page: &mut Page, base_url: &str, gif: &mut Option<Gif>) -> Result<()> {
    page.goto(&format!("{base_url}/"))?;
    snap(page, gif);
    page.click("#login-admin")?;
    page.wait_for_text(".toast >> :last", "Authenticated as usr_")?;
    page.wait_for_visible("#dashboard-section")?;
    page.wait_for_text("#devices-grid", "No devices found.")?;

    page.fill("#new-device-name", "Living Room Light")?;
    page.click("#create-device-form button[type=\"submit\"]")?;
    page.wait_for_text(".toast >> :last", "Device added successfully!")?;

    page.wait_for_text(".device-card >> :first >> .device-name", "Living Room Light")?;
    page.wait_for_text(".device-card >> :first >> .device-status", "OFF")?;
    page.wait_for_class(".device-card >> :first", "is-on", false)?;
    snap(page, gif);

    page.click(".device-card >> :first")?;
    page.wait_for_text(".toast >> :last", "Device turned ON")?;
    page.wait_for_text(".device-card >> :first >> .device-status", "ON")?;
    page.wait_for_class(".device-card >> :first", "is-on", true)?;
    snap(page, gif);

    page.click(".device-card >> :first")?;
    page.wait_for_text(".toast >> :last", "Device turned OFF")?;
    page.wait_for_text(".device-card >> :first >> .device-status", "OFF")?;
    page.wait_for_class(".device-card >> :first", "is-on", false)?;

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-section")?;

    page.click("#login-user")?;
    page.wait_for_text(".toast >> :last", "Authenticated as usr_")?;
    page.wait_for_visible("#admin-controls")?;
    page.wait_for_text("#devices-grid", "No devices found.")?;
    snap(page, gif);
    Ok(())
}

fn spec_support(page: &mut Page, base_url: &str, gif: &mut Option<Gif>) -> Result<()> {
    page.goto(base_url)?;
    snap(page, gif);
    page.click("text=New Ticket")?;
    page.wait_for_class("#createModal", "active", true)?;

    let title = "CDP Test Ticket 42";
    page.fill("#newTitle", title)?;
    page.fill("#newDescription", "I have a problem with my account.")?;
    page.click("text=Submit")?;
    page.wait_for_class("#createModal", "active", false)?;

    let item_sel = format!(".ticket-item:has-text(\"{title}\")");
    page.wait_for_visible(&item_sel)?;
    page.click(&item_sel)?;
    page.wait_for_visible(&format!(".ticket-details:has-text(\"{title}\")"))?;
    snap(page, gif);

    page.click("text=✨ AI Suggest Reply")?;
    let suggestion = page.wait_for_non_empty("#replyText")?;
    snap(page, gif);

    page.click("text=Send Reply")?;
    page.wait_for_visible(&format!(".reply-body:has-text({suggestion:?})"))?;

    page.click("text=Close Ticket")?;
    page.wait_for_visible(&format!("{item_sel} >> .status-closed"))?;
    snap(page, gif);
    Ok(())
}

fn spec_ticket_triage(page: &mut Page, base_url: &str, gif: &mut Option<Gif>) -> Result<()> {
    page.goto(&format!("{base_url}/"))?;
    snap(page, gif);
    page.click("#login-agent-billing")?;
    page.wait_for_text(".toast >> :last", "Authenticated as agent-b")?;
    page.wait_for_hidden("#auth-overlay")?;

    page.fill("#ticket-subject", "double charge")?;
    page.fill("#ticket-body", "charged twice for the same order")?;
    page.select_option("#ticket-queue", "billing")?;
    page.click("button:has-text(\"Create Ticket\")")?;
    page.wait_for_text(".toast >> :last", "Ticket created successfully!")?;

    page.fill("#search-query", "charge")?;
    page.select_option("#search-queue", "billing")?;
    page.click("button:has-text(\"Search\")")?;
    page.wait_for_count(".ticket-card", 1)?;
    page.wait_for_text(".ticket-card >> :first >> .ticket-subject", "double charge")?;
    snap(page, gif);

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-overlay")?;
    page.click("#login-agent-search")?;
    page.wait_for_text(".toast >> :last", "Authenticated as agent-s")?;
    page.wait_for_hidden("#auth-overlay")?;
    page.fill("#search-query", "charge")?;
    page.select_option("#search-queue", "billing")?;
    page.click("button:has-text(\"Search\")")?;
    page.wait_for_count(".ticket-card", 1)?;

    page.click(".ticket-card >> :first >> .resolve-btn")?;
    page.wait_for_text(".toast >> :last", "Access Denied")?;
    snap(page, gif);

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-overlay")?;
    page.click("#login-agent-billing")?;
    page.wait_for_text(".toast >> :last", "Authenticated as agent-b")?;
    page.wait_for_hidden("#auth-overlay")?;
    page.fill("#search-query", "charge")?;
    page.select_option("#search-queue", "billing")?;
    page.click("button:has-text(\"Search\")")?;

    page.click(".ticket-card >> :first >> .resolve-btn")?;
    page.wait_for_text(".toast >> :last", "Ticket resolved!")?;
    page.wait_for_text("#tickets-list", "No tickets found")?;
    snap(page, gif);
    Ok(())
}

fn spec_volunteer_shift(page: &mut Page, base_url: &str, gif: &mut Option<Gif>) -> Result<()> {
    page.goto(&format!("{base_url}/"))?;
    snap(page, gif);
    page.click("#login-coord")?;
    page.wait_for_text(".toast >> :last", "Authenticated as coordinator")?;
    page.wait_for_hidden("#auth-overlay")?;

    page.fill("#shift-title", "Beach Cleanup")?;
    page.fill("#shift-slots", "2")?;
    page.click("button:has-text(\"Create Shift\")")?;
    page.wait_for_text(".toast >> :last", "Shift created successfully!")?;
    page.wait_for_visible(".shift-card >> :first")?;
    page.wait_for_text(".shift-card >> :first >> .shift-title", "Beach Cleanup")?;
    snap(page, gif);

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-overlay")?;
    page.click("#login-vol-a")?;
    page.wait_for_text(".toast >> :last", "Authenticated as volunteer")?;
    page.wait_for_hidden("#auth-overlay")?;
    page.wait_for_visible(".shift-card >> :first >> .signup-btn")?;
    page.click(".shift-card >> :first >> .signup-btn")?;
    page.wait_for_text(".toast >> :last", "Signed up successfully!")?;
    page.wait_for_visible(".shift-card >> :first >> .cancel-btn")?;
    page.wait_for_hidden(".shift-card >> :first >> .signup-btn")?;
    page.wait_for_text(".shift-card >> :first >> .shift-slots", "Slots: 1 / 2")?;
    snap(page, gif);

    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-overlay")?;
    page.click("#login-vol-b")?;
    page.wait_for_text(".toast >> :last", "Authenticated as volunteer")?;
    page.wait_for_hidden("#auth-overlay")?;
    page.wait_for_visible(".shift-card >> :first >> .signup-btn")?;
    page.click(".shift-card >> :first >> .signup-btn")?;
    page.wait_for_text(".toast >> :last", "Signed up successfully!")?;
    page.wait_for_text(".shift-card >> :first >> .shift-slots", "Slots: 2 / 2")?;

    page.click(".shift-card >> :first >> .cancel-btn")?;
    page.wait_for_text(".toast >> :last", "Signup cancelled!")?;
    page.wait_for_text(".shift-card >> :first >> .shift-slots", "Slots: 1 / 2")?;
    page.click("#logout-btn")?;
    page.wait_for_visible("#auth-overlay")?;
    snap(page, gif);
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let root = repo_root();

    let apps: Vec<String> = if args.apps.is_empty() {
        DEFAULT_APPS.iter().map(|s| s.to_string()).collect()
    } else {
        args.apps.clone()
    };

    let engine_kind = match args.engine.as_str() {
        "lightpanda" => EngineKind::Lightpanda,
        "chromium" | "chrome" => EngineKind::Chromium,
        "visual" => EngineKind::AutoVisual,
        _ if args.record_gif_dir.is_some() => EngineKind::AutoVisual,
        _ => EngineKind::Auto,
    };

    let mocks_dir = tempfile::tempdir().context("tempdir for mock_provider")?;
    let mock_wasm = root.join("components/target/wasm32-wasip2/release/mock_provider.wasm");
    if !mock_wasm.is_file() {
        bail!("missing {} — run `cargo xtask build` first", mock_wasm.display());
    }
    std::fs::copy(&mock_wasm, mocks_dir.path().join("mock_provider.wasm"))?;

    let mut dirs = vec![mocks_dir.path().to_path_buf()];
    dirs.extend(holon_plug::default_dirs(&root));
    let catalog = holon_plug::Catalog::scan(&dirs);
    let out_dir = root.join("components/target/composed");

    let browser = BrowserServer::start(engine_kind)?;
    println!("holon-e2e: browser {:?} ready at {}", browser.kind(), browser.ws_url());

    if let Some(gif_dir) = &args.record_gif_dir {
        std::fs::create_dir_all(gif_dir)?;
    }

    let mut failed = Vec::new();
    for app in &apps {
        let name = app.strip_suffix("-domain").unwrap_or(app);
        let t0 = Instant::now();
        print!("=== {app} ... ");
        let composed = match holon_plug::compose_to(app, &catalog, &out_dir) {
            Ok(p) => p,
            Err(e) => {
                println!("FAIL (compose: {e})");
                failed.push(app.clone());
                continue;
            }
        };

        let hosted = match HostedApp::start(&root, app, &composed) {
            Ok(h) => h,
            Err(e) => {
                println!("FAIL (host: {e:#})");
                failed.push(app.clone());
                continue;
            }
        };

        let mut gif = match &args.record_gif_dir {
            Some(dir) => {
                let path = dir.join(format!("{name}.gif"));
                Some(
                    Gif::create(&path, args.gif_width, args.gif_height)
                        .map_err(|e| anyhow::anyhow!(e))?,
                )
            }
            None => None,
        };

        let mut page = browser.new_page()?;
        match run_spec(name, &mut page, &hosted.url, &mut gif) {
            Ok(()) => {
                if let Some(g) = gif.take() {
                    let _ = g.finish(Instant::now());
                }
                println!("ok ({:.2?})", t0.elapsed());
            }
            Err(e) => {
                println!("FAIL ({:.2?}): {e:#}", t0.elapsed());
                println!("--- host log:\n{}", hosted.log_tail());
                failed.push(app.clone());
            }
        }
    }

    if !failed.is_empty() {
        bail!("{} app(s) failed: {}", failed.len(), failed.join(" "));
    }
    println!("all {} app(s) passed", apps.len());
    Ok(())
}
