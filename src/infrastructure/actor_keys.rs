//! Canonical IAM identity to private Waveform storage-key mapping.
use sqlx::PgPool;
use uuid::Uuid;

pub(crate) async fn resolve(
    pool: &PgPool,
    plane: Uuid,
    public_id: &str,
) -> Result<Uuid, sqlx::Error> {
    if public_id.is_empty()
        || public_id.len() > 255
        || public_id.trim() != public_id
        || public_id.chars().any(char::is_control)
    {
        return Err(sqlx::Error::Protocol(
            "invalid canonical IAM identity".into(),
        ));
    }
    if let Some(key) = sqlx::query_scalar(
        "SELECT storage_actor_id FROM waveform_actor_keys WHERE plane_id=$1 AND public_id=$2",
    )
    .bind(plane)
    .bind(public_id)
    .fetch_optional(pool)
    .await?
    {
        return Ok(key);
    }
    sqlx::query_scalar("INSERT INTO waveform_actor_keys(plane_id,public_id,storage_actor_id) VALUES($1,$2,$3) ON CONFLICT(plane_id,public_id) DO UPDATE SET public_id=EXCLUDED.public_id RETURNING storage_actor_id")
        .bind(plane).bind(public_id).bind(Uuid::now_v7()).fetch_one(pool).await
}
