//! Bounded media, normalized audio, and Briefcase reference types.

use std::{fmt, num::NonZeroU32, str::FromStr};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{OffsetDateTime, UtcOffset};
use url::Url;

use super::identity::RequestId;

/// Default and hard maximum source-media size: 25 MiB.
pub const MAX_SOURCE_MEDIA_BYTES: u64 = 25 * 1024 * 1024;

/// Maximum accepted provider audio artifact size: 25 MiB.
pub const MAX_PROVIDER_AUDIO_BYTES: u64 = 25 * 1024 * 1024;

/// Normalized source media types accepted by the STT contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceMediaType {
    /// MP3 or MPEG audio.
    Mpeg,
    /// RIFF/WAVE audio.
    Wav,
    /// FLAC audio.
    Flac,
    /// Ogg audio container.
    Ogg,
    /// `WebM` audio container.
    WebM,
    /// MPEG-4 audio container.
    Mp4,
    /// Raw AAC transport stream.
    Aac,
    /// MPEG-4 Audio presentation commonly identified as M4A.
    M4a,
    /// MPEG-4 video container with an audio track.
    Mp4Video,
    /// MPEG video container with an audio track.
    MpegVideo,
    /// `WebM` video container with an audio track.
    WebMVideo,
}

impl SourceMediaType {
    /// Every canonical media type exposed by `/capabilities`.
    pub const ALL: [Self; 11] = [
        Self::Mpeg,
        Self::Wav,
        Self::Flac,
        Self::Ogg,
        Self::WebM,
        Self::Mp4,
        Self::Aac,
        Self::M4a,
        Self::Mp4Video,
        Self::MpegVideo,
        Self::WebMVideo,
    ];

    /// Returns the canonical MIME spelling.
    #[must_use]
    pub const fn as_mime_str(self) -> &'static str {
        match self {
            Self::Mpeg => "audio/mpeg",
            Self::Wav => "audio/wav",
            Self::Flac => "audio/flac",
            Self::Ogg => "audio/ogg",
            Self::WebM => "audio/webm",
            Self::Mp4 => "audio/mp4",
            Self::Aac => "audio/aac",
            Self::M4a => "audio/m4a",
            Self::Mp4Video => "video/mp4",
            Self::MpegVideo => "video/mpeg",
            Self::WebMVideo => "video/webm",
        }
    }
}

impl fmt::Display for SourceMediaType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_mime_str())
    }
}

impl FromStr for SourceMediaType {
    type Err = MediaError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let essence = value
            .split(';')
            .next()
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_lowercase();

        match essence.as_str() {
            "audio/mpeg" | "audio/mp3" | "audio/mpga" => Ok(Self::Mpeg),
            "audio/wav" | "audio/x-wav" | "audio/wave" => Ok(Self::Wav),
            "audio/flac" | "audio/x-flac" => Ok(Self::Flac),
            "audio/ogg" => Ok(Self::Ogg),
            "audio/webm" => Ok(Self::WebM),
            "audio/mp4" => Ok(Self::Mp4),
            "audio/aac" => Ok(Self::Aac),
            "audio/m4a" | "audio/x-m4a" => Ok(Self::M4a),
            "video/mp4" => Ok(Self::Mp4Video),
            "video/mpeg" => Ok(Self::MpegVideo),
            "video/webm" => Ok(Self::WebMVideo),
            _ => Err(MediaError::UnsupportedMediaType),
        }
    }
}

/// Deployment media-size limit, constrained by Waveform's synchronous ceiling.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MediaSizeLimit(NonZeroU32);

impl MediaSizeLimit {
    /// Constructs a non-zero limit no greater than 25 MiB.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidSizeLimit`] when the value is zero or
    /// above Waveform's synchronous 25 MiB ceiling.
    pub fn new(bytes: u64) -> Result<Self, MediaError> {
        if bytes == 0 || bytes > MAX_SOURCE_MEDIA_BYTES {
            return Err(MediaError::InvalidSizeLimit);
        }
        u32::try_from(bytes)
            .ok()
            .and_then(NonZeroU32::new)
            .map(Self)
            .ok_or(MediaError::InvalidSizeLimit)
    }

