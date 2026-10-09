//! Audio Waveform holds because it could not (or must not) store it in Briefcase.
//! Served only to the account that requested it (its own token, or an app's
//! proof with `waveform.tts` acting for it) until it expires.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    response::Response,
};
use http::{HeaderMap, HeaderValue, StatusCode, header};
use uuid::Uuid;

use super::{ControlError, ControlState};
use crate::domain::auth::WaveformAction;

pub(super) async fn download(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
) -> Result<Response, ControlError> {
    let caller = state
        .caller(&headers, WaveformAction::SynthesizeSpeech)
        .await?;
    // Another account's request id answers exactly like an unknown or expired one.
    let file = state
        .held_audio
        .fetch(request_id, caller.record.storage_key)
        .await?
        .ok_or_else(|| {
            ControlError::not_found(
                "audio_not_found",
                format!("Waveform holds no audio for request {request_id} for this account (it was stored in Briefcase, expired, or belongs to another account)."),
            )
        })?;
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", file.filename))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    let mut response = Response::new(Body::from(file.bytes));
    *response.status_mut() = StatusCode::OK;
    let response_headers = response.headers_mut();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&file.media_type)
            .unwrap_or_else(|_| HeaderValue::from_static("audio/mpeg")),
    );
    response_headers.insert(header::CONTENT_DISPOSITION, disposition);
    response_headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    if let Ok(expires) = file
        .expires_at
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| ())
        .and_then(|value| HeaderValue::from_str(&value).map_err(|_| ()))
    {
        response_headers.insert("x-waveform-audio-expires-at", expires);
    }
    Ok(response)
}
