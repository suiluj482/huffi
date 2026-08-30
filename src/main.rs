mod config;
mod ui;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use clap::error::ErrorKind;
use clap::parser::ValueSource;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use gtk4::glib;
use huffi::engine::Engine;

use crate::config::Config;
use ui::control::{self, ControlRequest};

/// Arguments valid at every level: how to reach the daemon, and which config file
/// to load.
#[derive(Args)]
struct AppArgs {
    /// Single-instance wake socket path
    #[arg(long, global = true, value_name = "PATH", env = "HUFFI_SOCKET")]
    socket: Option<PathBuf>,

    /// Path to a config file (default $XDG_CONFIG_HOME/huffi/config.toml)
    #[arg(long, global = true, value_name = "PATH", env = "HUFFI_CONFIG")]
    config: Option<PathBuf>,
}

/// Arguments shared by the subcommands that can start an instance.
#[derive(Args)]
struct SpawnArgs {
    /// Query to show in the launcher on wake
    #[arg(short, long)]
    query: Option<String>,

    /// Huffi data directory (history and per-provider state)
    #[arg(long, value_name = "PATH", env = "HUFFI_DATA")]
    data: Option<PathBuf>,

    /// Don't record history or execute actions (enables test providers)
    #[arg(long, env = "HUFFI_DRY_RUN")]
    dry_run: bool,

    /// Quit any running instance and take over its control socket
    #[arg(long)]
    reload: bool,
}

/// Whether any top-level spawn flag was typed on the command line.
///
/// Ids are derived from the `SpawnArgs` schema so the check stays in sync when
/// a spawn flag is added. Queried against the top-level `Cli` matches, this
/// distinguishes a spawn flag given *before* an explicit subcommand from one
/// given *after* it.
///
/// We do this instead of `args_conflicts_with_subcommands(true)`, which would
/// also reject the globals `--socket`/`--config` before a subcommand.
fn any_spawn_flag_on_cli(matches: &clap::ArgMatches) -> bool {
    SpawnArgs::augment_args(clap::Command::new("huffi"))
        .get_arguments()
        .any(|arg| matches.value_source(arg.get_id().as_str()) == Some(ValueSource::CommandLine))
}

#[derive(Subcommand)]
enum Command {
    /// Ensure a hidden resident instance is running (start one if needed)
    Preload {
        #[command(flatten)]
        spawn: SpawnArgs,
    },
    /// Ensure a resident is running, then show the window
    Show {
        #[command(flatten)]
        spawn: SpawnArgs,
    },
    /// Hide the window if it is visible
    Hide,
    /// Show the window if hidden, hide it if visible
    Toggle {
        #[command(flatten)]
        spawn: SpawnArgs,
    },
    /// Quit the running instance
    Quit,
    /// Report whether an instance is running and its window state
    Status,
}

#[derive(Parser)]
#[command(
    name = "huffi",
    version,
    about = "launcher with query-dependent history"
)]
struct Cli {
    #[command(flatten)]
    app: AppArgs,

    /// Belong to the implicit subcommand `show`; in front of
    /// an explicit subcommand they are rejected.
    #[command(flatten)]
    spawn: SpawnArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

fn main() -> anyhow::Result<()> {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches)?;
    let config = Config::load(cli.app.config.as_deref()).context("failed to load configuration")?;
    run_command(cli, &matches, config)
}

