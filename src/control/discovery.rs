//! Live IAM discovery. No environment root authority is retained.
use super::{ControlError, ControlState, Plane};
use silicon_iam_client::Credential;
use sqlx::Row as _;
use uuid::Uuid;

impl ControlState {
    pub(super) async fn discover_plane(&self, secret: &str) -> Result<Plane, ControlError> {
        let iam = self
            .iam
            .with_credential(Credential::application(&self.app_id, secret))
            .with_testing_application(&self.app_id, secret)
            .map_err(ControlError::iam)?;
        let current = iam
            .applications()
            .testing_context()
            .await
            .map_err(ControlError::iam)?;
        let meta = current
            .environment
            .as_ref()
            .ok_or_else(ControlError::unauthorized)?;
        let id = current.environment_id;
        if id.is_nil()
            || current.application.app_id != self.app_id
            || meta.environment_id != id
            || meta.version < 1
        {
            return Err(ControlError::unauthorized());
        }
        let vault = self.vault()?;
        let digest = vault
            .digest(secret, "environment-root")
            .map_err(ControlError::internal)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 17))")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        let prior = sqlx::query("SELECT iam_control_version,iam_cleaned_at,deleted_at FROM waveform_environments WHERE id=$1 FOR UPDATE")
            .bind(id).fetch_optional(&mut *tx).await?;
        if let Some(row) = &prior {
            let revision: Option<i64> = row.try_get("iam_control_version")?;
            let deleted: Option<time::OffsetDateTime> = row.try_get("deleted_at")?;
            if revision.is_some_and(|v| v > meta.version) || deleted.is_some() {
                return Err(ControlError::unauthorized());
            }
            let cleaned: Option<time::OffsetDateTime> = row.try_get("iam_cleaned_at")?;
            if cleaned != meta.cleaned_at {
                // The environment row lock also fences existing idempotent speech work.
                for table in [
                    "waveform_idempotency_records",
                    "waveform_jobs",
                    "waveform_webhook_events",
                    "waveform_account_preferences",
                    "waveform_provider_keys",
                    "waveform_bug_reports",
                ] {
                    sqlx::query(sqlx::AssertSqlSafe(format!(
                        "DELETE FROM {table} WHERE plane_id=$1"
                    )))
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }
        // IAM creator IDs may be public handles. They are metadata, never local authority.
        let creator = Uuid::parse_str(&meta.creator_id).unwrap_or(Uuid::nil());
        let root = vault
            .seal(secret, &format!("{id}/root-key"))
            .map_err(ControlError::internal)?;
        let app = vault
            .seal(secret, &format!("{id}/app-secret"))
            .map_err(ControlError::internal)?;
        sqlx::query("INSERT INTO waveform_environments(id,org_id,creator_id,name,description,root_key_hash,root_key_cipher,iam_key_cipher,briefcase_key_cipher,app_secret_cipher,iam_environment_id,iam_control_version,iam_cleaned_at,webhook_key_digest) VALUES($1,$2,$3,$4,$5,$6,$7,''::bytea,''::bytea,$8,$1,$9,$10,$11) ON CONFLICT(id) DO UPDATE SET name=EXCLUDED.name,description=EXCLUDED.description,root_key_hash=EXCLUDED.root_key_hash,root_key_cipher=EXCLUDED.root_key_cipher,app_secret_cipher=EXCLUDED.app_secret_cipher,iam_control_version=EXCLUDED.iam_control_version,iam_cleaned_at=EXCLUDED.iam_cleaned_at,webhook_key_digest=EXCLUDED.webhook_key_digest,last_activity_at=now()")
            .bind(id).bind(&meta.org_id).bind(creator).bind(&meta.name).bind(&meta.description).bind(digest).bind(root).bind(app).bind(meta.version).bind(meta.cleaned_at).bind(&current.webhook_key_digest).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO waveform_voice_profiles(plane_id,id,profile) SELECT $1,id,profile FROM waveform_voice_profiles WHERE plane_id=$2 ON CONFLICT DO NOTHING").bind(id).bind(Uuid::nil()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO waveform_provider_defaults(plane_id,tts_order,stt_order,voice_profile) SELECT $1,tts_order,stt_order,voice_profile FROM waveform_provider_defaults WHERE plane_id=$2 ON CONFLICT DO NOTHING").bind(id).bind(Uuid::nil()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Plane {
            id,
            iam,
            iam_environment_id: Some(id),
        })
    }
}
