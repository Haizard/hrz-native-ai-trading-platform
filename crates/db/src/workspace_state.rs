//! The workspace the shell restores on load (`docs/54`).

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbError;

/// The caller's stored workspace snapshot, if one has been saved.
pub async fn get_workspace_state(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<Value>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT state FROM workspace_state WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?)
}

/// Replace the caller's snapshot and return its `updated_at` in nanoseconds.
pub async fn put_workspace_state(
    pool: &PgPool,
    user_id: Uuid,
    state: &Value,
) -> Result<i64, DbError> {
    let updated: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "INSERT INTO workspace_state (user_id, state) VALUES ($1, $2) \
         ON CONFLICT (user_id) DO UPDATE SET state = EXCLUDED.state, updated_at = now() \
         RETURNING updated_at",
    )
    .bind(user_id)
    .bind(state)
    .fetch_one(pool)
    .await?;
    Ok(crate::repositories::dt_to_ns(updated))
}
