//! xmusic: terminal client for the xmusic-player daemon (crate `xmusic-tui`).

mod adapters;
mod effects;
mod model;
mod panes;
mod update;
mod view;

use std::io::{self, Write};
use std::time::Duration;

use crossterm::event::{self, Event as TermEvent, KeyEventKind};

use adapters::daemon_process as daemon;
use adapters::installation;
use effects::Runner;
use model::Model;
use update::{update, Message};

/// How long the render loop waits for a key before redrawing anyway, and so the upper bound on how stale the progress meter looks.
const TICK: Duration = Duration::from_millis(80);

const USAGE: &str = "\
xMusic - terminal client for YouTube Music

USAGE:
    xmusic [COMMAND] [OPTIONS]

COMMANDS:
    update            Replace both binaries with the newest release, using
                      whatever installed them (Homebrew or cargo). Add --force,
                      or say `reinstall`, to install again regardless
    login             Copy your YouTube session from your browser into the
                      player, and exit. Same as pressing L in the interface
    restart           Stop any daemon (running, stuck or half-dead) and start a
                      fresh one, then open the interface
    status            Report whether the daemon is running, and exit
    stop              Stop the running daemon and exit
    uninstall         Stop the daemon and delete its data, including the
                      imported YouTube session. Asks first
    version           Print the installed version and exit
    help              Show this message

Every command is also spelled as a flag - --update, --login, --restart,
--daemon-status, --kill-daemon, --uninstall, --version, --help - and:

OPTIONS:
    --no-spawn        Do not start the daemon; fail if it isn't already running
    --force           With update: install again even when nothing is newer

Google will not accept a sign-in from an embedded webview, so xMusic never
asks for your password. Sign in with your normal browser instead, then press L
(or run --login) to copy that session into the player.

The daemon (xmusic-player) keeps playing when this client exits. Stop it with
--kill-daemon, or with Q from inside the interface.
";

/// Folds a bare word onto the flag that means the same thing.
///
/// `xmusic update` is what people type first, and answering it with "unknown
/// option" only teaches them that the tool is fussy. The flags stay, because
/// they are what is documented and scripted; the words are the same commands
/// spelled the way a subcommand-shaped tool would spell them.
fn canonical(arg: &str) -> Option<&'static str> {
    match arg {
        "update" | "--update" | "upgrade" | "--upgrade" => Some("--update"),
        // Reinstalling is updating with the "nothing is newer" answer ignored.
        "reinstall" | "--reinstall" => Some("--reinstall"),
        "login" | "--login" | "signin" | "sign-in" => Some("--login"),
        "restart" | "--restart" => Some("--restart"),
        "uninstall" | "--uninstall" => Some("--uninstall"),
        "status" | "--status" | "--daemon-status" => Some("--daemon-status"),
        "stop" | "--stop" | "kill" | "--kill-daemon" => Some("--kill-daemon"),
        "version" | "--version" | "-V" => Some("--version"),
        "help" | "--help" | "-h" => Some("--help"),
        "--no-spawn" => Some("--no-spawn"),
        "--force" | "-f" => Some("--force"),
        _ => None,
    }
}

