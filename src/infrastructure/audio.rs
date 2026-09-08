//! Bounded audio validation and normalization.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use bytes::Bytes;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

use crate::{
    application::ports::{AudioNormalizationError, AudioNormalizer},
    domain::{
        identity::RequestId,
        media::{AudioArtifact, MediaDuration, NormalizedAudio, ProviderAudioFormat},
    },
};

/// MIME type returned by Waveform for all synthesized speech.
pub const MP3_MEDIA_TYPE: &str = "audio/mpeg";

const STDERR_LIMIT: usize = 8 * 1024;

/// Bounds applied while invoking `FFmpeg`.
#[derive(Clone, Debug)]
pub struct AudioNormalizerConfig {
    /// Absolute production path, or development command name, for `FFmpeg`.
    pub ffmpeg_path: PathBuf,
    /// Maximum source payload accepted by the adapter.
    pub max_input_bytes: usize,
    /// Maximum normalized MP3 payload retained in memory.
    pub max_output_bytes: usize,
    /// Constant output bitrate passed to the MP3 encoder.
    pub bitrate_kbps: u16,
    /// Maximum wall-clock duration for one normalization.
    pub timeout: Duration,
}

impl AudioNormalizerConfig {
    /// Builds production-oriented bounds around a configured executable.
    #[must_use]
    pub fn new(ffmpeg_path: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg_path: ffmpeg_path.into(),
            max_input_bytes: 25 * 1024 * 1024,
            max_output_bytes: 25 * 1024 * 1024,
            bitrate_kbps: 128,
            timeout: Duration::from_secs(20),
        }
    }
}

/// Safe failures from the audio boundary.
#[derive(Clone, Copy, Debug, Error)]
pub enum AudioError {
    /// Input contained no audio bytes.
    #[error("audio input is empty")]
    EmptyInput,
    /// Input exceeded the configured byte limit.
    #[error("audio input exceeds the configured limit")]
    InputTooLarge,
    /// `FFmpeg` could not be started.
    #[error("audio normalizer is unavailable")]
    ProcessUnavailable,
    /// `FFmpeg` exceeded its operation timeout.
    #[error("audio normalization timed out")]
    Timeout,
    /// `FFmpeg` rejected or could not decode the input.
    #[error("audio normalization failed")]
    ProcessFailed,
    /// `FFmpeg` emitted more data than the configured bound.
    #[error("normalized audio exceeds the configured limit")]
    OutputTooLarge,
    /// The claimed or generated MP3 was empty, truncated, or malformed.
    #[error("invalid MP3 audio")]
    InvalidMp3,
    /// Raw PCM metadata or the configured encoder bitrate was invalid.
    #[error("invalid audio format configuration")]
    InvalidFormat,
    /// `FFmpeg` path or encoder configuration is invalid.
    #[error("invalid audio normalizer configuration")]
    InvalidConfiguration,
}

/// FFmpeg-backed normalizer. It never invokes a shell.
#[derive(Clone, Debug)]
pub struct FfmpegAudioNormalizer {
    config: AudioNormalizerConfig,
}

impl FfmpegAudioNormalizer {
    /// Creates a normalizer with explicit process and memory bounds.
    #[must_use]
    pub const fn new(config: AudioNormalizerConfig) -> Self {
        Self { config }
    }

    /// Passes through valid MP3 or transcodes another provider format to MP3.
    async fn normalize(
        &self,
        input: Bytes,
        media_type: &str,
        exact_duration_ms: Option<u64>,
    ) -> Result<NormalizedAudio, AudioError> {
        self.validate_input_size(&input)?;

        if is_mp3_media_type(media_type) {
            if input.len() > self.config.max_output_bytes {
                return Err(AudioError::OutputTooLarge);
            }
            let duration_ms = mp3_duration_ms(&input)?;
            return NormalizedAudio::new(input, MediaDuration::from_millis(duration_ms))
                .map_err(|_| AudioError::InvalidMp3);
        }

        let output = self.run_ffmpeg(input, media_type).await?;
        let encoded_duration_ms = mp3_duration_ms(&output)?;
        let duration_ms = exact_duration_ms.unwrap_or(encoded_duration_ms);
        NormalizedAudio::new(Bytes::from(output), MediaDuration::from_millis(duration_ms))
            .map_err(|_| AudioError::InvalidMp3)
    }

