-- IAM's cutover must pass its global collision preflight before this migration.
-- Only declared identity columns are rewritten. Signed/replay JSON, request
-- hashes, free text, ciphertext and private UUID keys are deliberately retained.
CREATE OR REPLACE FUNCTION pg_temp.schema_actor(value text) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT AS $$
BEGIN
    IF value ~ '^si:[a-z0-9_-]{3,50}$' OR value ~ '^c:[a-z0-9_-]{3,30}$' THEN RETURN value; END IF;
    IF value ~ '^[a-z0-9_-]{3,50}:[a-z0-9_-]{3,50}$' THEN RETURN 'si:' || split_part(value, ':', 1); END IF;
    IF value ~ '^[a-z0-9_-]{3,30}$' THEN RETURN 'c:' || value; END IF;
    RAISE EXCEPTION 'unmapped public actor ID in schema cutover: %', value USING ERRCODE='22023';
END $$;
CREATE OR REPLACE FUNCTION pg_temp.schema_app(value text) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT AS $$
BEGIN
    IF value ~ '^[a-z][a-z0-9_-]{0,79}$' THEN RETURN value; END IF;
    IF value ~ '^[a-z0-9_-]+>[a-z][a-z0-9_-]{0,79}$' THEN RETURN split_part(value, '>', 2); END IF;
    RAISE EXCEPTION 'unmapped application ID in schema cutover: %', value USING ERRCODE='22023';
END $$;

CREATE TEMP TABLE waveform_schema_mapping ON COMMIT DROP AS
SELECT plane_id,public_id AS old_id,pg_temp.schema_actor(public_id) AS new_id,storage_actor_id
FROM waveform_actor_keys;
CREATE UNIQUE INDEX ON waveform_schema_mapping(plane_id,new_id);
UPDATE waveform_actor_keys k SET public_id=m.new_id FROM waveform_schema_mapping m
WHERE k.plane_id=m.plane_id AND k.public_id=m.old_id;
UPDATE waveform_lifecycle SET app_id=pg_temp.schema_app(app_id);
-- All jobs, provider keys, profiles, replay records and encryption AAD retain
-- their existing storage_actor_id UUID. No ciphertext or response JSON rewrite.
