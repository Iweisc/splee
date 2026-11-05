use sqlx::postgres::PgRow;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

/// Loads a row from a table with a `project_id` column, but only when that project belongs to the org.
/// `table` is always a literal from our own code, never client input.
pub async fn load<T>(db: &PgPool, table: &'static str, id: Uuid, org_id: Uuid) -> AppResult<T>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    let sql = format!("select t.* from {table} t join projects p on p.id = t.project_id where t.id = $1 and p.org_id = $2");
    sqlx::query_as::<_, T>(&sql)
        .bind(id)
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}
