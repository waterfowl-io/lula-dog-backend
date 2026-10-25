//! Live agent chat for an app, in two modes: `data` (ask questions, make
//! changes through the same actions the UI uses -- `crate::actions`) and
//! `build` (edit the app definition itself; a 501 stub in this phase, see
//! `handlers::stream_chat`).
//!
//! Conversations and messages are customer data, so -- like opportunities,
//! leads and tasks -- they live in the workspace's own SQLite file
//! (`storage`), never the Postgres control plane; see
//! `migrations/0005_chat.sql`. `provider` is the LLM seam (a deterministic
//! `Fake` for tests/local dev, `Grok` for xAI); `tools` is what a model
//! turn can call; `sse` is the wire format streamed back to the frontend;
//! `handlers` ties all of it to the `/w/{workspace_id}/apps/{app}/chat...`
//! routes.

pub mod handlers;
pub mod provider;
pub mod sse;
pub mod storage;
pub mod tools;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Which of the two chat modes a conversation (and every message in it)
/// belongs to. Stored as lowercase text in `chat_conversations.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum Mode {
    Data,
    Build,
}

impl Mode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Build => "build",
        }
    }
}
