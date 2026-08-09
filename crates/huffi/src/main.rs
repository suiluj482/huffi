use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use huffi::daemon::Daemon;
use huffi::provider::ProviderCollection;
use huffi_protocol::default_socket_path;

fn default_data_path() -> PathBuf {
    let data = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".local/share")
        });
    data.join("huffi/history.json")
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let mut socket_path = default_socket_path();
    let mut data_path = default_data_path();
    let mut dry_run = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" => {
                i += 1;
                socket_path = args.get(i)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        eprintln!("error: --socket requires a path argument");
                        std::process::exit(1);
                    });
            }
            "--data" => {
                i += 1;
                data_path = args.get(i)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        eprintln!("error: --data requires a path argument");
                        std::process::exit(1);
                    });
            }
            "--dry-run" => {
                dry_run = true;
            }
            other => {
                eprintln!("error: unknown argument: {other}");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    eprintln!("huffi-daemon listening on {}", socket_path.display());

    if let Some(parent) = data_path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create data directory");
    }

    let mut provider_collection = ProviderCollection::new(&data_path, dry_run)?;
    eprintln!("history loaded from {}", data_path.display());

    provider_collection.add_provider(Box::new(huffi::provider::MetaProvider::new(
        &socket_path,
        &data_path,
        dry_run,
    )));

    if dry_run {
        use huffi::provider::entry;
        use huffi::provider::TestProvider;
        use huffi::scoring::MatchField;

        let test_entries = vec![
            entry("firefox.desktop", "Firefox")
                .comment("Browse the World Wide Web")
                .icon("firefox")
                .history_key("firefox.desktop")
                .exec(vec!["firefox".into()])
                .match_fields(vec![MatchField { text: "Firefox".into(), weight: 1.0 }]),
            entry("org.gnome.Calculator.desktop", "Calculator")
                .icon("accessories-calculator")
                .history_key("org.gnome.Calculator.desktop")
                .exec(vec!["gnome-calculator".into()])
                .match_fields(vec![MatchField { text: "Calculator".into(), weight: 1.0 }]),
            entry("brave-browser.desktop", "Brave Browser")
                .history_key("brave-browser.desktop")
                .exec(vec!["brave-browser".into()])
                .match_fields(vec![MatchField { text: "Brave Browser".into(), weight: 1.0 }]),
            entry("breeze.desktop", "Breeze")
                .history_key("breeze.desktop")
                .exec(vec!["breeze".into()])
                .match_fields(vec![MatchField { text: "Breeze".into(), weight: 1.0 }]),
        ];

        provider_collection.add_provider(Box::new(TestProvider::new("test", test_entries)));
    }

    let daemon = Arc::new(Daemon::new(provider_collection));

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let daemon = Arc::clone(&daemon);
                thread::spawn(move || {
                    if let Err(e) = daemon.handle(stream) {
                        eprintln!("connection error: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("accept error: {e}");
            }
        }
    }

    Ok(())
}
