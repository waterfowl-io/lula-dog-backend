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
    "opportunity.create",
    "opportunity.update_stage",
    "paste.submit",
    "proposal.accept",
    "proposal.reject",
    "proposal.unaccept",
    "proposal.unreject",
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
    /// App-level branding: display name, mark, and the (static -- no live
    /// agent) chat shell's copy. Optional so a minimal app definition
    /// doesn't need to author it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<Brand>,
    /// Top-level navigation links, rendered in the top bar. Each `path`
    /// must equal some page's `path`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nav: Vec<NavItem>,
    pub pages: Vec<Page>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Brand {
    pub title: String,
    /// A named mark/logo id the frontend knows how to render (e.g.
    /// `"lula"`); unrecognized values just render no mark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatBrand>,
}

/// Copy for the chat panel's static shell. There is no live agent behind
/// it -- this only customizes the header status line and composer
/// placeholder text, not conversation content.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ChatBrand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct NavItem {
    pub label: String,
    /// Must equal some page's `path` exactly.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Page {
    pub id: String,
    /// E.g. `/opportunities/:opportunity_id`; segments starting with `:`
    /// are route params a binding can reference as `$route.<name>`.
    pub path: String,
    pub title: String,
    /// Optional subtitle and inline actions rendered on the title row,
    /// instead of `title` standing alone. A page that doesn't author one
    /// just gets the bare `title`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<PageHeader>,
    pub layout: LayoutNode,
    #[serde(default)]
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct PageHeader {
    /// A query whose first row fills `subtitle_template`'s `{field}`
    /// tokens, e.g. `"{deal_count} deals across {stage_count} stages"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<QueryBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle_template: Option<String>,
    /// Ids of this page's own `components`, rendered inline on the title
    /// row (e.g. a "New deal" form's trigger button) instead of in
    /// `layout`'s normal flow. Each must still be one of the page's
    /// `components`; it is simply not also placed in `layout`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
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
        /// A fixed CSS width (e.g. `"320px"`), for a column that shouldn't
        /// grow with its siblings -- a narrow details rail beside a wider
        /// activity column, say.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<String>,
        /// A flex-grow factor relative to sibling columns in the same row
        /// that also set one (both default to `1` otherwise).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flex: Option<u32>,
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
    /// A single write binding, for components that only ever need one
    /// (a kanban's drag action, a single-button form). Mutually usable
    /// alongside `actions` below; `all_actions` sees both.
    #[serde(default)]
    pub action: Option<ActionBinding>,
    /// Named write bindings, for components that offer more than one verb
    /// on a row -- e.g. a proposal list's `accept`/`reject`, a lead list's
    /// `accept`/`reject`/`snooze`. Keys are an app author's choice of label
    /// for each button; the frontend decides presentation per key, this is
    /// just storage and validation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub actions: BTreeMap<String, ActionBinding>,
    /// Component-specific configuration: chart `chart_type`/`encoding`,
    /// table `columns`, form `fields`, kanban `group_field`, etc. --
    /// deliberately open-ended so the fixed `ComponentKind`s don't need a
    /// bespoke shape each; `validate` checks the few fields that matter for
    /// binding safety per kind.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub props: serde_json::Map<String, Value>,
}

