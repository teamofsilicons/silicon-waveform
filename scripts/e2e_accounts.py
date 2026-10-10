#!/usr/bin/env python3
"""Waveform end to end against a Silicon Accounts development stack (the entry point is e2e-accounts.sh).

Runs eight scenarios with real Silicon Accounts tokens against the API and CLI that scripts/dev-accounts.sh starts:
  1 a Carbon on the API          2 a Silicon on the CLI          3 the device flow
  4 custodians and outsiders     5 webhooks                      6 proofs in and out
  7 discovery from a package     8 restart safety
Each check prints PASS or FAIL with the evidence; the run ends with a summary and exits 1 when any check failed.
Transcripts (no secrets) go to .mig/e2e/<run>/. Identities are new for every run (`waveform-e2e-…-<run>`).

Needs (environment, or .mig/dev-accounts.env): WAVEFORM_DEV_STACK (see dev-accounts.sh), WAVEFORM_E2E_MINT (the
stack's identity helper, mint.mts) and WAVEFORM_E2E_TSX (the tsx that runs it), WAVEFORM_E2E_ACCOUNTS_CLI (a
silicon-accounts CLI for the stack). Options: --only 1,2,…  --keep-running (leave the stack up)  --no-start.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import traceback
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import dev_accounts as dev  # noqa: E402  (the dev stack's configuration and HTTP helpers)

ROOT = dev.ROOT
CLI = Path(os.environ.get("WAVEFORM_E2E_CLI", ROOT / "target/mig/debug/waveform"))
BRIEFCASE_SCOPES = ["briefcase.uploads.reserve", "briefcase.uploads.commit", "briefcase.entries.list",
                    "briefcase.files.read"]


class Run:
    def __init__(self) -> None:
        self.config = dev.Config()
        self.n = os.environ.get("WAVEFORM_E2E_RUN") or time.strftime("%H%M%S")
        self.dir = dev.MIG / "e2e" / self.n
        self.dir.mkdir(parents=True, exist_ok=True)
        self.results: list[dict] = []
        self.scenario = "setup"
        self.mint_path = os.environ.get("WAVEFORM_E2E_MINT", "")
        self.tsx = os.environ.get("WAVEFORM_E2E_TSX", "")
        self.accounts_cli = os.environ.get("WAVEFORM_E2E_ACCOUNTS_CLI", "")
        for name, value in (("WAVEFORM_E2E_MINT", self.mint_path), ("WAVEFORM_E2E_TSX", self.tsx),
                            ("WAVEFORM_E2E_ACCOUNTS_CLI", self.accounts_cli)):
            if not value or not Path(value).is_file():
                dev.fail(f"set {name} (see scripts/e2e-accounts.sh --help)")
        self.secrets: set[str] = set()
        self.ids: dict = {}

    # -- recording

    def hide(self, *values) -> None:
        self.secrets.update(value for value in values if isinstance(value, str) and len(value) >= 8)

    def scrub(self, text: str) -> str:
        for value in self.secrets:
            text = text.replace(value, "<secret>")
        return text

    def check(self, name: str, ok, detail="") -> bool:
        ok = bool(ok)
        detail = self.scrub(detail if isinstance(detail, str) else json.dumps(detail, default=str))
        self.results.append({"scenario": self.scenario, "check": name, "ok": ok, "detail": detail[:600]})
        print(f"  {'PASS' if ok else 'FAIL'}  {name}" + (f"  [{detail[:300]}]" if detail else ""), flush=True)
        return ok

    def na(self, name: str, reason: str) -> None:
        """A rule of the suite that does not apply to Waveform: recorded with the reason, not counted."""
        self.results.append({"scenario": self.scenario, "check": name, "ok": None, "detail": reason})
        print(f"  N/A   {name}  [{reason}]", flush=True)

    def note(self, text: str) -> None:
        print(f"        {self.scrub(text)}", flush=True)

    def transcript(self, name: str, content) -> None:
        text = content if isinstance(content, str) else json.dumps(content, indent=1, default=str)
        (self.dir / name).write_text(self.scrub(text))

    # -- the stack's helpers

    def mint(self, *args: str) -> dict:
        for attempt in range(3):
            result = subprocess.run([self.tsx, self.mint_path, *args], capture_output=True, text=True, timeout=180,
                                    env={**os.environ, "NODE_NO_WARNINGS": "1"})
            if result.returncode == 0:
                body = json.loads(result.stdout)
                for key in ("access_token", "refresh_token", "stk", "slt"):
                    self.hide(body.get(key))
                for key in ("access_token", "refresh_token"):
                    self.hide((body.get("tokens") or {}).get(key))
                return body
            limited = re.search(r"too many|rate.?limit|429", result.stderr, re.I)
            if not limited or attempt == 2:
                raise RuntimeError(f"mint {args[0]} failed: {self.scrub(result.stderr.strip()[-800:])}")
            self.note("the stack rate-limited sign-in codes; waiting 45 s (the janitor resets it)")
            time.sleep(45)
        raise AssertionError("unreachable")

    def api(self, method: str, path: str, token: str | None = None, proof: str | None = None, body=None,
            idem: bool = False, headers: dict | None = None, raw: bool = False):
        """Calls Waveform; answers (status, JSON or bytes, headers)."""
        import urllib.error
        import urllib.request
        request = urllib.request.Request(self.config.api_url + path, method=method,
                                         data=None if body is None else json.dumps(body).encode())
        if body is not None:
            request.add_header("content-type", "application/json")
        if token:
            request.add_header("authorization", f"Bearer {token}")
        if proof:
            request.add_header("authorization", f"Proof {proof}")
        if idem:
            request.add_header("idempotency-key", idem if isinstance(idem, str) else f"e2e-{uuid.uuid4()}")
        for key, value in (headers or {}).items():
            request.add_header(key, value)
        try:
            with urllib.request.urlopen(request, timeout=120) as response:
                status, data, hdrs = response.status, response.read(), dict(response.headers)
        except urllib.error.HTTPError as error:
            status, data, hdrs = error.code, error.read(), dict(error.headers)
        hdrs = {key.lower(): value for key, value in hdrs.items()}
        if raw or "json" not in hdrs.get("content-type", ""):
            return status, data, hdrs
        return status, json.loads(data) if data else None, hdrs

    def accounts(self, method: str, path: str, bearer: str | None = None, app: str | None = None, body=None,
                 idem: bool = False):
        headers = {}
        if bearer:
            headers["authorization"] = f"Bearer {bearer}"
        if app:
            headers["authorization"] = self.config.basic(app)
        if idem:
            headers["idempotency-key"] = f"waveform-e2e-{uuid.uuid4()}"
        return dev.http(method, self.config.accounts_api + path, body, headers)

    def stub(self, method: str, path: str, body=None):
        return dev.http(method, self.config.briefcase_url + path, body)

    def stub_calls(self) -> list[dict]:
        status, body = self.stub("GET", "/_stub/calls")
        return body["calls"] if status == 200 else []

    def sql(self, statement: str) -> str:
        return dev.sql(self.config, statement, self.config.database)

    def home(self, name: str) -> Path:
        path = Path(tempfile.mkdtemp(prefix=f"wf-e2e-{name}-", dir=str(self.dir)))
        return path

    def cli(self, home: Path, *args: str, stdin: str | None = None, binary: Path | None = None,
            timeout: float = 120) -> tuple[int, str, str, object]:
        env = {"HOME": str(home), "SILICON_HOME": str(home), "PATH": "/usr/bin:/bin",
               "WAVEFORM_URL": self.config.api_url, "ACCOUNTS_URL": self.config.accounts_public,
               "WAVEFORM_TELEMETRY": "0"}
        result = subprocess.run([str(binary or CLI), *args], input=stdin, capture_output=True, text=True,
                                timeout=timeout, env=env)
        parsed = None
        lines = [line for line in result.stdout.strip().splitlines() if line.strip()]
        for candidate in (result.stdout, lines[-1] if lines else ""):
            try:
                parsed = json.loads(candidate)
                break
            except ValueError:
                continue
        return result.returncode, result.stdout, result.stderr, parsed


def err(body) -> dict:
    return body.get("error", {}) if isinstance(body, dict) else {}


def why(body) -> str:
    error = err(body)
    return error.get("reason") or error.get("code") or ""


def is_mp3(data: bytes) -> bool:
    """An ID3 tag or an MPEG audio frame sync at the start."""
    return data[:3] == b"ID3" or (len(data) > 1 and data[0] == 0xFF and data[1] & 0xE0 == 0xE0)


def wait_for(probe, seconds: float = 30, interval: float = 0.5):
    deadline, value = time.time() + seconds, None
    while time.time() < deadline:
        value = probe()
        if value:
            return value
        time.sleep(interval)
    return value


def signed_delivery(run: Run, event: dict, secret: str | None = None, timestamp: int | None = None,
                    signature: str | None = None):
    """POSTs an Accounts-style delivery to Waveform's webhook, signed like Silicon Accounts signs it."""
    import urllib.error
    import urllib.request
    raw = json.dumps(event, separators=(",", ":")).encode()
    timestamp = int(time.time()) if timestamp is None else timestamp
    if signature is None:
        key = (secret or dev.WEBHOOK_SECRET.read_text().strip()).encode()
        signature = "v1=" + hmac.new(key, f"{timestamp}.".encode() + raw, hashlib.sha256).hexdigest()
    request = urllib.request.Request(f"{run.config.api_url}/webhooks/accounts", data=raw, method="POST", headers={
        "content-type": "application/json", "x-accounts-timestamp": str(timestamp),
        "x-accounts-signature": signature, "x-accounts-event-id": event["event_id"],
        "x-accounts-event-type": event["type"]})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


