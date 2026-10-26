//! Alerts. For now, one: stale deals.

use axum::Json;

use crate::error::AppResult;
use crate::opportunities::{self, OpportunityView};
use crate::workspace_db::WorkspaceContext;

/// Open deals that need attention: no touch in 14 days, or an overdue open
/// task. Longest without a touch first.
#[utoipa::path(
    get,
    path = "/w/{workspace_id}/alerts/stale",
    params(("workspace_id" = Uuid, Path)),
    responses((status = 200, body = [OpportunityView])),
    tag = "alerts"
)]
pub async fn stale(ws: WorkspaceContext) -> AppResult<Json<Vec<OpportunityView>>> {
    let mut conn = ws.pool.acquire().await?;
    Ok(Json(
        opportunities::list_views(&mut conn, None, true).await?,
    ))
}
