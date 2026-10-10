#!/usr/bin/env python3
"""Run Waveform locally against a Silicon Accounts development stack (the entry point is dev-accounts.sh).

  scripts/dev-accounts.sh start [--fresh-db] [--no-stub]   start (idempotent): database, keys, sign-in setup,
                                                           webhook, Briefcase stub, API; then a signed ping
  scripts/dev-accounts.sh stop [--keep-webhook]            stop what start started; hand the webhook back
  scripts/dev-accounts.sh restart-api                      stop and start only the API (database and stub stay)
  scripts/dev-accounts.sh status                           what runs, and where Waveform's webhook points
  scripts/dev-accounts.sh env                              shell exports for the CLI against this API

Configuration (environment, or KEY=VALUE lines in .mig/dev-accounts.env, which is not committed):
  WAVEFORM_DEV_STACK       the stack description (JSON): accounts_public_url, accounts_api_url, fake_apps_url and
                           apps.{waveform,briefcase}.app_secret (development secrets only)
  WAVEFORM_DEV_BASE_PORT   first port of Waveform's block (4160): the website, +1 the API, +2 the Briefcase stub
  WAVEFORM_DEV_PG          PostgreSQL server URL without a database (postgres://postgres@127.0.0.1:5460)
  WAVEFORM_DEV_DATABASE    database name (waveform_e2e)
  WAVEFORM_BRIEFCASE_BASE_URL, WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN
                           point Waveform at a running Briefcase instead of the stub (no stub is started)
State lives in .mig/ (git-ignored): dev/ keeps the encryption keys and the webhook secret (0600), pids/ and logs/.
Nothing here is for production: it refuses an https Accounts URL that is not on this machine.
"""

from __future__ import annotations

import base64
import json
import os
import secrets
import shutil
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MIG = ROOT / ".mig"
STATE, PIDS, LOGS = MIG / "dev", MIG / "pids", MIG / "logs"
APP_ID = "waveform"
API_PID, STUB_PID = PIDS / "dev-api", PIDS / "dev-briefcase-stub"
WEBHOOK_SECRET = STATE / "webhook.secret"
PROOF_ISSUERS = "waveform.tts=interface;waveform.stt=interface,dm"


def say(message: str) -> None:
    print(f"dev-accounts: {message}", flush=True)


def fail(message: str) -> None:
    print(f"dev-accounts: error: {message}", file=sys.stderr, flush=True)
    sys.exit(1)


def load_local_env() -> None:
    path = MIG / "dev-accounts.env"
    if path.is_file():
        for line in path.read_text().splitlines():
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                key, value = line.split("=", 1)
                os.environ.setdefault(key.strip(), value.strip())


class Config:
    def __init__(self) -> None:
        load_local_env()
        stack_path = os.environ.get("WAVEFORM_DEV_STACK", "")
        if not stack_path or not Path(stack_path).is_file():
            fail("set WAVEFORM_DEV_STACK to the stack description (JSON with accounts URLs and the apps' development secrets)")
        self.stack = json.loads(Path(stack_path).read_text())
        self.accounts_public = self.stack["accounts_public_url"].rstrip("/")
        self.accounts_api = self.stack.get("accounts_api_url", self.accounts_public).rstrip("/")
        self.fake_apps = (self.stack.get("fake_apps_url") or "").rstrip("/")
        for url in (self.accounts_public, self.accounts_api):
            host = urllib.request.urlparse(url).hostname or ""
            if host not in ("localhost", "127.0.0.1", "::1") and not host.endswith(".localhost"):
                fail(f"{url} is not a development stack on this machine; this script never talks to production")
        self.base = int(os.environ.get("WAVEFORM_DEV_BASE_PORT", "4160"))
        self.api_port, self.stub_port = self.base + 1, self.base + 2
        self.api_url = f"http://127.0.0.1:{self.api_port}"
        self.pg = os.environ.get("WAVEFORM_DEV_PG", "postgres://postgres@127.0.0.1:5460").rstrip("/")
        self.database = os.environ.get("WAVEFORM_DEV_DATABASE", "waveform_e2e")
        self.external_briefcase = os.environ.get("WAVEFORM_BRIEFCASE_BASE_URL", "")
        self.briefcase_url = self.external_briefcase or f"http://127.0.0.1:{self.stub_port}"
        self.permanent_origin = os.environ.get("WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN", self.briefcase_url)
        self.webhook_url = f"{self.api_url}/webhooks/accounts"
        self.binary = Path(os.environ.get("WAVEFORM_DEV_API_BINARY", ROOT / "target/mig/debug/waveform-api"))

    def secret(self, app: str) -> str:
        value = (self.stack.get("apps", {}).get(app) or {}).get("app_secret")
        if not value:
            fail(f"the stack description has no development secret for '{app}'")
        return value

    def basic(self, app: str) -> str:
        return "Basic " + base64.b64encode(f"{app}:{self.secret(app)}".encode()).decode()