def setup(run: Run) -> None:
    n, base = run.n, run.config.base
    redirect = f"http://localhost:{base}/auth/callback"
    c1_email, c2_email = f"waveform-e2e-c1-{n}@example.test", f"waveform-e2e-c2-{n}@example.test"
    first_party = run.mint("carbon", "--email", c1_email)
    signin = run.mint("app-signin", "--app", "waveform", "--email", c1_email, "--redirect", redirect, "--exchange")
    tokens = signin["tokens"]
    silicon = run.mint("silicon", "--custodian-email", c1_email, "--handle", f"waveform-e2e-s1-{n}")
    other = run.mint("app-signin", "--app", "waveform", "--email", c2_email, "--redirect", redirect, "--exchange")
    run.ids = {
        "c1": {"email": c1_email, "uuid": tokens["account"]["uuid"], "id": tokens["account"]["id"],
               "first_party_token": first_party["access_token"], "waveform_token": tokens["access_token"],
               "waveform_refresh": tokens["refresh_token"]},
        "s1": {"uuid": silicon["uuid"], "id": silicon["id"], "stk": silicon["stk"]},
        "c2": {"email": c2_email, "uuid": other["tokens"]["account"]["uuid"], "id": other["tokens"]["account"]["id"],
               "waveform_token": other["tokens"]["access_token"]},
    }
    c1, s1, c2 = run.ids["c1"], run.ids["s1"], run.ids["c2"]
    run.check("the Carbon signed in to Waveform through the hosted pages (email code, PKCE, code exchange)",
              tokens.get("token_type", "").lower() == "bearer" and c1["uuid"] == first_party["uuid"],
              f"{c1['id']} uuid {c1['uuid']}, scope {tokens.get('scope')}")
    run.check("a Silicon was created under that Carbon", s1["id"].startswith("si:") and silicon["custodian"]["uuid"] == c1["uuid"],
              f"{s1['id']} uuid {s1['uuid']}, custodian {silicon['custodian']['id']}")
    run.check("an unrelated Carbon signed in to Waveform", c2["uuid"] not in (c1["uuid"], s1["uuid"]), f"{c2['id']} uuid {c2['uuid']}")
    run.transcript("identities.json", {key: {k: v for k, v in value.items() if "token" not in k and k != "stk"}
                                       for key, value in run.ids.items()})


def scenario_1(run: Run) -> None:
    """A Carbon on the API: discovery, /me, provider keys, preferences, speech, history, Briefcase access."""
    c1 = run.ids["c1"]
    token = c1["waveform_token"]
    status, body, _ = run.api("GET", "/api/v1/accounts")
    run.check("GET /api/v1/accounts (public) names the app and Silicon Accounts",
              status == 200 and body["app_id"] == "waveform" and body["accounts_url"] == run.config.accounts_public,
              f"{status} {body}")
    status, body, _ = run.api("GET", "/api/v1/me", token)
    account = (body or {}).get("account", {}) if isinstance(body, dict) else {}
    run.check("GET /api/v1/me answers the Carbon's uuid, id and kind",
              status == 200 and account.get("uuid") == c1["uuid"] and account.get("id") == c1["id"]
              and account.get("kind") == "carbon", f"{status} {account}")
    run.check("…and its display name and photo from Silicon Accounts",
              bool(account.get("display_name")) and str(account.get("pfp_url") or "").startswith("http"),
              f"display_name={account.get('display_name')!r} pfp_url={account.get('pfp_url')!r}")
    status, body, _ = run.api("GET", "/api/v1/me", c1["first_party_token"])
    run.check("a first-party Silicon Accounts token is refused (issued for another app)",
              status == 401 and "audience" in why(body), f"{status} {why(body)}")

    # Provider keys: create, list, read, replace, delete.
    first, second = f"sk-e2e-{uuid.uuid4().hex}", f"sk-e2e-{uuid.uuid4().hex}"
    run.hide(first, second)
    status, body, _ = run.api("PUT", "/api/v1/provider-keys/openai", token, body={"api_key": first})
    run.check("create: PUT /api/v1/provider-keys/openai", status == 204, f"{status} {body}")
    status, body, _ = run.api("GET", "/api/v1/provider-keys", token)
    items = {item["provider"]: item for item in (body or {}).get("items", [])} if status == 200 else {}
    run.check("list: the saved key is listed as configured, never shown",
              "openai" in items and items["openai"].get("configured") is True and first not in json.dumps(body),
              f"{status} {list(items)}")
    before = items.get("openai", {}).get("updated_at")
    time.sleep(1.1)
    status, body, _ = run.api("PUT", "/api/v1/provider-keys/openai", token, body={"api_key": second})
    status2, body2, _ = run.api("GET", "/api/v1/provider-keys", token)
    after = {item["provider"]: item for item in (body2 or {}).get("items", [])}.get("openai", {}).get("updated_at")
    run.check("update: saving again replaces it (read back: a newer updated_at)",
              status == 204 and status2 == 200 and after and after != before, f"{before} -> {after}")
    status, body, _ = run.api("DELETE", "/api/v1/provider-keys/openai", token)
    status2, body2, _ = run.api("GET", "/api/v1/provider-keys", token)
    run.check("delete: DELETE /api/v1/provider-keys/openai, then it is gone",
              status == 204 and status2 == 200 and not any(i["provider"] == "openai" for i in body2.get("items", [])),
              f"{status} then {body2.get('items') if isinstance(body2, dict) else body2}")

    # Preferences.
    status, body, _ = run.api("PATCH", "/api/v1/preferences", token,
                              body={"voice_profile": "puck", "tts_order": ["elevenlabs", "gemini", "openai"]})
    status2, body2, _ = run.api("GET", "/api/v1/preferences", token)
    run.check("preferences: PATCH then GET shows the new voice and order",
              status == 200 and status2 == 200 and body2.get("voice_profile") == "puck"
              and body2.get("tts_order", [None])[0] == "elevenlabs", f"{status}/{status2} {body2}")

    # Speech, history and Briefcase.
    key = f"e2e-c1-tts-{run.n}"
    status, tts, _ = run.api("POST", "/api/v1/tts", token, body={"text": "Hello from Waveform's end-to-end run."}, idem=key)
    origin = run.config.permanent_origin.rstrip("/")
    run.check("TTS stores the MP3 in the Carbon's own Briefcase drive, under apps/waveform",
              status == 200 and tts["storage"]["status"] == "stored"
              and str(tts.get("file_url", "")).startswith(f"{origin}/{c1['id']}/apps/waveform/tts_"),
              f"{status} {tts.get('file_url') if isinstance(tts, dict) else tts}")
    c1["tts_request_id"], c1["tts_file_url"] = tts.get("request_id"), tts.get("file_url")
    status, replay, _ = run.api("POST", "/api/v1/tts", token, body={"text": "Hello from Waveform's end-to-end run."}, idem=key)
    run.check("the same Idempotency-Key answers the same speech (no second job)",
              status == 200 and replay.get("request_id") == c1["tts_request_id"], f"{replay.get('request_id')}")
    status, jobs, _ = run.api("GET", "/api/v1/jobs?limit=10", token)
    ids = [job["id"] for job in (jobs or {}).get("items", [])] if status == 200 else []
    run.check("history lists the job", c1["tts_request_id"] in ids, f"{status} {len(ids)} job(s)")
    status, job, _ = run.api("GET", f"/api/v1/jobs/{c1['tts_request_id']}", token)
    run.check("history reads the job: completed tts, content-free",
              status == 200 and job.get("status") == "completed" and job.get("operation") == "tts", f"{status} {job}")
    status, stt, _ = run.api("POST", "/api/v1/stt", token, body={"file_url": c1["tts_file_url"]}, idem=True)
    run.check("STT reads that file back from Briefcase with the proof and transcribes it",
              status == 200 and "STT worked" in str(stt.get("transcript", "")), f"{status} {why(stt) or stt.get('provider')}")
    status, target, _ = run.api("POST", "/api/v1/stt/source-target", token, body={})
    run.check("POST /api/v1/stt/source-target names the account's Waveform folder",
              status == 200 and target.get("folder_path") == "apps/waveform" and bool(target.get("folder_id")),
              f"{status} {target}")

    status, auth, _ = run.api("GET", "/api/v1/storage-authorizations", token)
    first_proof = (auth or {}).get("briefcase", {}).get("proof_id")
    run.check("Briefcase access is active, with the four Briefcase scopes",
              status == 200 and auth["briefcase"]["status"] == "active" and auth["briefcase"]["scopes"] == BRIEFCASE_SCOPES,
              f"{status} {auth.get('briefcase', {}).get('status') if isinstance(auth, dict) else auth}")
    status, auth, _ = run.api("DELETE", "/api/v1/storage-authorizations", token)
    run.check("revoke: DELETE /api/v1/storage-authorizations", status == 200 and auth["briefcase"]["status"] == "revoked",
              f"{status} {auth.get('briefcase', {}).get('revoked_reason') if isinstance(auth, dict) else auth}")
    status, held, _ = run.api("POST", "/api/v1/tts", token, body={"text": "Spoken after revoking."}, idem=True)
    c1["held_request_id"] = (held or {}).get("request_id")
    status2, auth, _ = run.api("GET", "/api/v1/storage-authorizations", token)
    run.check("after a revoke, speech is held by Waveform and the access stays revoked (as documented)",
              status == 200 and held["storage"]["status"] == "held" and held["storage"].get("reason") == "storage_not_authorized"
              and auth["briefcase"]["status"] == "revoked",
              f"storage={held.get('storage', {}).get('reason')} access={auth.get('briefcase', {}).get('status')}")
    status, auth, _ = run.api("POST", "/api/v1/storage-authorizations", token)
    run.check("approve again: POST issues a NEW proof (not the revoked one)",
              status == 201 and auth["briefcase"]["status"] == "active" and auth["briefcase"]["proof_id"] != first_proof,
              f"{status} new={auth.get('briefcase', {}).get('proof_id')} old={first_proof}")
    status, tts, _ = run.api("POST", "/api/v1/tts", token, body={"text": "Stored again after approving."}, idem=True)
    run.check("…and speech lands in Briefcase again", status == 200 and tts["storage"]["status"] == "stored",
              f"{tts.get('storage') if isinstance(tts, dict) else tts}")
    calls = [c for c in run.stub_calls() if c.get("user_uuid") == c1["uuid"] and c.get("outcome") == "accepted"]
    run.check("Briefcase saw only valid proofs issued by waveform for this Carbon, with the four scopes",
              calls and all(c["issuing_app"] == "waveform" and c["receiving_app"] == "briefcase"
                            and c["scopes"] == BRIEFCASE_SCOPES for c in calls), f"{len(calls)} accepted call(s)")


