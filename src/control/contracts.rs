//! Durable API compatibility and quiet-period retirement, independent of analytics.
use super::{ControlError, ControlState, single_header};
use axum::{
    Json,
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use http::{HeaderMap, HeaderValue, StatusCode};
use serde_json::{Value, json};
use sqlx::{PgPool, Row as _};
use std::sync::Arc;

/// Contract version of API v1 implemented by this build.
pub const CONTRACT_VERSION: &str = "2.0.0";

pub(super) async fn describe(
    State(state): State<Arc<ControlState>>,
) -> Result<Json<Value>, ControlError> {
    let rows=sqlx::query("SELECT version,contract_version,state,successor,deprecated_at,sunset_at FROM waveform_api_contracts ORDER BY version")
        .fetch_all(&state.pool).await?;
    let contracts=rows.into_iter().map(|row|->Result<Value,sqlx::Error>{
        let date=|name|->Result<Option<String>,sqlx::Error>{Ok(row.try_get::<Option<time::OffsetDateTime>,_>(name)?.and_then(|v|v.format(&time::format_description::well_known::Rfc3339).ok()))};
        Ok(json!({"api_version":row.try_get::<String,_>("version")?,"contract_version":row.try_get::<String,_>("contract_version")?,"state":row.try_get::<String,_>("state")?,"successor":row.try_get::<Option<String>,_>("successor")?,"deprecated_at":date("deprecated_at")?,"sunset_at":date("sunset_at")?}))
    }).collect::<Result<Vec<_>,_>>()?;
    Ok(Json(
        json!({"service":"silicon-waveform","default_api_version":"v1","protocol_versions":["1"],"contracts":contracts,"compatibility":[{"api_version":"v1","protocol_version":"1","contract_version":CONTRACT_VERSION,"rust_client":">=0.6.0, <0.7.0","cli":">=0.6.0, <0.7.0","authentication":["bearer","proof"]}],"policy":{"quiet_days_before_sunset":7,"requires_active_successor":true,"guide":"https://docs.waveform.teamofsilicons.com/api-contracts/"}}),
    ))
}

impl ControlState {
    pub(crate) async fn sunset_contracts(&self) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query("UPDATE waveform_api_contracts c SET state='sunset',sunset_at=now() WHERE c.state='deprecated' AND GREATEST(c.deprecated_at,COALESCE(c.last_request_at,c.deprecated_at)) <= now() - interval '7 days' AND EXISTS(SELECT 1 FROM waveform_api_contracts next WHERE next.version=c.successor AND next.state='active')")
            .execute(&self.pool).await?.rows_affected())
    }
    async fn contract_request(&self, version: &str) -> Result<String, ControlError> {
        let mut tx = self.pool.begin().await?;
        let row=sqlx::query("SELECT state,deprecated_at,last_request_at,successor FROM waveform_api_contracts WHERE version=$1 FOR UPDATE")
            .bind(version).fetch_optional(&mut *tx).await?.ok_or_else(unsupported_version)?;
        let mut state: String = row.try_get("state")?;
        // Production requests and the maintenance worker share row locking. Traffic
        // at the boundary cannot revive a version whose quiet period has elapsed.
        if state == "deprecated" {
            let sunset:bool=sqlx::query_scalar("SELECT GREATEST(deprecated_at,COALESCE(last_request_at,deprecated_at)) <= now() - interval '7 days' AND EXISTS(SELECT 1 FROM waveform_api_contracts next WHERE next.version=c.successor AND next.state='active') FROM waveform_api_contracts c WHERE version=$1")
                .bind(version).fetch_one(&mut *tx).await?;
            if sunset {
                sqlx::query("UPDATE waveform_api_contracts SET state='sunset',sunset_at=now() WHERE version=$1").bind(version).execute(&mut *tx).await?;
                state = "sunset".into();
            }
        }
        if state != "sunset" {
            sqlx::query("UPDATE waveform_api_contracts SET last_request_at=now() WHERE version=$1")
                .bind(version)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        if state == "sunset" {
            return Err(ControlError::new(
                StatusCode::GONE,
                "api_version_sunset",
                format!(
                    "API {version} was retired; GET /api/contracts lists the supported versions."
                ),
            ));
        }
        Ok(state)
    }
}

