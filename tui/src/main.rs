//! xmusic: terminal client for the xmusic-player daemon (crate `xmusic-tui`).

mod app;
mod client;
mod cookies;
mod daemon;
mod ui;

use std::io::{self, Write};
use std::time::Duration;

use crossterm::event::{self, Event as TermEvent, KeyEventKind};

use app::App;
use client::Client;

/// How long the render loop waits for a key before redrawing anyway, and so the upper bound on how stale the progress meter looks.
const TICK: Duration = Duration::from_millis(80);

const USAGE: &str = "\
xmusic - terminal client for YouTube Music

USAGE:
    xmusic [OPTIONS]

OPTIONS:
    --login           Copy your YouTube session from your browser into the
                      player, and exit. Same as pressing L in the interface
    --restart         Stop any daemon (running, stuck or half-dead) and start a
                      fresh one, then open the interface
    --uninstall       Stop the daemon and delete its data, including the
                      imported YouTube session. Asks first
    --no-spawn        Do not start the daemon; fail if it isn't already running
    --kill-daemon     Stop the running daemon and exit
    --daemon-status   Report whether the daemon is running, and exit
    -h, --help        Show this message

Google will not accept a sign-in from an embedded webview, so xmusic never
asks for your password. Sign in with your normal browser instead, then press L
(or run --login) to copy that session into the player.

The daemon (xmusic-player) keeps playing when this client exits. Stop it with
--kill-daemon, or with Q from inside the interface.
";

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    for arg in &args {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--kill-daemon" => {
                return match daemon::stop() {
                    Ok(message) => {
                        println!("xmusic: {message}");
                        Ok(())
                    }
                    Err(message) => {
                        eprintln!("xmusic: {message}");
                        std::process::exit(1);
                    }
                };
            }
            "--daemon-status" => {
                match daemon::status() {
                    daemon::Status::Compatible => {
                        println!(
                            "xmusic: daemon {} is running",
                            env!("CARGO_PKG_VERSION")
                        );
                        std::process::exit(0);
                    }
                    daemon::Status::Incompatible(version) => {
                        eprintln!(
                            "xmusic: daemon {version} is running, client is {}",
                            env!("CARGO_PKG_VERSION")
                        );
                        std::process::exit(2);
                    }
                    daemon::Status::Unreachable(message) => {
                        eprintln!("xmusic: daemon process is not answering: {message}");
                        std::process::exit(2);
                    }
                    daemon::Status::Unrecognized(message) => {
                        eprintln!("xmusic: {message}");
                        std::process::exit(2);
                    }
                    daemon::Status::Stopped => {
                        println!("xmusic: daemon is not running");
                        std::process::exit(1);
                    }
                }
            }
            "--uninstall" => return uninstall(),
            "--login" => {
                if let Err(message) = daemon::ensure_running() {
                    eprintln!("xmusic: {message}");
                    std::process::exit(1);
                }
                return match client::sign_in() {
                    Ok(message) => {
                        println!("xmusic: {message}");
                        Ok(())
                    }
                    Err(message) => {
                        eprintln!("xmusic: {message}");
                        std::process::exit(1);
                    }
                };
            }
            "--no-spawn" | "--restart" => {}
            other => {
                eprintln!("xmusic: unknown option {other}\n");
                eprint!("{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let no_spawn = args.iter().any(|arg| arg == "--no-spawn");
    let restart = args.iter().any(|arg| arg == "--restart");
    if restart && no_spawn {
        eprintln!("xmusic: --restart and --no-spawn contradict each other");
        std::process::exit(2);
    }
    if restart {
        match daemon::reset() {
            Ok(message) => println!("xmusic: {message}"),
            Err(message) => {
                eprintln!("xmusic: {message}");
                std::process::exit(1);
            }
        }
    }

    let opening = match prepare_daemon(no_spawn) {
        Ok(message) => message,
        Err(message) => {
            eprintln!("xmusic: {message}");
            std::process::exit(1);
        }
    };

    // Enter the alternate screen only once the daemon question is settled, so a startup error stays readable.
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, opening);
    ratatui::restore();
    result
}

/// Removes the daemon's data after showing exactly what that means, leaving the binaries to Homebrew or cargo.
fn uninstall() -> io::Result<()> {
    let targets = daemon::removable();
    if targets.is_empty() {
        println!("xmusic: nothing to remove; run `brew uninstall xmusic` for the binaries");
        return Ok(());
    }

    println!("xmusic will stop the daemon and delete:");
    for path in &targets {
        println!("    {}", path.display());
    }
    println!("\nThis includes the YouTube session imported from your browser.");
    print!("Continue? [y/N] ");
    io::stdout().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        println!("xmusic: cancelled, nothing removed");
        return Ok(());
    }

    for line in daemon::uninstall() {
        println!("xmusic: {line}");
    }
    println!("xmusic: run `brew uninstall xmusic` to remove the binaries");
    Ok(())
}

fn prepare_daemon(no_spawn: bool) -> Result<String, String> {
    if no_spawn {
        return match daemon::status() {
            daemon::Status::Compatible => Ok("Connected to the running daemon".into()),
            daemon::Status::Incompatible(version) => Err(format!(
                "daemon version {version} is incompatible with client {}; restart it",
                env!("CARGO_PKG_VERSION")
            )),
            daemon::Status::Unreachable(message) => {
                Err(format!("daemon process is not answering: {message}"))
            }
            daemon::Status::Unrecognized(message) => Err(message),
            daemon::Status::Stopped => {
                Err("no daemon on 127.0.0.1:13723 and --no-spawn was given".into())
            }
        };
    }
    match daemon::ensure_running()? {
        true => Ok("Started xmusic-player — YouTube Music is loading…".into()),
        false => Ok("Connected to the running daemon".into()),
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, opening: String) -> io::Result<()> {
    let mut app = App::new(Client::spawn());
    app.status = opening;

    while !app.should_quit {
        app.absorb_events();
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        if event::poll(TICK)? {
            // Drain everything queued rather than one key per frame, or a held-down arrow leaves the interface running behind the keyboard.
            loop {
                match event::read()? {
                    TermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                        app.handle_key(key.code, key.modifiers)
                    }
                    // Redraw happens next iteration regardless; nothing to do.
                    _ => {}
                }
                if app.should_quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
    }
    Ok(())
}