fn run_command(cli: Cli, matches: &clap::ArgMatches, config: Config) -> anyhow::Result<()> {
    let Cli {
        app,
        spawn,
        command,
    } = cli;
    let command = match command {
        Some(command) => {
            if any_spawn_flag_on_cli(matches) {
                Cli::command()
                    .error(
                        ErrorKind::ArgumentConflict,
                        "leading flags for a subcommand before the subcommand are not accepted; pass them after the subcommand",
                    )
                    .exit();
            }
            command
        }
        None => Command::Show { spawn },
    };
    match command {
        Command::Hide => {
            let path = resolve_socket(app.socket, &config);
            control::send_request(&path, &ControlRequest::Hide)?;
            Ok(())
        }
        Command::Toggle { spawn } => {
            let path = resolve_socket(app.socket, &config);
            control::send_request(&path, &ControlRequest::Toggle { query: spawn.query })?;
            Ok(())
        }
        Command::Quit => {
            let path = resolve_socket(app.socket, &config);
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
        Command::Status => {
            let path = resolve_socket(app.socket, &config);
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
        Command::Preload { spawn } => spawn_instance(spawn, app.socket, true, &config),
        Command::Show { spawn } => spawn_instance(spawn, app.socket, false, &config),
    }
}

fn resolve_socket(socket: Option<PathBuf>, config: &Config) -> PathBuf {
    socket.unwrap_or_else(|| config.paths.socket.clone())
}

fn spawn_instance(
    spawn: SpawnArgs,
    socket: Option<PathBuf>,
    hidden: bool,
    config: &Config,
) -> anyhow::Result<()> {
    let data_dir = spawn
        .data
        .clone()
        .unwrap_or_else(|| config.paths.data_dir.clone());
    let control_socket = resolve_socket(socket, config);

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

    let mut engine = Engine::new_with_config(&data_dir, spawn.dry_run, &config.engine)?;
    engine.add_provider(Box::new(huffi::engine::provider::MetaProvider::new(
        &control_socket,
        &data_dir,
        spawn.dry_run,
    )))?;

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

        engine.add_provider(Box::new(TestProvider::new("test", test_entries)))?;
    }

    let main_loop = glib::MainLoop::new(None::<&glib::MainContext>, false);
    let launcher = ui::app::Launcher::new(
        listener,
        Arc::new(Mutex::new(engine)),
        main_loop.clone(),
        config.ui.clone(),
    );
    if !hidden {
        launcher.show_with_query(spawn.query.unwrap_or_default());
    }

    main_loop.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> (Cli, clap::ArgMatches) {
        let matches = Cli::command().get_matches_from(args);
        let cli = Cli::from_arg_matches(&matches).unwrap();
        (cli, matches)
    }

    #[test]
    fn bare_flags_default_to_show() {
        let (cli, _) = parse(&["huffi", "-q", "fire", "--dry-run"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.spawn.query.as_deref(), Some("fire"));
        assert!(cli.spawn.dry_run);
    }

    #[test]
    fn leading_flags_detected_before_explicit_subcommand() {
        let (cli, matches) = parse(&[
            "huffi", "-q", "leading", "--socket", "/x.sock", "show", "-q", "trailing",
        ]);
        assert!(cli.command.is_some());
        assert!(any_spawn_flag_on_cli(&matches));
        assert_eq!(
            cli.app.socket.as_deref(),
            Some(std::path::Path::new("/x.sock"))
        );
    }

    #[test]
    fn leading_flags_detected_before_non_start_subcommand() {
        let (cli, matches) = parse(&["huffi", "-q", "fire", "--reload", "quit"]);
        assert!(matches!(cli.command, Some(Command::Quit)));
        assert!(any_spawn_flag_on_cli(&matches));
    }

    #[test]
    fn spawn_flags_after_subcommand_not_treated_as_leading() {
        let (cli, matches) = parse(&["huffi", "show", "-q", "fire", "--dry-run"]);
        assert!(matches!(cli.command, Some(Command::Show { .. })));
        assert!(!any_spawn_flag_on_cli(&matches));
    }

    #[test]
    fn global_socket_before_subcommand_still_allowed() {
        let (cli, matches) = parse(&["huffi", "--socket", "/x.sock", "quit"]);
        assert!(matches!(cli.command, Some(Command::Quit)));
        assert!(!any_spawn_flag_on_cli(&matches));
        assert_eq!(
            cli.app.socket.as_deref(),
            Some(std::path::Path::new("/x.sock"))
        );
    }

    #[test]
    fn non_start_scope_rejects_query() {
        let err = Cli::try_parse_from(["huffi", "quit", "-q", "fire"])
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn quit_accepts_global_socket() {
        let (cli, _) = parse(&["huffi", "quit", "--socket", "/x.sock"]);
        assert!(matches!(cli.command, Some(Command::Quit)));
        assert_eq!(
            cli.app.socket.as_deref(),
            Some(std::path::Path::new("/x.sock"))
        );
    }

    #[test]
    fn env_provides_data_and_dry_run() {
        for (key, val) in [("HUFFI_DATA", "/env/data"), ("HUFFI_DRY_RUN", "true")] {
            unsafe { std::env::set_var(key, val) };
        }
        let (cli, matches) = parse(&["huffi", "preload"]);
        unsafe {
            std::env::remove_var("HUFFI_DATA");
            std::env::remove_var("HUFFI_DRY_RUN");
        }

        assert_eq!(
            cli.spawn.data.as_deref(),
            Some(std::path::Path::new("/env/data"))
        );
        assert!(cli.spawn.dry_run);
        assert!(!any_spawn_flag_on_cli(&matches));
    }

    #[test]
    fn env_provides_socket_and_config() {
        for (key, val) in [
            ("HUFFI_SOCKET", "/env/huffi.sock"),
            ("HUFFI_CONFIG", "/env/config.toml"),
        ] {
            unsafe { std::env::set_var(key, val) };
        }
        let (cli, _) = parse(&["huffi", "status"]);
        unsafe {
            std::env::remove_var("HUFFI_SOCKET");
            std::env::remove_var("HUFFI_CONFIG");
        }

        assert_eq!(
            cli.app.socket.as_deref(),
            Some(std::path::Path::new("/env/huffi.sock"))
        );
        assert_eq!(
            cli.app.config.as_deref(),
            Some(std::path::Path::new("/env/config.toml"))
        );
    }

    #[test]
    fn env_spawn_vars_do_not_error_before_subcommand() {
        unsafe {
            std::env::set_var("HUFFI_DATA", "/env/data");
            std::env::set_var("HUFFI_DRY_RUN", "true");
        }
        let (cli, matches) = parse(&["huffi", "quit"]);
        unsafe {
            std::env::remove_var("HUFFI_DATA");
            std::env::remove_var("HUFFI_DRY_RUN");
        }

        assert!(matches!(cli.command, Some(Command::Quit)));
        assert!(!any_spawn_flag_on_cli(&matches));
    }
}
