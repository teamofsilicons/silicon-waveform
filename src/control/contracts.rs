//! Durable API compatibility and quiet-period retirement, independent of analytics.
use super::{ControlError, ControlState, single_header};
use axum::{
    Json,
    extract::{Path, Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use http::{HeaderMap, HeaderValue, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row as _;
use std::sync::Arc;

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
        json!({"service":"silicon-waveform","default_api_version":"v1","protocol_versions":["1"],"contracts":contracts,"compatibility":[{"api_version":"v1","protocol_version":"1","rust_client":">=0.1.0, <0.3.0","cli":">=0.1.0, <0.3.0"}],"policy":{"quiet_days_before_sunset":7,"requires_active_successor":true,"guide":"https://docs.waveform.teamofsilicons.com/api-contracts/"}}),
    ))
}

impl ControlState {
    pub(super) async fn sunset_contracts(&self) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query("UPDATE waveform_api_contracts c SET state='sunset',sunset_at=now() WHERE c.state='deprecated' AND GREATEST(c.deprecated_at,COALESCE(c.last_request_at,c.deprecated_at)) <= now() - interval '7 days' AND EXISTS(SELECT 1 FROM waveform_api_contracts next WHERE next.version=c.successor AND next.state='active')")
            .execute(&self.pool).await?.rows_affected())
    }
    async fn contract_request(&self, version: &str, testing: bool) -> Result<String, ControlError> {
        let mut tx = self.pool.begin().await?;
        let row=sqlx::query("SELECT state,deprecated_at,last_request_at,successor FROM waveform_api_contracts WHERE version=$1 FOR UPDATE")
            .bind(version).fetch_optional(&mut *tx).await?.ok_or(ControlError{status:StatusCode::NOT_ACCEPTABLE,code:"unsupported_api_version"})?;
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
        if state != "sunset" && !testing {
            sqlx::query("UPDATE waveform_api_contracts SET last_request_at=now() WHERE version=$1")
                .bind(version)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        if state == "sunset" {
            return Err(ControlError {
                status: StatusCode::GONE,
                code: "api_version_sunset",
            });
        }
        Ok(state)
    }
}

