CREATE TABLE waveform_api_contracts (
 version text PRIMARY KEY, contract_version text NOT NULL,
 state text NOT NULL CHECK(state IN ('active','deprecated','sunset')),
 successor text REFERENCES waveform_api_contracts(version),
 deprecated_at timestamptz, last_request_at timestamptz, sunset_at timestamptz
);
INSERT INTO waveform_api_contracts(version,contract_version,state) VALUES('v1','1.0.0','active');
-- Sandbox traffic does not extend a production contract's lifetime.
CREATE TABLE waveform_test_contract_usage (
 plane_id uuid NOT NULL REFERENCES waveform_environments(id) ON DELETE CASCADE,
 version text NOT NULL REFERENCES waveform_api_contracts(version),
 last_request_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY(plane_id,version)
);