def cli_env(run: Run, home: Path) -> dict:
    return {"HOME": str(home), "SILICON_HOME": str(home), "PATH": "/usr/bin:/bin", "WAVEFORM_TELEMETRY": "0",
            "WAVEFORM_URL": run.config.api_url, "ACCOUNTS_URL": run.config.accounts_public}


def scenario_2(run: Run) -> None:
    """A Silicon on the CLI: short-lived token in, a speech journey, out."""
    s1, c1 = run.ids["s1"], run.ids["c1"]
    minted = run.mint("slt", "--silicon", s1["id"], "--stk", s1["stk"], "--app", "waveform")
    home = run.home("silicon")
    code, out, errs, signed = run.cli(home, "login", "--slt-stdin", "--json", stdin=minted["slt"])
    run.check("`waveform login --slt-stdin` in a fresh SILICON_HOME signs the Silicon in",
              code == 0 and isinstance(signed, dict) and signed.get("event") == "signed_in" and signed.get("id") == s1["id"]
              and signed.get("method") == "slt", f"exit {code} {signed if signed else errs[-300:]}")
    run.check("the short-lived token appears in no output and no file",
              minted["slt"] not in out + errs and not any(minted["slt"] in p.read_text(errors="ignore")
                                                          for p in home.rglob("*") if p.is_file()), "")
    code, out, errs, status = run.cli(home, "login", "status", "--json")
    run.check("`login status --json`: authenticated, uuid, id, kind silicon, its custodian",
              code == 0 and status.get("authenticated") is True and status.get("uuid") == s1["uuid"]
              and status.get("id") == s1["id"] and status.get("kind") == "silicon"
              and (status.get("custodian") or {}).get("uuid") == c1["uuid"], f"exit {code} {status}")
    code, out, errs, tts = run.cli(home, "tts", "Hello from a Silicon on the command line.", "--json")
    file_url = (tts or {}).get("file_url") or ""
    run.check("write: `waveform tts` stores the MP3 in the Silicon's own drive",
              code == 0 and tts["storage"]["status"] == "stored" and f"/{s1['id']}/apps/waveform/" in file_url,
              f"exit {code} {file_url or errs[-300:]}")
    s1["tts_request_id"], s1["tts_file_url"] = (tts or {}).get("request_id"), file_url
    code, out, errs, jobs = run.cli(home, "jobs", "--limit", "5", "--json")
    run.check("read: `waveform jobs` lists it",
              code == 0 and any(job["id"] == s1["tts_request_id"] for job in (jobs or {}).get("items", [])),
              f"exit {code}")
    code, out, errs, prefs = run.cli(home, "preferences", "--voice-profile", "sulafat", "--stt-order", "deepgram", "--json")
    code2, out2, errs2, prefs2 = run.cli(home, "preferences", "--json")
    run.check("write+read: `waveform preferences` sets and shows the default voice and STT order",
              code == 0 and code2 == 0 and prefs2.get("voice_profile") == "sulafat" and prefs2.get("stt_order", [None])[0] == "deepgram",
              f"exit {code}/{code2} {prefs2}")
    key = f"gm-e2e-{uuid.uuid4().hex}"
    run.hide(key)
    code, out, errs, saved = run.cli(home, "provider-keys", "set", "gemini", "--key-file", "-", "--json", stdin=key)
    code2, out2, errs2, listed = run.cli(home, "provider-keys", "--json")
    run.check("write+read: `waveform provider-keys set` then list (the key is never printed)",
              code == 0 and code2 == 0 and any(item["provider"] == "gemini" for item in (listed or {}).get("items", []))
              and key not in out + out2 + errs + errs2, f"exit {code}/{code2}")
    code, out, errs, stt = run.cli(home, "stt", file_url, "--json")
    run.check("read: `waveform stt` transcribes the Silicon's own file from Briefcase",
              code == 0 and "STT worked" in str((stt or {}).get("transcript", "")), f"exit {code} {errs[-300:]}")
    code, out, errs, folder = run.cli(home, "stt-folder", "--json")
    run.check("read: `waveform stt-folder` names the Silicon's Waveform folder",
              code == 0 and (folder or {}).get("folder_path") == "apps/waveform", f"exit {code} {folder or errs[-300:]}")
    code, out, errs, revoked = run.cli(home, "storage", "revoke", "--json")
    code2, out2, errs2, held = run.cli(home, "tts", "Held while access is revoked.", "--json")
    run.check("`waveform storage revoke`, then speech is held by Waveform (storage_not_authorized)",
              code == 0 and (revoked or {}).get("briefcase", {}).get("status") == "revoked" and code2 == 0
              and (held or {}).get("storage", {}).get("reason") == "storage_not_authorized",
              f"exit {code}/{code2} {(held or {}).get('storage')}")
    target = home / "held.mp3"
    code, out, errs, saved = run.cli(home, "audio", str((held or {}).get("request_id")), "--output", str(target), "--json")
    run.check("`waveform audio` downloads the held MP3", code == 0 and target.is_file() and target.stat().st_size > 1000
              and is_mp3(target.read_bytes()), f"exit {code} {target.stat().st_size if target.is_file() else 0} bytes")
    code, out, errs, approved = run.cli(home, "storage", "approve", "--json")
    code2, out2, errs2, tts = run.cli(home, "tts", "Stored again after approving.", "--json")
    run.check("`waveform storage approve` gives access back, and speech lands in Briefcase again",
              code == 0 and (approved or {}).get("briefcase", {}).get("status") == "active" and code2 == 0
              and (tts or {}).get("storage", {}).get("status") == "stored", f"exit {code}/{code2} {(tts or {}).get('storage')}")
    code, out, errs, bye = run.cli(home, "logout", "--json")
    run.check("`waveform logout` ends the sign-in at Silicon Accounts", code == 0 and bye.get("signed_out") is True
              and bye.get("revoked") is True, f"exit {code} {bye}")
    code, out, errs, status = run.cli(home, "login", "status", "--json")
    run.check("`login status --json` then says {\"authenticated\":false} (exit 0)",
              code == 0 and status == {"authenticated": False}, f"exit {code} {status}")


