//! Persistence for `migrations/0005_chat.sql`: conversations, messages and
//! the action log, all in the workspace's own SQLite file. Ids are
//! app-assigned `Uuid`s stored as TEXT (not SQLite's `INTEGER PRIMARY KEY`
//! rowid), per the migration's own doc comment -- so a conversation's id is
//! known before its row round-trips through `RETURNING`, which matters for
//! streaming (the id is minted before the first SSE byte).
//!
//! Because the id is a random `Uuid`, not a monotonic rowid, `ORDER BY id`
//! would NOT be chronological. Every ordinary SQLite table still has a
//! hidden, monotonically-increasing `rowid` even when its declared primary
//! key is something else, so listings here sort by that instead.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use sqlx::sqlite::SqlitePool;
use sqlx::types::Json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::actions::Undo;
use crate::error::{AppError, AppResult};

use super::Mode;

const TITLE_MAX_CHARS: usize = 80;

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct Conversation {
    pub id: String,
    /// A `control.app_definitions.id`, stored as text -- see
    /// `migrations/0005_chat.sql`'s doc comment.
    pub app_id: String,
    pub mode: Mode,
    pub user_id: String,
    pub title: Option<String>,
    pub created_at: String,
    pub last_message_at: String,
}

/// Start a conversation, titled from `first_message` (truncated).
pub async fn create_conversation(
    pool: &SqlitePool,
    app_id: &str,
    mode: Mode,
    user_id: &str,
    first_message: &str,
) -> AppResult<Conversation> {
    let id = Uuid::new_v4().to_string();
    let title = truncate_title(first_message);
    Ok(sqlx::query_as::<_, Conversation>(
        "INSERT INTO chat_conversations (id, app_id, mode, user_id, title)
         VALUES (?1, ?2, ?3, ?4, ?5)
         RETURNING *",
    )
    .bind(&id)
    .bind(app_id)
    .bind(mode)
    .bind(user_id)
    .bind(title)
    .fetch_one(pool)
    .await?)
}

fn truncate_title(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.chars().count() <= TITLE_MAX_CHARS {
        trimmed.to_owned()
    } else {
        let head: String = trimmed.chars().take(TITLE_MAX_CHARS).collect();
        format!("{head}…")
    }
}

/// A conversation by id, scoped implicitly to the caller's workspace --
/// `pool` is already that workspace's own SQLite file, so a foreign
/// conversation id simply isn't a row in it. 404, never a cross-workspace
/// read.
pub async fn fetch_conversation(pool: &SqlitePool, id: &str) -> AppResult<Conversation> {
    sqlx::query_as::<_, Conversation>("SELECT * FROM chat_conversations WHERE id = ?1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound("conversation"))
}

/// A workspace's conversations for one app and mode, most recently active
/// first.
pub async fn list_conversations(
    pool: &SqlitePool,
    app_id: &str,
    mode: Mode,
) -> AppResult<Vec<Conversation>> {
    Ok(sqlx::query_as::<_, Conversation>(
        "SELECT * FROM chat_conversations WHERE app_id = ?1 AND mode = ?2
         ORDER BY last_message_at DESC",
    )
    .bind(app_id)
    .bind(mode)
    .fetch_all(pool)
    .await?)
}

async fn touch_conversation(pool: &SqlitePool, conversation_id: &str) -> AppResult<()> {
    sqlx::query("UPDATE chat_conversations SET last_message_at = CURRENT_TIMESTAMP WHERE id = ?1")
        .bind(conversation_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    Tool,
}

/// One tool call an assistant message made, as recorded in
/// `chat_messages.tool_calls` (a JSON array of these).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    #[schema(value_type = Object)]
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub role: MessageRole,
    pub content: Option<String>,
    #[schema(value_type = Option<Vec<ToolCallRecord>>)]
    pub tool_calls: Option<Json<Vec<ToolCallRecord>>>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    #[schema(value_type = Object)]
    pub tool_result: Option<Json<Value>>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub latency_ms: Option<i64>,
    pub created_at: String,
}

/// A conversation's messages, oldest first (see the module doc comment on
/// why this sorts by `rowid`, not `id`).
pub async fn list_messages(pool: &SqlitePool, conversation_id: &str) -> AppResult<Vec<Message>> {
    Ok(sqlx::query_as::<_, Message>(
        "SELECT * FROM chat_messages WHERE conversation_id = ?1 ORDER BY rowid",
    )
    .bind(conversation_id)
    .fetch_all(pool)
    .await?)
}

pub async fn insert_user_message(
    pool: &SqlitePool,
    conversation_id: &str,
    content: &str,
) -> AppResult<Message> {
    let id = Uuid::new_v4().to_string();
    let message = sqlx::query_as::<_, Message>(
        "INSERT INTO chat_messages (id, conversation_id, role, content)
         VALUES (?1, ?2, 'user', ?3)
         RETURNING *",
    )
    .bind(&id)
    .bind(conversation_id)
    .bind(content)
    .fetch_one(pool)
    .await?;
    touch_conversation(pool, conversation_id).await?;
    Ok(message)
}

/// What `insert_assistant_message` records about one model turn, beyond its
/// text/tool calls: everything the brief asks to track per assistant
/// message (provider, resolved model version, token counts, cost, latency).
pub struct AssistantTurn<'a> {
    pub content: Option<&'a str>,
    pub tool_calls: &'a [ToolCallRecord],
    pub provider: &'a str,
    pub model: &'a str,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    /// `None` when `crate::config::ChatConfig`'s per-token prices aren't
    /// configured for this provider -- never a guess.
    pub cost_usd: Option<f64>,
    pub latency_ms: i64,
}