    fn validate_input_size(&self, input: &[u8]) -> Result<(), AudioError> {
        if input.is_empty() {
            return Err(AudioError::EmptyInput);
        }
        if input.len() > self.config.max_input_bytes {
            return Err(AudioError::InputTooLarge);
        }
        Ok(())
    }

    async fn run_ffmpeg(&self, input: Bytes, media_type: &str) -> Result<Vec<u8>, AudioError> {
        let mut child = fixed_ffmpeg_command(
            &self.config.ffmpeg_path,
            media_type,
            self.config.bitrate_kbps,
        )?
        .spawn()
        .map_err(|_| AudioError::ProcessUnavailable)?;

        let mut stdin = child.stdin.take().ok_or(AudioError::ProcessUnavailable)?;
        let stdout = child.stdout.take().ok_or(AudioError::ProcessUnavailable)?;
        let stderr = child.stderr.take().ok_or(AudioError::ProcessUnavailable)?;
        let output_limit = self.config.max_output_bytes;

        let operation = async {
            let write_input = async move {
                stdin.write_all(&input).await?;
                stdin.shutdown().await
            };
            let read_output = read_bounded(stdout, output_limit);
            let read_stderr = read_bounded(stderr, STDERR_LIMIT);
            let wait = child.wait();

            let (write_result, output_result, stderr_result, status_result) =
                tokio::join!(write_input, read_output, read_stderr, wait);
            write_result.map_err(|_| AudioError::ProcessFailed)?;
            let output = output_result?;
            // Drain stderr to ensure FFmpeg cannot block on a full pipe. Its contents are
            // deliberately discarded because provider media details must not reach callers.
            drop(stderr_result);
            let status = status_result.map_err(|_| AudioError::ProcessFailed)?;
            if !status.success() {
                return Err(AudioError::ProcessFailed);
            }
            Ok(output)
        };

        if let Ok(result) = tokio::time::timeout(self.config.timeout, operation).await {
            result
        } else {
            // `kill_on_drop` is also enabled, but explicitly requesting termination makes
            // timeout behavior deterministic while the child handle is still available.
            let _ = child.start_kill();
            Err(AudioError::Timeout)
        }
    }
}

fn fixed_ffmpeg_command(
    path: &Path,
    media_type: &str,
    bitrate_kbps: u16,
) -> Result<Command, AudioError> {
    if !(32..=320).contains(&bitrate_kbps) {
        return Err(AudioError::InvalidConfiguration);
    }
    let mut command = Command::new(path);
    command
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-protocol_whitelist",
            "pipe",
        ]);
    if let Some(pcm) = raw_pcm_specification(media_type)? {
        command
            .args(["-f", "s16le", "-ar"])
            .arg(pcm.sample_rate_hz.to_string())
            .arg("-ac")
            .arg(pcm.channels.to_string());
    }
    command
        .args([
            "-i",
            "pipe:0",
            "-map_metadata",
            "-1",
            "-vn",
            "-codec:a",
            "libmp3lame",
            "-b:a",
        ])
        .arg(format!("{bitrate_kbps}k"))
        .args(["-f", "mp3", "pipe:1"]);
    Ok(command)
}

#[async_trait::async_trait]
impl AudioNormalizer for FfmpegAudioNormalizer {
    async fn normalize_to_mp3(
        &self,
        artifact: AudioArtifact,
        _request_id: RequestId,
    ) -> Result<NormalizedAudio, AudioNormalizationError> {
        let exact_duration_ms = match artifact.format() {
            ProviderAudioFormat::LinearPcm(specification) => Some(pcm_duration_ms(
                artifact.bytes().len(),
                specification.sample_rate_hz(),
                specification.channels(),
                specification.bits_per_sample(),
            )?),
            ProviderAudioFormat::Mp3 | ProviderAudioFormat::Wav => None,
        };
        let media_type = match artifact.format() {
            ProviderAudioFormat::Mp3 => MP3_MEDIA_TYPE.to_owned(),
            ProviderAudioFormat::Wav => "audio/wav".to_owned(),
            ProviderAudioFormat::LinearPcm(specification) => {
                if specification.bits_per_sample() != 16 {
                    return Err(AudioNormalizationError::InvalidAudio);
                }
                format!(
                    "audio/L16;codec=pcm;rate={};channels={}",
                    specification.sample_rate_hz(),
                    specification.channels()
                )
            }
        };
        self.normalize(artifact.into_bytes(), &media_type, exact_duration_ms)
            .await
            .map_err(map_audio_error)
    }