fn main() -> io::Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut args: Vec<&'static str> = Vec::with_capacity(raw.len());
    for arg in &raw {
        match canonical(arg) {
            Some(canonical) => args.push(canonical),
            None => {
                eprintln!("xmusic: unknown option {arg}\n");
                eprint!("{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let forced = args.contains(&"--force") || args.contains(&"--reinstall");

    for arg in &args {
        match *arg {
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
            "--version" => {
                println!("xmusic {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--uninstall" => return uninstall(),
            "--update" | "--reinstall" => return install_update(forced),
            "--login" => {
                if let Err(message) = daemon::ensure_running() {
                    eprintln!("xmusic: {message}");
                    std::process::exit(1);
                }
                return match effects::sign_in() {
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
            // --force only means anything to update, which has already run by
            // the time we get here, so on its own it is a no-op.
            "--no-spawn" | "--restart" | "--force" => {}
            other => {
                eprintln!("xmusic: unknown option {other}\n");
                eprint!("{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let no_spawn = args.contains(&"--no-spawn");
    let restart = args.contains(&"--restart");
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

/// Replaces both binaries through whatever installed them.
///
/// The daemon is stopped first. A client refuses a daemon whose version differs
/// from its own, so a new client beside an old running daemon is a handshake
/// that cannot succeed; stopping it before the swap makes the next start clean
/// rather than an error the user has to work out for themselves.
fn install_update(force: bool) -> io::Result<()> {
    let installed = env!("CARGO_PKG_VERSION");
    let source = match installation::detect() {
        Ok(source) => source,
        Err(message) => {
            eprintln!("xmusic: {message}");
            std::process::exit(1);
        }
    };
    println!("xmusic: {installed}, installed by {}", source.describe());

    match installation::latest_release() {
        Ok(Some(latest)) if installation::is_newer(&latest, installed) => {
            println!("xmusic: {latest} is available");
        }
        Ok(Some(latest)) if force => {
            println!("xmusic: nothing newer than {latest} published; installing again as asked");
        }
        Ok(Some(latest)) if latest == installed => {
            println!(
                "xmusic: {latest} is the newest release and you have it; \
                 `xmusic update --force` installs it again anyway"
            );
            return Ok(());
        }
        // A local build can sit in front of everything published, and telling
        // someone they are up to date is a different statement from telling them
        // they are ahead of the last release.
        Ok(Some(latest)) => {
            println!("xmusic: {installed} is ahead of the newest release ({latest}); nothing to update to");
            return Ok(());
        }
        // No releases yet is not a failure, and a source install still has
        // somewhere to install from, so say so and carry on.
        Ok(None) => println!("xmusic: no release published yet; installing from the repository"),
        Err(message) => eprintln!("xmusic: could not check for a newer version: {message}"),
    }

    let commands = match source.upgrade(force) {
        Ok(commands) => commands,
        Err(message) => {
            eprintln!("xmusic: {message}");
            std::process::exit(1);
        }
    };

    match daemon::reset() {
        Ok(message) => println!("xmusic: {message}"),
        Err(message) => {
            eprintln!("xmusic: {message}");
            std::process::exit(1);
        }
    }

    for mut command in commands {
        let spelled = installation::spelled_out(&command);
        println!("xmusic: {spelled}");
        // Inherited output on purpose: these builds take minutes, and a silent
        // wait with the log withheld until the end reads as a hang.
        match command.status() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!("xmusic: {spelled} failed with {status}");
                std::process::exit(1);
            }
            Err(error) => {
                eprintln!("xmusic: cannot run {spelled}: {error}");
                std::process::exit(1);
            }
        }
    }

    println!("xmusic: updated - run `xmusic` to start the new version");
    Ok(())
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

/// The loop: messages in, one model, effects out, one drawing of what is left.
///
/// Nothing here decides anything. Terminal events and the runner's replies both
/// become messages, `update` is the only thing that changes the model, and every
/// effect it returns goes straight back out to the runner.
fn run(terminal: &mut ratatui::DefaultTerminal, opening: String) -> io::Result<()> {
    let runner = Runner::spawn();
    let mut model = Model::default();
    model.status = opening;

    while !model.should_quit {
        for message in runner.drain() {
            for effect in update(&mut model, message) {
                runner.send(effect);
            }
        }
        terminal.draw(|frame| view::draw(frame, &mut model))?;

        if event::poll(TICK)? {
            // Drain everything queued rather than one key per frame, or a held-down arrow leaves the interface running behind the keyboard.
            loop {
                if let TermEvent::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        for effect in update(&mut model, Message::Key(key.code, key.modifiers)) {
                            runner.send(effect);
                        }
                    }
                }
                if model.should_quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
    }
    Ok(())
}
