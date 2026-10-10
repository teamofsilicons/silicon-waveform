#!/usr/bin/env python3
"""A local stand-in for Silicon Briefcase's delegated routes, for Waveform's end-to-end runs.

It answers the routes Waveform calls, the way Briefcase's Silicon Accounts contract does:

  POST /api/v1/obo/uploads/reserve          scope briefcase.uploads.reserve
  PUT  /api/v1/obo/uploads/{id}/content     the reservation's upload capability only (no Authorization)
  POST /api/v1/obo/uploads/commit           scope briefcase.uploads.commit
  POST /api/v1/obo/uploads/status           scope briefcase.uploads.status
  POST /api/v1/obo/entries/list             scope briefcase.entries.list
  POST /api/v1/obo/files/read               scope briefcase.files.read

Every `Authorization: Proof sap_…` is verified with Silicon Accounts (`POST /v1/proofs/verify`) using
Briefcase's own credentials, on every call, and must be a valid User verification proof for
`receiving_app` briefcase, carrying the route's scope, from an issuer allowed for that scope. The app then
acts as the proof's account, only inside `apps/{issuing app}/` of that account's own drive (a proof never
sees the drive root or `apps/` itself). Permanent URLs are `{origin}/{owner id}/{path}`. Files live in
memory and are lost when the stub stops.

Test hooks (this stub binds to loopback only):
  GET  /_stub/health                 liveness
  GET  /_stub/calls                  every proof-authorized call: route, outcome, proof digest and id
  POST /_stub/fail                   {"count": n, "status": 503}: answer the next n API calls with that status
  POST /_stub/files                  {"owner_uuid", "owner_id", "app", "name", "content_type", "base64"}:
                                     put a file in apps/{app}/ of a drive; answers the entry
The app secret is read from the environment variable named by --app-secret-env, never from argv.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import secrets
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SCOPES = (
    "briefcase.uploads.reserve",
    "briefcase.uploads.commit",
    "briefcase.uploads.status",
    "briefcase.uploads.cancel",
    "briefcase.entries.list",
    "briefcase.files.read",
)
RANGE = re.compile(r"^bytes=(\d+)-(\d+)?$")


def now_iso() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


class Refusal(Exception):
    def __init__(self, status: int, code: str, message: str):
        super().__init__(message)
        self.status, self.code, self.message = status, code, message


class Drives:
    """In-memory drives keyed by account uuid, each with entries keyed by entry id."""

    def __init__(self, origin: str):
        self.origin = origin.rstrip("/")
        self.lock = threading.Lock()
        self.drives: dict[str, dict] = {}
        self.uploads: dict[str, dict] = {}

    def drive(self, owner_uuid: str, owner_id: str) -> dict:
        drive = self.drives.setdefault(owner_uuid, {"owner_id": owner_id, "entries": {}})
        if owner_id:
            drive["owner_id"] = owner_id
        return drive

    def ensure_folder(self, drive: dict, path: str, app: str | None) -> dict:
        found = self.by_path(drive, path)
        if found:
            return found
        parent_path, _, name = path.rpartition("/")
        parent = self.ensure_folder(drive, parent_path, app) if parent_path else None
        entry = {
            "id": str(uuid.uuid4()),
            "type": "folder",
            "name": name,
            "path": path,
            "parent_id": parent["id"] if parent else None,
            "content_type": None,
            "size": None,
            "origin_app_id": app,
            "created_at": now_iso(),
            "deleted_at": None,
        }
        drive["entries"][entry["id"]] = entry
        return entry

    @staticmethod
    def by_path(drive: dict, path: str) -> dict | None:
        for entry in drive["entries"].values():
            if entry["path"] == path and entry["deleted_at"] is None:
                return entry
        return None

    def permanent_url(self, drive: dict, path: str) -> str:
        owner = urllib.parse.quote(drive["owner_id"] or "", safe=":")
        segments = "/".join(urllib.parse.quote(part, safe="") for part in path.split("/") if part)
        return f"{self.origin}/{owner}/{segments}"

    def view(self, drive: dict, owner_uuid: str, entry: dict) -> dict:
        shown = {key: value for key, value in entry.items() if key not in ("data", "sha256")}
        shown["org_id"] = owner_uuid
        shown["permanent_url"] = self.permanent_url(drive, entry["path"])
        return shown


def visible(path: str, app: str) -> bool:
    return path == f"apps/{app}" or path.startswith(f"apps/{app}/")


class Stub:
    def __init__(self, args: argparse.Namespace, secret: str):
        self.accounts = args.accounts_api_url.rstrip("/")
        self.app_id = args.app_id
        self.auth = "Basic " + base64.b64encode(f"{args.app_id}:{secret}".encode()).decode()
        self.issuers = self.parse_issuers(args.issuers)
        self.drives = Drives(args.permanent_origin or f"http://127.0.0.1:{args.port}")
        self.calls: list[dict] = []
        self.calls_lock = threading.Lock()
        self.fail = {"count": 0, "status": 503}
        self.log_path = args.log

    @staticmethod
    def parse_issuers(spec: str) -> dict[str, set[str]]:
        allowed: dict[str, set[str]] = {}
        for item in re.split(r"[;,\s]+", spec.strip()):
            if not item:
                continue
            scope, _, apps = item.partition("=")
            if scope not in SCOPES or not apps:
                raise SystemExit(f"--issuers: '{item}' must be scope=app|app with a scope in {', '.join(SCOPES)}")
            allowed.setdefault(scope, set()).update(apps.split("|"))
        return allowed

    def record(self, entry: dict) -> None:
        entry["at"] = now_iso()
        with self.calls_lock:
            self.calls.append(entry)
        if self.log_path:
            with open(self.log_path, "a", encoding="utf-8") as log:
                log.write(json.dumps(entry) + "\n")

    def verify(self, headers, scope: str, route: str) -> tuple[dict, str]:
        """Checks the proof like Briefcase: answers (verification, issuing app) or raises Refusal."""
        auth = headers.get("Authorization", "")
        call = {"route": route, "scope": scope}
        try:
            if auth.startswith("Bearer "):
                raise Refusal(401, "proof_required", "This route takes Authorization: Proof sap_… from an app.")
            if not auth.startswith("Proof ") or not auth[6:].startswith("sap_"):
                raise Refusal(401, "proof_required", "Send Authorization: Proof sap_….")
            token = auth[6:]
            call["proof_digest"] = hashlib.sha256(token.encode()).hexdigest()[:16]
            request = urllib.request.Request(
                f"{self.accounts}/v1/proofs/verify",
                data=json.dumps({"proof_token": token}).encode(),
                headers={"authorization": self.auth, "content-type": "application/json"},
                method="POST",
            )
            try:
                with urllib.request.urlopen(request, timeout=10) as response:
                    verification = json.loads(response.read())
            except (urllib.error.URLError, TimeoutError, ValueError) as error:
                raise Refusal(503, "accounts_unavailable", f"Silicon Accounts could not verify the proof: {error}")
            call.update(
                proof_id=verification.get("proof_id"),
                valid=verification.get("valid"),
                issuing_app=(verification.get("issuing_app") or {}).get("app_id"),
                receiving_app=(verification.get("receiving_app") or {}).get("app_id"),
                user_uuid=(verification.get("user") or {}).get("uuid"),
                user_id=(verification.get("user") or {}).get("id"),
                scopes=verification.get("scopes"),
                expires_at=verification.get("expires_at"),
            )
            if not verification.get("valid"):
                raise Refusal(401, "proof_invalid", "Silicon Accounts says this proof is not valid (unknown, expired, revoked or for another app).")
            if verification.get("kind") != "user_verification":
                raise Refusal(403, "proof_kind_not_accepted", "Only User verification proofs name an account.")
            if call["receiving_app"] != self.app_id:
                raise Refusal(401, "proof_invalid", f"The proof is for {call['receiving_app']}, not {self.app_id}.")
            if scope not in (verification.get("scopes") or []):
                raise Refusal(403, "proof_scope_missing", f"The proof does not carry {scope}.")
            if call["issuing_app"] not in self.issuers.get(scope, set()):
                raise Refusal(403, "proof_issuer_not_allowed", f"{call['issuing_app']} may not use {scope}.")
            call["outcome"] = "accepted"
            return verification, call["issuing_app"]
        except Refusal as refusal:
            call["outcome"] = refusal.code
            raise
        finally:
            self.record(call)


def strict_body(raw: bytes, allowed: set[str], required: set[str] = frozenset()) -> dict:
    """Parses JSON and refuses unknown or missing fields, like Briefcase's deny_unknown_fields."""
    try:
        body = json.loads(raw or b"null")
    except ValueError:
        raise Refusal(400, "invalid_json", "The body is not JSON.")
    if not isinstance(body, dict):
        raise Refusal(400, "invalid_json", "The body must be a JSON object.")
    unknown = set(body) - allowed
    if unknown:
        raise Refusal(400, "invalid_json", f"Unknown fields: {', '.join(sorted(unknown))}.")
    missing = required - set(body)
    if missing:
        raise Refusal(400, "invalid_json", f"Missing fields: {', '.join(sorted(missing))}.")
    return body


