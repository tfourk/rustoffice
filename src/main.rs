#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cal;
mod config;
mod demo;
mod engine;
mod fonts;
mod icons;
mod mime;
mod model;
mod notify;
mod outlook;
mod secrets;
mod store;
mod theme;
mod ui;
mod util;
mod vcard;

use app::{Module, StartOptions};

const USAGE: &str = "RustMail — desktop e-mail client

USAGE:
    rustmail [OPTIONS]

OPTIONS:
    --demo                 Add the offline demo mailbox on first start
    --data-dir <DIR>       Keep settings and mail in DIR (default: platform folders)
    --module <NAME>        Start in mail, calendar, people or todo
    --theme <light|dark>   Override the theme for this run
    --open-first           Open the first message of the inbox
    --compose              Open a sample message in the composer
    --settings [accounts]  Open the settings dialog (optionally at Accounts)
    --wizard               Open the add-account wizard
    --add-account <EMAIL>  Start adding the account with this address
    --google-setup         Open the Google sign-in setup guide
    --import-outlook <FILE>  Import an Outlook data file (.pst or .ost) and exit
    -h, --help             Show this help
    -V, --version          Show the version
";

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut start = StartOptions {
        demo: false,
        module: None,
        theme: None,
        open_first: false,
        compose: false,
        settings: false,
        wizard: false,
        open_subject: None,
        add_account: None,
        google_setup: false,
        settings_accounts: false,
        search: None,
        month: false,
        contact: None,
    };
    let mut import_outlook: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--demo" => start.demo = true,
            "--data-dir" => {
                i += 1;
                if let Some(d) = args.get(i) {
                    std::env::set_var("RUSTMAIL_HOME", d);
                }
            }
            "--module" => {
                i += 1;
                start.module = Some(match args.get(i).map(|s| s.as_str()) {
                    Some("calendar") => Module::Calendar,
                    Some("people") => Module::People,
                    Some("todo") => Module::Todo,
                    _ => Module::Mail,
                });
            }
            "--theme" => {
                i += 1;
                start.theme = match args.get(i).map(|s| s.as_str()) {
                    Some("dark") => Some(theme::ThemeMode::Dark),
                    Some("light") => Some(theme::ThemeMode::Light),
                    _ => None,
                };
            }
            "--open-first" => start.open_first = true,
            "--search" => {
                i += 1;
                start.search = args.get(i).cloned();
            }
            "--month" => start.month = true,
            "--contact" => {
                i += 1;
                start.contact = args.get(i).cloned();
            }
            "--open-subject" => {
                i += 1;
                start.open_subject = args.get(i).cloned();
            }
            "--compose" => start.compose = true,
            "--settings" => {
                start.settings = true;
                if args.get(i + 1).is_some_and(|a| a == "accounts") {
                    i += 1;
                    start.settings_accounts = true;
                }
            }
            "--wizard" => start.wizard = true,
            "--add-account" => {
                i += 1;
                start.add_account = args.get(i).cloned();
            }
            "--google-setup" => start.google_setup = true,
            "--import-outlook" => {
                i += 1;
                import_outlook = args.get(i).cloned();
                if import_outlook.is_none() {
                    eprintln!("--import-outlook needs a file");
                    std::process::exit(2);
                }
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("rustmail {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => eprintln!("Ignoring unknown option {other} (see --help)"),
        }
        i += 1;
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,rustmail=info")).init();

    if let Some(file) = import_outlook {
        std::process::exit(import_outlook_file(std::path::Path::new(&file)));
    }

    let (cfg, _) = config::AppConfig::load();
    let screenshot = std::env::var("RUSTMAIL_SCREENSHOT").is_ok();
    let w = &cfg.window;
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("RustMail")
        .with_app_id("rustmail")
        .with_min_inner_size([880.0, 560.0])
        .with_decorations(!cfg.appearance.custom_titlebar)
        .with_icon(app_icon());
    if screenshot {
        viewport = viewport.with_inner_size([1440.0, 900.0]);
    } else {
        viewport = viewport
            .with_inner_size([w.width.max(900.0), w.height.max(560.0)])
            .with_maximized(w.maximized);
    }
    let mut options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    // Screenshot runs may be off-screen; don't block on the compositor.
    options.glow_options.vsync = !screenshot;
    eframe::run_native(
        "RustMail",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, start)))),
    )
}

/// `--import-outlook`: imports the file without opening a window and
/// returns the exit code. Close RustMail first if it is running, so it
/// picks the new mailbox up when it starts.
fn import_outlook_file(file: &std::path::Path) -> i32 {
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    let (mut cfg, _) = config::AppConfig::load();
    let store = match store::Store::open(&config::Paths::get().db_file()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Could not open the mail database: {e}");
            return 1;
        }
    };
    let account = outlook::mailbox_account(file, &cfg);
    let progress = Mutex::new(outlook::Progress::default());
    let last = Mutex::new(Instant::now());
    let report = || {
        let (Ok(p), Ok(mut last)) = (progress.lock(), last.lock()) else { return };
        if last.elapsed() >= Duration::from_secs(2) {
            *last = Instant::now();
            eprintln!("{} of {} items… {}", p.done, p.total, p.folder);
        }
    };
    let result = outlook::run(file, &store, &account.id, &progress, &AtomicBool::new(false), &report);
    let mut progress = progress.into_inner().unwrap_or_default();
    progress.finished = Some(result.map_err(|e| format!("{e:#}")));
    let label = account.label.clone();
    let mailbox = outlook::apply(&mut cfg, &store, account, &progress);
    if let Some(Err(e)) = &progress.finished {
        eprintln!("Could not import {}: {e}", file.display());
        return 1;
    }
    if let Err(e) = cfg.save() {
        eprintln!("Could not save the settings: {e}");
        return 1;
    }
    println!("{}.", progress.summary());
    if mailbox {
        println!("The mail is in the mailbox \"{label}\".");
    }
    if progress.skipped > 0 {
        println!("{} item(s) were left out (already imported, or of a kind RustMail has no place for).", progress.skipped);
    }
    if progress.failed > 0 {
        println!("{} item(s) could not be read:", progress.failed);
        for e in &progress.errors {
            println!("  {e}");
        }
    }
    0
}

/// A small envelope icon drawn in code (no image assets needed).
fn app_icon() -> egui::IconData {
    let size = 64u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let accent = [15u8, 108, 189];
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let idx = ((y * size + x) * 4) as usize;
            // Rounded square background.
            let r = 12.0;
            let cx = fx.clamp(r, size as f32 - r);
            let cy = fy.clamp(r, size as f32 - r);
            let inside = (fx - cx).powi(2) + (fy - cy).powi(2) <= r * r;
            if !inside {
                continue;
            }
            let mut c = accent;
            // Envelope body.
            let (l, t, rr, b) = (14.0, 20.0, 50.0, 46.0);
            if fx >= l && fx <= rr && fy >= t && fy <= b {
                c = [255, 255, 255];
                // Flap lines.
                let mid = (l + rr) / 2.0;
                let flap_y = t + (mid - (fx - mid).abs() - l) * 0.62;
                if (fy - flap_y).abs() < 1.6 {
                    c = accent;
                }
            }
            rgba[idx..idx + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}
