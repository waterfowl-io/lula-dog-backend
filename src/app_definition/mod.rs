//! The app definition: pages, layout and components an AI assembles into a
//! dashboard/CRM/ops tool. This is DATA (stored as `jsonb` in
//! `app_definition_versions.definition`), not code -- see
//! `crate::control::app_definitions` for storage and versioning, and
//! `docs/architecture.md` for the bigger picture.
//!
//! The component library is fixed (`ComponentKind`); each component binds to
//! a saved query (reads) and/or a named action from `KNOWN_ACTIONS` (writes).
//! `validate` checks the shape AND, given a workspace's saved queries, that
//! every reference actually resolves.

use std::collections::{BTreeMap, BTreeSet};

use axum::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::control::ControlPool;
use crate::control::saved_queries;
use crate::error::{AppError, AppResult};

/// Actions a component's `action` binding may name; each maps to an
/// existing endpoint on the backend.
pub const KNOWN_ACTIONS: &[&str] = &[
    "opportunity.update_stage",
    "paste.submit",
    "proposal.accept",
    "proposal.reject",
    "task.create",
    "task.complete",
    "lead.accept",
    "lead.reject",
    "lead.snooze",
];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AppDefinitionDoc {
    /// The app definition schema format version (not the app's own
    /// version number, which is `app_definition_versions.version`).
    pub version: u32,
    pub pages: Vec<Page>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Page {
    pub id: String,
    /// E.g. `/opportunities/:opportunity_id`; segments starting with `:`
    /// are route params a binding can reference as `$route.<name>`.
    pub path: String,
    pub title: String,
    pub layout: LayoutNode,
    #[serde(default)]
    pub components: Vec<Component>,
}

/// Layout primitives. A `component` leaf names one of `Page::components` by
/// id; the component itself carries no layout information.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutNode {
    Row {
        #[serde(default)]
        children: Vec<Self>,
    },
    Column {
        #[serde(default)]
        children: Vec<Self>,
    },
    Section {
        title: Option<String>,
        #[serde(default)]
        children: Vec<Self>,
    },
    Component {
        component_id: String,
    },
}

/// The fixed component library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Table,
    Chart,
    MetricCard,
    Form,
    Kanban,
    DetailView,
    List,
}

/// A read binding to a saved query, by key, within the owning workspace.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct QueryBinding {
    pub query: String,
    /// Named params passed to the query when it runs. A value of
    /// `"$route.<name>"` is substituted with the page's route param of
    /// that name at request time; any other value is passed through.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub params: BTreeMap<String, Value>,
}

/// A write binding to a named action from `KNOWN_ACTIONS`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ActionBinding {
    pub action: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub params: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Component {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ComponentKind,
    pub title: Option<String>,
    #[serde(default)]
    pub query: Option<QueryBinding>,
    #[serde(default)]
    pub action: Option<ActionBinding>,
    /// Component-specific configuration: chart `chart_type`/`encoding`,
    /// table `columns`, form `fields`, kanban `group_field`, etc. --
    /// deliberately open-ended so the fixed `ComponentKind`s don't need a
    /// bespoke shape each; `validate` checks the few fields that matter for
    /// binding safety per kind.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub props: serde_json::Map<String, Value>,
}

/// Validate shape (pure, no I/O) and, given `pool`, that every saved-query
/// reference resolves within `workspace_id`. Known-action membership and
/// route-param references are also shape checks, since the action registry
/// and a page's own path are both static.
pub async fn validate(
    pool: &ControlPool,
    workspace_id: Uuid,
    doc: &AppDefinitionDoc,
) -> AppResult<()> {
    let query_keys = validate_shape(doc)?;
    for key in query_keys {
        if saved_queries::find_by_key(pool, workspace_id, key)
            .await?
            .is_none()
        {
            return Err(AppError::BadRequest(format!(
                "app definition references unknown saved query `{key}`"
            )));
        }
    }
    Ok(())
}

/// The shape checks that don't need a database: unique page/component ids,
/// every layout `component` leaf resolves within its page, every action is
/// in `KNOWN_ACTIONS`, every `$route.<name>` reference is a param the
/// page's own `path` declares, and each component kind has the bindings it
/// needs to be useful. Returns the saved-query keys referenced, for the
/// caller to check against the workspace's actual saved queries.
fn validate_shape(doc: &AppDefinitionDoc) -> AppResult<Vec<&str>> {
    let mut page_ids = BTreeSet::new();
    let mut query_keys = Vec::new();
    for page in &doc.pages {
        if !page_ids.insert(page.id.as_str()) {
            return Err(AppError::BadRequest(format!(
                "duplicate page id `{}`",
                page.id
            )));
        }
        let route_params = route_params(&page.path);
        let mut component_ids = BTreeSet::new();
        for component in &page.components {
            if !component_ids.insert(component.id.as_str()) {
                return Err(AppError::BadRequest(format!(
                    "duplicate component id `{}` on page `{}`",
                    component.id, page.id
                )));
            }
            validate_component(component)?;
            if let Some(query) = &component.query {
                query_keys.push(query.query.as_str());
                check_route_refs(&query.params, &route_params, page.id.as_str())?;
            }
            if let Some(action) = &component.action {
                if !KNOWN_ACTIONS.contains(&action.action.as_str()) {
                    return Err(AppError::BadRequest(format!(
                        "component `{}` uses unknown action `{}`",
                        component.id, action.action
                    )));
                }
                check_route_refs(&action.params, &route_params, page.id.as_str())?;
            }
        }
        check_layout_refs(&page.layout, &component_ids, page.id.as_str())?;
    }
    Ok(query_keys)
}

