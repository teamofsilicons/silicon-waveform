-- Control receipts survive data purges so stale retries cannot recreate a sandbox.
CREATE TABLE waveform_lifecycle (
 environment_id uuid PRIMARY KEY CHECK(environment_id <> '00000000-0000-0000-0000-000000000000'),
 org_id text NOT NULL, app_id text NOT NULL,
 environment_revision bigint NOT NULL CHECK(environment_revision > 0),
 generation bigint NOT NULL CHECK(generation > 0),
 key_version bigint NOT NULL CHECK(key_version > 0),
 state text NOT NULL CHECK(state IN ('pending','active','disabled','purged')),
 operation_id uuid NOT NULL,
 cleaned_at timestamptz,
 last_activity_at timestamptz
);
CREATE TABLE waveform_lifecycle_operations (
 environment_id uuid NOT NULL, operation_id uuid NOT NULL,
 request_hash bytea NOT NULL, receipt jsonb NOT NULL,
 PRIMARY KEY(environment_id,operation_id)
);
