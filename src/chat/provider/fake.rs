//! A deterministic provider: no network, but it exercises the real
//! tool-calling loop. Used by default (`CHAT_PROVIDER` unset, no
//! `XAI_API_KEY`) and by every backend chat test.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{ChatProvider, ProviderError, ProviderMessage, ProviderToolCall, ProviderTurn};
use crate::app_definition::{AppDefinitionDoc, Component, ComponentKind, LayoutNode, QueryBinding};
use crate::chat::sse::ChatEvent;
use crate::chat::tools::ToolSpec;

const MODEL: &str = "fake-1";

pub struct FakeProvider;

#[async_trait]
impl ChatProvider for FakeProvider {
    async fn complete(
        &self,
        messages: &[ProviderMessage],
        tools: &[ToolSpec],
        _max_tokens: u32,
        deltas: &mpsc::Sender<ChatEvent>,
    ) -> Result<ProviderTurn, ProviderError> {
        // Build mode has its own, disjoint tool set (`propose_app_definition`
        // only exists there); checking for it is how this stays deterministic
        // without `ChatProvider::complete` needing a `Mode` parameter of its
        // own.
        let build_mode = tools.iter().any(|t| t.name == "propose_app_definition");
        let turn = if build_mode {
            decide_build(messages)
        } else {
            decide(messages)
        };
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
    tool_call_with_id("fake_call_1", name, args)
}

/// Like `tool_call`, but with an explicit id. Needed in build mode: unlike
/// every data-mode turn (at most one tool call), a build turn can make two
/// calls in a row (`get_app_definition` then `propose_app_definition`), and
/// the frontend keys live tool-activity rows by call id -- reusing
/// `"fake_call_1"` for both within the same turn would collide.
fn tool_call_with_id(id: &str, name: &str, args: serde_json::Value) -> ProviderToolCall {
    ProviderToolCall {
        id: id.to_owned(),
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

/// The most recent `User` message's text, scanning backwards -- build mode
/// needs the original request even once a tool result is the last message.
fn last_user_text(messages: &[ProviderMessage]) -> Option<&str> {
    messages.iter().rev().find_map(|m| match m {
        ProviderMessage::User(text) => Some(text.as_str()),
        _ => None,
    })
}

/// Decide the next turn for build mode: fetch the definition, make one
/// deterministic change from the original request's keywords, propose it,
/// then summarize -- the same "tool result in -> decide what's next" shape
/// `decide` uses for data mode, just with build's own tools.
fn decide_build(messages: &[ProviderMessage]) -> ProviderTurn {
    match messages.last() {
        Some(ProviderMessage::Tool { name, content, .. }) if name == "get_app_definition" => {
            let request = last_user_text(messages).unwrap_or_default();
            let Some(definition) = content.get("definition") else {
                return turn(
                    Some("I couldn't read the app's current definition."),
                    vec![],
                );
            };
            build_definition_change(request, definition).map_or_else(
                || {
                    turn(
                        Some(
                            "I'm not sure how to make that change yet -- try asking me to add a \
                             metric card or rename a column.",
                        ),
                        vec![],
                    )
                },
                |new_definition| {
                    turn(
                        None,
                        vec![tool_call_with_id(
                            "fake_call_2",
                            "propose_app_definition",
                            json!({ "definition": new_definition }),
                        )],
                    )
                },
            )
        }
        Some(ProviderMessage::Tool { name, content, .. }) if name == "propose_app_definition" => {
            if let Some(error) = content.get("error").and_then(Value::as_str) {
                return turn(
                    Some(&format!("I couldn't save that draft: {error}")),
                    vec![],
                );
            }
            let diff = content
                .get("diff_summary")
                .and_then(Value::as_str)
                .unwrap_or("the app");
            turn(Some(&format!("I made a draft:\n{diff}")), vec![])
        }
        Some(ProviderMessage::Tool { name, content, .. }) if name == "edit_pipeline" => {
            let message = content
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Pipeline editing isn't built yet.");
            turn(Some(message), vec![])
        }
        Some(ProviderMessage::Tool { .. }) => turn(Some("Done."), vec![]),
        Some(ProviderMessage::User(text)) => {
            let lower = text.to_lowercase();
            // Deliberately not bare "pipeline": that's also the name of a
            // real app page, so a request like "add a metric card to the
            // pipeline page" must not be mistaken for an ingress/egress
            // pipeline edit.
            if lower.contains("ingress")
                || lower.contains("egress")
                || lower.contains("data pipeline")
            {
                return turn(
                    None,
                    vec![tool_call("edit_pipeline", json!({"request": text}))],
                );
            }
            turn(None, vec![tool_call("get_app_definition", json!({}))])
        }
        _ => turn(Some("(fake provider) hello"), vec![]),
    }
}

/// One deterministic edit to `definition`, from keywords in the original
/// request. `None` means "I don't recognize this request" (a real model
/// would attempt something; the fake provider only supports what the e2e
/// brief actually exercises: adding a metric card, or renaming a column).
fn build_definition_change(request: &str, definition: &Value) -> Option<Value> {
    let doc: AppDefinitionDoc = serde_json::from_value(definition.clone()).ok()?;
    let lower = request.to_lowercase();
    let changed = if lower.contains("column") || lower.contains("rename") {
        rename_first_column(doc)
            .or_else(|| add_metric_card(serde_json::from_value(definition.clone()).ok()?))
    } else {
        add_metric_card(doc)
    }?;
    serde_json::to_value(changed).ok()
}

/// Reuse an existing saved-query key from anywhere in `doc`, so the new
/// component passes `crate::app_definition::validate` without the fake
/// provider needing to know which saved queries exist in this workspace.
fn first_query_key(doc: &AppDefinitionDoc) -> Option<String> {
    doc.pages.iter().find_map(|page| {
        page.components
            .iter()
            .find_map(|c| c.query.as_ref().map(|q| q.query.clone()))
    })
}

const METRIC_COMPONENT_ID: &str = "chat_build_metric";

fn add_metric_card(mut doc: AppDefinitionDoc) -> Option<AppDefinitionDoc> {
    let query = first_query_key(&doc)?;
    let page = doc.pages.first_mut()?;
    if page.components.iter().any(|c| c.id == METRIC_COMPONENT_ID) {
        return None; // already added this component; nothing new to propose
    }
    page.components.push(Component {
        id: METRIC_COMPONENT_ID.to_owned(),
        kind: ComponentKind::MetricCard,
        title: Some("New Metric".to_owned()),
        query: Some(QueryBinding {
            query,
            params: BTreeMap::default(),
        }),
        action: None,
        actions: BTreeMap::default(),
        props: serde_json::Map::default(),
    });
    page.layout = LayoutNode::Row {
        children: vec![
            page.layout.clone(),
            LayoutNode::Component {
                component_id: METRIC_COMPONENT_ID.to_owned(),
            },
        ],
    };
    Some(doc)
}

fn rename_first_column(mut doc: AppDefinitionDoc) -> Option<AppDefinitionDoc> {
    for page in &mut doc.pages {
        for component in &mut page.components {
            if component.kind != ComponentKind::Table {
                continue;
            }
            let Some(columns) = component
                .props
                .get_mut("columns")
                .and_then(Value::as_array_mut)
            else {
                continue;
            };
            let Some(label) = columns
                .first()
                .and_then(Value::as_object)
                .and_then(|o| o.get("label"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let new_label = format!("{label} (renamed)");
            if let Some(obj) = columns.first_mut().and_then(Value::as_object_mut) {
                obj.insert("label".to_owned(), Value::String(new_label));
                return Some(doc);
            }
        }
    }
    None
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