def http(method: str, url: str, body=None, headers: dict | None = None, timeout: float = 15):
    """One HTTP call; answers (status, parsed JSON or text). Never raises for HTTP statuses."""
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method)
    for key, value in (headers or {}).items():
        request.add_header(key, value)
    if data is not None:
        request.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read()
            status = response.status
    except urllib.error.HTTPError as error:
        raw, status = error.read(), error.code
    except (urllib.error.URLError, TimeoutError, ConnectionError) as error:
        return 0, str(error)
    try:
        return status, json.loads(raw) if raw else None
    except ValueError:
        return status, raw.decode(errors="replace")


def accounts_app(config: Config, method: str, path: str, body=None, idempotent: bool = False):
    headers = {"authorization": config.basic(APP_ID)}
    if idempotent:
        headers["idempotency-key"] = f"waveform-dev-{uuid.uuid4()}"
    return http(method, config.accounts_api + path, body, headers)


def alive(pid_file: Path) -> int | None:
    try:
        pid = int(pid_file.read_text().strip())
        os.kill(pid, 0)
        return pid
    except (OSError, ValueError):
        return None


def write_secret(path: Path, value: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = path.with_suffix(".tmp")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as handle:
        handle.write(value)
    os.replace(temporary, path)


def psql() -> str:
    for candidate in (shutil.which("psql"), "/opt/homebrew/opt/postgresql@16/bin/psql", "/opt/homebrew/bin/psql",
                      "/usr/local/bin/psql"):
        if candidate and Path(candidate).is_file():
            return candidate
    fail("psql was not found (install PostgreSQL client tools or put psql on PATH)")
    return ""


def sql(config: Config, statement: str, database: str = "postgres") -> str:
    result = subprocess.run([psql(), f"{config.pg}/{database}", "-v", "ON_ERROR_STOP=1", "-Atc", statement],
                            capture_output=True, text=True, timeout=60)
    if result.returncode != 0:
        fail(f"PostgreSQL refused `{statement}`: {result.stderr.strip()}")
    return result.stdout.strip()


def ensure_database(config: Config, fresh: bool) -> None:
    if not config.database.startswith("waveform_"):
        fail("WAVEFORM_DEV_DATABASE must start with waveform_ (shared test server)")
    exists = sql(config, f"SELECT 1 FROM pg_database WHERE datname = '{config.database}'") == "1"
    if exists and fresh:
        sql(config, f"DROP DATABASE {config.database} WITH (FORCE)")
        exists = False
        say(f"dropped database {config.database}")
    if not exists:
        sql(config, f"CREATE DATABASE {config.database}")
        say(f"created database {config.database}")


def ensure_keys() -> dict[str, str]:
    keys = {}
    for name, make in (("encryption.key", lambda: secrets.token_hex(32)),
                       ("digest.key", lambda: secrets.token_urlsafe(48))):
        path = STATE / name
        if not path.is_file():
            write_secret(path, make())
        keys[name] = path.read_text().strip()
    return keys


def ensure_signin_setup(config: Config) -> None:
    """Makes Waveform's sign-in setup on the stack offer what local runs need (only adds; never removes)."""
    status, app = accounts_app(config, "GET", f"/v1/apps/{APP_ID}")
    if status != 200:
        fail(f"could not read Waveform's sign-in setup ({status}): {app}")
    current = app.get("signin_config") or {}
    web = [f"http://localhost:{config.base}", f"http://127.0.0.1:{config.base}"]
    wanted_uris = [f"{origin}/auth/callback" for origin in web]
    if config.fake_apps:
        wanted_uris.append(f"{config.fake_apps}/{APP_ID}/callback")
    patch: dict = {}
    if not (current.get("methods") or {}).get("email"):
        patch["methods"] = {"email": True}
    for flag in ("device_flow", "public_client"):
        if not current.get(flag):
            patch[flag] = True
    for field, wanted in (("redirect_uris", wanted_uris), ("allowed_origins", web)):
        have = list(current.get(field) or [])
        missing = [item for item in wanted if item not in have]
        if missing:
            patch[field] = have + missing
    if not patch:
        say("Waveform's sign-in setup already offers email codes, the device flow, the public client and the local callbacks")
        return
    patch["expected_version"] = app.get("config_version")
    status, answer = accounts_app(config, "PATCH", f"/v1/apps/{APP_ID}/signin-config", patch, idempotent=True)
    if status != 200:
        fail(f"could not update Waveform's sign-in setup ({status}): {answer}")
    changed = ", ".join(sorted(key for key in patch if key != "expected_version"))
    say(f"updated Waveform's sign-in setup on the stack ({changed}); version {answer.get('config_version')}")


def webhook_secret(config: Config, rotate: bool = False) -> str:
    """Points Waveform's webhook at the local API and answers its signing secret (kept in .mig/dev)."""
    status, current = accounts_app(config, "GET", f"/v1/apps/{APP_ID}/webhook")
    if status != 200:
        fail(f"could not read Waveform's webhook ({status}): {current}")
    if not rotate and WEBHOOK_SECRET.is_file() and (current or {}).get("url") == config.webhook_url \
            and (current or {}).get("events") is None:
        return WEBHOOK_SECRET.read_text().strip()
    status, generated = accounts_app(config, "POST", f"/v1/apps/{APP_ID}/webhook/generate-secret", idempotent=True)
    if status != 200 or not str((generated or {}).get("secret", "")).startswith("whsec_"):
        fail(f"could not generate a webhook secret ({status}): {generated}")
    write_secret(WEBHOOK_SECRET, generated["secret"])
    status, saved = accounts_app(config, "PUT", f"/v1/apps/{APP_ID}/webhook",
                                 {"url": config.webhook_url, "events": None}, idempotent=True)
    if status != 200:
        fail(f"could not point Waveform's webhook at {config.webhook_url} ({status}): {saved}")
    say(f"Waveform's webhook now goes to {config.webhook_url} (every update; secret kept in .mig/dev/webhook.secret)")
    return generated["secret"]


def service_env(config: Config, keys: dict[str, str], secret: str) -> dict[str, str]:
    env = {key: value for key, value in os.environ.items() if key in ("PATH", "HOME", "TMPDIR", "LANG")}
    ffmpeg = shutil.which("ffmpeg") or "/opt/homebrew/bin/ffmpeg"
    env.update({
        "WAVEFORM_ENVIRONMENT": "development",
        "WAVEFORM_BIND_ADDR": f"127.0.0.1:{config.api_port}",
        "WAVEFORM_PUBLIC_URL": config.api_url,
        "WAVEFORM_DATABASE_URL": f"{config.pg}/{config.database}",
        "ACCOUNTS_URL": config.accounts_public,
        "ACCOUNTS_API_URL": config.accounts_api,
        "WAVEFORM_APP_ID": APP_ID,
        "WAVEFORM_APP_SECRET": config.secret(APP_ID),
        "WAVEFORM_ACCOUNTS_WEBHOOK_SECRET": secret,
        "WAVEFORM_PROOF_ISSUERS": os.environ.get("WAVEFORM_PROOF_ISSUERS", PROOF_ISSUERS),
        "WAVEFORM_ENCRYPTION_KEY": keys["encryption.key"],
        "WAVEFORM_IDEMPOTENCY_DIGEST_KEY": keys["digest.key"],
        "WAVEFORM_SPEECH_FIXTURES": os.environ.get("WAVEFORM_SPEECH_FIXTURES", "true"),
        "WAVEFORM_BRIEFCASE_BASE_URL": config.briefcase_url,
        "WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN": config.permanent_origin,
        "WAVEFORM_FFMPEG_PATH": ffmpeg,
        "WAVEFORM_LOG_FILTER": os.environ.get("WAVEFORM_LOG_FILTER", "silicon_waveform=info,tower_http=info"),
        "WAVEFORM_TELEMETRY": "0",
    })
    return env


def spawn(name: str, pid_file: Path, argv: list[str], env: dict[str, str]) -> int:
    LOGS.mkdir(parents=True, exist_ok=True)
    PIDS.mkdir(parents=True, exist_ok=True)
    log = open(LOGS / f"{name}.log", "ab")
    log.write(f"\n--- {time.strftime('%Y-%m-%dT%H:%M:%S')} start {name}\n".encode())
    log.flush()
    process = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                               env=env, cwd=ROOT, start_new_session=True)
    pid_file.write_text(str(process.pid))
    return process.pid