    async fn source_duration(
        &self,
        media: &crate::domain::media::SourceMedia,
        _request_id: RequestId,
    ) -> Result<MediaDuration, AudioNormalizationError> {
        self.validate_input_size(media.bytes())
            .map_err(map_audio_error)?;
        let mut child = Command::new(&self.config.ffmpeg_path)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-protocol_whitelist",
                "pipe",
                "-i",
                "pipe:0",
                "-map",
                "0:a:0",
                "-vn",
                "-ac",
                "1",
                "-ar",
                "16000",
                "-f",
                "s16le",
                "pipe:1",
            ])
            .spawn()
            .map_err(|_| AudioNormalizationError::Unavailable)?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or(AudioNormalizationError::Unavailable)?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or(AudioNormalizationError::Unavailable)?;
        let stderr = child
            .stderr
            .take()
            .ok_or(AudioNormalizationError::Unavailable)?;
        let operation = async {
            let write = async move {
                stdin.write_all(media.bytes()).await?;
                stdin.shutdown().await
            };
            let count = async {
                let mut buffer = [0_u8; 8192];
                let mut length = 0_u64;
                loop {
                    let read = stdout
                        .read(&mut buffer)
                        .await
                        .map_err(|_| AudioNormalizationError::Failed)?;
                    if read == 0 {
                        break;
                    }
                    length = length
                        .checked_add(
                            u64::try_from(read).map_err(|_| AudioNormalizationError::Failed)?,
                        )
                        .ok_or(AudioNormalizationError::Failed)?;
                }
                Ok::<_, AudioNormalizationError>(length)
            };
            let (write, length, _stderr, status) = tokio::join!(
                write,
                count,
                read_bounded(stderr, STDERR_LIMIT),
                child.wait()
            );
            write.map_err(|_| AudioNormalizationError::InvalidAudio)?;
            if !status
                .map_err(|_| AudioNormalizationError::Failed)?
                .success()
            {
                return Err(AudioNormalizationError::InvalidAudio);
            }
            let length = length?;
            if length == 0 || length % 2 != 0 {
                return Err(AudioNormalizationError::InvalidAudio);
            }
            Ok(MediaDuration::from_millis(length.div_ceil(32)))
        };
        tokio::time::timeout(self.config.timeout, operation)
            .await
            .map_err(|_| AudioNormalizationError::Timeout)?
    }

    async fn check_ready(&self) -> Result<(), AudioNormalizationError> {
        if !(32..=320).contains(&self.config.bitrate_kbps) {
            return Err(AudioNormalizationError::Unavailable);
        }
        let mut command = fixed_ffmpeg_command(
            &self.config.ffmpeg_path,
            "audio/L16;codec=pcm;rate=8000;channels=1",
            self.config.bitrate_kbps,
        )
        .map_err(|_| AudioNormalizationError::Unavailable)?;
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| AudioNormalizationError::Unavailable)?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or(AudioNormalizationError::Unavailable)?;
        let operation = async {
            stdin
                .write_all(&[0_u8; 320])
                .await
                .map_err(|_| AudioNormalizationError::Unavailable)?;
            stdin
                .shutdown()
                .await
                .map_err(|_| AudioNormalizationError::Unavailable)?;
            // ChildStdin::shutdown does not close the pipe. FFmpeg must see EOF
            // before it can finish encoding this finite readiness sample.
            drop(stdin);
            let status = child
                .wait()
                .await
                .map_err(|_| AudioNormalizationError::Unavailable)?;
            status
                .success()
                .then_some(())
                .ok_or(AudioNormalizationError::Unavailable)
        };
        if let Ok(result) = tokio::time::timeout(self.config.timeout, operation).await {
            result
        } else {
            let _ = child.start_kill();
            Err(AudioNormalizationError::Timeout)
        }
    }
}

