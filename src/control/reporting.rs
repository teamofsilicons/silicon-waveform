//! Durable bug reports, mailed to the Team inbox through Postmark.
use super::{ControlError, ControlState};
use crate::domain::{auth::WaveformAction, idempotency::ACCOUNT_SCOPE};
use axum::{Json, extract::State};
use http::{HeaderMap, StatusCode};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row as _;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

pub(super) struct Mail {
    token: SecretString,
    endpoint: url::Url,
}
impl Mail {
    pub(super) fn from_env() -> Result<Option<Self>, &'static str> {
        let Ok(token) = std::env::var("WAVEFORM_POSTMARK_TOKEN") else {
            return Ok(None);
        };
        let endpoint = std::env::var("WAVEFORM_POSTMARK_URL")
            .unwrap_or_else(|_| "https://api.postmarkapp.com/email".into())
            .parse::<url::Url>()
            .map_err(|_| "invalid Postmark URL")?;
        if endpoint.scheme() != "https"
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || token.trim().is_empty()
        {
            return Err("invalid Postmark configuration");
        }
        Ok(Some(Self {
            token: token.into(),
            endpoint,
        }))
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Report {
    message: String,
    pr: Option<String>,
    #[serde(default)]
    client_version: String,
}
/// Credential prefixes refused in reports (Silicon Accounts tokens and secrets,
/// webhook secrets, JWT headers, common provider keys). Matched at word starts.
const CREDENTIAL_PREFIXES: [&str; 12] = [
    "slt_", "sar_", "sap_", "sapr_", "sarq_", "stk-", "whsec_", "sa_app_", "eyJhbGci", "eyJ0eXAi",
    "sk-", "AIza",
];

fn contains_credential(text: &str) -> bool {
    CREDENTIAL_PREFIXES.iter().any(|prefix| {
        text.match_indices(prefix).any(|(index, _)| {
            index == 0
                || !text[..index]
                    .chars()
                    .next_back()
                    .is_some_and(|before| before.is_ascii_alphanumeric() || before == '_')
        })
    })
}

fn validate(report: &Report) -> Result<(), ControlError> {
    if report.message.trim().is_empty()
        || report.message.len() > 60_000
        || report.client_version.len() > 100
    {
        return Err(ControlError::bad_request(
            "report_requires_1_to_60000_bytes",
            "message must be 1 to 60000 bytes and client_version at most 100.",
        ));
    }
    if let Some(pr) = &report.pr
        && !url::Url::parse(pr).is_ok_and(|u| {
            u.scheme() == "https"
                && u.host_str() == Some("github.com")
                && u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none()
                && u.path()
                    .strip_prefix("/teamofsilicons/silicon-waveform/pull/")
                    .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        })
    {
        return Err(ControlError::bad_request(
            "pr_must_reference_silicon_waveform_pull_request",
            "pr must be https://github.com/teamofsilicons/silicon-waveform/pull/<number>.",
        ));
    }
    // Refuse credentials before any durable write or delivery.
    if contains_credential(&report.message) || contains_credential(&report.client_version) {
        return Err(ControlError::bad_request(
            "remove_credentials_from_report",
            "The report contains what looks like a token, secret or API key. Remove it and send the report again.",
        ));
    }
    Ok(())
}
pub(super) async fn submit(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(report): Json<Report>,
) -> Result<(StatusCode, Json<Value>), ControlError> {
    validate(&report)?;
    let caller = state
        .caller(&headers, WaveformAction::ManageAccount)
        .await?;
    let storage_key = caller.record.storage_key;
    let key = super::single_header(&headers, "idempotency-key")?.ok_or_else(|| {
        ControlError::bad_request(
            "idempotency_key_required",
            "Send an Idempotency-Key header (1 to 128 letters, digits, hyphens or underscores).",
        )
    })?;
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(ControlError::bad_request(
            "invalid_idempotency_key",
            "Idempotency-Key must be 1 to 128 letters, digits, hyphens or underscores.",
        ));
    }
    if state.mail.is_none() {
        return Err(ControlError::unavailable(
            "postmark_not_configured",
            "Bug reports cannot be delivered: WAVEFORM_POSTMARK_TOKEN is not configured.",
        ));
    }
    let payload = json!(report);
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29))")
        .bind(format!("report:{}:{storage_key}", Uuid::nil()))
        .execute(&mut *tx)
        .await?;
    let old = sqlx::query("SELECT id,payload,status FROM waveform_bug_reports WHERE plane_id=$1 AND org_id=$2 AND actor_id=$3 AND idempotency_key=$4").bind(Uuid::nil()).bind(ACCOUNT_SCOPE).bind(storage_key).bind(key).fetch_optional(&mut *tx).await?;
    let (id, status): (Uuid, String) = if let Some(old) = old {
        if old.get::<Value, _>("payload") != payload {
            return Err(ControlError::new(
                StatusCode::CONFLICT,
                "report_idempotency_conflict",
                "This Idempotency-Key was already used for a different report; use a new key.",
            ));
        }
        (old.get("id"), old.get("status"))
    } else {
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM waveform_bug_reports WHERE plane_id=$1 AND actor_id=$2 AND created_at>now()-interval '1 hour'").bind(Uuid::nil()).bind(storage_key).fetch_one(&mut *tx).await?;
        if count >= 10 {
            return Err(ControlError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "report_hourly_limit",
                "An account can send at most 10 bug reports per hour; try again later.",
            ));
        }
        let id = Uuid::new_v4();
        let status = "queued";
        sqlx::query("INSERT INTO waveform_bug_reports(id,plane_id,org_id,actor_id,idempotency_key,payload,status) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(id).bind(Uuid::nil()).bind(ACCOUNT_SCOPE).bind(storage_key).bind(key).bind(payload).bind(status).execute(&mut *tx).await?;
        (id, status.into())
    };
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(
            json!({"id":id,"submitted":true,"notification":status,"repository":"https://github.com/teamofsilicons/silicon-waveform"}),
        ),
    ))
}
impl ControlState {
    /// Attempts one production report delivery; failures remain queued for retry.
    pub(crate) async fn deliver_report(&self) -> Result<(), ControlError> {
        let Some(mail) = &self.mail else {
            return Ok(());
        };
        let mut tx = self.pool.begin().await?;
        let Some(row)=sqlx::query("SELECT id,payload FROM waveform_bug_reports WHERE plane_id=$1 AND status='queued' AND next_attempt_at<=now() ORDER BY next_attempt_at FOR UPDATE SKIP LOCKED LIMIT 1").bind(Uuid::nil()).fetch_optional(&mut *tx).await? else {return Ok(());};
        let id: Uuid = row.get("id");
        let payload: Value = row.get("payload");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                ControlError::unavailable("postmark_unavailable", "Postmark is unavailable.")
            })?;
        let response=client.post(mail.endpoint.clone()).header("X-Postmark-Server-Token",mail.token.expose_secret()).json(&json!({"From":"waveform@teamofsilicons.com","To":"saketdev12@gmail.com,shubhastro2@gmails.com,bugs@teamofsilicons.com","Subject":format!("Waveform bug report {id}"),"TextBody":format!("Report: {id}\nVersion: {}\n\n{}\n\nPR: {}",payload["client_version"].as_str().unwrap_or_default(),payload["message"].as_str().unwrap_or_default(),payload["pr"].as_str().unwrap_or("No PR attached")),"MessageStream":"outbound","TrackOpens":false,"TrackLinks":"None","Metadata":{"report_id":id.to_string()}})).send().await;
        let sent = match response {
            Ok(r) if r.status().is_success() => r
                .json::<Value>()
                .await
                .is_ok_and(|v| v["ErrorCode"] == 0 && v["MessageID"].is_string()),
            _ => false,
        };
        if sent {
            sqlx::query("UPDATE waveform_bug_reports SET status='sent',sent_at=now(),attempts=attempts+1 WHERE id=$1").bind(id).execute(&mut *tx).await?;
        } else {
            sqlx::query("UPDATE waveform_bug_reports SET attempts=attempts+1,next_attempt_at=now()+make_interval(secs=>least(3600,60*power(2,least(attempts,6)))::double precision) WHERE id=$1").bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_reports_before_storage() {
        let report = |message: &str, pr: Option<&str>| Report {
            message: message.into(),
            pr: pr.map(str::to_owned),
            client_version: "0.2.0".into(),
        };
        assert!(
            validate(&report(
                "Broken speech",
                Some("https://github.com/teamofsilicons/silicon-waveform/pull/12")
            ))
            .is_ok()
        );
        assert!(validate(&report("", None)).is_err());
        assert!(validate(&report("token sar_secret", None)).is_err());
        assert!(validate(&report("proof sap_secret", None)).is_err());
        assert!(validate(&report("hook whsec_secret", None)).is_err());
        // A prefix inside a word is not a credential.
        assert!(validate(&report("the task_id field and disk-usage", None)).is_ok());
        assert!(
            validate(&report(
                "Broken speech",
                Some("https://github.com/other/repo/pull/12")
            ))
            .is_err()
        );
    }
}