def scenario_3(run: Run) -> None:
    """The device flow: a Carbon approves a code, then uses the CLI."""
    c1, s1 = run.ids["c1"], run.ids["s1"]
    home = run.home("carbon")
    process = subprocess.Popen([str(CLI), "login", "--json", "--label", f"waveform e2e {run.n}"], text=True,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=cli_env(run, home))
    first = json.loads(process.stdout.readline() or "{}")
    code = first.get("user_code", "")
    run.check("`waveform login` prints a code and the approval page", first.get("event") == "device_code" and code
              and str(first.get("verification_uri", "")).startswith(run.config.accounts_public),
              f"{first.get('verification_uri')} {code}")
    approved = run.mint("approve", "--email", c1["email"], "--code", code)
    try:
        out, errs = process.communicate(timeout=90)
    except subprocess.TimeoutExpired:
        process.kill()
        out, errs = process.communicate()
    last = json.loads(out.strip().splitlines()[-1]) if out.strip() else {}
    run.check("approving it as the Carbon finishes the sign-in by itself",
              approved.get("status") in (200, 204) and process.returncode == 0 and last.get("event") == "signed_in"
              and last.get("id") == c1["id"] and last.get("method") == "device", f"exit {process.returncode} {last}")
    exit_code, out, errs, me = run.cli(home, "me", "--json")
    run.check("one command: `waveform me` answers the Carbon",
              exit_code == 0 and (me or {}).get("account", {}).get("uuid") == c1["uuid"], f"exit {exit_code}")
    exit_code, out, errs, silicons = run.cli(home, "silicons", "--json")
    run.check("`waveform silicons` lists the Silicon it looks after",
              exit_code == 0 and any(item["uuid"] == s1["uuid"] for item in (silicons or {}).get("items", [])),
              f"exit {exit_code} {silicons}")
    c1["cli_home"] = str(home)


def silicon_token(run: Run, silicon_id: str) -> str:
    """A Waveform access token for the Silicon, exchanged server side (what an app's server does with an SLT)."""
    s1 = run.ids["s1"]
    minted = run.mint("slt", "--silicon", silicon_id, "--stk", s1["stk"], "--app", "waveform")
    return run.mint("app-token", "--app", "waveform", "--slt", minted["slt"])["access_token"]


def scenario_4(run: Run) -> None:
    """Custodians manage their Silicons' Waveform data; outsiders see nothing; nobody speaks as another account."""
    c1, s1, c2 = run.ids["c1"], run.ids["s1"], run.ids["c2"]
    t1, t2, sid = c1["waveform_token"], c2["waveform_token"], s1["id"]
    status, jobs, _ = run.api("GET", f"/api/v1/jobs?account={sid}", t1)
    run.check("custodian reads the Silicon's history (?account=si:…)",
              status == 200 and any(job["id"] == s1.get("tts_request_id") for job in jobs.get("items", [])),
              f"{status} {len((jobs or {}).get('items', [])) if isinstance(jobs, dict) else jobs}")
    status, job, _ = run.api("GET", f"/api/v1/jobs/{s1.get('tts_request_id')}?account={sid}", t1)
    run.check("custodian reads one of the Silicon's jobs", status == 200 and job.get("operation") == "tts", f"{status}")
    status, prefs, _ = run.api("PATCH", f"/api/v1/preferences?account={sid}", t1,
                               body={"tts_order": ["openai", "gemini", "elevenlabs"]})
    run.check("custodian changes the Silicon's preferences",
              status == 200 and prefs["account"]["uuid"] == s1["uuid"] and prefs["tts_order"][0] == "openai", f"{status}")
    key = f"el-e2e-{uuid.uuid4().hex}"
    run.hide(key)
    status, _, _ = run.api("PUT", f"/api/v1/provider-keys/elevenlabs?account={sid}", t1, body={"api_key": key})
    status2, listed, _ = run.api("GET", f"/api/v1/provider-keys?account={sid}", t1)
    status3, _, _ = run.api("DELETE", f"/api/v1/provider-keys/elevenlabs?account={sid}", t1)
    run.check("custodian saves, lists and deletes a provider key for the Silicon (write-only)",
              status == 204 and status2 == 200 and any(i["provider"] == "elevenlabs" for i in listed["items"])
              and key not in json.dumps(listed) and status3 == 204, f"{status}/{status2}/{status3}")
    status, auth, _ = run.api("GET", f"/api/v1/storage-authorizations?account={sid}", t1)
    run.check("custodian sees the Silicon's Briefcase access", status == 200 and auth["account"]["uuid"] == s1["uuid"],
              f"{status} {auth.get('briefcase', {}).get('status') if isinstance(auth, dict) else auth}")
    status, body, _ = run.api("POST", f"/api/v1/tts?account={sid}", t1, body={"text": "Not as the Silicon."}, idem=True)
    run.check("custodian cannot run speech as the Silicon (?account on TTS is refused, not ignored)",
              status == 400 and why(body) == "speech_runs_as_yourself", f"{status} {why(body) or (body or {}).get('request_id')}")
    status, body, _ = run.api("POST", "/api/v1/storage-authorizations?account=" + sid, t1)
    run.check("custodian cannot give Briefcase access for the Silicon", status == 403, f"{status} {why(body)}")

    s1["waveform_token"] = silicon_token(run, sid)
    status, body, _ = run.api("GET", f"/api/v1/preferences?account={c1['id']}", s1["waveform_token"])
    run.check("the Silicon cannot select its custodian", status == 403 and why(body) == "not_custodian", f"{status} {why(body)}")

    for name, method, path, payload, expected in (
        ("an outsider Carbon cannot list the Silicon's history", "GET", f"/api/v1/jobs?account={sid}", None, 403),
        ("…nor read its job by id", "GET", f"/api/v1/jobs/{s1.get('tts_request_id')}", None, 404),
        ("…nor read its preferences", "GET", f"/api/v1/preferences?account={sid}", None, 403),
        ("…nor save a provider key for it", "PUT", f"/api/v1/provider-keys/openai?account={sid}", {"api_key": "sk-nope-123456"}, 403),
        ("…nor revoke its Briefcase access", "DELETE", f"/api/v1/storage-authorizations?account={sid}", None, 403),
        ("…nor manage another Carbon", "GET", f"/api/v1/jobs?account={c1['id']}", None, 403),
        ("…nor read that Carbon's job", "GET", f"/api/v1/jobs/{c1.get('tts_request_id')}", None, 404),
        ("…nor download that Carbon's held audio", "GET", f"/api/v1/audio/{c1.get('held_request_id')}", None, 404),
    ):
        status, body, _ = run.api(method, path, t2, body=payload)
        run.check(name, status == expected, f"{status} {why(body) if isinstance(body, dict) else ''}")
    status, silicons, _ = run.api("GET", "/api/v1/silicons", t2)
    run.check("…and lists no Silicons", status == 200 and silicons.get("items") == [], f"{status} {silicons}")
    status, target, _ = run.api("POST", "/api/v1/stt/source-target", t2, body={})
    run.check("known limit: an empty Waveform folder is not shown to Waveform (Briefcase lists an app's folder only "
              "through its files), answered exactly", status == 404 and why(target) == "briefcase_folder_not_found"
              and "waveform tts" in err(target).get("hint", ""), f"{status} {why(target)}")
    status, tts, _ = run.api("POST", "/api/v1/tts", t2, body={"text": "The second Carbon speaks for itself."}, idem=True)
    run.check("the outsider uses Waveform for itself (its own drive)",
              status == 200 and f"/{c2['id']}/apps/waveform/" in str(tts.get("file_url")), f"{tts.get('file_url')}")
    c2["tts_file_url"] = tts.get("file_url")
    status, target, _ = run.api("POST", "/api/v1/stt/source-target", t2, body={})
    run.check("…and once a file is in it, source-target names the folder", status == 200
              and target.get("folder_path") == "apps/waveform" and bool(target.get("folder_id")), f"{status} {target}")
    run.na("sharing with another Carbon by c: id, and unsharing",
           "Waveform has nothing to share: history, settings, keys and held audio are private to the account "
           "(and visible to a Silicon's custodian); stored audio is the account's Briefcase file, shared in Briefcase "
           "(docs/migration/decisions.md, No Teams)")
    run.na("Silicons are not open to the world",
           "Waveform has no inbound action (no messages, invitations or shares), so nothing from outside a Silicon's "
           "circle can reach it through Waveform; the outsider checks above show it cannot reach the Silicon's data")


