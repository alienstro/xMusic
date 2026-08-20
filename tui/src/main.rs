//! xmusic: terminal client for the xmusic-player daemon (crate `xmusic-tui`).

mod app;
mod client;
mod daemon;
mod ui;

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event as TermEvent, KeyEventKind};

use app::App;
use client::Client;

const TICK: Duration = Duration::from_millis(150);

const USAGE: &str = "\
xmusic - terminal client for YouTube Music

USAGE:
    xmusic [OPTIONS]

OPTIONS:
    --no-spawn        Do not start the daemon; fail if it isn't already running
    --kill-daemon     Stop the running daemon and exit
    --daemon-status   Report whether the daemon is running, and exit
    -h, --help        Show this message

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
            "--no-spawn" => {}
            other => {
                eprintln!("xmusic: unknown option {other}\n");
                eprint!("{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let no_spawn = args.iter().any(|arg| arg == "--no-spawn");
    let opening = match prepare_daemon(no_spawn) {
        Ok(message) => message,
        Err(message) => {
            eprintln!("xmusic: {message}");
            std::process::exit(1);
        }
    };

    // Only enter the alternate screen once the daemon question is settled, so
    // any startup error is readable in the normal terminal.
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, opening);
    ratatui::restore();
    result
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
            match event::read()? {
                TermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    app.handle_key(key.code, key.modifiers)
                }
                // Redraw happens next iteration regardless; nothing else to do.
                _ => {}
            }
        }
    }
    Ok(())
}
