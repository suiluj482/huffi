use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use crate::engine::Engine;
use huffi_protocol::{
    read_message, write_message, HistoryEntry, ProviderEntry, QueryHit, Request, Response,
};

const BOOST_WEIGHT: f64 = 10.0;
const MAX_RESULTS: usize = 20;

pub struct Daemon {
    engine: Mutex<Engine>,
}

impl Daemon {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine: Mutex::new(engine),
        }
    }

    pub fn handle(&self, stream: UnixStream) -> anyhow::Result<()> {
        let peer = stream.peer_addr().map(|a| format!("{a:?}")).unwrap_or_default();
        stream.set_nonblocking(false)?;

        let mut reader = BufReader::new(&stream);
        let mut writer = BufWriter::new(&stream);

        loop {
            let msg: Option<Request> = match read_message(&mut reader) {
                Ok(msg) => msg,
                Err(e) => {
                    eprintln!("[{peer}] read error: {e}");
                    break;
                }
            };

            let Some(req) = msg else {
                break;
            };

            let mut engine = self.engine.lock().unwrap();
            let resp = Self::dispatch(&mut engine, req);
            drop(engine);

            if let Err(e) = write_message(&mut writer, &resp) {
                eprintln!("[{peer}] write error: {e}");
                break;
            }
        }

        Ok(())
    }

    fn dispatch(engine: &mut Engine, req: Request) -> Response {
        match req {
            Request::Query { query, offset, length } => Self::handle_query(engine, &query, offset, length),
            Request::Select { query, entry_id } => Self::handle_select(engine, &query, &entry_id),
            Request::Boost { query, history_key } => Self::handle_boost(engine, &query, &history_key),
            Request::Delete { query, history_key } => Self::handle_delete(engine, &query, &history_key),
            Request::History { prefix } => Self::handle_history(engine, &prefix),
            Request::Providers => Self::handle_providers(engine),
        }
    }

    fn handle_query(engine: &mut Engine, query: &str, offset: usize, length: usize) -> Response {
        let (prefix, scored) = engine.query(query);
        let total = scored.len();
        let length = length.min(MAX_RESULTS);

        let results: Vec<QueryHit> = scored
            .into_iter()
            .skip(offset)
            .take(length)
            .map(|s| {
                let entry = s.entry;
                QueryHit {
                    entry_id: entry.id,
                    history_key: s.history_key,
                    base_score: s.base_score,
                    history_score: s.history_score,
                    score: s.combined,
                    title: entry.title,
                    subtitle: entry.subtitle,
                    comment: entry.comment,
                    icon: entry.icon,
                    extra: entry.extra,
                    set_query: entry.set_query,
                }
            })
            .collect();

        Response::QueryResult { prefix, results, total }
    }

    fn handle_select(engine: &mut Engine, query: &str, entry_id: &str) -> Response {
        engine.select(query, entry_id);
        Response::Ok
    }

    fn handle_boost(engine: &mut Engine, query: &str, history_key: &str) -> Response {
        engine.record_boost(query, history_key, BOOST_WEIGHT);
        Response::Ok
    }

    fn handle_delete(engine: &mut Engine, query: &str, history_key: &str) -> Response {
        engine.delete(query, history_key);
        Response::Ok
    }

    fn handle_history(engine: &mut Engine, prefix: &str) -> Response {
        let raw = engine.list_entries(prefix);

        let entries: Vec<HistoryEntry> = raw
            .into_iter()
            .map(|entry| HistoryEntry {
                entry_id: entry.key,
                score: entry.record.score,
                n: entry.record.n,
                last_update: entry.record.last_update,
            })
            .collect();

        Response::History { entries }
    }

    fn handle_providers(engine: &mut Engine) -> Response {
        let entries: Vec<ProviderEntry> = engine
            .providers()
            .into_iter()
            .map(|provider| ProviderEntry {
                id: provider.id,
                prefixes: provider.prefixes,
            })
            .collect();

        Response::Providers { entries }
    }
}