def event_row(run: Run, event_type: str, account: str) -> tuple[str, str]:
    row = run.sql(f"SELECT event_id || '|' || outcome FROM waveform_accounts_webhook_events WHERE event_type = "
                  f"'{event_type}' AND account_uuid = '{account}' ORDER BY received_at DESC LIMIT 1")
    return tuple(row.split("|", 1)) if row else ("", "")


def accounts_cli(run: Run, home: Path, *args: str, stdin: str | None = None) -> tuple[int, str, str]:
    result = subprocess.run([run.accounts_cli, "--url", run.config.accounts_public, "--home", str(home), *args],
                            input=stdin, capture_output=True, text=True, timeout=120,
                            env={"PATH": "/usr/bin:/bin", "HOME": str(home)})
    return result.returncode, result.stdout, result.stderr


def scenario_5(run: Run) -> None:
    """Webhooks: an id change, access removed, forged and replayed deliveries, coming back."""
    c1, s1 = run.ids["c1"], run.ids["s1"]
    t1 = c1["waveform_token"]
    new_id = f"si:waveform-e2e-s1r-{run.n}"
    status, body = run.accounts("POST", f"/v1/me/silicons/{s1['uuid']}/id", bearer=c1["first_party_token"], body={"id": new_id})
    run.check("the custodian changes the Silicon's id at Silicon Accounts", status == 200, f"{status} {why(body)}")
    shown = wait_for(lambda: any(i["uuid"] == s1["uuid"] and i["id"] == new_id
                                 for i in (run.api("GET", "/api/v1/silicons", t1)[1] or {}).get("items", [])), 40)
    event_id, outcome = event_row(run, "account.id_changed", s1["uuid"])
    run.check("Waveform shows the new id (account.id_changed, signed and applied)", shown and outcome == "id_updated",
              f"event {event_id} outcome {outcome}")
    s1["old_id"], s1["id"], s1["id_event"] = s1["id"], new_id, event_id
    status, deliveries = run.accounts("GET", "/v1/apps/waveform/webhook/deliveries?limit=100", app="waveform")
    delivery = next((d["id"] for d in (deliveries or {}).get("items", []) if d.get("event_id") == event_id), None)
    status, replay = run.accounts("POST", "/v1/apps/waveform/webhook/replay", app="waveform",
                                  body={"delivery_ids": [delivery]}, idem=True)
    detail = wait_for(lambda: (lambda d: d if d.get("manual_replays", 0) >= 1 and d.get("status") == "delivered"
                                         and len(d.get("attempts", [])) >= 2 else None)(
        run.accounts("GET", f"/v1/apps/waveform/webhook/deliveries/{delivery}", app="waveform")[1] or {}), 40)
    rows = run.sql(f"SELECT count(*) || '|' || min(outcome) FROM waveform_accounts_webhook_events WHERE event_id = '{event_id}'")
    run.check("Silicon Accounts replays the id change (same event_id): delivered again, recorded and applied once",
              status == 200 and delivery in (replay or {}).get("replayed", []) and detail and rows == "1|id_updated",
              f"replay {status} {(replay or {}).get('replayed')}, attempts {len((detail or {}).get('attempts', []))}, rows {rows}")
    status, me, _ = run.api("GET", "/api/v1/me", s1["waveform_token"])
    run.check("the Silicon's token issued before the change shows the new id too",
              status == 200 and me["account"]["id"] == new_id, f"{status} {(me or {}).get('account', {}).get('id')}")

    # The Silicon signs in again the documented way, and speaks.
    accounts_home, home = run.home("accounts"), run.home("silicon-again")
    code, out, errs = accounts_cli(run, accounts_home, "login", "--silicon", new_id, "--stk-stdin", "--json", stdin=s1["stk"])
    run.check("the Silicon signs in to Silicon Accounts with its new id (silicon-accounts login --silicon)",
              code == 0, f"exit {code} {run.scrub(errs[-300:])}")
    code, slt, errs = accounts_cli(run, accounts_home, "login", "--app", "waveform", "-q")
    run.hide(slt.strip())
    code2, out2, errs2, signed = run.cli(home, "login", "--slt-stdin", "--json", stdin=slt.strip())
    run.check("`silicon-accounts login --app waveform -q | waveform login --slt-stdin`",
              code == 0 and code2 == 0 and (signed or {}).get("id") == new_id, f"exit {code}/{code2} {signed or errs2[-300:]}")
    code, out, errs, tts = run.cli(home, "tts", "Speaking again after a new sign-in.", "--json")
    run.check("after its earlier sign-in ended, the Silicon's speech still lands in Briefcase (Waveform replaces the proof that ended with it)",
              code == 0 and (tts or {}).get("storage", {}).get("status") == "stored",
              f"exit {code} {(tts or {}).get('storage') or errs[-300:]}")

    # The Silicon removes Waveform's access.
    old = s1["waveform_token"]
    status, _, _ = run.api("GET", "/api/v1/me", old)
    code, out, errs = accounts_cli(run, accounts_home, "apps", "remove", "waveform", "--json")
    run.check("the Silicon removes Waveform's access (silicon-accounts apps remove waveform)", status == 200 and code == 0,
              f"before {status}, exit {code} {run.scrub(errs[-300:])}")
    refused = wait_for(lambda: (lambda r: r if r[0] == 401 else None)(run.api("GET", "/api/v1/me", old)), 40)
    event_id, outcome = event_row(run, "membership.access_removed", s1["uuid"])
    run.check("its earlier Waveform token is refused afterwards (membership.access_removed applied)",
              refused and why(refused[1]) == "sign_in_ended" and outcome == "sign_ins_ended",
              f"{refused[0] if refused else 'still accepted'} {why(refused[1]) if refused else ''}, event outcome {outcome}")
    code, out, errs, me = run.cli(home, "me", "--json")
    code2, out2, errs2, status_json = run.cli(home, "login", "status", "--json")
    run.check("…and the CLI's sign-in too: `waveform me` refused, `login status` not authenticated",
              code == 3 and code2 == 0 and status_json.get("authenticated") is False, f"exit {code} {status_json}")
    status, auth, _ = run.api("GET", f"/api/v1/storage-authorizations?account={new_id}", t1)
    run.check("Waveform's Briefcase access for the Silicon ended with it",
              status == 200 and auth["briefcase"]["status"] == "revoked", f"{status} {auth.get('briefcase') if isinstance(auth, dict) else auth}")

    # Forged and replayed deliveries.
    forged = {"event_id": str(uuid.uuid4()), "type": "account.id_changed", "app_id": "waveform", "silicon": None,
              "occurred_at": time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime()),
              "data": {"uuid": s1["uuid"], "membership_id": f"waveform:{s1['uuid']}", "kind": "silicon",
                       "old_id": new_id, "new_id": f"si:forged-{run.n}"}}
    for name, kwargs in (("a delivery signed with another secret is refused", {"secret": "whsec_" + "A" * 43}),
                         ("a delivery with a stale timestamp is refused", {"timestamp": int(time.time()) - 600}),
                         ("an unsigned delivery is refused", {"signature": ""})):
        status, body = signed_delivery(run, forged, **kwargs)
        run.check(name, status == 401 and why(body) == "invalid_webhook_signature", f"{status} {why(body)}")
    status, silicons, _ = run.api("GET", "/api/v1/silicons", t1)
    run.check("…and changed nothing", any(i["id"] == new_id for i in silicons["items"]), f"{[i['id'] for i in silicons['items']]}")
    replayed = dict(forged, event_id=s1["id_event"])
    replayed["data"] = dict(forged["data"], new_id=f"si:replayed-{run.n}")
    status, body = signed_delivery(run, replayed)
    status2, silicons, _ = run.api("GET", "/api/v1/silicons", t1)
    run.check("a correctly signed delivery reusing a processed event_id is ignored",
              status == 200 and body.get("duplicate") is True and any(i["id"] == new_id for i in silicons["items"]),
              f"{status} {body}")
    time.sleep(1.2)
    status, me, _ = run.api("GET", "/api/v1/me", silicon_token(run, new_id))
    run.check("signing in again after removing access works (a newer sign-in is accepted)",
              status == 200 and me["account"]["id"] == new_id, f"{status} {why(me)}")
    transfer_custodian(run)


