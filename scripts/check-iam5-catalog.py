#!/usr/bin/env python3
"""Check the prepared IAM 5 catalog against Waveform's storage/receiver contract."""
import argparse
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]


def check(metadata):
    storage = (ROOT / "src/control/storage.rs").read_text()
    endpoints = re.search(r"const ENDPOINTS:.*?= \[(.*?)\];", storage, re.S)
    assert endpoints, "Runtime storage endpoint list was not found"
    roots = re.findall(r'"(briefcase\.[^"]+)"', endpoints.group(1))
    expected = {("briefcase", endpoint) for endpoint in roots}
    external = metadata["app_scope"]["external"]
    actual = {(item["app_id"], item["endpoint_id"]) for item in external}
    assert actual == expected and len(external) == len(expected), (
        f"External scope must match the runtime roots: {sorted(expected)}"
    )
    assert metadata["local_app_id"] == "waveform", "Use the canonical Waveform ID"
    assert metadata["app_scope"]["iam"] == [
        "self.identity.read", "self.profile.read", "self.membership.read", "self.tags.read"
    ], "Preserve ordinary IAM scopes; OBO roots belong in external scope"

    auth = (ROOT / "src/infrastructure/auth.rs").read_text()
    receivers = dict(re.findall(r'\("(waveform\.(?:tts|stt))", "(/api/v1/(?:tts|stt))"\)', auth))
    assert len(receivers) == 2, "Runtime TTS/STT verification paths were not found"
    declared = {item["endpoint_id"]: item for item in metadata["obo_endpoints"]}
    assert set(declared) == set(receivers) and len(metadata["obo_endpoints"]) == 2, (
        "Declare both existing speech receivers; an empty list retires them"
    )
    # Existing feature contract: TTS stores and resolves output; STT resolves and reads input.
    downstream = {
        "waveform.tts": {"briefcase.uploads.reserve", "briefcase.uploads.commit", "briefcase.entries.list"},
        "waveform.stt": {"briefcase.entries.list", "briefcase.files.read"},
    }
    for endpoint, route in receivers.items():
        item = declared[endpoint]
        assert item["path"] == route, f"Wrong receiver path for {endpoint}"
        edges = item["downstream"]
        expected_edges = {("briefcase", value) for value in downstream[endpoint]}
        actual_edges = {(edge["audience"], edge["endpoint_id"]) for edge in edges}
        assert actual_edges == expected_edges and len(edges) == len(expected_edges), (
            f"Wrong downstream graph for {endpoint}"
        )
        assert actual_edges <= actual, f"Undeclared external dependency for {endpoint}"
        assert item["metadata"] == {}, "Speech does not require additional OBO metadata"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--metadata", type=Path, default=ROOT / "deploy/honeycomb/application-metadata.json")
    args = parser.parse_args()
    check(json.loads(args.metadata.read_text()))
    print("IAM 5 catalog matches four storage roots, canonical IDs, and both receiver graphs.")
