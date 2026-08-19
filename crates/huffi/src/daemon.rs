use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use crate::provider::ProviderCollection;
use huffi_protocol::{
    read_message, write_message, HistoryEntry, ProviderEntry, QueryHit, Request, Response,
};

const BOOST_WEIGHT: f64 = 10.0;
const MAX_RESULTS: usize = 20;

pub struct Daemon {
    provider_collection: Mutex<ProviderCollection>,
}

impl Daemon {
    pub fn new(provider_collection: ProviderCollection) -> Self {
        Self {
            provider_collection: Mutex::new(provider_collection),
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

            let mut pc = self.provider_collection.lock().unwrap();
            let resp = Self::dispatch(&mut pc, req);
            drop(pc);

            if let Err(e) = write_message(&mut writer, &resp) {
                eprintln!("[{peer}] write error: {e}");
                break;
            }
        }

        Ok(())
    }

    fn dispatch(pc: &mut ProviderCollection, req: Request) -> Response {
        match req {
            Request::Query { query, offset, length } => Self::handle_query(pc, &query, offset, length),
            Request::Select { query, entry_id } => Self::handle_select(pc, &query, &entry_id),
            Request::Boost { query, history_key } => Self::handle_boost(pc, &query, &history_key),
            Request::Delete { query, history_key } => Self::handle_delete(pc, &query, &history_key),
            Request::History { prefix } => Self::handle_history(pc, &prefix),
            Request::Providers => Self::handle_providers(pc),
        }
    }

    fn handle_query(pc: &mut ProviderCollection, query: &str, offset: usize, length: usize) -> Response {
        let (prefix, scored) = pc.query(query);
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

    fn handle_select(pc: &mut ProviderCollection, query: &str, entry_id: &str) -> Response {
        pc.select(query, entry_id);
        Response::Ok
    }

    fn handle_boost(pc: &mut ProviderCollection, query: &str, history_key: &str) -> Response {
        pc.record_boost(query, history_key, BOOST_WEIGHT);
        Response::Ok
    }

    fn handle_delete(pc: &mut ProviderCollection, query: &str, history_key: &str) -> Response {
        pc.delete(query, history_key);
        Response::Ok
    }

    fn handle_history(pc: &mut ProviderCollection, prefix: &str) -> Response {
        let raw = pc.list_entries(prefix);

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

    fn handle_providers(pc: &mut ProviderCollection) -> Response {
        let entries: Vec<ProviderEntry> = pc
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