fn validate(headers: &HeaderMap, version: &str) -> Result<(), ControlError> {
    if version != "v1" {
        return Err(ControlError {
            status: StatusCode::NOT_ACCEPTABLE,
            code: "unsupported_api_version",
        });
    }
    if single_header(headers, "x-waveform-api-version")?.is_some_and(|v| v != version) {
        return Err(ControlError {
            status: StatusCode::NOT_ACCEPTABLE,
            code: "api_version_mismatch",
        });
    }
    if single_header(headers, "x-waveform-protocol-version")?.is_some_and(|v| v != "1") {
        return Err(ControlError {
            status: StatusCode::NOT_ACCEPTABLE,
            code: "unsupported_protocol_version",
        });
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
    let testing = request.headers().contains_key("x-testing-environment-key");
    let status = match state.contract_request(&version, testing).await {
        Ok(status) => status,
        Err(e) => return e.into_response(),
    };
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("x-waveform-api-version", HeaderValue::from_static("v1"));
    headers.insert(
        "x-waveform-contract-version",
        HeaderValue::from_static("1.0.0"),
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Deprecate {
    successor: String,
}
pub(super) async fn deprecate(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(version): Path<String>,
    Json(body): Json<Deprecate>,
) -> Result<Json<Value>, ControlError> {
    state.service_authority(&headers)?;
    if version == body.successor {
        return Err(ControlError::bad_request("distinct_successor_required"));
    }
    let changed=sqlx::query("UPDATE waveform_api_contracts SET state='deprecated',successor=$2,deprecated_at=COALESCE(deprecated_at,now()) WHERE version=$1 AND state IN ('active','deprecated') AND EXISTS(SELECT 1 FROM waveform_api_contracts next WHERE next.version=$2 AND next.state='active') RETURNING version")
        .bind(version).bind(body.successor).fetch_optional(&state.pool).await?;
    if changed.is_none() {
        return Err(ControlError::bad_request("active_successor_required"));
    }
    Ok(Json(
        json!({"state":"deprecated","quiet_days_before_sunset":7}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::tests::{call, database, fixture};
    use axum::body::{Body, to_bytes};
    use tower::ServiceExt as _;
    use wiremock::MockServer;

    #[tokio::test]
    #[ignore = "requires WAVEFORM_TEST_DATABASE_URL"]
    async fn consumer_negotiation_and_quiet_period_are_durable()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pool, schema) = database().await?;
        let iam = MockServer::start().await;
        let state = fixture(pool.clone(), &iam)?;
        let app = crate::control::router(state.clone()).layer(
            axum::middleware::from_fn_with_state(state.clone(), negotiate),
        );
        // Existing consumers omit negotiation headers and keep the same response shape.
        let response = app
            .clone()
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/iam")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-waveform-api-version"], "v1");
        assert_eq!(response.headers()["x-waveform-protocol-version"], "1");
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 65536).await?)?;
        assert_eq!(body["app_id"], "tos>waveform");
        for (path, header, value) in [
            ("/api/v2/iam", "x-waveform-api-version", "v2"),
            ("/api/v1/iam", "x-waveform-api-version", "v2"),
            ("/api/v1/iam", "x-waveform-protocol-version", "2"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    http::Request::builder()
                        .uri(path)
                        .header(header, value)
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
        }
        assert_eq!(
            call(
                &app,
                "POST",
                "/internal/contracts/v1/deprecate",
                Some("test-only-honeycomb-service-token-123456"),
                json!({"successor":"v2"})
            )
            .await?
            .0,
            StatusCode::BAD_REQUEST
        );
        sqlx::query("INSERT INTO waveform_api_contracts(version,contract_version,state) VALUES('v2','2.0.0','active')").execute(&pool).await?;
        assert_eq!(
            call(
                &app,
                "POST",
                "/internal/contracts/v1/deprecate",
                Some("test-only-honeycomb-service-token-123456"),
                json!({"successor":"v2"})
            )
            .await?
            .0,
            StatusCode::OK
        );
        sqlx::query("UPDATE waveform_api_contracts SET deprecated_at=now()-interval '8 days',last_request_at=now()-interval '6 days' WHERE version='v1'").execute(&pool).await?;
        assert_eq!(state.sunset_contracts().await?, 0);
        assert_eq!(
            state
                .contract_request("v1", false)
                .await
                .map_err(|_| "contract")?,
            "deprecated"
        );
        let fresh:bool=sqlx::query_scalar("SELECT last_request_at>now()-interval '1 minute' FROM waveform_api_contracts WHERE version='v1'").fetch_one(&pool).await?;
        assert!(fresh);
        sqlx::query("UPDATE waveform_api_contracts SET last_request_at=now()-interval '6 days' WHERE version='v1'").execute(&pool).await?;
        state
            .contract_request("v1", true)
            .await
            .map_err(|_| "test contract")?;
        let isolated:bool=sqlx::query_scalar("SELECT last_request_at<now()-interval '5 days' FROM waveform_api_contracts WHERE version='v1'").fetch_one(&pool).await?;
        assert!(isolated);
        sqlx::query("UPDATE waveform_api_contracts SET last_request_at=now()-interval '7 days' WHERE version='v1'").execute(&pool).await?;
        assert_eq!(state.sunset_contracts().await?, 1);
        assert_eq!(
            call(&app, "GET", "/api/v1/iam", None, Value::Null).await?.0,
            StatusCode::GONE
        );
        let (status, discovery) = call(&app, "GET", "/api/contracts", None, Value::Null).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(discovery["contracts"][0]["state"], "sunset");
        assert_eq!(state.sunset_contracts().await?, 0);
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
            .execute(&pool)
            .await?;
        pool.close().await;
        Ok(())
    }
}