    /// Returns the limit in bytes.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get() as u64
    }
}

impl Default for MediaSizeLimit {
    fn default() -> Self {
        // This expression is statically non-zero and inferred as `u32` by the
        // constructor, so the defensive branch cannot be reached.
        let value = match NonZeroU32::new(25 * 1024 * 1024) {
            Some(value) => value,
            None => NonZeroU32::MIN,
        };
        Self(value)
    }
}

/// Authorized, bounded media loaded from Briefcase for transcription.
#[derive(Clone, Eq, PartialEq)]
pub struct SourceMedia {
    media_type: SourceMediaType,
    bytes: Bytes,
}

impl SourceMedia {
    /// Constructs source media while enforcing the requested and global limits.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::EmptyMedia`] for an empty body or
    /// [`MediaError::MediaTooLarge`] when either size bound is exceeded.
    pub fn new(
        media_type: SourceMediaType,
        bytes: Bytes,
        limit: MediaSizeLimit,
    ) -> Result<Self, MediaError> {
        if bytes.is_empty() {
            return Err(MediaError::EmptyMedia);
        }
        let length = u64::try_from(bytes.len()).map_err(|_| MediaError::MediaTooLarge {
            limit_bytes: limit.get(),
        })?;
        if length > limit.get() || length > MAX_SOURCE_MEDIA_BYTES {
            return Err(MediaError::MediaTooLarge {
                limit_bytes: limit.get(),
            });
        }
        Ok(Self { media_type, bytes })
    }

    /// Returns the normalized declared media type.
    #[must_use]
    pub const fn media_type(&self) -> SourceMediaType {
        self.media_type
    }

    /// Returns the media bytes.
    #[must_use]
    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    /// Consumes the artifact and returns its media bytes.
    #[must_use]
    pub fn into_bytes(self) -> Bytes {
        self.bytes
    }

    /// Returns the byte length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns whether the artifact contains no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for SourceMedia {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceMedia")
            .field("media_type", &self.media_type)
            .field("byte_length", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// Exact encoding of an audio artifact returned by a TTS provider.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProviderAudioFormat {
    /// MPEG Layer III audio.
    Mp3,
    /// WAV container.
    Wav,
    /// Headerless linear PCM with explicit sample metadata.
    LinearPcm(PcmSpecification),
}

/// Description needed to interpret and measure raw linear PCM.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PcmSpecification {
    sample_rate_hz: NonZeroU32,
    channels: NonZeroU32,
    bits_per_sample: NonZeroU32,
}

impl PcmSpecification {
    /// Constructs a supported PCM description.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidPcm`] for zero values, more than eight
    /// channels, or an unsupported sample width.
    pub fn new(
        sample_rate_hz: u32,
        channels: u32,
        bits_per_sample: u32,
    ) -> Result<Self, MediaError> {
        let sample_rate_hz = NonZeroU32::new(sample_rate_hz).ok_or(MediaError::InvalidPcm)?;
        let channels = NonZeroU32::new(channels).ok_or(MediaError::InvalidPcm)?;
        let bits_per_sample = NonZeroU32::new(bits_per_sample).ok_or(MediaError::InvalidPcm)?;
        if channels.get() > 8 || !matches!(bits_per_sample.get(), 8 | 16 | 24 | 32) {
            return Err(MediaError::InvalidPcm);
        }
        Ok(Self {
            sample_rate_hz,
            channels,
            bits_per_sample,
        })
    }

    /// Samples per second per channel.
    #[must_use]
    pub const fn sample_rate_hz(self) -> u32 {
        self.sample_rate_hz.get()
    }

    /// Interleaved channel count.
    #[must_use]
    pub const fn channels(self) -> u32 {
        self.channels.get()
    }

    /// Number of bits in each sample.
    #[must_use]
    pub const fn bits_per_sample(self) -> u32 {
        self.bits_per_sample.get()
    }
}

