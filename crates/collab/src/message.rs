//! Wire messages between a client and the session server, and their binary encoding (postcard;
//! property values travel as JSON text inside, see [`crate::Json`]).

use serde::{Deserialize, Serialize};

use crate::{Change, Error, Handle, Result};

/// Largest message accepted: a welcome carries a whole snapshot.
pub const MAX_MESSAGE_BYTES: usize = crate::snapshot::MAX_BYTES + (1 << 20);

/// What a client sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientMsg {
    /// The first message: who is joining, and, from the person who starts the session, the
    /// drawing to start it with (a [`crate::snapshot`]); ignored once the session has one.
    /// `create`: this must be a new file; the server refuses if the name is taken. `resume`: the
    /// client id this person had before their connection dropped, to keep their handle space.
    Hello { name: String, seed: Option<Vec<u8>>, create: bool, resume: Option<u32> },
    /// One command's changes. `batch` numbers the client's batches, so replies can name them;
    /// `base` is the last batch the server had confirmed to this client when it made them, so
    /// everyone can tell which batches were made at the same time.
    Batch { batch: u64, base: u64, changes: Vec<Change> },
    /// On the server's file list (`/files`): ask for the list.
    List,
    /// Where this person is and what they're doing; the server passes it on to the others.
    Presence(Presence),
    /// Lock block `block` while this person edits it in the Block Editor (`on`), or let it go.
    Lock { block: String, on: bool },
}

/// Most selected objects a presence message carries.
pub const MAX_SELECTION: usize = 2000;

/// Where someone is and what they're doing (sent often, never part of the drawing).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Presence {
    /// The layout shown ("" for model space).
    pub layout: String,
    /// The cursor in drawing coordinates, while it is over the drawing.
    pub cursor: Option<[f64; 2]>,
    /// What is selected (at most [`MAX_SELECTION`]).
    pub selection: Vec<Handle>,
    /// The command running, e.g. "LINE".
    pub command: Option<String>,
    /// The view: center x, center y and height, in drawing units.
    pub view: Option<[f64; 3]>,
    /// The running command's rubber band: the objects it would make at the cursor (at most
    /// [`MAX_PREVIEW`], as entity JSON).
    pub preview: Vec<crate::Json>,
}

/// Most preview objects a presence message carries, and their largest total size in bytes.
pub const MAX_PREVIEW: usize = 20;
pub const MAX_PREVIEW_BYTES: usize = 32 * 1024;

/// A file in the cloud: a saved or live session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub name: String,
    /// Last change, in seconds since 1970.
    pub modified: u64,
    /// Who has it open now.
    pub people: Vec<String>,
}

/// Someone in the session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub client: u32,
    pub name: String,
}

/// What the server sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServerMsg {
    /// The reply to `Hello`: the client's id (its handle space) and the drawing as of `seq`.
    Welcome { client: u32, seq: u64, snapshot: Vec<u8> },
    /// A batch the server accepted, numbered `seq`, sent to everyone (its author included).
    Accepted { seq: u64, client: u32, batch: u64, base: u64, changes: Vec<Change> },
    /// A batch the server refused, sent to its author only; nobody else ever sees it.
    Rejected { batch: u64, reason: String },
    /// Who is in the session, after someone joins or leaves.
    Peers { peers: Vec<Peer> },
    /// The server can't go on with this client (it closes the link after this).
    Error { message: String },
    /// The reply to `List`, newest first.
    Files { files: Vec<FileInfo> },
    /// Someone else's [`Presence`].
    Presence { client: u32, presence: Presence },
    /// The blocks locked now, with who holds each, after any lock changes.
    Locks { locks: Vec<(String, u32)> },
}

/// Encode a message for the wire.
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    postcard::to_allocvec(msg).map_err(|e| Error::Encode(e.to_string()))
}

/// Decode a message from the wire.
pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(Error::TooLarge(MAX_MESSAGE_BYTES));
    }
    postcard::from_bytes(bytes).map_err(|e| Error::Decode(e.to_string()))
}
