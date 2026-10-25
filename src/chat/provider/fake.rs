//! A deterministic provider: no network, but it exercises the real
//! tool-calling loop. Used by default (`CHAT_PROVIDER` unset, no
//! `XAI_API_KEY`) and by every backend chat test.

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;

use super::{ChatProvider, ProviderError, ProviderMessage, ProviderToolCall, ProviderTurn};
use crate::chat::sse::ChatEvent;
use crate::chat::tools::ToolSpec;

const MODEL: &str = "fake-1";

pub struct FakeProvider;

#[async_trait]
impl ChatProvider for FakeProvider {
    async fn complete(
        &self,
        messages: &[ProviderMessage],
        _tools: &[ToolSpec],
        _max_tokens: u32,
        deltas: &mpsc::Sender<ChatEvent>,
    ) -> Result<ProviderTurn, ProviderError> {
        let turn = decide(messages);
        if let Some(text) = &turn.content {
            let _ignored = deltas
                .send(ChatEvent::TextDelta { text: text.clone() })
                .await;
        }
        Ok(turn)
    }
}

fn turn(content: Option<&str>, tool_calls: Vec<ProviderToolCall>) -> ProviderTurn {
    ProviderTurn {
        content: content.map(str::to_owned),
        tool_calls,
        model: MODEL.to_owned(),
        prompt_tokens: Some(0),
        completion_tokens: Some(0),
    }
}

fn tool_call(name: &str, args: serde_json::Value) -> ProviderToolCall {
    ProviderToolCall {
        id: "fake_call_1".to_owned(),
        name: name.to_owned(),
        args,
    }
}

/// Decide the next turn from the message history: a fresh tool result at
/// the end means "summarize it"; a fresh user message means "decide
/// whether a tool answers it".
fn decide(messages: &[ProviderMessage]) -> ProviderTurn {
    match messages.last() {
        Some(ProviderMessage::Tool { name, content, .. }) => summarize_tool_result(name, content),
        Some(ProviderMessage::User(text)) => decide_from_user_text(text),
        _ => turn(Some("(fake provider) hello"), vec![]),
    }
}

fn summarize_tool_result(name: &str, content: &serde_json::Value) -> ProviderTurn {
    if let Some(error) = content.get("error").and_then(serde_json::Value::as_str) {
        return turn(Some(&format!("I couldn't do that: {error}")), vec![]);
    }
    let text = match name {
        "run_saved_query" | "read_only_sql" => {
            let rows = content
                .get("row_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            format!("I ran that query and found {rows} row(s).")
        }
        "get_deal" | "get_lead" => "Here's what I found.".to_owned(),
        _ => "Done.".to_owned(),
    };
    turn(Some(&text), vec![])
}

fn decide_from_user_text(text: &str) -> ProviderTurn {
    let lower = text.to_lowercase();
    if lower.contains("stale") {
        return turn(
            None,
            vec![tool_call(
                "run_saved_query",
                json!({"query": "stale_deals"}),
            )],
        );
    }
    if let Some((opportunity_id, stage)) = parse_move_deal(&lower) {
        return turn(
            None,
            vec![tool_call(
                "opportunity_update_stage",
                json!({"opportunity_id": opportunity_id, "stage": stage}),
            )],
        );
    }
    if let Some(company) = parse_create_deal(text, &lower) {
        return turn(
            None,
            vec![tool_call("opportunity_create", json!({"company": company}))],
        );
    }
    turn(
        Some(&format!("(fake provider) I heard: {}", text.trim())),
        vec![],
    )
}

const STAGES: [&str; 5] = ["lead", "qualified", "proposal", "won", "lost"];

/// `"move deal 1 to qualified"`, `"advance deal #3 to proposal"`, etc.: a
/// `deal <n>` reference plus any known stage word, in either order.
fn parse_move_deal(lower: &str) -> Option<(i64, &'static str)> {
    if !(lower.contains("move") || lower.contains("advance") || lower.contains("stage")) {
        return None;
    }
    let id = extract_deal_id(lower)?;
    let stage = STAGES.into_iter().find(|s| lower.contains(s))?;
    Some((id, stage))
}

fn extract_deal_id(lower: &str) -> Option<i64> {
    let words: Vec<&str> = lower.split_whitespace().collect();
    words.iter().enumerate().find_map(|(i, word)| {
        if *word != "deal" {
            return None;
        }
        let next = words.get(i + 1)?;
        let digits: String = next.chars().filter(char::is_ascii_digit).collect();
        digits.parse().ok()
    })
}

/// `"create a deal for Acme Corp"`, `"new deal for Acme"`: whatever follows
/// " for " once the message is clearly about creating a deal.
fn parse_create_deal(text: &str, lower: &str) -> Option<String> {
    if !lower.contains("deal") || !(lower.contains("create") || lower.contains("new deal")) {
        return None;
    }
    let idx = lower.find(" for ")?;
    let company = text
        .get(idx + 5..)?
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_owned();
    (!company.is_empty()).then_some(company)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_deal_id_and_stage() {
        assert_eq!(
            parse_move_deal("move deal 1 to qualified please"),
            Some((1, "qualified"))
        );
        assert_eq!(
            parse_move_deal("advance deal #42 to won"),
            Some((42, "won"))
        );
        assert_eq!(parse_move_deal("how is deal 1 doing"), None);
    }

    #[test]
    fn extracts_company_for_create() {
        assert_eq!(
            parse_create_deal("create a deal for Acme Corp", "create a deal for acme corp"),
            Some("Acme Corp".to_owned())
        );
        assert_eq!(
            parse_create_deal(
                "please make a new deal for Fernhill Candle Co.",
                "please make a new deal for fernhill candle co."
            ),
            Some("Fernhill Candle Co".to_owned())
        );
        assert_eq!(parse_create_deal("hello there", "hello there"), None);
    }
}
