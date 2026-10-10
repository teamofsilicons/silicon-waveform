#!/usr/bin/env python3
"""Local checks for the development tools: the Briefcase stand-in and the dev stack's safety rails.

The stand-in (scripts/dev_briefcase_stub.py) runs in-process against a fake Silicon Accounts verify endpoint, so
these checks need no network and no running stack.

Run: python3 scripts/test_dev_tools.py
"""
import base64
import hashlib
import importlib.util
import json
import os
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent


def load(name):
    spec = importlib.util.spec_from_file_location(name, HERE / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


stub = load("dev_briefcase_stub")
dev = load("dev_accounts")

SCOPES = ["briefcase.uploads.reserve", "briefcase.uploads.commit", "briefcase.entries.list", "briefcase.files.read"]
# Proof token -> what the fake Silicon Accounts answers to POST /v1/proofs/verify.
PROOFS = {
    "sap_ada": {"valid": True, "proof_id": "p1", "kind": "user_verification", "issuing_app": {"app_id": "waveform"},
                "receiving_app": {"app_id": "briefcase"}, "user": {"uuid": "zQo", "id": "c:ada"}, "scopes": SCOPES},
    "sap_other_app": {"valid": True, "proof_id": "p2", "kind": "user_verification", "issuing_app": {"app_id": "waveform"},
                      "receiving_app": {"app_id": "remind"}, "user": {"uuid": "zQo", "id": "c:ada"}, "scopes": SCOPES},
    "sap_no_scope": {"valid": True, "proof_id": "p3", "kind": "user_verification", "issuing_app": {"app_id": "waveform"},
                     "receiving_app": {"app_id": "briefcase"}, "user": {"uuid": "zQo", "id": "c:ada"},
                     "scopes": ["briefcase.files.read"]},
    "sap_dm": {"valid": True, "proof_id": "p4", "kind": "user_verification", "issuing_app": {"app_id": "dm"},
               "receiving_app": {"app_id": "briefcase"}, "user": {"uuid": "zQo", "id": "c:ada"}, "scopes": SCOPES},
}


class FakeAccounts(BaseHTTPRequestHandler):
    def log_message(self, *args):
        return

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        expected = "Basic " + base64.b64encode(b"briefcase:sa_app_test").decode()
        answer = PROOFS.get(body["proof_token"], {"valid": False, "expires_at": None})
        if self.path != "/v1/proofs/verify" or self.headers.get("authorization") != expected:
            answer = {"valid": False, "expires_at": None}
        raw = json.dumps(answer).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)


def serve(handler):
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def call(base, method, path, body=None, headers=None, raw=None):
    data = raw if raw is not None else (None if body is None else json.dumps(body).encode())
    request = urllib.request.Request(base + path, data=data, method=method)
    for key, value in (headers or {}).items():
        request.add_header(key, value)
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            return response.status, response.read(), dict(response.headers)
    except urllib.error.HTTPError as error:
        with error:
            return error.code, error.read(), dict(error.headers)


def as_json(answer):
    return json.loads(answer[1] or b"null")


