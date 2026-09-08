-- Shared immutable product fixture, not account or environment-owned content.
-- The application seeds the versioned MP3 from its packaged asset at startup.
CREATE TABLE waveform_speech_fixtures (
    name text PRIMARY KEY,
    mp3 bytea NOT NULL CHECK (octet_length(mp3) BETWEEN 4 AND 1048576)
);
