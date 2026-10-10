//! Generated speech Waveform keeps itself, for the idempotency retention, when
//! it cannot (or must not) store it in the account's Briefcase files.

use std::time::Duration;

use async_trait::async_trait;
use sqlx::{PgPool, Row as _, postgres::types::PgInterval};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    application::ports::{HeldAudio, HeldAudioError, HeldAudioStore},
    domain::identity::{ActorId, RequestId},
};

/// One held MP3, as served to its owner.
#[derive(Clone, Debug)]
pub struct HeldAudioFile {
    /// Download file name.
    pub filename: String,
    /// Media type (always `audio/mpeg` today).
    pub media_type: String,
    /// The MP3 bytes.
    pub bytes: Vec<u8>,
    /// When Waveform deletes it.
    pub expires_at: OffsetDateTime,
}

/// Postgres-backed held-audio store.
#[derive(Clone, Debug)]
pub struct PostgresHeldAudio {
    pool: PgPool,
    retention: Duration,
}

impl PostgresHeldAudio {
    /// Keeps audio for `retention` (the idempotency record lifetime).
    #[must_use]
    pub const fn new(pool: PgPool, retention: Duration) -> Self {
        Self { pool, retention }
    }

    /// The held MP3 for this request, only for its owner and only before it expires.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn fetch(
        &self,
        request_id: Uuid,
        owner: Uuid,
    ) -> Result<Option<HeldAudioFile>, sqlx::Error> {
        let row = sqlx::query("SELECT filename, media_type, audio, expires_at FROM waveform_held_audio WHERE request_id=$1 AND storage_actor_id=$2 AND expires_at > now()")
            .bind(request_id)
            .bind(owner)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            Ok(HeldAudioFile {
                filename: row.try_get("filename")?,
                media_type: row.try_get("media_type")?,
                bytes: row.try_get("audio")?,
                expires_at: row.try_get("expires_at")?,
            })
        })
        .transpose()
    }

    /// Deletes a bounded batch of expired audio.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub async fn delete_expired(&self, batch: u32) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query("DELETE FROM waveform_held_audio WHERE request_id IN (SELECT request_id FROM waveform_held_audio WHERE expires_at <= now() ORDER BY expires_at LIMIT $1)")
            .bind(i64::from(batch))
            .execute(&self.pool)
            .await?
            .rows_affected())
    }
}

#[async_trait]
impl HeldAudioStore for PostgresHeldAudio {
    async fn hold(&self, audio: HeldAudio) -> Result<OffsetDateTime, HeldAudioError> {
        let retention = PgInterval::try_from(self.retention).map_err(|_| HeldAudioError)?;
        let row = sqlx::query("INSERT INTO waveform_held_audio(request_id, storage_actor_id, filename, media_type, audio, reason, expires_at) VALUES($1, $2, $3, 'audio/mpeg', $4, $5, now() + $6) ON CONFLICT (request_id) DO UPDATE SET audio = EXCLUDED.audio, reason = EXCLUDED.reason WHERE waveform_held_audio.storage_actor_id = EXCLUDED.storage_actor_id RETURNING expires_at")
            .bind(audio.request_id.as_uuid())
            .bind(audio.owner.as_uuid())
            .bind(audio.filename.as_str())
            .bind(audio.audio.bytes().as_ref())
            .bind(audio.reason.as_str())
            .bind(retention)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| HeldAudioError)?;
        row.ok_or(HeldAudioError)?
            .try_get("expires_at")
            .map_err(|_| HeldAudioError)
    }

    async fn is_held(&self, request_id: RequestId, owner: ActorId) -> Result<bool, HeldAudioError> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM waveform_held_audio WHERE request_id=$1 AND storage_actor_id=$2 AND expires_at > now())")
            .bind(request_id.as_uuid())
            .bind(owner.as_uuid())
            .fetch_one(&self.pool)
            .await
            .map_err(|_| HeldAudioError)
    }
}