class BriefcaseStandIn(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.accounts = serve(FakeAccounts)
        args = SimpleNamespace(accounts_api_url=f"http://127.0.0.1:{cls.accounts.server_port}", app_id="briefcase",
                               issuers=";".join(f"{scope}=waveform" for scope in stub.SCOPES), port=0,
                               permanent_origin="https://files.example", log="")
        stub.Handler.stub = stub.Stub(args, "sa_app_test")
        cls.server = serve(stub.Handler)
        cls.base = f"http://127.0.0.1:{cls.server.server_port}"

    @classmethod
    def tearDownClass(cls):
        for server in (cls.server, cls.accounts):
            server.shutdown()
            server.server_close()

    def proof(self, token="sap_ada"):
        return {"authorization": f"Proof {token}", "content-type": "application/json"}

    def test_an_upload_lands_in_the_apps_folder_with_a_permanent_url_naming_the_owner(self):
        data = b"mp3-bytes"
        operation = str(uuid.uuid4())
        reserved = as_json(call(self.base, "POST", "/api/v1/obo/uploads/reserve", {
            "operation_id": operation, "parent_path": "", "name": "a b.mp3", "content_type": "audio/mpeg",
            "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}, self.proof()))
        self.assertEqual(reserved["state"], "reserved")
        refused = call(self.base, "PUT", f"/api/v1/obo/uploads/{reserved['upload_id']}/content", raw=data,
                       headers={"x-briefcase-upload-capability": reserved["capability"], "authorization": "Proof sap_ada"})
        self.assertEqual(as_json(refused)["error"]["code"], "ambiguous_authentication")
        staged = call(self.base, "PUT", f"/api/v1/obo/uploads/{reserved['upload_id']}/content", raw=data,
                      headers={"x-briefcase-upload-capability": reserved["capability"]})
        self.assertEqual(as_json(staged)["state"], "staged")
        committed = as_json(call(self.base, "POST", "/api/v1/obo/uploads/commit",
                                 {"operation_id": operation, "upload_id": reserved["upload_id"]}, self.proof()))
        self.assertEqual(committed["state"], "committed")
        listed = as_json(call(self.base, "POST", "/api/v1/obo/entries/list", {"path": "apps/waveform"}, self.proof()))
        entry = next(item for item in listed["items"] if item["id"] == committed["published_entry_id"])
        self.assertEqual(entry["permanent_url"], "https://files.example/c:ada/apps/waveform/a%20b.mp3")
        self.assertEqual((entry["origin_app_id"], entry["path"]), ("waveform", "apps/waveform/a b.mp3"))
        folder = as_json(call(self.base, "POST", "/api/v1/obo/entries/list", {"limit": 1}, self.proof()))
        self.assertTrue(folder["items"] and folder["items"][0]["parent_id"])
        status, body, headers = call(self.base, "POST", "/api/v1/obo/files/read",
                                     {"entry_id": entry["id"], "range": "bytes=0-0", "download": False}, self.proof())
        self.assertEqual((status, body, headers.get("content-range")), (206, b"m", f"bytes 0-0/{len(data)}"))

    def test_an_app_sees_only_its_own_folder(self):
        for body in ({"path": "apps"}, {"path": ""}, {"drive": "c:bob"}):
            status = call(self.base, "POST", "/api/v1/obo/entries/list", body, self.proof())[0]
            self.assertIn(status, (404, 422), body)

    def test_proofs_are_checked_like_briefcase_checks_them(self):
        cases = [("sap_unknown", 401, "proof_invalid"), ("sap_other_app", 401, "proof_invalid"),
                 ("sap_no_scope", 403, "proof_scope_missing"), ("sap_dm", 403, "proof_issuer_not_allowed")]
        for token, status, code in cases:
            answer = call(self.base, "POST", "/api/v1/obo/entries/list", {}, self.proof(token))
            self.assertEqual((answer[0], as_json(answer)["error"]["code"]), (status, code), token)
        bearer = call(self.base, "POST", "/api/v1/obo/entries/list", {}, {"authorization": "Bearer eyJ"})
        self.assertEqual(as_json(bearer)["error"]["code"], "proof_required")
        unknown = call(self.base, "POST", "/api/v1/obo/entries/list", {"surprise": 1}, self.proof())
        self.assertEqual(as_json(unknown)["error"]["code"], "invalid_json")

    def test_an_injected_failure_answers_once(self):
        call(self.base, "POST", "/_stub/fail", {"count": 1, "status": 503})
        self.assertEqual(call(self.base, "POST", "/api/v1/obo/entries/list", {}, self.proof())[0], 503)
        self.assertEqual(call(self.base, "POST", "/api/v1/obo/entries/list", {}, self.proof())[0], 200)

    def test_issuer_lists_name_known_scopes_only(self):
        with self.assertRaises(SystemExit):
            stub.Stub.parse_issuers("briefcase.everything=waveform")


class DevStackSafety(unittest.TestCase):
    def test_it_refuses_a_silicon_accounts_that_is_not_on_this_machine(self):
        with tempfile.TemporaryDirectory() as directory:
            stack = Path(directory) / "stack.json"
            stack.write_text(json.dumps({"accounts_public_url": "https://accounts.teamofsilicons.com", "apps": {}}))
            os.environ["WAVEFORM_DEV_STACK"] = str(stack)
            try:
                with self.assertRaises(SystemExit):
                    dev.Config()
            finally:
                del os.environ["WAVEFORM_DEV_STACK"]

    def test_secrets_are_written_private(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "nested" / "secret"
            dev.write_secret(path, "whsec_x")
            self.assertEqual((path.read_text(), path.stat().st_mode & 0o777), ("whsec_x", 0o600))


if __name__ == "__main__":
    unittest.main(verbosity=2)
