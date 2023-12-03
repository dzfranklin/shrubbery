use crate::DocId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    /// Client sends negative monotonic integers, server sends positive
    pub id: i32,
    pub reply_to: Option<i32>,
    #[serde(flatten)]
    pub frame: FrameType,
}

impl Frame {
    pub fn new(id: i32, frame: FrameType) -> Frame {
        Frame {
            id,
            frame,
            reply_to: None,
        }
    }

    pub fn new_reply(id: i32, reply_to: i32, inner: FrameType) -> Frame {
        Frame {
            id,
            frame: inner,
            reply_to: Some(reply_to),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
#[non_exhaustive]
pub enum FrameType {
    Authenticate {
        token: String,
    },
    Ok,
    Error {
        error: String,
    },
    MintToken {
        user: String,
        info: Option<serde_json::Value>,
        lifetime_seconds: u64,
    },
    MintTokenResponse {
        token: String,
    },
    RevokeTokensForUser {
        user: String,
    },
    Open {
        doc: DocId,
    },
    UpdatePresence {
        doc: DocId,
        presence: serde_json::Value,
    },
    Presence {
        updates: Vec<PresenceFrame>,
    },
    #[serde(other)]
    UnknownFrame,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceFrame {
    pub client: u32,
    pub doc: DocId,
    pub user: String,
    pub info: Option<serde_json::Value>,
    pub presence: Option<serde_json::Value>,
}