fn unsupported_version() -> ControlError {
    ControlError::new(
        StatusCode::NOT_ACCEPTABLE,
        "unsupported_api_version",
        "This build serves API v1 only; GET /api/contracts lists the supported versions.",
    )
}

fn validate(headers: &HeaderMap, version: &str) -> Result<(), ControlError> {
    if version != "v1" {
        return Err(unsupported_version());
    }
    if single_header(headers, "x-waveform-api-version")?.is_some_and(|v| v != version) {
        return Err(ControlError::new(
            StatusCode::NOT_ACCEPTABLE,
            "api_version_mismatch",
            "X-Waveform-API-Version must match the version in the path (v1).",
        ));
    }
    if single_header(headers, "x-waveform-protocol-version")?.is_some_and(|v| v != "1") {
        return Err(ControlError::new(
            StatusCode::NOT_ACCEPTABLE,
            "unsupported_protocol_version",
            "X-Waveform-Protocol-Version must be 1.",
        ));
    }
    Ok(())
}

pub(crate) async fn negotiate(
    State(state): State<Arc<ControlState>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if !path.starts_with("/api/v") {
        return next.run(request).await;
    }
    let version = path.split('/').nth(2).unwrap_or("").to_owned();
    if let Err(error) = validate(request.headers(), &version) {
        return error.into_response();
    }
    let status = match state.contract_request(&version).await {
        Ok(status) => status,
        Err(e) => return e.into_response(),
    };
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("x-waveform-api-version", HeaderValue::from_static("v1"));
    headers.insert(
        "x-waveform-contract-version",
        HeaderValue::from_static(CONTRACT_VERSION),
    );
    headers.insert("x-waveform-protocol-version", HeaderValue::from_static("1"));
    if status == "deprecated" {
        if let Ok(Some(timestamp)) = sqlx::query_scalar::<_, Option<time::OffsetDateTime>>(
            "SELECT deprecated_at FROM waveform_api_contracts WHERE version='v1'",
        )
        .fetch_one(&state.pool)
        .await
            && let Ok(value) = HeaderValue::from_str(&format!("@{}", timestamp.unix_timestamp()))
        {
            headers.insert("deprecation", value);
        }
        headers.insert(
            "link",
            HeaderValue::from_static(
                "</api/contracts>; rel=\"deprecation\"; type=\"application/json\"",
            ),
        );
    }
    response
}

/// Operator action (`waveform-api contracts deprecate <version> --successor <version>`):
/// marks a version deprecated. It sunsets after seven days without requests,
/// once its successor is active. Retry-safe; the start time is kept.
///
/// # Errors
///
/// A message when the successor is missing, inactive or the same version.
pub async fn deprecate(pool: &PgPool, version: &str, successor: &str) -> Result<(), String> {
    if version == successor {
        return Err("the successor must be a different version".to_owned());
    }
    let changed = sqlx::query("UPDATE waveform_api_contracts SET state='deprecated',successor=$2,deprecated_at=COALESCE(deprecated_at,now()) WHERE version=$1 AND state IN ('active','deprecated') AND EXISTS(SELECT 1 FROM waveform_api_contracts next WHERE next.version=$2 AND next.state='active') RETURNING version")
        .bind(version)
        .bind(successor)
        .fetch_optional(pool)
        .await
        .map_err(|error| format!("database error: {error}"))?;
    if changed.is_none() {
        return Err(format!(
            "{version} was not deprecated: it must be active or deprecated, and {successor} must already be registered and active"
        ));
    }
    Ok(())
}
