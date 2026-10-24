//! Alerts. For now, one: stale deals.

use axum::Json;
use axum::extract::State;

use crate::AppState;
use crate::error::AppResult;
use crate::opportunities::{self, OpportunityView};

/// Open deals that need attention: no touch in 14 days, or an overdue open
/// task. Longest without a touch first.
#[utoipa::path(
    get,
    path = "/alerts/stale",
    responses((status = 200, body = [OpportunityView])),
    tag = "alerts"
)]
pub async fn stale(State(state): State<AppState>) -> AppResult<Json<Vec<OpportunityView>>> {
    let mut conn = state.pool.acquire().await?;
    Ok(Json(
        opportunities::list_views(&mut conn, None, true).await?,
    ))
}