def transfer_custodian(run: Run) -> None:
    """silicon.custodian_changed: the Silicon moves to the second Carbon, and so do the custodian's powers."""
    c1, s1, c2 = run.ids["c1"], run.ids["s1"], run.ids["c2"]
    c2["first_party_token"] = run.mint("carbon", "--email", c2["email"])["access_token"]
    status, request = run.accounts("POST", f"/v1/me/silicons/{s1['uuid']}/transfer", bearer=c1["first_party_token"],
                                   body={"to": c2["id"]})
    request_id = ((request or {}).get("request") or {}).get("id")
    status2, accepted = run.accounts("POST", f"/v1/me/custodian-requests/{request_id}/accept", bearer=c2["first_party_token"])
    run.check("the custodian transfers the Silicon to the second Carbon, who accepts", status == 201 and status2 == 204,
              f"transfer {status} {why(request)}, accept {status2} {why(accepted)}")
    applied = wait_for(lambda: event_row(run, "silicon.custodian_changed", s1["uuid"])[1] == "custodian_updated", 40)
    by_uuid = run.api("GET", f"/api/v1/jobs?account={s1['uuid']}", c1["waveform_token"])
    by_id = run.api("GET", f"/api/v1/jobs?account={s1['id']}", c1["waveform_token"])
    run.check("silicon.custodian_changed applied: the former custodian can no longer manage the Silicon (by uuid or id)",
              applied and by_uuid[0] == 403 and by_id[0] == 403,
              f"event applied {bool(applied)}, by uuid {by_uuid[0]} {why(by_uuid[1])}, by id {by_id[0]}")
    status, silicons, _ = run.api("GET", "/api/v1/silicons", c2["waveform_token"])
    status2, jobs, _ = run.api("GET", f"/api/v1/jobs?account={s1['id']}", c2["waveform_token"])
    run.check("…and the new custodian lists it and reads its history",
              status == 200 and any(i["uuid"] == s1["uuid"] for i in silicons["items"]) and status2 == 200
              and jobs["account"]["uuid"] == s1["uuid"], f"{status}/{status2}")
    status, me, _ = run.api("GET", "/api/v1/me", silicon_token(run, s1["id"]))
    run.check("…and the Silicon's /me names its new custodian",
              status == 200 and (me["account"].get("custodian") or {}).get("uuid") == c2["uuid"],
              f"{(me or {}).get('account', {}).get('custodian') if isinstance(me, dict) else me}")


def issue_proof(run: Run, app: str, subject: str, receiving: str, scopes: list[str]) -> dict:
    status, proof = run.accounts("POST", "/v1/proofs/user-verification", app=app, idem=True,
                                 body={"subject_token": subject, "receiving_app": receiving, "scopes": scopes})
    if status not in (200, 201):
        raise RuntimeError(f"{app} could not issue a proof for {receiving}: {status} {why(proof)}")
    run.hide(proof.get("proof_token"), proof.get("proof_refresh_token"))
    return proof


def storage_key(run: Run, account_uuid: str) -> str:
    return run.sql(f"SELECT storage_actor_id FROM waveform_account_keys WHERE account_uuid = '{account_uuid}'")


