//! Canonical IAM identity to private Waveform storage-key mapping.
use sqlx::PgPool;
use uuid::Uuid;

pub(crate) async fn resolve(
    pool: &PgPool,
    plane: Uuid,
    public_id: &str,
) -> Result<Uuid, sqlx::Error> {
    if canonical_kind(public_id).is_none() {
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

/// Classify a public principal only by the explicit canonical prefix.
pub(crate) fn canonical_kind(value: &str) -> Option<crate::domain::identity::ActorKind> {
    use crate::domain::identity::ActorKind;
    let (kind, handle, maximum) = if let Some(handle) = value.strip_prefix("c:") {
        (ActorKind::Carbon, handle, 30)
    } else if let Some(handle) = value.strip_prefix("si:") {
        (ActorKind::Silicon, handle, 50)
    } else {
        return None;
    };
    ((3..=maximum).contains(&handle.len())
        && handle.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        }))
    .then_some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::ActorKind;
    #[test]
    fn public_principals_require_explicit_prefixes() {
        assert_eq!(canonical_kind("c:12345678"), Some(ActorKind::Carbon));
        assert_eq!(canonical_kind("si:assistant"), Some(ActorKind::Silicon));
        for invalid in [
            "alice",
            "assistant:tos",
            "c:a",
            "si:UPPER",
            "si:assistant:tos",
        ] {
            assert_eq!(canonical_kind(invalid), None);
        }
    }
}
