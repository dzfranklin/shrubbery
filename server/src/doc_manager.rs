use crate::db::DocDb;
use shrubbery_common::frame::PresenceFrame;
use shrubbery_common::DocId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio::time::interval;
use tracing::trace;

#[derive(Debug, Clone)]
pub struct DocManager {
    db: DocDb,
    opens: OpenMap,
}

type OpenMap = Arc<Mutex<HashMap<DocId, mpsc::Sender<OpenRequest>>>>;

struct OpenRequest {
    user: String,
    user_info: Option<serde_json::Value>,
    presence_tx: mpsc::Sender<Vec<PresenceFrame>>,
    reply_tx: OpenReplier,
}

type OpenReplier = oneshot::Sender<DocHandle>;

pub struct DocHandle {
    doc: DocId,
    id: u32,
    user: String,
    user_info: Option<serde_json::Value>,
    presence_tx: mpsc::Sender<(Instant, PresenceFrame)>,
}

impl DocManager {
    pub fn new(db: DocDb) -> Self {
        let opens = OpenMap::default();
        Self { db, opens }
    }

    pub async fn open(
        &self,
        doc: DocId,
        user: String,
        user_info: Option<serde_json::Value>,
        presence_tx: mpsc::Sender<Vec<PresenceFrame>>,
    ) -> eyre::Result<DocHandle> {
        loop {
            let tx = {
                let mut map = self.opens.lock().unwrap();
                let mut maybe_tx = map.get(&doc).cloned();
                if let Some(sender) = maybe_tx.as_ref() {
                    if sender.is_closed() {
                        map.remove(&doc);
                        maybe_tx = None;
                    }
                }
                match maybe_tx {
                    Some(tx) => tx,
                    None => {
                        let (tx, rx) = mpsc::channel(1);
                        let db = self.db.clone();
                        tokio::spawn(async move {
                            doc_worker(doc, db, rx).await;
                        });
                        map.insert(doc, tx.clone());
                        tx
                    }
                }
            };

            let (reply_tx, reply_rx) = oneshot::channel();
            let req = OpenRequest {
                user: user.clone(),
                user_info: user_info.clone(),
                presence_tx: presence_tx.clone(),
                reply_tx,
            };

            if tx.send(req).await.is_err() {
                continue;
            }

            let Ok(handle) = reply_rx.await else {
                continue;
            };

            return Ok(handle);
        }
    }
}

impl DocHandle {
    pub async fn update_presence(&mut self, presence: serde_json::Value) -> eyre::Result<()> {
        let frame = PresenceFrame {
            client: self.id,
            doc: self.doc,
            user: self.user.clone(),
            info: self.user_info.clone(),
            presence: Some(presence),
        };
        self.presence_tx.send((Instant::now(), frame)).await?;
        Ok(())
    }
}

// TODO: Have two broadcast channels. One for presence frames and one for doc frames
//   Clients should ignore missed presence frames, as they'll only be slightly behind.
//   For doc frames they should maybe send a request for the missed changes? Or maybe kill them?
//   Maybe add a layer of indirection and buffer the updates in the per-client processor? Could mask
//   issues that build up and effect everyone though.
//
//   Buffering in the client might make sense if we can be slightly smarter and condense repeated
//   changes. On the other hand this might require whole-protocol changes. So for now just kick and
//   once we have a working test case we can think about again.

async fn doc_worker(doc: DocId, db: DocDb, mut open_rx: mpsc::Receiver<OpenRequest>) {
    trace!("creating doc worker for {}", doc);
    let mut next_handle_id = 1;
    let mut client_map: HashMap<u32, ClientEntry> = HashMap::new();
    let mut presence_map: HashMap<u32, (Instant, PresenceFrame)> = HashMap::new();
    let (presence_tx, mut presence_rx) = mpsc::channel(1);
    let mut presence_interval = interval(Duration::from_secs(10));
    loop {
        select! {
            req = open_rx.recv() => {
                let req = match req {
                    Some(tx) => tx,
                    None => break,
                };

                let handle = DocHandle {
                    doc,
                    id: next_handle_id,
                    user: req.user,
                    user_info: req.user_info,
                    presence_tx: presence_tx.clone(),
                };
                next_handle_id += 1;

                client_map.insert(handle.id, ClientEntry {
                    presence_tx: req.presence_tx,
                });

                let _ = req.reply_tx.send(handle);
            }

            Some((last_update, frame)) = presence_rx.recv() => {
                let client = frame.client;
                presence_map.insert(client, (last_update, frame.clone()));
                let frame = vec![frame];
                for (&peer_id, peer) in &client_map {
                    if peer_id != client {
                        let _ = peer.presence_tx.try_send(frame.clone());
                    }
                }
            }

            _ = presence_interval.tick() => {
                let now = Instant::now();
                presence_map.retain(|_, (last_update, _)| {
                    now.duration_since(*last_update) < Duration::from_secs(30)
                });

                if presence_map.len() > 0 {
                    let mut frames = Vec::with_capacity(presence_map.len());
                    for (_, (_, frame)) in &presence_map {
                        frames.push(frame.clone());
                    }
                    for (_, peer) in &client_map {
                        let _ = peer.presence_tx.try_send(frames.clone());
                    }
                }
            }
        }
    }

    trace!("closed doc worker for {}", doc);
}

struct ClientEntry {
    presence_tx: mpsc::Sender<Vec<PresenceFrame>>,
}

impl std::fmt::Debug for DocHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
