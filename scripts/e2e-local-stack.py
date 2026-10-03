#!/usr/bin/env python3
"""Run real Waveform + isolated PostgreSQL with loopback-only dummy upstreams.

Uses the built target/debug/waveform-api. No production credentials are loaded.
POST fixture :4383/__control {"outcomes":{"gemini":"rate_limited"},"reset":true}
GET  fixture :4383/__events returns captured dummy upstream requests.
Sign in through IAM with the frontend at localhost:4381, or SLT oac_waveform_e2e.
Ctrl-C stops the API and task-owned database. State remains under .local-test.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlencode, urlsplit, urlunsplit
import uuid

ROOT = Path(__file__).resolve().parents[1]
ACTOR = '00000000-0000-4000-8000-000000000123'
SCOPES = ['roles.read', 'memberships.read', 'obo:briefcase:briefcase.files.create', 'obo:briefcase:briefcase.files.read']
ORIGIN = 'https://briefcase.e2e.test'
AUDIO = (ROOT / 'src/infrastructure/test-audio/kore.mp3').read_bytes()
CONTRACT = json.loads((ROOT / 'tests/fixtures/briefcase-v1.1.0.json').read_text())
LOCK = threading.RLock()
EVENTS = []
OUTCOMES = {}
PROOFS = {}
FILES = {}
STOP = threading.Event()


def entry(name, data):
    path = f'private/e2e/apps/waveform/{name}'
    return {'id': str(uuid.uuid4()), 'org_id': 'tos', 'type': 'file', 'visibility': 'full', 'name': name,
            'path': path, 'root_type': 'private', 'content_type': 'audio/mpeg', 'size': len(data),
            'permanent_url': f'{ORIGIN}/org/tos/{path}', 'origin_app_id': 'waveform',
            'effective_access': ['read'], 'created_at': '2026-09-20T00:00:00Z', 'updated_at': '2026-09-20T00:00:00Z', 'deleted_at': None}


def record(event):
    with LOCK:
        EVENTS.append(event)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send(self, status, body=None, headers=None):
        if isinstance(body, (dict, list)):
            raw = json.dumps(body).encode()
            kind = 'application/json'
        elif isinstance(body, bytes):
            raw, kind = body, 'audio/mpeg'
        else:
            raw, kind = (body or '').encode(), 'text/plain'
        self.send_response(status)
        self.send_header('content-type', kind)
        self.send_header('content-length', str(len(raw)))
        self.send_header('cache-control', 'no-store')
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        url = urlsplit(self.path)
        if url.path == '/__events':
            with LOCK:
                return self.send(200, {'events': list(EVENTS), 'outcomes': dict(OUTCOMES), 'files': [v[0] for v in FILES.values()]})
        if url.path == '/login':
            redirect = parse_qs(url.query).get('redirect_uri', [''])[0]
            callback = urlsplit(redirect)
            if callback.hostname not in ['localhost', '127.0.0.1'] or callback.path != '/auth/callback':
                return self.send(400, 'Loopback frontend callback required')
            query = parse_qs(callback.query)
            query['slt'] = ['oac_waveform_e2e']
            target = urlunsplit((callback.scheme, callback.netloc, callback.path, urlencode(query, doseq=True), ''))
            return self.send(303, headers={'location': target})
        if url.path == '/api/version':
            return self.send(200, CONTRACT, {'briefcase-api-version': 'v1'})
        if url.path.startswith('/api/v1/obo-access/applications/') and url.path.endswith('/endpoints'):
            return self.send(200, {'application': {'app_id': 'briefcase', 'org_id': 'tos'}, 'endpoints': [
                {'critical': False, 'endpoint_id': 'briefcase.files.create', 'path': '/api/v1/obo/files', 'metadata': {'path': {'type': 'string'}, 'name': {'type': 'string'}, 'content_type': {'type': 'string'}}},
                {'critical': False, 'endpoint_id': 'briefcase.entries.list', 'path': '/api/v1/obo/entries/list', 'metadata': {}},
                {'critical': False, 'endpoint_id': 'briefcase.files.read', 'path': '/api/v1/obo/files/read', 'metadata': {}}]})
        if url.path.startswith('/audio/'):
            return self.send(200, AUDIO)
        return self.send(404, {'error': 'fixture_route_missing', 'path': url.path})

    def do_DELETE(self):
        return self.send(200, {})

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get('content-length', 0)))
        path = urlsplit(self.path).path
        try:
            body = json.loads(raw)
        except (json.JSONDecodeError, UnicodeDecodeError):
            body = None
        if path == '/__control':
            with LOCK:
                if body.get('reset'):
                    EVENTS.clear()
                if 'outcomes' in body:
                    OUTCOMES.clear()
                    OUTCOMES.update(body['outcomes'])
            return self.send(200, {'ok': True})
        if path == '/api/v1/app-auth/tokens':
            record({'kind': 'iam', 'path': path})
            return self.send(200, {'access_token': 'oat_waveform_e2e', 'refresh_token': 'ort_waveform_e2e', 'token_type': 'Bearer', 'expires_in': 1800,
                                  'scope': ' '.join(SCOPES), 'actor': {'principal_id': ACTOR, 'type': 'carbon', 'public_id': 'c:12345678'}, 'org_id': 'tos'})
        if path == '/api/v1/oauth/introspect':
            authority = {'principal_id': ACTOR, 'actor_type': 'carbon', 'public_id': 'c:12345678',
                         'organization_id': '00000000-0000-4000-8000-000000000002', 'org_id': 'tos',
                         'membership_id': 'c:12345678[tos]', 'membership_version': 1,
                         'authorization_epoch': 1, 'audience': 'waveform', 'testing_environment_id': None,
                         'scopes': SCOPES, 'org_role': 'member', 'tags': []}
            return self.send(200, {'active': True, 'principal_id': ACTOR, 'actor_type': 'carbon', 'org_id': 'tos', 'scope': ' '.join(SCOPES),
                                   'audience': 'waveform', 'expires_at': 4102444800, 'authorization': authority})
        if path == '/api/v1/oauth/revoke':
            return self.send(200, {})
        if path == '/api/v1/obo-access/exchanges':
            proof = 'obo_e2e_' + uuid.uuid4().hex
            with LOCK:
                PROOFS[proof] = body
            record({'kind': 'delegation', 'endpoint': body['endpoint_id'], 'body_sha256': body['request']['body_sha256']})
            return self.send(201, {'access_proof': proof, 'proof_id': str(uuid.uuid4()), 'expires_in': 60, 'expires_at': '2099-01-01T00:00:00Z'})
        if path.startswith('/api/v1/obo/'):
            proof = self.headers.get('x-iam-obo-access-proof')
            with LOCK:
                claim = PROOFS.pop(proof, None)
            if not claim or claim['request']['body_sha256'] != hashlib.sha256(raw).hexdigest() or self.headers.get('authorization'):
                return self.send(403, {'error': 'fixture_invalid_exact_byte_proof'})
            if path == '/api/v1/obo/files':
                result = entry(claim['metadata']['name'], raw)
                with LOCK:
                    FILES[result['id']] = (result, raw)
                record({'kind': 'upload', 'name': result['name'], 'size': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()})
                return self.send(201, result)
            if path == '/api/v1/obo/entries/list':
                with LOCK:
                    return self.send(200, {'items': [v[0] for v in FILES.values()], 'next_cursor': None})
            if path == '/api/v1/obo/files/read':
                with LOCK:
                    data = FILES.get(body['entry_id'])
                if not data:
                    return self.send(404, {})
                if body.get('range') == 'bytes=0-0':
                    return self.send(206, data[1][:1], {'content-range': f'bytes 0-0/{len(data[1])}'})
                return self.send(200, data[1])
        provider = None
        operation = 'tts'
        if path == '/v1beta/interactions':
            provider = 'gemini'
        elif path.startswith('/v1/text-to-speech/'):
            provider = 'elevenlabs'
        elif path == '/v1/audio/speech':
            provider = 'openai'
        elif path == '/v1/audio/transcriptions':
            provider, operation = 'openai', 'stt'
        elif path == '/v1/listen':
            provider, operation = 'deepgram', 'stt'
        elif path == '/upload/v1beta/files':
            provider, operation = 'gemini', 'stt'
        if provider:
            key = self.headers.get('x-goog-api-key') or self.headers.get('xi-api-key') or self.headers.get('authorization', '').removeprefix('Bearer ').removeprefix('Token ')
            # Never accept/store real provider credentials in a fixture capture.
            if not key.startswith('e2e-'):
                return self.send(400, {'error': {'code': 'invalid_api_key', 'message': 'Only e2e- dummy fixture keys are accepted'}})
            record({'kind': 'provider', 'provider': provider, 'operation': operation, 'path': path, 'key': key, 'body': body, 'bytes': len(raw)})
            with LOCK:
                outcome = OUTCOMES.get(f'{provider}:{operation}', OUTCOMES.get(provider, 'success'))
            if '-fail-' in key:
                outcome = key.rsplit('-fail-', 1)[1]
            elif key.endswith('-success'):
                outcome = 'success'
            if outcome != 'success':
                status = {'rate_limited': 429, 'invalid_api_key': 401, 'quota_exceeded': 429, 'voice_not_found': 404, 'model_not_found': 404}.get(outcome, 500)
                return self.send(status, {'error': {'code': outcome, 'message': 'fixture failure includes e2e-private-message-never-expose'}})
            if operation == 'stt':
                if provider == 'openai':
                    return self.send(200, {'text': 'End to end transcription succeeded.', 'language': 'english', 'duration': 2.0})
                if provider == 'deepgram':
                    return self.send(200, {'metadata': {'duration': 2.0}, 'results': {'channels': [{'detected_language': 'en', 'alternatives': [{'transcript': 'End to end transcription succeeded.', 'languages': ['en']}]}]}})
                # Gemini upload success is intentionally unsupported; force its failure for STT fallback tests.
                return self.send(503, {'error': {'code': 'unavailable'}})
            if provider == 'gemini':
                return self.send(200, {'status': 'completed', 'steps': [{'content': [{'type': 'audio', 'data': base64.b64encode(AUDIO).decode(), 'mime_type': 'audio/mpeg'}]}]})
            return self.send(200, AUDIO)
        record({'kind': 'unhandled', 'path': path, 'body': body})
        return self.send(404, {'error': 'fixture_route_missing', 'path': path})


def main():
    (ROOT / '.local-test').mkdir(parents=True, exist_ok=True)
    state = Path(tempfile.mkdtemp(prefix='tts-e2e-', dir=ROOT / '.local-test'))
    state.chmod(0o700)
    pg = Path('/opt/homebrew/opt/postgresql@16/bin')
    password = state / 'password'
    password.write_text(secrets.token_hex(32))
    password.chmod(0o600)
    data = state / 'postgres'
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        pg_port = sock.getsockname()[1]
    pg_env = {key: value for key, value in os.environ.items() if key in ['PATH', 'HOME', 'TMPDIR', 'LANG']}
    pg_env['PGPASSWORD'] = password.read_text()
    subprocess.run([str(pg/'initdb'), '-D', str(data), '-U', 'waveform_e2e', '-A', 'scram-sha-256', '--pwfile', str(password)], check=True, stdout=subprocess.DEVNULL, env=pg_env)
    subprocess.run([str(pg/'pg_ctl'), '-D', str(data), '-l', str(state/'postgres.log'), '-o', f'-h 127.0.0.1 -p {pg_port} -k {state}', '-w', 'start'], check=True, stdout=subprocess.DEVNULL, env=pg_env)
    subprocess.run([str(pg/'createdb'), '-h', '127.0.0.1', '-p', str(pg_port), '-U', 'waveform_e2e', 'waveform_e2e'], check=True, env=pg_env)
    env = {key: value for key, value in os.environ.items() if key in ['PATH', 'HOME', 'TMPDIR', 'LANG']}
    # Every configured remote origin points at this process; keys are dummy.
    env.update({'WAVEFORM_ENVIRONMENT': 'development', 'WAVEFORM_BIND_ADDR': '127.0.0.1:4382',
                'WAVEFORM_DATABASE_URL': f'postgres://waveform_e2e:{password.read_text()}@127.0.0.1:{pg_port}/waveform_e2e',
                'WAVEFORM_IAM_BASE_URL': 'http://127.0.0.1:4383', 'WAVEFORM_IAM_APP_ID': 'waveform', 'WAVEFORM_IAM_AUDIENCE': 'waveform',
                'WAVEFORM_IAM_APP_SECRET': 'e2e-waveform-dummy-app-secret', 'WAVEFORM_BRIEFCASE_BASE_URL': 'http://127.0.0.1:4383',
                'WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN': ORIGIN, 'WAVEFORM_BRIEFCASE_CDN_ORIGIN': ORIGIN, 'WAVEFORM_BRIEFCASE_APP_ID': 'waveform',
                'WAVEFORM_ENCRYPTION_KEY': '19'*32, 'WAVEFORM_IDEMPOTENCY_DIGEST_KEY': 'e2e-dummy-digest-key-12345678901234567890',
                'WAVEFORM_TELEMETRY': '0', 'WAVEFORM_TELEMETRY_KEY': '', 'WAVEFORM_HONEYCOMB_SERVICE_TOKEN': '',
                'WAVEFORM_LOG_FILTER': 'silicon_waveform=info', 'WAVEFORM_LOG_JSON': 'false'})
    for provider in ['GEMINI', 'ELEVENLABS', 'OPENAI', 'DEEPGRAM']:
        env[f'WAVEFORM_{provider}_BASE_URL'] = 'http://127.0.0.1:4383'
        env[f'WAVEFORM_{provider}_API_KEY'] = f'e2e-shared-{provider.lower()}'
    (state/'environment.json').write_text(json.dumps({'database_url': env['WAVEFORM_DATABASE_URL'], 'pg_data': str(data), 'pg_port': pg_port}))
    (state/'environment.json').chmod(0o600)
    server = ThreadingHTTPServer(('127.0.0.1', 4383), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    api = None
    log = (state/'waveform.log').open('w')
    try:
        # An unrelated temp cwd prevents dotenv loading the repository's environment.
        with tempfile.TemporaryDirectory(prefix='waveform-e2e-cwd-') as cwd:
            api = subprocess.Popen([str(ROOT/'target/debug/waveform-api')], cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT)
            print(json.dumps({'backend': 'http://127.0.0.1:4382', 'fixture': 'http://127.0.0.1:4383', 'slt': 'oac_waveform_e2e', 'actor': ACTOR, 'state': str(state)}), flush=True)
            while not STOP.wait(1):
                if api.poll() is not None:
                    raise SystemExit(f'Waveform exited {api.returncode}; check {state}/waveform.log')
    finally:
        if api and api.poll() is None:
            api.terminate()
            api.wait(timeout=40)
        server.shutdown()
        subprocess.run([str(pg/'pg_ctl'), '-D', str(data), '-m', 'fast', '-w', 'stop'], stdout=subprocess.DEVNULL, env=pg_env)
        log.close()
        (state/'events.json').write_text(json.dumps(EVENTS, indent=2))


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: STOP.set())
    signal.signal(signal.SIGINT, lambda *_: STOP.set())
    main()