/// Non-empty, bounded audio returned by a provider before MP3 normalization.
#[derive(Clone, Eq, PartialEq)]
pub struct AudioArtifact {
    format: ProviderAudioFormat,
    bytes: Bytes,
}

impl AudioArtifact {
    /// Constructs a bounded provider artifact.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::EmptyMedia`] or [`MediaError::MediaTooLarge`]
    /// when the provider payload violates the artifact bounds.
    pub fn new(format: ProviderAudioFormat, bytes: Bytes) -> Result<Self, MediaError> {
        if bytes.is_empty() {
            return Err(MediaError::EmptyMedia);
        }
        if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_PROVIDER_AUDIO_BYTES) {
            return Err(MediaError::MediaTooLarge {
                limit_bytes: MAX_PROVIDER_AUDIO_BYTES,
            });
        }
        Ok(Self { format, bytes })
    }

    /// Returns the artifact encoding.
    #[must_use]
    pub const fn format(&self) -> ProviderAudioFormat {
        self.format
    }

    /// Returns the encoded audio bytes.
    #[must_use]
    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    /// Consumes the artifact and returns its bytes.
    #[must_use]
    pub fn into_bytes(self) -> Bytes {
        self.bytes
    }
}

impl fmt::Debug for AudioArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AudioArtifact")
            .field("format", &self.format)
            .field("byte_length", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// Duration measured from decoded media metadata or exact PCM sample counts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MediaDuration(u64);

impl MediaDuration {
    /// Constructs a duration from whole milliseconds.
    #[must_use]
    pub const fn from_millis(milliseconds: u64) -> Self {
        Self(milliseconds)
    }

    /// Returns whole milliseconds.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }
}

/// Validated MP3 plus its measured duration.
#[derive(Clone, Eq, PartialEq)]
pub struct NormalizedAudio {
    bytes: Bytes,
    duration: MediaDuration,
}

impl NormalizedAudio {
    /// Constructs normalized MP3 output after codec validation.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::EmptyMedia`] or [`MediaError::MediaTooLarge`]
    /// when the normalized payload violates the output bounds.
    pub fn new(bytes: Bytes, duration: MediaDuration) -> Result<Self, MediaError> {
        if bytes.is_empty() {
            return Err(MediaError::EmptyMedia);
        }
        if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_PROVIDER_AUDIO_BYTES) {
            return Err(MediaError::MediaTooLarge {
                limit_bytes: MAX_PROVIDER_AUDIO_BYTES,
            });
        }
        Ok(Self { bytes, duration })
    }

    /// Returns the MP3 bytes.
    #[must_use]
    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    /// Returns the measured audio duration.
    #[must_use]
    pub const fn duration(&self) -> MediaDuration {
        self.duration
    }

    /// Consumes the artifact and returns its MP3 bytes.
    #[must_use]
    pub fn into_bytes(self) -> Bytes {
        self.bytes
    }
}

impl fmt::Debug for NormalizedAudio {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NormalizedAudio")
            .field("byte_length", &self.bytes.len())
            .field("duration", &self.duration)
            .finish_non_exhaustive()
    }
}

/// Configured HTTPS origin used to reject caller-controlled fetch targets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BriefcaseOrigin(Url);

impl BriefcaseOrigin {
    /// Constructs a credential-free HTTPS origin URL.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidUrl`] when the URL is not HTTPS, has no
    /// host, embeds credentials, or contains a query or fragment.
    pub fn new(mut value: Url) -> Result<Self, MediaError> {
        validate_https_url(&value)?;
        if value.query().is_some() || value.fragment().is_some() {
            return Err(MediaError::InvalidUrl);
        }
        value.set_path("/");
        Ok(Self(value))
    }

    /// Returns true only when a URL has exactly this scheme, host, and port.
    #[must_use]
    pub fn contains(&self, value: &BriefcaseFileUrl) -> bool {
        self.0.origin() == value.0.origin()
    }
}

