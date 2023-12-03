use crate::db::UserDb;
use crate::doc_manager::{DocHandle, DocManager};
use crate::state::authorizer;
use crate::state::authorizer::Authorizer;
use crate::Frame;
use eyre::eyre;
use futures::{SinkExt, StreamExt};
use shrubbery_common::frame::{FrameType, PresenceFrame};
use shrubbery_common::DocId;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::time::Duration;
use tokio::select;
use tokio::sync::mpsc;
use tracing::{info, trace, warn};

pub struct SocketProcessor<S> {
    authorizer: Authorizer,
    doc_manager: DocManager,
    user_db: UserDb,
    _socket: PhantomData<S>,
}

impl<S> SocketProcessor<S> {
    pub fn new(authorizer: Authorizer, doc_manager: DocManager, user_db: UserDb) -> Self {
        Self {
            authorizer,
            doc_manager,
            user_db,
            _socket: PhantomData,
        }
    }
}

impl<S> SocketProcessor<S>
where
    S: futures::Stream<Item = std::io::Result<Frame>>
        + futures::Sink<Frame, Error = std::io::Error>
        + Unpin,
{
    pub async fn accept(&self, socket: S) {
        let res = State::accept(
            socket,
            self.authorizer.clone(),
            self.user_db.clone(),
            self.doc_manager.clone(),
        )
        .await;

        if let Err(err) = res {
            warn!("Error processing socket: {}", err);
        }
    }
}

impl<S> Clone for SocketProcessor<S> {
    fn clone(&self) -> Self {
        Self::new(
            self.authorizer.clone(),
            self.doc_manager.clone(),
            self.user_db.clone(),
        )
    }
}

struct State<S> {
    socket: S,
    authorizer: Authorizer,
    user_db: UserDb,
    doc_manager: DocManager,
    open: HashMap<DocId, DocHandle>,
    presence_tx: mpsc::Sender<Vec<PresenceFrame>>,
    presence_rx: mpsc::Receiver<Vec<PresenceFrame>>,
    auth: authorizer::Entry,
    next_frame_id: i32,
}

impl<S> State<S>
where
    S: futures::Stream<Item = std::io::Result<Frame>>
        + futures::Sink<Frame, Error = std::io::Error>
        + Unpin,
{
    async fn accept(
        mut socket: S,
        authorizer: Authorizer,
        user_db: UserDb,
        doc_manager: DocManager,
    ) -> eyre::Result<()> {
        trace!("Processing connection");
        let Some(frame) = socket.next().await else {
            return Err(eyre!("disconnected"));
        };
        let frame = frame?;
        trace!("got frame at authenticate stage: {:?}", frame);

        let res = match frame.frame {
            FrameType::Authenticate { token } => {
                if let Some(user) = authorizer.authenticate(&token) {
                    Ok(user)
                } else {
                    Err(eyre!("invalid token"))
                }
            }
            _ => return Err(eyre::eyre!("Expected Authenticate frame")),
        };
        let entry = match res {
            Ok(role) => role,
            Err(err) => {
                let message = err.to_string();
                let _ = socket
                    .send(Frame::new_reply(
                        1,
                        frame.id,
                        FrameType::Error { error: message },
                    ))
                    .await;
                return Err(err);
            }
        };
        info!("Authenticated as {}", &entry.user);
        socket
            .send(Frame::new_reply(1, frame.id, FrameType::Ok))
            .await?;

        let (presence_tx, presence_rx) = mpsc::channel(12);
        let mut processor = State {
            authorizer,
            user_db,
            doc_manager,
            open: HashMap::new(),
            socket,
            presence_tx,
            presence_rx,
            auth: entry,
            next_frame_id: 2,
        };
        processor.run().await;
        Ok(())
    }

    async fn run(&mut self) -> eyre::Result<()> {
        loop {
            select! {
                frame = self.socket.next() => {
                    let Some(frame) = frame else {
                        return Ok(())
                    };
                    let frame = frame?;
                    let frame_id = frame.id;
                    trace!("got frame: {:?}", frame);
                    if let Err(err) = self.process_frame(frame).await {
                        info!("Error processing frame: {}", err);
                        let message = err.to_string();
                        let _ = self.send_error(frame_id, message).await;
                        self.next_frame_id += 1;
                    }
                }

                Some(mut frame) = self.presence_rx.recv() => {
                    while let Ok(next_frame) = self.presence_rx.try_recv() {
                        frame.extend(next_frame);
                    }
                    let frame = Frame::new(
                        self.next_frame_id,
                        FrameType::Presence { updates: frame },
                    );
                    self.socket.send(frame).await?;
                    self.next_frame_id += 1;
                }
            }
        }
    }

    async fn process_frame(&mut self, frame: Frame) -> eyre::Result<()> {
        match frame.frame {
            FrameType::Error { error: _ } => return Err(eyre!("unexpected error frame")),
            FrameType::MintToken {
                user: mint_user,
                lifetime_seconds,
                info: mint_info,
            } => {
                if self.auth.user != "root" {
                    return Err(eyre!("forbidden"));
                }
                info!(
                    "Minting token for user {} with lifetime {}",
                    mint_user, lifetime_seconds
                );
                let lifetime = std::time::Instant::now() + Duration::from_secs(lifetime_seconds);
                let token = self.authorizer.mint_token(authorizer::Entry {
                    user: mint_user,
                    expiry: lifetime,
                    info: mint_info,
                });
                self.socket
                    .send(Frame::new_reply(
                        self.next_frame_id,
                        frame.id,
                        FrameType::MintTokenResponse { token },
                    ))
                    .await?;
                self.next_frame_id += 1;
                Ok(())
            }
            FrameType::RevokeTokensForUser { user: revoke_user } => {
                if self.auth.user != "root" {
                    return Err(eyre!("forbidden"));
                }
                if revoke_user == "root" {
                    return Err(eyre!("cannot use RevokeTokensForUser on root"));
                }
                info!("Revoking tokens for user {}", revoke_user);
                self.authorizer.revoke_tokens_for_user(revoke_user);
                self.send_ok(frame.id).await?;
                Ok(())
            }
            FrameType::Open { doc } => {
                let handle = self
                    .doc_manager
                    .open(
                        doc,
                        self.auth.user.clone(),
                        self.auth.info.clone(),
                        self.presence_tx.clone(),
                    )
                    .await?;
                self.open.insert(doc, handle);
                self.send_ok(frame.id).await?;
                Ok(())
            }
            FrameType::UpdatePresence { doc, presence } => {
                let Some(handle) = self.open.get_mut(&doc) else {
                    return Err(eyre!("doc not open"));
                };
                handle.update_presence(presence).await?;
                Ok(())
            }
            _ => {
                info!("received unexpected frame: {:?}", frame);
                Ok(())
            }
        }
    }

    async fn send_ok(&mut self, reply_to: i32) -> std::io::Result<()> {
        self.socket
            .send(Frame::new_reply(
                self.next_frame_id,
                reply_to,
                FrameType::Ok,
            ))
            .await?;
        self.next_frame_id += 1;
        Ok(())
    }

    async fn send_error(&mut self, reply_to: i32, message: String) -> std::io::Result<()> {
        self.socket
            .send(Frame::new_reply(
                self.next_frame_id,
                reply_to,
                FrameType::Error { error: message },
            ))
            .await?;
        self.next_frame_id += 1;
        Ok(())
    }
}
