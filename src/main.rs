#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::time::Duration;

use ghostchat::{storage, ui};

const USAGE: &str = "GhostChat — private family chat

usage: ghostchat [--profile NAME] [--wipe-identity]

  --profile NAME     use a separate identity/contacts (e.g. to run two copies on one PC)
  --wipe-identity    delete this computer's identity key and contacts, then exit";

struct Args {
    profile: Option<String>,
    wipe_identity: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        profile: None,
        wipe_identity: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--profile" => {
                let name = it.next().ok_or("--profile needs a name")?;
                // The profile becomes a directory name and a secure-store
                // service name, so keep it to a safe character set.
                if name.is_empty()
                    || name.len() > 32
                    || !name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                {
                    return Err("profile names may use A-Z, 0-9, '-' and '_' (max 32)".into());
                }
                args.profile = Some(name);
            }
            "--wipe-identity" => args.wipe_identity = true,
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }
    Ok(args)
}

fn with_icon(b: eframe::egui::ViewportBuilder) -> eframe::egui::ViewportBuilder {
    match eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/icon-256.png")) {
        Ok(icon) => b.with_icon(icon),
        Err(_) => b,
    }
}

fn main() {
    ghostchat::hardening::disable_crash_dumps();
    ghostchat::hardening::init_debug_logging();
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let profile = match storage::Profile::new(args.profile.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("GhostChat: {e}");
            std::process::exit(1);
        }
    };
    if args.wipe_identity {
        match storage::wipe_identity(&profile) {
            Ok(()) => println!("GhostChat identity and contacts deleted."),
            Err(e) => {
                eprintln!("GhostChat: could not delete everything: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let handle = rt.handle().clone();
    let title = match &args.profile {
        Some(p) => format!("GhostChat [{p}]"),
        None => "GhostChat".to_string(),
    };
    let result = eframe::run_native(
        &title,
        eframe::NativeOptions {
            viewport: with_icon(eframe::egui::ViewportBuilder::default())
                .with_inner_size([980.0, 640.0])
                .with_min_inner_size([640.0, 420.0])
                .with_title(&title),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc, handle, profile)))),
    );
    // Let the engine send its close frames before the runtime goes away.
    rt.shutdown_timeout(Duration::from_secs(2));
    if let Err(e) = result {
        eprintln!("GhostChat could not open its window: {e}");
        std::process::exit(1);
    }
}