fn pcm_duration_ms(
    byte_length: usize,
    sample_rate_hz: u32,
    channels: u32,
    bits_per_sample: u32,
) -> Result<u64, AudioNormalizationError> {
    let bytes_per_sample = bits_per_sample
        .checked_div(8)
        .filter(|bytes| *bytes > 0 && bits_per_sample.is_multiple_of(8))
        .ok_or(AudioNormalizationError::InvalidAudio)?;
    let bytes_per_frame = channels
        .checked_mul(bytes_per_sample)
        .filter(|bytes| *bytes > 0)
        .ok_or(AudioNormalizationError::InvalidAudio)?;
    let byte_length =
        u128::try_from(byte_length).map_err(|_| AudioNormalizationError::InvalidAudio)?;
    let bytes_per_frame = u128::from(bytes_per_frame);
    if !byte_length.is_multiple_of(bytes_per_frame) || sample_rate_hz == 0 {
        return Err(AudioNormalizationError::InvalidAudio);
    }
    let sample_frames = byte_length / bytes_per_frame;
    let numerator = sample_frames
        .checked_mul(1_000)
        .ok_or(AudioNormalizationError::InvalidAudio)?;
    let rate = u128::from(sample_rate_hz);
    let milliseconds = numerator
        .checked_add(rate - 1)
        .ok_or(AudioNormalizationError::InvalidAudio)?
        / rate;
    u64::try_from(milliseconds).map_err(|_| AudioNormalizationError::InvalidAudio)
}

