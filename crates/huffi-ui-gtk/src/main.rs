mod app;
mod control;
mod daemon;
mod tasks;
mod theme;

use anyhow::Context;
use gtk4::glib;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut initial_query = String::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--query" | "-q" => {
                if let Some(query) = args.get(i + 1) {
                    initial_query = query.clone();
                    i += 2;
                } else {
                    eprintln!("error: --query requires an argument");
                    std::process::exit(1);
                }
            }
            "--help" | "-h" => {
                eprintln!("Usage: huffi-ui-gtk [--query <query>]");
                std::process::exit(0);
            }
            other => {
                eprintln!("error: unknown argument: {other}");
                std::process::exit(1);
            }
        }
    }

    let control_socket = control::socket_path();
    match control::notify_running_instance(&control_socket, &initial_query) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(err) => eprintln!("warning: couldn't reach running instance: {err}"),
    }

    if std::env::var("GSK_RENDERER").is_err() {
        // SAFETY: called single-threaded before any GTK or GDK init.
        unsafe { std::env::set_var("GSK_RENDERER", "gl") };
    }

    gtk4::init().context("failed to initialize GTK")?;

    let Some(listener) = control::bind(&control_socket).context("failed to bind control socket")?
    else {
        return Ok(());
    };

    let launcher = app::Launcher::new(listener, huffi_protocol::default_socket_path());
    launcher.show_with_query(initial_query);

    glib::MainLoop::new(None::<&glib::MainContext>, false).run();
    Ok(())
}