def wait_http(url: str, seconds: float, pid_file: Path, name: str) -> None:
    deadline = time.time() + seconds
    while time.time() < deadline:
        status, _ = http("GET", url, timeout=3)
        if status == 200:
            return
        if not alive(pid_file):
            tail = (LOGS / f"{name}.log").read_text(errors="replace").splitlines()[-15:]
            fail(f"{name} exited while starting; last log lines:\n" + "\n".join(tail))
        time.sleep(0.5)
    fail(f"{name} did not answer {url} within {seconds:.0f} s (log: .mig/logs/{name}.log)")


def stop_pid(pid_file: Path, name: str) -> None:
    pid = alive(pid_file)
    if pid:
        os.kill(pid, signal.SIGTERM)
        for _ in range(40):
            if not alive(pid_file):
                break
            time.sleep(0.25)
        else:
            os.kill(pid, signal.SIGKILL)
        say(f"stopped {name} (pid {pid})")
    pid_file.unlink(missing_ok=True)


def ping_delivered(config: Config) -> tuple[bool, str]:
    """Asks the stack for a signed ping and waits for Waveform to accept it."""
    status, queued = accounts_app(config, "POST", f"/v1/apps/{APP_ID}/webhook/test", idempotent=True)
    if status not in (200, 201, 202):
        return False, f"test ping refused ({status}): {queued}"
    delivery = queued.get("delivery_id")
    deadline, last = time.time() + 40, None
    while time.time() < deadline:
        status, detail = accounts_app(config, "GET", f"/v1/apps/{APP_ID}/webhook/deliveries/{delivery}")
        if status == 200:
            last = detail
            if detail.get("status") == "delivered":
                return True, delivery
            if detail.get("status") == "failed":
                break
            attempts = detail.get("attempts") or []
            if attempts and attempts[-1].get("status_code") in (401, 400, 503):
                break
        time.sleep(1)
    return False, f"ping {delivery} not delivered: {json.dumps(last)[:600]}"