fn map_audio_error(error: AudioError) -> AudioNormalizationError {
    match error {
        AudioError::EmptyInput
        | AudioError::InputTooLarge
        | AudioError::OutputTooLarge
        | AudioError::InvalidMp3
        | AudioError::InvalidFormat => AudioNormalizationError::InvalidAudio,
        AudioError::ProcessUnavailable | AudioError::InvalidConfiguration => {
            AudioNormalizationError::Unavailable
        }
        AudioError::Timeout => AudioNormalizationError::Timeout,
        AudioError::ProcessFailed => AudioNormalizationError::Failed,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RawPcmSpecification {
    sample_rate_hz: u32,
    channels: u8,
}

fn raw_pcm_specification(media_type: &str) -> Result<Option<RawPcmSpecification>, AudioError> {
    let mut pieces = media_type.split(';');
    let essence = pieces.next().map(str::trim).unwrap_or_default();
    if !essence.eq_ignore_ascii_case("audio/l16") {
        return Ok(None);
    }

    let mut sample_rate_hz = None;
    let mut channels = None;
    for parameter in pieces {
        let Some((name, value)) = parameter.trim().split_once('=') else {
            return Err(AudioError::InvalidFormat);
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "rate" => {
                sample_rate_hz = value.trim().parse::<u32>().ok();
            }
            "channels" => {
                channels = value.trim().parse::<u8>().ok();
            }
            "codec" if value.trim().eq_ignore_ascii_case("pcm") => {}
            "codec" => return Err(AudioError::InvalidFormat),
            _ => {}
        }
    }
    let sample_rate_hz = sample_rate_hz.filter(|rate| (8_000..=384_000).contains(rate));
    let channels = channels.unwrap_or(1);
    if !(1..=8).contains(&channels) {
        return Err(AudioError::InvalidFormat);
    }
    Ok(Some(RawPcmSpecification {
        sample_rate_hz: sample_rate_hz.ok_or(AudioError::InvalidFormat)?,
        channels,
    }))
}

async fn read_bounded(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, AudioError> {
    let read_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut bounded = reader.take(read_limit);
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    bounded
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| AudioError::ProcessFailed)?;
    if bytes.len() > limit {
        return Err(AudioError::OutputTooLarge);
    }
    Ok(bytes)
}

fn is_mp3_media_type(media_type: &str) -> bool {
    matches!(
        media_type
            .split(';')
            .next()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("audio/mpeg" | "audio/mp3")
    )
}

/// Validates MPEG audio framing and derives duration from frame sample counts.
pub(crate) fn mp3_duration_ms(bytes: &[u8]) -> Result<u64, AudioError> {
    let mut offset = id3v2_end(bytes)?;
    let mut frame_count = 0_u64;
    let mut total_samples = 0_u128;
    let mut stream_shape = None;

    while offset < bytes.len() {
        if is_trailing_tag(&bytes[offset..]) {
            break;
        }
        let header = parse_frame_header(bytes, offset)?;
        let end = offset
            .checked_add(header.frame_len)
            .ok_or(AudioError::InvalidMp3)?;
        if end > bytes.len() {
            return Err(AudioError::InvalidMp3);
        }
        let shape = (header.sample_rate, header.samples_per_frame);
        if stream_shape.is_some_and(|expected| expected != shape) {
            return Err(AudioError::InvalidMp3);
        }
        stream_shape = Some(shape);
        total_samples = total_samples
            .checked_add(u128::from(header.samples_per_frame))
            .ok_or(AudioError::InvalidMp3)?;
        frame_count = frame_count.saturating_add(1);
        offset = end;
    }

    if frame_count == 0 {
        return Err(AudioError::InvalidMp3);
    }
    let (sample_rate, _) = stream_shape.ok_or(AudioError::InvalidMp3)?;
    let numerator = total_samples
        .checked_mul(1_000)
        .ok_or(AudioError::InvalidMp3)?;
    let sample_rate = u128::from(sample_rate);
    let millis = numerator
        .checked_add(sample_rate - 1)
        .ok_or(AudioError::InvalidMp3)?
        / sample_rate;
    u64::try_from(millis).map_err(|_| AudioError::InvalidMp3)
}

/// Performs full frame-level validation for binary provider MP3 responses.
pub(crate) fn validate_mp3_bytes(bytes: &[u8]) -> bool {
    mp3_duration_ms(bytes).is_ok()
}

#[derive(Clone, Copy, Debug)]
struct FrameHeader {
    frame_len: usize,
    sample_rate: u32,
    samples_per_frame: u16,
}

fn parse_frame_header(bytes: &[u8], offset: usize) -> Result<FrameHeader, AudioError> {
    let header_bytes = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or(AudioError::InvalidMp3)?;
    let raw = u32::from_be_bytes([
        header_bytes[0],
        header_bytes[1],
        header_bytes[2],
        header_bytes[3],
    ]);
    if raw & 0xffe0_0000 != 0xffe0_0000 {
        return Err(AudioError::InvalidMp3);
    }

    let version_bits = (raw >> 19) & 0b11;
    let layer_bits = (raw >> 17) & 0b11;
    let bitrate_index = ((raw >> 12) & 0b1111) as usize;
    let sample_rate_index = ((raw >> 10) & 0b11) as usize;
    let padding = ((raw >> 9) & 1) as usize;

    let version = match version_bits {
        0b11 => MpegVersion::One,
        0b10 => MpegVersion::Two,
        0b00 => MpegVersion::TwoPointFive,
        _ => return Err(AudioError::InvalidMp3),
    };
    let layer = match layer_bits {
        0b11 => MpegLayer::One,
        0b10 => MpegLayer::Two,
        0b01 => MpegLayer::Three,
        _ => return Err(AudioError::InvalidMp3),
    };
    if !matches!(layer, MpegLayer::Three) {
        return Err(AudioError::InvalidMp3);
    }
    if bitrate_index == 0 || bitrate_index == 15 || sample_rate_index == 3 {
        return Err(AudioError::InvalidMp3);
    }

    let sample_rate = sample_rate(version, sample_rate_index);
    let bitrate = bitrate_kbps(version, layer, bitrate_index)
        .checked_mul(1_000)
        .ok_or(AudioError::InvalidMp3)?;
    let (frame_len, samples_per_frame) = match layer {
        MpegLayer::One => (((12 * bitrate / sample_rate) as usize + padding) * 4, 384),
        MpegLayer::Two => ((144 * bitrate / sample_rate) as usize + padding, 1_152),
        MpegLayer::Three if version == MpegVersion::One => {
            ((144 * bitrate / sample_rate) as usize + padding, 1_152)
        }
        MpegLayer::Three => ((72 * bitrate / sample_rate) as usize + padding, 576),
    };
    if frame_len < 4 {
        return Err(AudioError::InvalidMp3);
    }

    Ok(FrameHeader {
        frame_len,
        sample_rate,
        samples_per_frame,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MpegVersion {
    One,
    Two,
    TwoPointFive,
}

#[derive(Clone, Copy, Debug)]
enum MpegLayer {
    One,
    Two,
    Three,
}

fn sample_rate(version: MpegVersion, index: usize) -> u32 {
    let base = [44_100, 48_000, 32_000][index];
    match version {
        MpegVersion::One => base,
        MpegVersion::Two => base / 2,
        MpegVersion::TwoPointFive => base / 4,
    }
}

fn bitrate_kbps(version: MpegVersion, layer: MpegLayer, index: usize) -> u32 {
    const V1_L1: [u32; 16] = [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 0,
    ];
    const V1_L2: [u32; 16] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0,
    ];
    const V1_L3: [u32; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    const V2_L1: [u32; 16] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 0,
    ];
    const V2_L23: [u32; 16] = [
        0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
    ];
    match (version, layer) {
        (MpegVersion::One, MpegLayer::One) => V1_L1[index],
        (MpegVersion::One, MpegLayer::Two) => V1_L2[index],
        (MpegVersion::One, MpegLayer::Three) => V1_L3[index],
        (_, MpegLayer::One) => V2_L1[index],
        (_, MpegLayer::Two | MpegLayer::Three) => V2_L23[index],
    }
}

fn id3v2_end(bytes: &[u8]) -> Result<usize, AudioError> {
    if !bytes.starts_with(b"ID3") {
        return Ok(0);
    }
    let header = bytes.get(..10).ok_or(AudioError::InvalidMp3)?;
    if header[6..10].iter().any(|byte| byte & 0x80 != 0) {
        return Err(AudioError::InvalidMp3);
    }
    let size = header[6..10]
        .iter()
        .fold(0_usize, |value, byte| (value << 7) | usize::from(*byte));
    let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
    10_usize
        .checked_add(size)
        .and_then(|value| value.checked_add(footer))
        .filter(|end| *end <= bytes.len())
        .ok_or(AudioError::InvalidMp3)
}

fn is_trailing_tag(bytes: &[u8]) -> bool {
    (bytes.len() == 128 && bytes.starts_with(b"TAG")) || bytes.starts_with(b"APETAGEX")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mpeg1_layer3_frame() -> Vec<u8> {
        // MPEG-1 Layer III, 128 kbps, 44.1 kHz, no padding: 417 bytes/frame.
        let mut frame = vec![0_u8; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        frame
    }

    #[test]
    fn validates_frames_and_calculates_duration() {
        let mut mp3 = mpeg1_layer3_frame();
        mp3.extend(mpeg1_layer3_frame());

        let duration = mp3_duration_ms(&mp3);

        assert_eq!(duration.ok(), Some(53));
    }

    #[test]
    fn mp3_duration_rounds_only_after_accumulating_all_samples() {
        let frame = mpeg1_layer3_frame();
        let mut mp3 = Vec::with_capacity(frame.len() * 1_000);
        for _ in 0..1_000 {
            mp3.extend_from_slice(&frame);
        }

        assert_eq!(mp3_duration_ms(&mp3).ok(), Some(26_123));
    }

    #[test]
    fn skips_a_bounded_id3v2_tag() {
        let mut mp3 = b"ID3\x04\x00\x00\x00\x00\x00\x03abc".to_vec();
        mp3.extend(mpeg1_layer3_frame());

        assert_eq!(mp3_duration_ms(&mp3).ok(), Some(27));
    }

    #[test]
    fn rejects_truncated_or_unframed_data() {
        assert!(matches!(
            mp3_duration_ms(b"not an mp3"),
            Err(AudioError::InvalidMp3)
        ));

        let truncated = &mpeg1_layer3_frame()[..20];
        assert!(matches!(
            mp3_duration_ms(truncated),
            Err(AudioError::InvalidMp3)
        ));
    }

    #[tokio::test]
    async fn reports_a_missing_ffmpeg_without_shelling_out() {
        let config = AudioNormalizerConfig {
            ffmpeg_path: PathBuf::from("/definitely/missing/ffmpeg"),
            max_input_bytes: 32,
            max_output_bytes: 32,
            bitrate_kbps: 96,
            timeout: Duration::from_millis(10),
        };
        let normalizer = FfmpegAudioNormalizer::new(config);

        let result = normalizer
            .normalize(Bytes::from_static(b"not-mp3"), "audio/wav", None)
            .await;

        assert!(matches!(result, Err(AudioError::ProcessUnavailable)));
    }

    #[test]
    fn raw_pcm_input_uses_explicit_demuxer_metadata_and_configured_bitrate() {
        let command = fixed_ffmpeg_command(
            Path::new("/usr/bin/ffmpeg"),
            "audio/L16;codec=pcm;rate=24000",
            96,
        );
        let Ok(command) = command else {
            return;
        };
        let arguments = command
            .as_std()
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(arguments.windows(2).any(|pair| pair == ["-f", "s16le"]));
        assert!(arguments.windows(2).any(|pair| pair == ["-ar", "24000"]));
        assert!(arguments.windows(2).any(|pair| pair == ["-ac", "1"]));
        assert!(arguments.windows(2).any(|pair| pair == ["-b:a", "96k"]));
    }

    #[test]
    fn rejects_raw_pcm_without_a_valid_rate() {
        assert!(matches!(
            raw_pcm_specification("audio/L16;codec=pcm;rate=zero"),
            Err(AudioError::InvalidFormat)
        ));
    }

    #[test]
    fn pcm_duration_uses_exact_aligned_sample_frames() {
        assert_eq!(pcm_duration_ms(48_000, 24_000, 1, 16), Ok(1_000));
        assert_eq!(
            pcm_duration_ms(48_001, 24_000, 1, 16),
            Err(AudioNormalizationError::InvalidAudio)
        );
    }

    #[tokio::test]
    async fn mp3_passthrough_enforces_the_output_bound() {
        let mut config = AudioNormalizerConfig::new("/unused/ffmpeg");
        config.max_input_bytes = 1_024;
        config.max_output_bytes = 128;
        let normalizer = FfmpegAudioNormalizer::new(config);

        let result = normalizer
            .normalize(Bytes::from(mpeg1_layer3_frame()), MP3_MEDIA_TYPE, None)
            .await;

        assert!(matches!(result, Err(AudioError::OutputTooLarge)));
    }
    #[tokio::test]
    async fn readiness_finishes_encoding_its_finite_sample() {
        use crate::application::ports::AudioNormalizer as _;
        let mut config = AudioNormalizerConfig::new("ffmpeg");
        config.timeout = Duration::from_secs(3);
        let normalizer = FfmpegAudioNormalizer::new(config);

        assert_eq!(normalizer.check_ready().await, Ok(()));
    }

    #[tokio::test]
    async fn measures_decoded_wav_and_rejects_non_audio() -> Result<(), Box<dyn std::error::Error>>
    {
        use crate::{
            application::ports::AudioNormalizer as _,
            domain::media::{MediaSizeLimit, SourceMedia, SourceMediaType},
        };
        let normalizer = FfmpegAudioNormalizer::new(AudioNormalizerConfig::new("ffmpeg"));
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&16036_u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&8000_u32.to_le_bytes());
        wav.extend_from_slice(&16000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&16000_u32.to_le_bytes());
        wav.resize(16044, 0);
        let media = SourceMedia::new(
            SourceMediaType::Wav,
            Bytes::from(wav),
            MediaSizeLimit::default(),
        )?;
        let id = RequestId::new(uuid::Uuid::new_v4())?;
        assert_eq!(
            normalizer.source_duration(&media, id).await?,
            MediaDuration::from_millis(1000)
        );
        let invalid = SourceMedia::new(
            SourceMediaType::Mpeg,
            Bytes::from_static(b"this is not audio"),
            MediaSizeLimit::default(),
        )?;
        assert_eq!(
            normalizer.source_duration(&invalid, id).await,
            Err(AudioNormalizationError::InvalidAudio)
        );
        Ok(())
    }
}