class Handler(BaseHTTPRequestHandler):
    stub: Stub
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # keep stdout quiet; calls are recorded instead
        return

    def send_json(self, status: int, body, extra: dict | None = None) -> None:
        raw = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(raw)))
        self.send_header("cache-control", "no-store")
        for key, value in (extra or {}).items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(raw)

    def body(self) -> bytes:
        length = int(self.headers.get("content-length") or 0)
        return self.rfile.read(length) if length else b""

    def dispatch(self, method: str) -> None:
        path = urllib.parse.urlsplit(self.path).path
        stub = self.stub
        try:
            raw = self.body()
            if path.startswith("/_stub/"):
                return self.hooks(method, path, raw)
            with stub.drives.lock:
                if stub.fail["count"] > 0:
                    stub.fail["count"] -= 1
                    stub.record({"route": path, "outcome": f"injected_{stub.fail['status']}"})
                    raise Refusal(stub.fail["status"], "unavailable", "Injected failure (test hook).")
            routes = {
                ("POST", "/api/v1/obo/uploads/reserve"): self.reserve,
                ("POST", "/api/v1/obo/uploads/commit"): self.commit,
                ("POST", "/api/v1/obo/uploads/status"): self.status,
                ("POST", "/api/v1/obo/entries/list"): self.list_entries,
                ("POST", "/api/v1/obo/files/read"): self.read_file,
            }
            match = re.fullmatch(r"/api/v1/obo/uploads/([0-9a-f-]{36})/content", path)
            if method == "PUT" and match:
                return self.transfer(match.group(1), raw)
            handler = routes.get((method, path))
            if handler is None:
                raise Refusal(404, "not_found", f"{method} {path} is not a route of this stub.")
            handler(path, raw)
        except Refusal as refusal:
            self.send_json(refusal.status, {"error": {"code": refusal.code, "message": refusal.message}})

    def do_GET(self):
        self.dispatch("GET")

    def do_POST(self):
        self.dispatch("POST")

    def do_PUT(self):
        self.dispatch("PUT")

    # -- test hooks

    def hooks(self, method: str, path: str, raw: bytes) -> None:
        stub = self.stub
        if (method, path) == ("GET", "/_stub/health"):
            return self.send_json(200, {"ok": True, "app_id": stub.app_id})
        if (method, path) == ("GET", "/_stub/calls"):
            with stub.calls_lock:
                return self.send_json(200, {"calls": list(stub.calls)})
        if (method, path) == ("POST", "/_stub/fail"):
            body = strict_body(raw, {"count", "status"}, {"count"})
            with stub.drives.lock:
                stub.fail = {"count": int(body["count"]), "status": int(body.get("status", 503))}
            return self.send_json(200, stub.fail)
        if (method, path) == ("POST", "/_stub/files"):
            body = strict_body(raw, {"owner_uuid", "owner_id", "app", "name", "content_type", "base64"},
                               {"owner_uuid", "owner_id", "name", "content_type", "base64"})
            app = body.get("app") or "waveform"
            data = base64.b64decode(body["base64"])
            with stub.drives.lock:
                drive = stub.drives.drive(body["owner_uuid"], body["owner_id"])
                folder = stub.drives.ensure_folder(drive, f"apps/{app}", app)
                entry = self.publish(drive, folder, body["name"], body["content_type"], data, None)
                return self.send_json(201, stub.drives.view(drive, body["owner_uuid"], entry))
        raise Refusal(404, "not_found", f"{method} {path} is not a test hook.")

    # -- helpers

    def context(self, scope: str, route: str) -> tuple[dict, str, str, dict]:
        verification, app = self.stub.verify(self.headers, scope, route)
        user = verification["user"]
        drive = self.stub.drives.drive(user["uuid"], user.get("id", ""))
        return verification, app, user["uuid"], drive

    def publish(self, drive: dict, folder: dict, name: str, content_type: str, data: bytes, app: str | None) -> dict:
        path = f"{folder['path']}/{name}"
        existing = Drives.by_path(drive, path)
        entry = existing or {
            "id": str(uuid.uuid4()),
            "type": "file",
            "name": name,
            "path": path,
            "parent_id": folder["id"],
            "created_at": now_iso(),
            "deleted_at": None,
        }
        entry.update(content_type=content_type, size=len(data), data=data, origin_app_id=app,
                     sha256=hashlib.sha256(data).hexdigest(), updated_at=now_iso())
        drive["entries"][entry["id"]] = entry
        return entry

    @staticmethod
    def upload_status(upload: dict) -> dict:
        return {
            "operation_id": upload["operation_id"],
            "upload_id": upload["upload_id"],
            "state": upload["state"],
            "expires_at": upload["expires_at"],
            "published_entry_id": upload.get("published_entry_id"),
        }

    # -- delegated routes

    def reserve(self, route: str, raw: bytes) -> None:
        _, app, owner, drive = self.context("briefcase.uploads.reserve", route)
        body = strict_body(raw, {"operation_id", "parent_path", "name", "content_type", "size", "sha256"},
                           {"operation_id", "parent_path", "name", "content_type", "size", "sha256"})
        if not re.fullmatch(r"[0-9a-f]{64}", str(body["sha256"])):
            raise Refusal(422, "invalid_sha256", "sha256 must be 64 lowercase hex characters.")
        with self.stub.drives.lock:
            folder_path = body["parent_path"] or f"apps/{app}"
            if not visible(folder_path, app):
                raise Refusal(404, "not_found", "The destination folder is absent or not visible.")
            folder = self.stub.drives.ensure_folder(drive, f"apps/{app}", app)
            if folder_path != folder["path"]:
                folder = Drives.by_path(drive, folder_path)
                if not folder or folder["type"] != "folder":
                    raise Refusal(404, "not_found", "The destination folder is absent or not visible.")
            key = f"{owner}/{app}/{body['operation_id']}"
            upload = self.stub.drives.uploads.get(key)
            if upload is None:
                upload = {
                    "operation_id": body["operation_id"],
                    "upload_id": str(uuid.uuid4()),
                    "state": "reserved",
                    "expires_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() + 3600)),
                    "owner": owner,
                    "app": app,
                    "folder_id": folder["id"],
                    "name": body["name"],
                    "content_type": body["content_type"],
                    "size": int(body["size"]),
                    "sha256": body["sha256"],
                    "capability": "bcu_" + secrets.token_urlsafe(24),
                }
                self.stub.drives.uploads[key] = upload
                self.stub.drives.uploads[upload["upload_id"]] = upload
            status = self.upload_status(upload)
            status["capability"] = upload["capability"] if upload["state"] == "reserved" else None
        self.send_json(200, status)

    def transfer(self, upload_id: str, raw: bytes) -> None:
        if "Authorization" in self.headers:
            raise Refusal(400, "ambiguous_authentication", "The transfer takes only the upload capability.")
        with self.stub.drives.lock:
            upload = self.stub.drives.uploads.get(upload_id)
            if not upload or self.headers.get("x-briefcase-upload-capability") != upload["capability"]:
                self.stub.record({"route": "transfer", "outcome": "capability_refused"})
                raise Refusal(404, "not_found", "No reservation matches this capability.")
            if self.headers.get("x-org-id") not in (None, upload["owner"]):
                raise Refusal(400, "invalid_org_id", "X-Org-ID names another drive.")
            if len(raw) != upload["size"] or hashlib.sha256(raw).hexdigest() != upload["sha256"]:
                raise Refusal(422, "delegated_upload_manifest_mismatch", "The bytes do not match the reservation.")
            upload["data"], upload["state"] = raw, "staged"
            self.stub.record({"route": "transfer", "outcome": "staged", "upload_id": upload_id})
            self.send_json(200, self.upload_status(upload))

    def commit(self, route: str, raw: bytes) -> None:
        _, app, owner, drive = self.context("briefcase.uploads.commit", route)
        body = strict_body(raw, {"operation_id", "upload_id"}, {"operation_id", "upload_id"})
        with self.stub.drives.lock:
            upload = self.stub.drives.uploads.get(f"{owner}/{app}/{body['operation_id']}")
            if not upload or upload["upload_id"] != body["upload_id"]:
                raise Refusal(404, "not_found", "No such upload for this account and app.")
            if upload["state"] == "staged":
                folder = drive["entries"][upload["folder_id"]]
                entry = self.publish(drive, folder, upload["name"], upload["content_type"], upload["data"], app)
                upload["state"], upload["published_entry_id"] = "committed", entry["id"]
            elif upload["state"] != "committed":
                raise Refusal(409, "upload_not_staged", "Transfer the bytes before committing.")
            self.send_json(200, self.upload_status(upload))

    def status(self, route: str, raw: bytes) -> None:
        _, app, owner, _ = self.context("briefcase.uploads.status", route)
        body = strict_body(raw, {"operation_id"}, {"operation_id"})
        with self.stub.drives.lock:
            upload = self.stub.drives.uploads.get(f"{owner}/{app}/{body['operation_id']}")
            if not upload:
                raise Refusal(404, "not_found", "No such upload for this account and app.")
            self.send_json(200, self.upload_status(upload))

    def list_entries(self, route: str, raw: bytes) -> None:
        _, app, owner, drive = self.context("briefcase.entries.list", route)
        body = strict_body(raw, {"parent_id", "path", "drive", "filter", "cursor", "limit"})
        if body.get("drive") is not None:
            raise Refusal(422, "drive_not_delegated", "An app acting for an account lists only that account's drive.")
        limit = body.get("limit") or 100
        if not 1 <= int(limit) <= 100:
            raise Refusal(422, "invalid_pagination", "limit is 1 to 100.")
        with self.stub.drives.lock:
            app_folder = self.stub.drives.ensure_folder(drive, f"apps/{app}", app)
            if body.get("parent_id") and body.get("path"):
                raise Refusal(400, "ambiguous_parent", "Send parent_id or path, not both.")
            if body.get("parent_id"):
                parent = drive["entries"].get(body["parent_id"])
            elif body.get("path") is not None:
                parent = Drives.by_path(drive, body["path"])
            else:
                parent = app_folder
            if not parent or parent["type"] != "folder" or not visible(parent["path"], app):
                raise Refusal(404, "not_found", "The parent folder is absent or not visible.")
            children = sorted(
                (entry for entry in drive["entries"].values()
                 if entry["parent_id"] == parent["id"] and entry["deleted_at"] is None and visible(entry["path"], app)),
                key=lambda entry: entry["name"],
            )
            start = int(base64.urlsafe_b64decode(body["cursor"]).decode()) if body.get("cursor") else 0
            page = children[start:start + int(limit)]
            more = start + int(limit) < len(children)
            items = [self.stub.drives.view(drive, owner, entry) for entry in page]
        cursor = base64.urlsafe_b64encode(str(start + int(limit)).encode()).decode() if more else None
        self.send_json(200, {"items": items, "next_cursor": cursor})

    def read_file(self, route: str, raw: bytes) -> None:
        _, app, _, drive = self.context("briefcase.files.read", route)
        body = strict_body(raw, {"entry_id", "range", "download"}, {"entry_id"})
        with self.stub.drives.lock:
            entry = drive["entries"].get(body["entry_id"])
            if not entry or entry["type"] != "file" or entry["deleted_at"] or not visible(entry["path"], app):
                raise Refusal(404, "not_found", "No such file in this account's drive for this app.")
            data = entry["data"]
            content_type = "application/octet-stream" if body.get("download") else entry["content_type"]
        status, headers = 200, {}
        if body.get("range"):
            match = RANGE.match(body["range"])
            if not match:
                raise Refusal(416, "invalid_range", "range must be bytes=a-b.")
            first = int(match.group(1))
            last = min(int(match.group(2) or len(data) - 1), len(data) - 1)
            headers["content-range"] = f"bytes {first}-{last}/{len(data)}"
            data, status = data[first:last + 1], 206
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(data)))
        self.send_header("cache-control", "private, no-store")
        for key, value in headers.items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--accounts-api-url", required=True)
    parser.add_argument("--app-id", default="briefcase")
    parser.add_argument("--app-secret-env", default="STUB_BRIEFCASE_APP_SECRET")
    parser.add_argument("--permanent-origin", default="")
    parser.add_argument("--issuers", default=";".join(f"{scope}=waveform" for scope in SCOPES))
    parser.add_argument("--log", default="")
    args = parser.parse_args()
    secret = os.environ.get(args.app_secret_env, "")
    if not secret:
        print(f"dev_briefcase_stub: set {args.app_secret_env} to Briefcase's development app secret", file=sys.stderr)
        return 2
    Handler.stub = Stub(args, secret)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"Briefcase stub on http://127.0.0.1:{args.port} (verifying proofs at {args.accounts_api_url})", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
