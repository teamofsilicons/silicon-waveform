# syntax=docker/dockerfile:1.7

FROM rust:1.98.0-bookworm AS builder
WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations

RUN cargo build --locked --release --bin waveform-api

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates ffmpeg \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin waveform

COPY --from=builder /build/target/release/waveform-api /usr/local/bin/waveform-api

ENV WAVEFORM_ENVIRONMENT=production \
    WAVEFORM_BIND_ADDR=0.0.0.0:8080 \
    WAVEFORM_FFMPEG_PATH=/usr/bin/ffmpeg

USER waveform
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/waveform-api"]
