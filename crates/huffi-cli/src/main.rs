use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use huffi_protocol::{
    default_socket_path, get_or_spawn_daemon, read_message, write_message, Request, Response,
};

fn send_request(socket_path: &Path, req: &Request) -> anyhow::Result<Response> {
    let stream = get_or_spawn_daemon(socket_path)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;

    let mut reader = BufReader::new(&stream);
    let mut writer = BufWriter::new(&stream);

    write_message(&mut writer, req)?;
    let resp: Option<Response> = read_message(&mut reader)?;
    resp.ok_or_else(|| anyhow::anyhow!("no response from daemon"))
}

fn print_usage() {
    eprintln!("Usage: huffi <command> [args]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  query <query>                  Search for apps matching query");
    eprintln!("  select <query> <entry_id>      Record a launch (trains the model)");
    eprintln!("  boost <query> <history_key>    Boost an app at a prefix (no fan-out)");
    eprintln!("  delete <query> <history_key>   Remove association at exact prefix");
    eprintln!("  list <prefix>                  Dump raw scores for a prefix");
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut socket_path = default_socket_path();

    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--socket" {
            if let Some(path) = args.get(i + 1) {
                socket_path = PathBuf::from(path);
                i += 2;
                continue;
            }
            eprintln!("error: --socket requires a path argument");
            std::process::exit(1);
        }
        positional.push(args[i].clone());
        i += 1;
    }

    if positional.is_empty() {
        print_usage();
        std::process::exit(1);
    }

    match positional[0].as_str() {
        "query" => {
            let query = positional.get(1).ok_or_else(|| {
                print_usage();
                anyhow::anyhow!("query requires a query argument")
            })?;
            let req = Request::Query { query: query.clone(), offset: 0, length: 20 };
            let resp = send_request(&socket_path, &req)?;
            match resp {
                Response::QueryResult { results, .. } => {
                    for (i, hit) in results.iter().enumerate() {
                        match &hit.comment {
                            Some(comment) => println!(
                                "{:>2}. {:<30} {:<30} score={:.3}",
                                i + 1, hit.title, comment, hit.score
                            ),
                            None => println!(
                                "{:>2}. {:<40} score={:.3}",
                                i + 1, hit.title, hit.score
                            ),
                        }
                    }
                    if results.is_empty() {
                        eprintln!("(no matches)");
                    }
                }
                other => eprintln!("unexpected response: {other:?}"),
            }
        }

        "select" => {
            let query = positional.get(1).ok_or_else(|| {
                print_usage();
                anyhow::anyhow!("select requires a query argument")
            })?;
            let entry_id = positional.get(2).ok_or_else(|| {
                print_usage();
                anyhow::anyhow!("select requires an entry_id argument")
            })?;
            let req = Request::Select {
                query: query.clone(),
                entry_id: entry_id.clone(),
            };
            let resp = send_request(&socket_path, &req)?;
            match resp {
                Response::Ok => eprintln!("ok"),
                other => eprintln!("unexpected response: {other:?}"),
            }
        }

        "boost" => {
            let query = positional.get(1).ok_or_else(|| {
                print_usage();
                anyhow::anyhow!("boost requires a query argument")
            })?;
            let history_key = positional.get(2).ok_or_else(|| {
                print_usage();
                anyhow::anyhow!("boost requires a history_key argument")
            })?;
            let req = Request::Boost {
                query: query.clone(),
                history_key: history_key.clone(),
            };
            let resp = send_request(&socket_path, &req)?;
            match resp {
                Response::Ok => eprintln!("ok"),
                other => eprintln!("unexpected response: {other:?}"),
            }
        }

        "delete" => {
            let query = positional.get(1).ok_or_else(|| {
                print_usage();
                anyhow::anyhow!("delete requires a query argument")
            })?;
            let history_key = positional.get(2).ok_or_else(|| {
                print_usage();
                anyhow::anyhow!("delete requires a history_key argument")
            })?;
            let req = Request::Delete {
                query: query.clone(),
                history_key: history_key.clone(),
            };
            let resp = send_request(&socket_path, &req)?;
            match resp {
                Response::Ok => eprintln!("ok"),
                other => eprintln!("unexpected response: {other:?}"),
            }
        }

        "list" => {
            let prefix = positional.get(1).ok_or_else(|| {
                print_usage();
                anyhow::anyhow!("list requires a prefix argument")
            })?;
            let req = Request::List {
                prefix: prefix.clone(),
            };
            let resp = send_request(&socket_path, &req)?;
            match resp {
                Response::ListResult { entries } => {
                    for entry in &entries {
                        println!(
                            "{:<40} score={:.3}  n={:<4}  last_update={:.0}",
                            entry.entry_id, entry.score, entry.n, entry.last_update
                        );
                    }
                    if entries.is_empty() {
                        eprintln!("(no entries for prefix \"{prefix}\")");
                    }
                }
                other => eprintln!("unexpected response: {other:?}"),
            }
        }

        
        other => {
            eprintln!("unknown command: {other}");
            print_usage();
            std::process::exit(1);
        }
    }

    Ok(())
}