/// Stable authenticated Briefcase URL. Debug output is intentionally redacted.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct BriefcaseFileUrl(Url);

impl BriefcaseFileUrl {
    /// Validates a permanent, credential-free HTTPS URL.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidUrl`] for an unsafe URL or
    /// [`MediaError::InvalidPermanentUrl`] when it has a query or fragment.
    pub fn new(value: Url) -> Result<Self, MediaError> {
        validate_https_url(&value)?;
        if value.query().is_some() || value.fragment().is_some() {
            return Err(MediaError::InvalidPermanentUrl);
        }
        Ok(Self(value))
    }

    /// Returns the URL for explicit boundary translation.
    #[must_use]
    pub const fn as_url(&self) -> &Url {
        &self.0
    }
}

impl fmt::Debug for BriefcaseFileUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BriefcaseFileUrl([REDACTED])")
    }
}

impl fmt::Display for BriefcaseFileUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Expiring HTTPS delivery URL. Debug output is intentionally redacted.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct TemporaryMediaUrl(Url);

impl TemporaryMediaUrl {
    /// Validates a credential-free HTTPS URL, allowing a signed query string.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidUrl`] when the URL is unsafe or contains
    /// a fragment.
    pub fn new(value: Url) -> Result<Self, MediaError> {
        validate_https_url(&value)?;
        if value.fragment().is_some() {
            return Err(MediaError::InvalidUrl);
        }
        Ok(Self(value))
    }

    /// Returns the URL for explicit boundary translation.
    #[must_use]
    pub const fn as_url(&self) -> &Url {
        &self.0
    }
}

impl fmt::Debug for TemporaryMediaUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TemporaryMediaUrl([REDACTED])")
    }
}

impl fmt::Display for TemporaryMediaUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Safe filename for generated TTS audio in Briefcase.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GeneratedAudioFileName(String);

impl GeneratedAudioFileName {
    /// Generates the collision-resistant documented UTC filename.
    #[must_use]
    pub fn for_request(generated_at: OffsetDateTime, request_id: RequestId) -> Self {
        let utc = generated_at.to_offset(UtcOffset::UTC);
        let date = utc.date();
        let time = utc.time();
        Self(format!(
            "tts_{:04}{:02}{:02}_{:02}{:02}{:02}_{}.mp3",
            date.year(),
            u8::from(date.month()),
            date.day(),
            time.hour(),
            time.minute(),
            time.second(),
            request_id
        ))
    }

    /// Returns the generated filename.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GeneratedAudioFileName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Durable and immediate-playback references returned by Briefcase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredAudio {
    /// Durable authenticated reference.
    pub permanent_url: BriefcaseFileUrl,
    /// Expiring immediate-playback reference.
    pub temporary_url: Option<TemporaryMediaUrl>,
}

/// Media validation failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum MediaError {
    /// Empty audio or source content is invalid.
    #[error("media must not be empty")]
    EmptyMedia,
    /// The synchronous media ceiling was exceeded.
    #[error("media exceeds the {limit_bytes}-byte limit")]
    MediaTooLarge {
        /// Enforced maximum.
        limit_bytes: u64,
    },
    /// Deployment limits must stay within the synchronous service ceiling.
    #[error("media size limit must be between 1 byte and 25 MiB")]
    InvalidSizeLimit,
    /// The declared source type is not accepted by the stable contract.
    #[error("unsupported source media type")]
    UnsupportedMediaType,
    /// Raw PCM metadata was incomplete or outside supported bounds.
    #[error("invalid PCM specification")]
    InvalidPcm,
    /// A URL was not a credential-free HTTPS resource.
    #[error("invalid HTTPS media URL")]
    InvalidUrl,
    /// Permanent references must not contain a query or fragment.
    #[error("invalid permanent Briefcase URL")]
    InvalidPermanentUrl,
}