def start_stub(config: Config) -> None:
    if config.external_briefcase:
        say(f"using the Briefcase at {config.external_briefcase} (no stub)")
        return
    if alive(STUB_PID):
        say(f"Briefcase stub already running on {config.briefcase_url}")
        return
    env = {key: value for key, value in os.environ.items() if key in ("PATH", "HOME", "TMPDIR", "LANG")}
    env["STUB_BRIEFCASE_APP_SECRET"] = config.secret("briefcase")
    pid = spawn("dev-briefcase-stub", STUB_PID,
                [sys.executable, "-I", str(ROOT / "scripts/dev_briefcase_stub.py"), "--port", str(config.stub_port),
                 "--accounts-api-url", config.accounts_api, "--permanent-origin", config.permanent_origin,
                 "--log", str(LOGS / "dev-briefcase-calls.jsonl")], env)
    wait_http(f"{config.briefcase_url}/_stub/health", 15, STUB_PID, "dev-briefcase-stub")
    say(f"Briefcase stub on {config.briefcase_url} (pid {pid}); it verifies proofs with Briefcase's development credentials")


def start_api(config: Config, secret: str) -> None:
    if alive(API_PID):
        say(f"API already running on {config.api_url}")
        return
    if not config.binary.is_file():
        fail(f"{config.binary} is missing: build it with `cargo build --locked --bin waveform-api` "
             "(CARGO_TARGET_DIR=target/mig)")
    env = service_env(config, ensure_keys(), secret)
    migrate = subprocess.run([str(config.binary), "migrate"], env=env, capture_output=True, text=True, timeout=120)
    if migrate.returncode != 0:
        fail(f"waveform-api migrate failed: {migrate.stderr.strip() or migrate.stdout.strip()}")
    pid = spawn("dev-api", API_PID, [str(config.binary)], env)
    wait_http(f"{config.api_url}/health/ready", 90, API_PID, "dev-api")
    say(f"API on {config.api_url} (pid {pid}, database {config.database}, speech fixtures "
        f"{env['WAVEFORM_SPEECH_FIXTURES']})")