fn validate_component(component: &Component) -> AppResult<()> {
    let needs_query = matches!(
        component.kind,
        ComponentKind::Table
            | ComponentKind::Chart
            | ComponentKind::MetricCard
            | ComponentKind::Kanban
            | ComponentKind::DetailView
            | ComponentKind::List
    );
    if needs_query && component.query.is_none() {
        return Err(AppError::BadRequest(format!(
            "component `{}` ({:?}) needs a `query` binding",
            component.id, component.kind
        )));
    }
    let needs_action = matches!(component.kind, ComponentKind::Form);
    if needs_action && component.action.is_none() {
        return Err(AppError::BadRequest(format!(
            "component `{}` ({:?}) needs an `action` binding",
            component.id, component.kind
        )));
    }
    if component.kind == ComponentKind::Kanban {
        if component.action.is_none() {
            return Err(AppError::BadRequest(format!(
                "component `{}` (Kanban) needs an `action` binding for moving cards",
                component.id
            )));
        }
        if !component.props.contains_key("group_field") {
            return Err(AppError::BadRequest(format!(
                "component `{}` (Kanban) needs a `group_field` in `props`",
                component.id
            )));
        }
    }
    if component.kind == ComponentKind::Chart {
        if !component.props.contains_key("chart_type") {
            return Err(AppError::BadRequest(format!(
                "component `{}` (Chart) needs a `chart_type` in `props`",
                component.id
            )));
        }
        if !component
            .props
            .get("encoding")
            .is_some_and(Value::is_object)
        {
            return Err(AppError::BadRequest(format!(
                "component `{}` (Chart) needs an `encoding` object in `props`",
                component.id
            )));
        }
    }
    Ok(())
}

/// `:name` segments of a route path.
fn route_params(path: &str) -> BTreeSet<&str> {
    path.split('/')
        .filter_map(|segment| segment.strip_prefix(':'))
        .collect()
}

fn check_route_refs(
    params: &BTreeMap<String, Value>,
    route_params: &BTreeSet<&str>,
    page_id: &str,
) -> AppResult<()> {
    for value in params.values() {
        if let Some(name) = value.as_str().and_then(|s| s.strip_prefix("$route."))
            && !route_params.contains(name)
        {
            return Err(AppError::BadRequest(format!(
                "page `{page_id}` has no route param `:{name}` for `$route.{name}`"
            )));
        }
    }
    Ok(())
}

fn check_layout_refs(
    node: &LayoutNode,
    component_ids: &BTreeSet<&str>,
    page_id: &str,
) -> AppResult<()> {
    match node {
        LayoutNode::Row { children } | LayoutNode::Column { children } => children
            .iter()
            .try_for_each(|c| check_layout_refs(c, component_ids, page_id)),
        LayoutNode::Section { children, .. } => children
            .iter()
            .try_for_each(|c| check_layout_refs(c, component_ids, page_id)),
        LayoutNode::Component { component_id } => {
            if component_ids.contains(component_id.as_str()) {
                Ok(())
            } else {
                Err(AppError::BadRequest(format!(
                    "page `{page_id}` layout references unknown component `{component_id}`"
                )))
            }
        }
    }
}

/// The JSON Schema for `AppDefinitionDoc`, so the frontend can generate TS
/// types. Committed at `schema/app-definition.schema.json`; a test checks
/// they match.
pub fn json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(AppDefinitionDoc))
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

/// The JSON Schema of the app definition, for the frontend to generate TS
/// types from.
#[utoipa::path(
    get,
    path = "/app-definition.schema.json",
    responses((status = 200, body = Object)),
    tag = "apps"
)]
pub async fn schema_json() -> Json<Value> {
    Json(json_schema())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_schema_matches_generated() -> anyhow::Result<()> {
        let generated = serde_json::to_string_pretty(&json_schema())? + "\n";
        let committed = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/schema/app-definition.schema.json"
        ))?;
        assert_eq!(
            generated, committed,
            "schema/app-definition.schema.json is stale; regenerate it from `app_definition::json_schema()`"
        );
        Ok(())
    }

    #[test]
    fn rejects_duplicate_component_ids() {
        let doc = AppDefinitionDoc {
            version: 1,
            pages: vec![Page {
                id: "p".to_owned(),
                path: "/p".to_owned(),
                title: "P".to_owned(),
                layout: LayoutNode::Row { children: vec![] },
                components: vec![
                    Component {
                        id: "c".to_owned(),
                        kind: ComponentKind::MetricCard,
                        title: None,
                        query: Some(QueryBinding {
                            query: "q".to_owned(),
                            params: BTreeMap::new(),
                        }),
                        action: None,
                        props: serde_json::Map::new(),
                    },
                    Component {
                        id: "c".to_owned(),
                        kind: ComponentKind::MetricCard,
                        title: None,
                        query: Some(QueryBinding {
                            query: "q".to_owned(),
                            params: BTreeMap::new(),
                        }),
                        action: None,
                        props: serde_json::Map::new(),
                    },
                ],
            }],
        };
        assert!(validate_shape(&doc).is_err());
    }

    #[test]
    fn rejects_unknown_route_param_ref() {
        let mut params = BTreeMap::new();
        params.insert("id".to_owned(), Value::String("$route.missing".to_owned()));
        let doc = AppDefinitionDoc {
            version: 1,
            pages: vec![Page {
                id: "p".to_owned(),
                path: "/p/:known".to_owned(),
                title: "P".to_owned(),
                layout: LayoutNode::Component {
                    component_id: "c".to_owned(),
                },
                components: vec![Component {
                    id: "c".to_owned(),
                    kind: ComponentKind::MetricCard,
                    title: None,
                    query: Some(QueryBinding {
                        query: "q".to_owned(),
                        params,
                    }),
                    action: None,
                    props: serde_json::Map::new(),
                }],
            }],
        };
        assert!(validate_shape(&doc).is_err());
    }
}