fn validate_https_url(value: &Url) -> Result<(), MediaError> {
    if value.scheme() != "https"
        || value.host_str().is_none()
        || !value.username().is_empty()
        || value.password().is_some()
    {
        return Err(MediaError::InvalidUrl);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use bytes::Bytes;
    use time::macros::datetime;
    use url::Url;
    use uuid::Uuid;

    use super::{
        AudioArtifact, BriefcaseFileUrl, BriefcaseOrigin, GeneratedAudioFileName,
        MAX_SOURCE_MEDIA_BYTES, MediaError, MediaSizeLimit, ProviderAudioFormat, SourceMedia,
        SourceMediaType,
    };
    use crate::domain::identity::RequestId;

    #[test]
    fn media_type_aliases_canonicalize() {
        assert_eq!(
            SourceMediaType::from_str("audio/mp3"),
            Ok(SourceMediaType::Mpeg)
        );
        assert_eq!(
            SourceMediaType::from_str("Audio/X-WAV; rate=16000"),
            Ok(SourceMediaType::Wav)
        );
        assert_eq!(SourceMediaType::Mpeg.to_string(), "audio/mpeg");
    }

    #[test]
    fn source_media_is_non_empty_and_bounded() {
        let limit = MediaSizeLimit::new(3);
        assert!(matches!(
            limit.and_then(|value| SourceMedia::new(SourceMediaType::Mpeg, Bytes::new(), value)),
            Err(MediaError::EmptyMedia)
        ));
        let limit = MediaSizeLimit::new(3);
        assert!(matches!(
            limit.and_then(|value| {
                SourceMedia::new(SourceMediaType::Mpeg, Bytes::from_static(b"four"), value)
            }),
            Err(MediaError::MediaTooLarge { limit_bytes: 3 })
        ));
        assert_eq!(MediaSizeLimit::default().get(), MAX_SOURCE_MEDIA_BYTES);
    }

    #[test]
    fn audio_debug_output_does_not_include_payload() {
        let artifact = AudioArtifact::new(
            ProviderAudioFormat::Mp3,
            Bytes::from_static(b"very-secret-audio"),
        );
        assert!(
            artifact.is_ok_and(|value| { !format!("{value:?}").contains("very-secret-audio") })
        );
    }

    #[test]
    fn permanent_urls_require_https_without_signed_components() {
        let insecure = Url::parse("http://briefcase.example/files/one");
        let signed = Url::parse("https://briefcase.example/files/one?signature=secret");

        assert!(matches!(
            insecure
                .map_err(|_| MediaError::InvalidUrl)
                .and_then(BriefcaseFileUrl::new),
            Err(MediaError::InvalidUrl)
        ));
        assert!(matches!(
            signed
                .map_err(|_| MediaError::InvalidUrl)
                .and_then(BriefcaseFileUrl::new),
            Err(MediaError::InvalidPermanentUrl)
        ));
    }

    #[test]
    fn origin_comparison_is_exact() {
        let origin = Url::parse("https://briefcase.example/api/v1")
            .map_err(|_| MediaError::InvalidUrl)
            .and_then(BriefcaseOrigin::new);
        let matching = Url::parse("https://briefcase.example/files/one")
            .map_err(|_| MediaError::InvalidUrl)
            .and_then(BriefcaseFileUrl::new);
        let attacker = Url::parse("https://briefcase.example.attacker.test/files/one")
            .map_err(|_| MediaError::InvalidUrl)
            .and_then(BriefcaseFileUrl::new);

        assert!(matches!((&origin, &matching), (Ok(origin), Ok(url)) if origin.contains(url)));
        assert!(matches!((&origin, &attacker), (Ok(origin), Ok(url)) if !origin.contains(url)));
    }

    #[test]
    fn generated_filename_uses_utc_timestamp_and_request_id() {
        let request_id = RequestId::new(Uuid::from_u128(1));
        let filename = request_id.map(|value| {
            GeneratedAudioFileName::for_request(datetime!(2026-08-31 12:34:56 +05:30), value)
        });

        assert_eq!(
            filename.map(|value| value.to_string()),
            Ok("tts_20260831_070456_00000000-0000-0000-0000-000000000001.mp3".to_owned())
        );
    }
}