def start(config: Config, fresh: bool, stub: bool) -> None:
    for directory in (STATE, PIDS, LOGS):
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    if fresh:
        stop_pid(API_PID, "API")
        stop_pid(STUB_PID, "Briefcase stub")
        (LOGS / "dev-briefcase-calls.jsonl").unlink(missing_ok=True)
    ensure_database(config, fresh)
    ensure_keys()
    ensure_signin_setup(config)
    if stub:
        start_stub(config)
    secret = webhook_secret(config)
    start_api(config, secret)
    delivered, detail = ping_delivered(config)
    if not delivered:
        say(f"the signed ping did not arrive ({detail}); making a new webhook secret and restarting the API")
        secret = webhook_secret(config, rotate=True)
        stop_pid(API_PID, "API")
        start_api(config, secret)
        delivered, detail = ping_delivered(config)
        if not delivered:
            fail(detail)
    say(f"Silicon Accounts delivered a signed ping to Waveform (delivery {detail})")
    print_env(config)


def restore_webhook(config: Config) -> None:
    status, current = accounts_app(config, "GET", f"/v1/apps/{APP_ID}/webhook")
    if status != 200 or (current or {}).get("url") != config.webhook_url:
        return
    if not config.fake_apps:
        say("left Waveform's webhook pointing at the stopped API (no fake app to hand it back to)")
        return
    status, answer = http("POST", f"{config.fake_apps}/{APP_ID}/_connect-webhook", {})
    if status == 200:
        say(f"handed Waveform's webhook back to {config.fake_apps}/{APP_ID} (it keeps its own new secret)")
    else:
        say(f"could not hand the webhook back to the fake app ({status}): {answer}")
    WEBHOOK_SECRET.unlink(missing_ok=True)


def stop(config: Config, keep_webhook: bool) -> None:
    stop_pid(API_PID, "API")
    stop_pid(STUB_PID, "Briefcase stub")
    if not keep_webhook:
        restore_webhook(config)


def status(config: Config) -> None:
    for name, pid_file, url in (("API", API_PID, f"{config.api_url}/health/ready"),
                                ("Briefcase stub", STUB_PID, f"{config.briefcase_url}/_stub/health")):
        pid = alive(pid_file)
        code, _ = http("GET", url, timeout=3) if pid else (0, None)
        say(f"{name}: {'running, pid ' + str(pid) + (' (healthy)' if code == 200 else ' (not answering)') if pid else 'stopped'}")
    code, current = accounts_app(config, "GET", f"/v1/apps/{APP_ID}/webhook")
    say(f"Waveform's webhook on the stack: {current.get('url') if code == 200 else code}")


def print_env(config: Config) -> None:
    print(f"export WAVEFORM_URL={config.api_url}")
    print(f"export ACCOUNTS_URL={config.accounts_public}")


def main(argv: list[str]) -> int:
    if not argv or argv[0] in ("-h", "--help", "help"):
        print(__doc__)
        return 0
    command, flags = argv[0], set(argv[1:])
    known = {"start": {"--fresh-db", "--no-stub"}, "stop": {"--keep-webhook"}, "restart-api": set(),
             "status": set(), "env": set()}
    if command not in known or not flags <= known[command]:
        print(__doc__, file=sys.stderr)
        return 2
    config = Config()
    if command == "start":
        start(config, "--fresh-db" in flags, "--no-stub" not in flags)
    elif command == "stop":
        stop(config, "--keep-webhook" in flags)
    elif command == "restart-api":
        stop_pid(API_PID, "API")
        start_api(config, webhook_secret(config))
    elif command == "status":
        status(config)
    else:
        print_env(config)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
