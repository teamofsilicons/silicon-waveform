//! Durable bug reports. Sandbox reports simulate delivery and never send mail.
use super::{ControlError, ControlState};
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
fn validate(report: &Report) -> Result<(), ControlError> {
    if report.message.trim().is_empty()
        || report.message.len() > 60_000
        || report.client_version.len() > 100
    {
        return Err(ControlError::bad_request(
            "report_requires_1_to_60000_bytes",
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
        ));
    }
    // Refuse common credentials before any durable write or delivery.
    if ["ask_", "oac_", "oat_", "ort_", "obo_", "sk-"]
        .iter()
        .any(|prefix| report.message.contains(prefix) || report.client_version.contains(prefix))
    {
        return Err(ControlError::bad_request("remove_credentials_from_report"));
    }
    Ok(())
}
pub(super) async fn submit(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(report): Json<Report>,
) -> Result<(StatusCode, Json<Value>), ControlError> {
    validate(&report)?;
    let identity = state
        .identity_for_org(&headers, super::single_header(&headers, "x-org-id")?)
        .await?;
    let key = super::single_header(&headers, "idempotency-key")?
        .ok_or_else(|| ControlError::bad_request("idempotency_key_required"))?;
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(ControlError::bad_request("invalid_idempotency_key"));
    }
    let simulated = !identity.plane.id.is_nil();
    if !simulated && state.mail.is_none() {
        return Err(ControlError::unavailable("postmark_not_configured"));
    }
    let payload = json!(report);
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29))")
        .bind(format!(
            "report:{}:{}",
            identity.plane.id, identity.authority.principal_id
        ))
        .execute(&mut *tx)
        .await?;
    let old = sqlx::query("SELECT id,payload,status FROM waveform_bug_reports WHERE plane_id=$1 AND org_id=$2 AND actor_id=$3 AND idempotency_key=$4").bind(identity.plane.id).bind(&identity.authority.org_id).bind(identity.authority.principal_id).bind(key).fetch_optional(&mut *tx).await?;
    let (id, status): (Uuid, String) = if let Some(old) = old {
        if old.get::<Value, _>("payload") != payload {
            return Err(ControlError {
                status: StatusCode::CONFLICT,
                code: "report_idempotency_conflict",
            });
        }
        (old.get("id"), old.get("status"))
    } else {
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM waveform_bug_reports WHERE plane_id=$1 AND actor_id=$2 AND created_at>now()-interval '1 hour'").bind(identity.plane.id).bind(identity.authority.principal_id).fetch_one(&mut *tx).await?;
        if count >= 10 {
            return Err(ControlError {
                status: StatusCode::TOO_MANY_REQUESTS,
                code: "report_hourly_limit",
            });
        }
        let id = Uuid::new_v4();
        let status = if simulated { "simulated" } else { "queued" };
        sqlx::query("INSERT INTO waveform_bug_reports(id,plane_id,org_id,actor_id,idempotency_key,payload,status) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(id).bind(identity.plane.id).bind(&identity.authority.org_id).bind(identity.authority.principal_id).bind(key).bind(payload).bind(status).execute(&mut *tx).await?;
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
            .map_err(|_| ControlError::unavailable("postmark_unavailable"))?;
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
        assert!(validate(&report("token oat_secret", None)).is_err());
        assert!(
            validate(&report(
                "Broken speech",
                Some("https://github.com/other/repo/pull/12")
            ))
            .is_err()
        );
    }
}