pub async fn insert_assistant_message(
    pool: &SqlitePool,
    conversation_id: &str,
    turn: AssistantTurn<'_>,
) -> AppResult<Message> {
    let id = Uuid::new_v4().to_string();
    let tool_calls = (!turn.tool_calls.is_empty()).then(|| Json(turn.tool_calls.to_vec()));
    let message = sqlx::query_as::<_, Message>(
        "INSERT INTO chat_messages
           (id, conversation_id, role, content, tool_calls, provider, model,
            prompt_tokens, completion_tokens, cost_usd, latency_ms)
         VALUES (?1, ?2, 'assistant', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         RETURNING *",
    )
    .bind(&id)
    .bind(conversation_id)
    .bind(turn.content)
    .bind(tool_calls)
    .bind(turn.provider)
    .bind(turn.model)
    .bind(turn.prompt_tokens)
    .bind(turn.completion_tokens)
    .bind(turn.cost_usd)
    .bind(turn.latency_ms)
    .fetch_one(pool)
    .await?;
    touch_conversation(pool, conversation_id).await?;
    Ok(message)
}

pub async fn insert_tool_message(
    pool: &SqlitePool,
    conversation_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    result: &Value,
) -> AppResult<Message> {
    let id = Uuid::new_v4().to_string();
    let message = sqlx::query_as::<_, Message>(
        "INSERT INTO chat_messages (id, conversation_id, role, tool_call_id, tool_name, tool_result)
         VALUES (?1, ?2, 'tool', ?3, ?4, ?5)
         RETURNING *",
    )
    .bind(&id)
    .bind(conversation_id)
    .bind(tool_call_id)
    .bind(tool_name)
    .bind(Json(result))
    .fetch_one(pool)
    .await?;
    touch_conversation(pool, conversation_id).await?;
    Ok(message)
}

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct ChatAction {
    pub id: String,
    pub conversation_id: String,
    pub message_id: String,
    pub user_id: String,
    pub action: String,
    #[schema(value_type = Object)]
    pub args: Json<Value>,
    #[schema(value_type = Object)]
    pub result: Json<Value>,
    pub undoable: bool,
    #[schema(value_type = Object)]
    pub undo: Option<Json<Undo>>,
    pub undone_at: Option<String>,
    pub created_at: String,
}

/// What `insert_action` records about one applied write, beyond which
/// conversation it's in.
pub struct NewAction<'a> {
    pub message_id: &'a str,
    pub user_id: &'a str,
    pub action: &'a str,
    pub args: &'a Value,
    pub result: &'a Value,
    pub undo: Option<&'a Undo>,
}

/// Record one applied data-mode write, for the audit trail the brief asks
/// for and for `fetch_action`/undo to replay later.
pub async fn insert_action(
    pool: &SqlitePool,
    conversation_id: &str,
    new: NewAction<'_>,
) -> AppResult<ChatAction> {
    let id = Uuid::new_v4().to_string();
    let undoable = new.undo.is_some();
    Ok(sqlx::query_as::<_, ChatAction>(
        "INSERT INTO chat_actions
           (id, conversation_id, message_id, user_id, action, args, result, undoable, undo)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         RETURNING *",
    )
    .bind(&id)
    .bind(conversation_id)
    .bind(new.message_id)
    .bind(new.user_id)
    .bind(new.action)
    .bind(Json(new.args))
    .bind(Json(new.result))
    .bind(undoable)
    .bind(new.undo.map(Json))
    .fetch_one(pool)
    .await?)
}

/// An action by id, scoped implicitly to the caller's workspace (see
/// `fetch_conversation`'s doc comment -- the same reasoning applies).
pub async fn fetch_action(pool: &SqlitePool, id: &str) -> AppResult<ChatAction> {
    sqlx::query_as::<_, ChatAction>("SELECT * FROM chat_actions WHERE id = ?1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound("action"))
}

/// Mark an action undone, once `crate::actions::undo` has actually
/// reversed it -- called after, not before, so a failed reversal never
/// marks `undone_at` on an action that wasn't really undone. Still guarded
/// (`undone_at IS NULL`) so two concurrent undo requests can't both record
/// success.
pub async fn mark_action_undone(pool: &SqlitePool, id: &str) -> AppResult<ChatAction> {
    sqlx::query_as::<_, ChatAction>(
        "UPDATE chat_actions SET undone_at = CURRENT_TIMESTAMP
         WHERE id = ?1 AND undone_at IS NULL
         RETURNING *",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Conflict(format!("action {id} was already undone")))
}
