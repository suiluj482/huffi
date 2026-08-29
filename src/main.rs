mod ui;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use gtk4::glib;
use huffi::engine::Engine;

use ui::control::{self, ControlRequest};

fn default_data_path() -> PathBuf {
    let data = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".local/share")
        });
    data.join("huffi/history.json")
}

/// Arguments shared by the subcommands that can start an instance.
#[derive(Args)]
struct SpawnArgs {
    /// Query to show in the launcher on wake
    #[arg(short, long)]
    query: Option<String>,

    /// History file path
    #[arg(long, value_name = "PATH")]
    data: Option<PathBuf>,

    /// Don't record history or execute actions (enables test providers)
    #[arg(long)]
    dry_run: bool,

    /// Quit any running instance and take over its control socket
    #[arg(long)]
    reload: bool,
}

/// Arguments shared by every subcommand (which socket to talk to).
#[derive(Args)]
struct SocketArgs {
    /// Single-instance wake socket path
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Ensure a hidden resident instance is running (start one if needed)
    Preload {
        #[command(flatten)]
        spawn: SpawnArgs,
        #[command(flatten)]
        socket: SocketArgs,
    },
    /// Ensure a resident is running, then show the window
    Show {
        #[command(flatten)]
        spawn: SpawnArgs,
        #[command(flatten)]
        socket: SocketArgs,
    },
    /// Hide the window if it is visible
    Hide {
        #[command(flatten)]
        socket: SocketArgs,
    },
    /// Show the window if hidden, hide it if visible
    Toggle {
        #[command(flatten)]
        spawn: SpawnArgs,
        #[command(flatten)]
        socket: SocketArgs,
    },
    /// Quit the running instance
    Quit {
        #[command(flatten)]
        socket: SocketArgs,
    },
    /// Report whether an instance is running and its window state
    Status {
        #[command(flatten)]
        socket: SocketArgs,
    },
}

#[derive(Parser)]
#[command(
    name = "huffi",
    version,
    about = "launcher with query-dependent history"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Mangle argv so a missing subcommand defaults to `show`: `huffi --socket X`
/// becomes `huffi show --socket X`. Help/version stay top-level.
fn cli_args() -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let first = args.get(1).and_then(|a| a.to_str());
    let is_flag = first.is_some_and(|f| f.starts_with('-'));
    let is_help = first.is_some_and(|f| matches!(f, "-h" | "--help" | "-V" | "--version"));
    if (first.is_none() || is_flag) && !is_help {
        args.insert(1, "show".into());
    }
    args
}

fn main() -> anyhow::Result<()> {
    let args = Cli::parse_from(cli_args());
    run_command(args.command)
}

fn run_command(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Hide { socket } => {
            let path = socket_path(socket.socket);
            control::send_request(&path, &ControlRequest::Hide)?;
            Ok(())
        }
        Command::Toggle { spawn, socket } => {
            let path = socket_path(socket.socket);
            control::send_request(&path, &ControlRequest::Toggle { query: spawn.query })?;
            Ok(())
        }
        Command::Quit { socket } => {
            let path = socket_path(socket.socket);
            match control::quit_running_instance(&path) {
                Ok(true) => Ok(()),
                Ok(false) => {
                    eprintln!("huffi: no running instance to quit");
                    std::process::exit(1);
                }
                Err(err) => {
                    eprintln!("huffi: couldn't reach running instance: {err}");
                    std::process::exit(1);
                }
            }
        }
        Command::Status { socket } => {
            let path = socket_path(socket.socket);
            match control::request_status(&path) {
                Ok(Some(resp)) => {
                    println!("running: yes");
                    println!("visible: {}", if resp.visible { "yes" } else { "no" });
                    Ok(())
                }
                Ok(None) => {
                    println!("not running");
                    std::process::exit(1);
                }
                Err(err) => {
                    eprintln!("huffi: couldn't reach running instance: {err}");
                    std::process::exit(1);
                }
            }
        }
        Command::Preload { spawn, socket } => spawn_instance(spawn, socket, true),
        Command::Show { spawn, socket } => spawn_instance(spawn, socket, false),
    }
}

fn socket_path(socket: Option<PathBuf>) -> PathBuf {
    socket.unwrap_or_else(control::default_socket_path)
}

fn spawn_instance(spawn: SpawnArgs, socket: SocketArgs, hidden: bool) -> anyhow::Result<()> {
    let data_path = spawn.data.unwrap_or_else(default_data_path);
    let control_socket = socket_path(socket.socket);

    if spawn.reload {
        // Tell any resident to quit, unlink its socket, and take over.
        let _ = control::quit_running_instance(&control_socket);
        let _ = std::fs::remove_file(&control_socket);
    } else if hidden {
        // preload: nothing to do if a resident already answers.
        if control::request_status(&control_socket)?.is_some() {
            return Ok(());
        }
    } else {
        // show: wake the resident if one is running.
        match control::send_request(
            &control_socket,
            &ControlRequest::Show {
                query: spawn.query.clone().unwrap_or_default(),
            },
        ) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(err) => eprintln!("warning: couldn't reach running instance: {err}"),
        }
    }

    gtk4::init().context("failed to initialize GTK")?;

    let Some(listener) = control::bind(&control_socket).context("failed to bind control socket")?
    else {
        return Ok(());
    };

    if let Some(parent) = data_path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create data directory");
    }

    let mut engine = Engine::new(&data_path, spawn.dry_run)?;
    engine.add_provider(Box::new(huffi::engine::provider::MetaProvider::new(
        &control_socket,
        &data_path,
        spawn.dry_run,
    )));

    if spawn.dry_run {
        use huffi::engine::provider::TestProvider;
        use huffi::engine::provider::entry;
        use huffi::engine::scoring::MatchField;

        let test_entries = vec![
            entry("firefox.desktop", "Firefox")
                .comment("Browse the World Wide Web")
                .icon("firefox")
                .history_key("firefox.desktop")
                .exec(vec!["firefox".into()])
                .match_fields(vec![MatchField {
                    text: "Firefox".into(),
                    weight: 1.0,
                }]),
            entry("org.gnome.Calculator.desktop", "Calculator")
                .icon("accessories-calculator")
                .history_key("org.gnome.Calculator.desktop")
                .exec(vec!["gnome-calculator".into()])
                .match_fields(vec![MatchField {
                    text: "Calculator".into(),
                    weight: 1.0,
                }]),
            entry("brave-browser.desktop", "Brave Browser")
                .history_key("brave-browser.desktop")
                .exec(vec!["brave-browser".into()])
                .match_fields(vec![MatchField {
                    text: "Brave Browser".into(),
                    weight: 1.0,
                }]),
            entry("breeze.desktop", "Breeze")
                .history_key("breeze.desktop")
                .exec(vec!["breeze".into()])
                .match_fields(vec![MatchField {
                    text: "Breeze".into(),
                    weight: 1.0,
                }]),
        ];

        engine.add_provider(Box::new(TestProvider::new("test", test_entries)));
    }

    let main_loop = glib::MainLoop::new(None::<&glib::MainContext>, false);
    let launcher =
        ui::app::Launcher::new(listener, Arc::new(Mutex::new(engine)), main_loop.clone());
    if !hidden {
        launcher.show_with_query(spawn.query.unwrap_or_default());
    }

    main_loop.run();
    Ok(())
}