def scenario_6(run: Run) -> None:
    """Proofs: Waveform as a receiving app (from interface and dm) and as an issuing app (to Briefcase)."""
    c1, c2 = run.ids["c1"], run.ids["c2"]
    t1 = c1["waveform_token"]
    fake = run.config.fake_apps
    interface = run.mint("app-signin", "--app", "interface", "--email", c1["email"], "--redirect",
                         f"{fake}/interface/callback", "--exchange")["tokens"]["access_token"]
    both = issue_proof(run, "interface", interface, "waveform", ["waveform.tts", "waveform.stt"])
    status, tts, _ = run.api("POST", "/api/v1/tts", proof=both["proof_token"], body={"text": "Spoken for an app."}, idem=True)
    run.check("receiver: an interface proof runs TTS for the Carbon (audio held: delegated call)",
              status == 200 and tts["storage"].get("reason") == "delegated_call" and tts.get("audio_url"),
              f"{status} {(tts or {}).get('storage') if isinstance(tts, dict) else why(tts)}")
    rid = (tts or {}).get("request_id")
    status, audio, headers = run.api("GET", f"/api/v1/audio/{rid}", proof=both["proof_token"], raw=True)
    run.check("…the app downloads that MP3 with the same proof",
              status == 200 and headers.get("content-type", "").startswith("audio/mpeg") and len(audio) > 1000,
              f"{status} {len(audio)} bytes")
    proof_file = run.dir / "interface.proof"
    fd = os.open(proof_file, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as handle:
        handle.write(both["proof_token"])
    empty = run.home("proof-cli")
    code, out, errs, spoken = run.cli(empty, "tts", "Spoken by an app's CLI.", "--proof-file", str(proof_file), "--json")
    run.check("`waveform tts --proof-file` acts for the account with the app's proof (nothing saved in the home)",
              code == 0 and (spoken or {}).get("storage", {}).get("reason") == "delegated_call" and not any(empty.iterdir()),
              f"exit {code} {(spoken or {}).get('storage') or errs[-200:]}")
    proof_file.unlink()
    status, jobs, _ = run.api("GET", "/api/v1/jobs?limit=20", t1)
    run.check("…and the Carbon's history records the issuing app",
              any(job["id"] == rid and job.get("issuing_app") == "interface" for job in jobs.get("items", [])), f"{status}")
    status, stt, _ = run.api("POST", "/api/v1/stt", proof=both["proof_token"], body={"file_url": c1["tts_file_url"]}, idem=True)
    run.check("receiver: STT for an app uses the Briefcase access the account gave Waveform",
              status == 200 and "STT worked" in str(stt.get("transcript")), f"{status} {why(stt)}")
    other = issue_proof(run, "interface", interface, "briefcase", ["briefcase.files.read"])
    status, body, _ = run.api("POST", "/api/v1/tts", proof=other["proof_token"], body={"text": "x"}, idem=True)
    run.check("a proof for another receiving app is refused", status == 401 and why(body) == "invalid_proof", f"{status} {why(body)}")
    stt_only = issue_proof(run, "interface", interface, "waveform", ["waveform.stt"])
    status, body, _ = run.api("POST", "/api/v1/tts", proof=stt_only["proof_token"], body={"text": "x"}, idem=True)
    run.check("a proof without waveform.tts is refused on TTS", status == 403 and why(body) == "proof_scope_missing", f"{status} {why(body)}")
    status, body, _ = run.api("GET", "/api/v1/me", proof=stt_only["proof_token"])
    run.check("a proof is refused on a route that takes only the account's own token",
              status in (401, 403) and why(body) == "proof_not_accepted", f"{status} {why(body)}")
    status, revoked = run.accounts("POST", "/v1/proofs/revoke", app="interface", body={"proof_id": both["proof_id"]})
    status2, body, _ = run.api("POST", "/api/v1/tts", proof=both["proof_token"], body={"text": "x"}, idem=True)
    run.check("a revoked proof is refused", status == 204 and status2 == 401 and why(body) == "invalid_proof",
              f"revoke {status}, then {status2} {why(body)}")
    dm = run.mint("app-signin", "--app", "dm", "--email", c2["email"], "--redirect", f"{fake}/dm/callback",
                  "--exchange")["tokens"]["access_token"]
    from_dm = issue_proof(run, "dm", dm, "waveform", ["waveform.tts", "waveform.stt"])
    status, body, _ = run.api("POST", "/api/v1/tts", proof=from_dm["proof_token"], body={"text": "x"}, idem=True)
    run.check("an issuer not allowed for waveform.tts (dm) is refused", status == 403 and why(body) == "proof_issuer_not_allowed",
              f"{status} {why(body)}")
    status, stt, _ = run.api("POST", "/api/v1/stt", proof=from_dm["proof_token"], body={"file_url": c2["tts_file_url"]}, idem=True)
    run.check("…while dm is allowed for waveform.stt (transcribes the second Carbon's own file)",
              status == 200 and "STT worked" in str(stt.get("transcript")), f"{status} {why(stt)}")

    # Waveform as the issuing app.
    calls = run.stub_calls()
    accepted = [c for c in calls if c.get("outcome") == "accepted"]
    refused = [c for c in calls if c.get("route", "").startswith("/api/v1/obo") and c.get("outcome") not in ("accepted",)
               and not str(c.get("outcome")).startswith("injected")]
    run.check("issuer: every proof Waveform sent Briefcase verified at Silicon Accounts with Briefcase's credentials",
              accepted and all(c["valid"] and c["issuing_app"] == "waveform" and c["receiving_app"] == "briefcase"
                               and c["scopes"] == BRIEFCASE_SCOPES for c in accepted),
              f"{len(accepted)} accepted, {len(refused)} refused so far: {[c.get('outcome') for c in refused]}")
    key = storage_key(run, c1["uuid"])
    before = run.sql(f"SELECT proof_id || '|' || token_expires_at FROM waveform_storage_proofs WHERE storage_actor_id = '{key}'")
    digests = {c.get("proof_digest") for c in calls if c.get("user_uuid") == c1["uuid"]}
    run.sql(f"UPDATE waveform_storage_proofs SET token_expires_at = now() + interval '30 seconds' WHERE storage_actor_id = '{key}'")
    status, tts, _ = run.api("POST", "/api/v1/tts", t1, body={"text": "Spoken with a refreshed proof."}, idem=True)
    after = run.sql(f"SELECT proof_id || '|' || token_expires_at || '|' || (token_expires_at > now() + interval '25 minutes')::text "
                    f"FROM waveform_storage_proofs WHERE storage_actor_id = '{key}'")
    fresh = [c for c in run.stub_calls() if c.get("user_uuid") == c1["uuid"] and c.get("proof_digest") not in digests
             and c.get("outcome") == "accepted"]
    run.check("issuer: a proof close to expiry is refreshed (same proof, new token, Briefcase accepts it)",
              status == 200 and tts["storage"]["status"] == "stored" and after.split("|")[0] == before.split("|")[0]
              and after.endswith("|true") and fresh and all(c["proof_id"] == before.split("|")[0] for c in fresh),
              f"{before} -> {after}; {len(fresh)} call(s) with the new token")
    run.stub("POST", "/_stub/fail", {"count": 1, "status": 503})
    status, held, _ = run.api("POST", "/api/v1/tts", t1, body={"text": "Briefcase is down."}, idem=True)
    status2, audio, headers = run.api("GET", f"/api/v1/audio/{(held or {}).get('request_id')}", t1, raw=True)
    status3, _, _ = run.api("GET", f"/api/v1/audio/{(held or {}).get('request_id')}", c2["waveform_token"], raw=True)
    run.check("Briefcase down: speech completes, held with briefcase_unavailable, served to its owner only",
              status == 200 and held["storage"].get("reason") == "briefcase_unavailable" and status2 == 200
              and headers.get("content-type", "").startswith("audio/mpeg") and status3 == 404,
              f"{held.get('storage', {}).get('reason') if isinstance(held, dict) else held}, owner {status2}, other {status3}")
    status, tts, _ = run.api("POST", "/api/v1/tts", t1, body={"text": "Briefcase is back."}, idem=True)
    run.check("…and the next speech lands in Briefcase again", status == 200 and tts["storage"]["status"] == "stored",
              f"{tts.get('storage') if isinstance(tts, dict) else tts}")
    status, auth, _ = run.api("GET", "/api/v1/storage-authorizations", t1)
    proof_id = (auth or {}).get("briefcase", {}).get("proof_id")
    status, body = run.accounts("DELETE", f"/v1/me/proofs/{proof_id}", bearer=c1["first_party_token"])
    status2, tts, _ = run.api("POST", "/api/v1/tts", t1, body={"text": "After the account revoked the proof."}, idem=True)
    status3, auth, _ = run.api("GET", "/api/v1/storage-authorizations", t1)
    run.check("the account revokes Waveform's proof at Silicon Accounts: speech is held and Waveform does not re-issue it",
              status == 204 and tts["storage"].get("reason") == "storage_not_authorized" and auth["briefcase"]["status"] == "revoked",
              f"revoke {status}, storage {tts.get('storage', {}).get('reason')}, access {auth['briefcase'].get('status')}/{auth['briefcase'].get('revoked_reason')}")
    status, auth, _ = run.api("POST", "/api/v1/storage-authorizations", t1)
    status2, tts, _ = run.api("POST", "/api/v1/tts", t1, body={"text": "Approved again."}, idem=True)
    run.check("…until the account approves again", status == 201 and tts["storage"]["status"] == "stored",
              f"{status} {tts.get('storage') if isinstance(tts, dict) else tts}")


def cargo_env() -> dict:
    return {**os.environ, "CARGO_TARGET_DIR": str(ROOT / "target/mig"), "CARGO_PROFILE_DEV_DEBUG": "0",
            "CARGO_INCREMENTAL": "0", "CARGO_BUILD_JOBS": os.environ.get("CARGO_BUILD_JOBS", "3")}


def scenario_7(run: Run) -> None:
    """The three discovery commands from a packaged archive, in an empty home."""
    version = re.search(r'^version\s*=\s*"([^"]+)"', (ROOT / "cli/Cargo.toml").read_text(), re.M).group(1)
    build = subprocess.run(["cargo", "build", "--release", "--locked", "--manifest-path", "cli/Cargo.toml"], cwd=ROOT,
                           env=cargo_env(), capture_output=True, text=True, timeout=1800)
    binary = ROOT / "target/mig/release/waveform"
    run.check("release CLI builds", build.returncode == 0 and binary.is_file(), build.stderr.strip().splitlines()[-1] if build.stderr else "")
    target = {"darwin": "macos", "linux": "linux"}.get(sys.platform, sys.platform) + "-" + \
        {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64"}.get(os.uname().machine, os.uname().machine)
    silicon_apps = os.environ.get("SILICON_APPS") or shutil.which("silicon-apps") or str(Path.home() / ".apps/bin/silicon-apps")
    pack = subprocess.run([str(ROOT / "scripts/package-apps.sh"), version, target, str(binary)], cwd=ROOT,
                          env={**os.environ, "SILICON_APPS": silicon_apps, "PACKAGE_DISCOVERY": "require"},
                          capture_output=True, text=True, timeout=600)
    archive = ROOT / f"dist/apps/waveform-{version}-{target}.tar.gz"
    run.check(f"scripts/package-apps.sh packs {archive.name} (validate + pack + discovery required)",
              pack.returncode == 0 and archive.is_file(), (pack.stdout + pack.stderr).strip().splitlines()[-1] if pack.stdout + pack.stderr else "")
    unpacked, empty = run.home("package"), run.home("empty")
    with tarfile.open(archive) as tar:
        members = sorted(member.name for member in tar.getmembers())
        tar.extractall(unpacked, filter="data")
    run.check("the archive holds apps.yaml and bin/waveform only", members == ["apps.yaml", "bin", "bin/waveform"]
              or members == ["apps.yaml", "bin/waveform"], f"{members}")
    env = {"HOME": str(empty), "SILICON_HOME": str(empty), "PATH": "/usr/bin:/bin"}
    def run_packaged(*args):
        return subprocess.run([str(unpacked / "bin/waveform"), *args], capture_output=True, text=True, timeout=60, env=env)
    help_ = run_packaged("--help")
    run.check("`waveform --help` exits 0 with help", help_.returncode == 0 and "waveform login" in help_.stdout,
              f"exit {help_.returncode}, {len(help_.stdout)} bytes")
    accounts = run_packaged("accounts", "--json")
    parsed = json.loads(accounts.stdout or "{}")
    run.check("`waveform accounts --json` exits 0 with \"app_id\":\"waveform\"",
              accounts.returncode == 0 and parsed.get("app_id") == "waveform" and '"app_id":"waveform"' in accounts.stdout.replace(" ", ""),
              f"exit {accounts.returncode} {parsed}")
    status = run_packaged("login", "status", "--json")
    run.check("`waveform login status --json` exits 0 with {\"authenticated\":false}",
              status.returncode == 0 and json.loads(status.stdout or "{}") == {"authenticated": False},
              f"exit {status.returncode} {status.stdout.strip()}")
    run.check("…and wrote nothing to the empty home", not any(empty.iterdir()), f"{list(empty.iterdir())}")
    if Path(silicon_apps).is_file():
        validate = subprocess.run([silicon_apps, "validate", str(unpacked), "--json", "--home", str(run.home("apps-home"))],
                                  capture_output=True, text=True, timeout=120,
                                  env={"PATH": "/usr/bin:/bin", "HOME": str(empty), "SILICON_APPS_NO_DAEMON": "1"})
        valid = json.loads(validate.stdout or "{}")
        run.check("`silicon-apps validate` accepts the unpacked archive (local check, nothing uploaded)",
                  validate.returncode == 0 and valid.get("valid") is True, f"{valid.get('errors')}")


def scenario_8(run: Run) -> None:
    """Restart safety: tokens, CLI sign-ins, webhook dedupe and stored proofs survive an API restart."""
    c1 = run.ids["c1"]
    before = dev.alive(dev.API_PID)
    restart = subprocess.run([str(ROOT / "scripts/dev-accounts.sh"), "restart-api"], cwd=ROOT, capture_output=True,
                             text=True, timeout=300)
    after = dev.alive(dev.API_PID)
    run.check("the API restarts (new process, healthy)", restart.returncode == 0 and after and after != before,
              f"pid {before} -> {after}")
    status, me, _ = run.api("GET", "/api/v1/me", c1["waveform_token"])
    run.check("the Carbon's access token from before the restart still works (stateless JWT)",
              status == 200 and me["account"]["uuid"] == c1["uuid"], f"{status}")
    code, out, errs, me = run.cli(Path(c1["cli_home"]), "me", "--json")
    run.check("the device-flow CLI sign-in still works without signing in again", code == 0, f"exit {code} {errs[-200:]}")
    replayed = {"event_id": run.ids["s1"]["id_event"], "type": "account.id_changed", "app_id": "waveform", "silicon": None,
                "occurred_at": time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime()),
                "data": {"uuid": run.ids["s1"]["uuid"], "membership_id": f"waveform:{run.ids['s1']['uuid']}",
                         "kind": "silicon", "old_id": run.ids["s1"]["id"], "new_id": f"si:after-restart-{run.n}"}}
    status, body = signed_delivery(run, replayed)
    run.check("webhook dedupe survived the restart (a processed event_id is still ignored)",
              status == 200 and body.get("duplicate") is True, f"{status} {body}")
    status, tts, _ = run.api("POST", "/api/v1/tts", c1["waveform_token"], body={"text": "After the restart."}, idem=True)
    run.check("the stored Briefcase proof still decrypts and works after the restart",
              status == 200 and tts["storage"]["status"] == "stored", f"{tts.get('storage') if isinstance(tts, dict) else tts}")
    delivered, detail = dev.ping_delivered(run.config)
    run.check("Silicon Accounts still delivers signed events after the restart", delivered, detail)


SCENARIOS = {1: scenario_1, 2: scenario_2, 3: scenario_3, 4: scenario_4, 5: scenario_5, 6: scenario_6,
             7: scenario_7, 8: scenario_8}
NAMES = {0: "setup", 1: "Carbon on the API", 2: "Silicon on the CLI", 3: "device flow", 4: "custodians and outsiders",
         5: "webhooks", 6: "proofs", 7: "discovery from a package", 8: "restart safety"}


def main(argv: list[str]) -> int:
    if "--help" in argv or "-h" in argv:
        print(__doc__)
        return 0
    only = None
    if "--only" in argv:
        only = {int(part) for part in argv[argv.index("--only") + 1].split(",")}
    dev.load_local_env()
    if "--no-start" not in argv:
        start = subprocess.run([str(ROOT / "scripts/dev-accounts.sh"), "start", "--fresh-db"], cwd=ROOT, timeout=600)
        if start.returncode != 0:
            return 1
    run = Run()
    print(f"Waveform end to end, run {run.n}: API {run.config.api_url}, Silicon Accounts {run.config.accounts_public}, "
          f"Briefcase {run.config.briefcase_url}", flush=True)
    try:
        for number in [0, *SCENARIOS]:
            if number and only and number not in only:
                continue
            run.scenario = f"{number} {NAMES[number]}"
            print(f"\n[{run.scenario}]", flush=True)
            try:
                setup(run) if number == 0 else SCENARIOS[number](run)
            except Exception as error:  # a broken step fails its scenario, the run goes on
                run.check(f"scenario {number} ran to the end", False, f"{type(error).__name__}: {error}")
                run.transcript(f"scenario-{number}-error.txt", traceback.format_exc())
    finally:
        counted = [r for r in run.results if r["ok"] is not None]
        failed = [r for r in counted if not r["ok"]]
        run.transcript("summary.json", {"run": run.n, "passed": len(counted) - len(failed), "failed": len(failed),
                                        "not_applicable": len(run.results) - len(counted), "results": run.results})
        print(f"\n{len(counted) - len(failed)} passed, {len(failed)} failed, {len(run.results) - len(counted)} not "
              f"applicable (run {run.n}; transcript .mig/e2e/{run.n}/)", flush=True)
        for result in failed:
            print(f"  FAILED [{result['scenario']}] {result['check']}: {result['detail'][:200]}", flush=True)
        if "--keep-running" not in argv and "--no-start" not in argv:
            subprocess.run([str(ROOT / "scripts/dev-accounts.sh"), "stop"], cwd=ROOT, timeout=120)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
