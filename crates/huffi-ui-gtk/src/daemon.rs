use std::io::{BufReader, BufWriter};
use std::path::Path;

use huffi_protocol::{
    ProviderEntry, QueryHit, Request, Response, get_or_spawn_daemon, read_message, write_message,
};

pub fn query(
    socket_path: &Path,
    query: &str,
    offset: usize,
    length: usize,
) -> anyhow::Result<(Option<String>, Vec<QueryHit>, usize)> {
    let stream = get_or_spawn_daemon(socket_path)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;

    let mut reader = BufReader::new(&stream);
    let mut writer = BufWriter::new(&stream);

    write_message(
        &mut writer,
        &Request::Query {
            query: query.to_string(),
            offset,
            length,
        },
    )?;
    let resp: Option<Response> = read_message(&mut reader)?;

    match resp {
        Some(Response::QueryResult {
            prefix,
            results,
            total,
        }) => Ok((prefix, results, total)),
        _ => Ok((None, Vec::new(), 0)),
    }
}

pub fn providers(socket_path: &Path) -> anyhow::Result<Vec<ProviderEntry>> {
    let stream = get_or_spawn_daemon(socket_path)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;

    let mut reader = BufReader::new(&stream);
    let mut writer = BufWriter::new(&stream);

    write_message(&mut writer, &Request::Providers)?;
    let resp: Option<Response> = read_message(&mut reader)?;

    match resp {
        Some(Response::Providers { entries }) => Ok(entries),
        _ => Ok(Vec::new()),
    }
}

pub fn select(socket_path: &Path, query: &str, entry_id: &str) -> anyhow::Result<()> {
    let stream = get_or_spawn_daemon(socket_path)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;

    let mut reader = BufReader::new(&stream);
    let mut writer = BufWriter::new(&stream);

    write_message(
        &mut writer,
        &Request::Select {
            query: query.to_string(),
            entry_id: entry_id.to_string(),
        },
    )?;
    let _resp: Option<Response> = read_message(&mut reader)?;
    Ok(())
}

pub fn boost(socket_path: &Path, query: &str, history_key: &str) -> anyhow::Result<()> {
    let stream = get_or_spawn_daemon(socket_path)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;

    let mut reader = BufReader::new(&stream);
    let mut writer = BufWriter::new(&stream);

    write_message(
        &mut writer,
        &Request::Boost {
            query: query.to_string(),
            history_key: history_key.to_string(),
        },
    )?;
    let _resp: Option<Response> = read_message(&mut reader)?;
    Ok(())
}

pub fn delete(socket_path: &Path, query: &str, history_key: &str) -> anyhow::Result<()> {
    let stream = get_or_spawn_daemon(socket_path)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;

    let mut reader = BufReader::new(&stream);
    let mut writer = BufWriter::new(&stream);

    write_message(
        &mut writer,
        &Request::Delete {
            query: query.to_string(),
            history_key: history_key.to_string(),
        },
    )?;
    let _resp: Option<Response> = read_message(&mut reader)?;
    Ok(())
}
