mod app;
mod autocomplete;
mod view;

use std::path::PathBuf;
use std::process::Command;

fn main() -> iced::Result {
    let args: Vec<String> = std::env::args().collect();

    // --version / -v / -V short-circuit so users get a sensible answer
    // without spinning up the iced runtime.
    if args
        .iter()
        .skip(1)
        .any(|a| a == "--version" || a == "-v" || a == "-V")
    {
        println!("rho {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }

    // Any args beyond the binary name → treat as a CLI invocation and hand
    // off to rho-cli, so `rho <prompt>` matches `rho-cli <prompt>`.
    if args.len() > 1 {
        let cli_path = find_rho_cli();
        match Command::new(&cli_path).args(&args[1..]).status() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(e) => {
                eprintln!(
                    "rho: failed to invoke rho-cli ({}): {}",
                    cli_path.display(),
                    e
                );
                std::process::exit(127);
            }
        }
    }

    // No args: launch the GUI.
    iced::application(app::RhoApp::new, app::RhoApp::update, view::view)
        .title("Rho")
        .theme(view::theme)
        .subscription(app::subscription)
        .default_font(view::FONT_INTER)
        .font(include_bytes!("../fonts/Inter-Regular.ttf").as_slice())
        .font(include_bytes!("../fonts/Inter-Medium.ttf").as_slice())
        .font(include_bytes!("../fonts/Inter-Bold.ttf").as_slice())
        .font(include_bytes!("../fonts/JetBrainsMono-Regular.ttf").as_slice())
        .font(include_bytes!("../fonts/JetBrainsMono-Medium.ttf").as_slice())
        .font(include_bytes!("../fonts/JetBrainsMono-Bold.ttf").as_slice())
        .font(include_bytes!("../fonts/JetBrainsMono-Italic.ttf").as_slice())
        .run()
}

// Prefer rho-cli alongside the current executable (correct version pairing in
// release bundles); fall back to whatever the OS PATH search finds.
fn find_rho_cli() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(if cfg!(windows) {
                "rho-cli.exe"
            } else {
                "rho-cli"
            });
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("rho-cli")
}