impl Component {
    /// Every action binding this component carries, combining the single
    /// `action` (keyed `"default"`) and the named `actions` map, so
    /// validation can check both forms uniformly.
    pub fn all_actions(&self) -> Vec<(&str, &ActionBinding)> {
        let mut out: Vec<(&str, &ActionBinding)> =
            self.actions.iter().map(|(k, v)| (k.as_str(), v)).collect();
        if let Some(action) = &self.action {
            out.push(("default", action));
        }
        out
    }
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
    let mut page_paths = BTreeSet::new();
    let mut query_keys = Vec::new();
    for page in &doc.pages {
        if !page_ids.insert(page.id.as_str()) {
            return Err(AppError::BadRequest(format!(
                "duplicate page id `{}`",
                page.id
            )));
        }
        page_paths.insert(page.path.as_str());
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
            for (name, action) in component.all_actions() {
                if !KNOWN_ACTIONS.contains(&action.action.as_str()) {
                    return Err(AppError::BadRequest(format!(
                        "component `{}` action `{name}` uses unknown action `{}`",
                        component.id, action.action
                    )));
                }
                check_route_refs(&action.params, &route_params, page.id.as_str())?;
            }
        }
        if let Some(header) = &page.header {
            if let Some(subtitle) = &header.subtitle {
                query_keys.push(subtitle.query.as_str());
                check_route_refs(&subtitle.params, &route_params, page.id.as_str())?;
            }
            for action_id in &header.actions {
                if !component_ids.contains(action_id.as_str()) {
                    return Err(AppError::BadRequest(format!(
                        "page `{}` header action references unknown component `{action_id}`",
                        page.id
                    )));
                }
            }
        }
        check_layout_refs(&page.layout, &component_ids, page.id.as_str())?;
    }
    for nav in &doc.nav {
        if !page_paths.contains(nav.path.as_str()) {
            return Err(AppError::BadRequest(format!(
                "nav item `{}` references unknown page path `{}`",
                nav.label, nav.path
            )));
        }
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
    if needs_action && component.all_actions().is_empty() {
        return Err(AppError::BadRequest(format!(
            "component `{}` ({:?}) needs an `action` or `actions` binding",
            component.id, component.kind
        )));
    }
    if component.kind == ComponentKind::Kanban {
        if component.all_actions().is_empty() {
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
        LayoutNode::Row { children } | LayoutNode::Column { children, .. } => children
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
            brand: None,
            nav: Vec::new(),
            pages: vec![Page {
                id: "p".to_owned(),
                path: "/p".to_owned(),
                title: "P".to_owned(),
                header: None,
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
                        actions: BTreeMap::new(),
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
                        actions: BTreeMap::new(),
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
            brand: None,
            nav: Vec::new(),
            pages: vec![Page {
                id: "p".to_owned(),
                path: "/p/:known".to_owned(),
                title: "P".to_owned(),
                header: None,
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
                    actions: BTreeMap::new(),
                    props: serde_json::Map::new(),
                }],
            }],
        };
        assert!(validate_shape(&doc).is_err());
    }

    fn component_with_actions(actions: BTreeMap<String, ActionBinding>) -> Component {
        Component {
            id: "c".to_owned(),
            kind: ComponentKind::List,
            title: None,
            query: Some(QueryBinding {
                query: "q".to_owned(),
                params: BTreeMap::new(),
            }),
            action: None,
            actions,
            props: serde_json::Map::new(),
        }
    }

    fn single_component_doc(component: Component) -> AppDefinitionDoc {
        AppDefinitionDoc {
            version: 1,
            brand: None,
            nav: Vec::new(),
            pages: vec![Page {
                id: "p".to_owned(),
                path: "/p".to_owned(),
                title: "P".to_owned(),
                header: None,
                layout: LayoutNode::Component {
                    component_id: "c".to_owned(),
                },
                components: vec![component],
            }],
        }
    }

    #[test]
    fn accepts_multiple_named_actions() {
        let mut actions = BTreeMap::new();
        actions.insert(
            "accept".to_owned(),
            ActionBinding {
                action: "proposal.accept".to_owned(),
                params: BTreeMap::new(),
            },
        );
        actions.insert(
            "reject".to_owned(),
            ActionBinding {
                action: "proposal.reject".to_owned(),
                params: BTreeMap::new(),
            },
        );
        let doc = single_component_doc(component_with_actions(actions));
        assert!(validate_shape(&doc).is_ok());
    }

    #[test]
    fn rejects_unknown_action_in_actions_map() {
        let mut actions = BTreeMap::new();
        actions.insert(
            "bogus".to_owned(),
            ActionBinding {
                action: "not.a.real.action".to_owned(),
                params: BTreeMap::new(),
            },
        );
        let doc = single_component_doc(component_with_actions(actions));
        assert!(validate_shape(&doc).is_err());
    }

    #[test]
    fn accepts_nav_referencing_a_real_page_path() {
        let mut doc = single_component_doc(component_with_actions(BTreeMap::new()));
        doc.brand = Some(Brand {
            title: "Jack's Sales".to_owned(),
            mark: Some("lula".to_owned()),
            chat: Some(ChatBrand {
                status: Some("Building Jack's Sales".to_owned()),
                placeholder: Some("Ask Lula to change this app…".to_owned()),
            }),
        });
        doc.nav = vec![NavItem {
            label: "P".to_owned(),
            path: "/p".to_owned(),
        }];
        assert!(validate_shape(&doc).is_ok());
    }

    #[test]
    fn rejects_nav_referencing_unknown_page_path() {
        let mut doc = single_component_doc(component_with_actions(BTreeMap::new()));
        doc.nav = vec![NavItem {
            label: "Nowhere".to_owned(),
            path: "/nowhere".to_owned(),
        }];
        assert!(validate_shape(&doc).is_err());
    }
}
