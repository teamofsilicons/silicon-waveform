#!/usr/bin/env python3
"""Exercise the real local gateway/API against e2e-local-stack.py dummy upstreams.

Start the local stack and the frontend at localhost:4381 first. All credentials
below are deliberately recognizable fixture values; no real keys are loaded.
Use --state-dir for an additional PostgreSQL plaintext/encryption check.
"""
import argparse
import http.cookiejar
import json
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gateway', default='http://localhost:4381')
    parser.add_argument('--fixture', default='http://127.0.0.1:4383')
    parser.add_argument('--backend', default='http://127.0.0.1:4382')
    parser.add_argument('--cli', type=Path, help='Optionally test a built Waveform CLI too')
    parser.add_argument('--state-dir', type=Path)
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    for origin in [args.gateway, args.fixture, args.backend]:
        if urllib.parse.urlsplit(origin).hostname not in ['localhost', '127.0.0.1']:
            parser.error('Only loopback test servers are allowed.')
    prefix = 'api-e2e-' + uuid.uuid4().hex[:10]
    secret_prefix = 'e2e-' + prefix
    jar = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    context = ''
    checks = []
    secrets = []
    saved_created = False

    def check(label, condition):
        if not condition:
            raise AssertionError(label)
        checks.append(label)
        print('PASS ' + label, flush=True)

    def call(path, body=None, method=None, headers=None, raw=None):
        nonlocal context
        data = raw if raw is not None else None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(args.gateway + path, data=data, method=method or ('POST' if data is not None else 'GET'), headers={
            'Origin': args.gateway,
            'x-waveform-context': context,
            **({'content-type': 'application/json'} if data is not None else {}),
            **(headers or {}),
        })
        try:
            response = opener.open(request, timeout=50)
        except urllib.error.HTTPError as error:
            response = error
        payload = response.read()
        try:
            value = json.loads(payload)
        except (json.JSONDecodeError, UnicodeDecodeError):
            value = None
        if isinstance(value, dict) and value.get('context'):
            context = value['context']
        return response.status, value, payload

    def events():
        with urllib.request.urlopen(args.fixture + '/__events', timeout=10) as response:
            return json.load(response)['events']

    def marked_events(marker):
        return [event for event in events() if event.get('kind') == 'provider' and
                (marker in json.dumps(event.get('body')) or marker in event.get('key', ''))]

    def key(label, suffix='success'):
        value = f'{secret_prefix}-{label}-{suffix}'
        secrets.append(value)
        return value

    def tts(label, extra=None, idem=None):
        body = {'text': prefix + ' ' + label, 'provider_order': ['openai'], **(extra or {})}
        return call('/api/v1/tts', body, headers={'idempotency-key': idem or prefix + '-' + label, 'x-request-id': str(uuid.uuid4())})

    def save(value):
        nonlocal saved_created
        status, _, _ = call('/api/v1/provider-keys/openai', {'api_key': value}, 'PUT')
        check('saved OpenAI key accepted', status == 204)
        saved_created = True

    def sql(query):
        config = json.loads((args.state_dir / 'environment.json').read_text())
        url = urllib.parse.urlsplit(config['database_url'])
        env = dict(os.environ, PGPASSWORD=urllib.parse.unquote(url.password))
        command = ['/opt/homebrew/opt/postgresql@16/bin/psql', '-X', '-h', url.hostname,
                   '-p', str(url.port), '-U', url.username, '-d', url.path.lstrip('/'), '-tAc', query]
        result = subprocess.run(command, env=env, capture_output=True, text=True, check=True)
        return result.stdout

    status, session, _ = call('/api/session/login', {'slt': 'oac_waveform_e2e', 'org': 'tos'})
    check('gateway session login reaches real API', status == 200 and session.get('authenticated'))
    status, caps, _ = call('/api/v1/capabilities')
    check('capabilities advertise default-off TTS and both BYOK modes', status == 200 and caps['tts']['auto_fallback_default'] is False and caps['byok']['saved'] and caps['byok']['per_request'])

    try:
        invalids = [
            ('fallback-controls', {'auto_fallback': True, 'provider_options': {'openai': {'speed': 1}}}),
            ('mismatched-controls', {'provider_options': {'gemini': {'scene': 'The wrong provider'}}}),
            ('model-instructions', {'provider_options': {'openai': {'instructions': 'Requires explicit mini model'}}}),
            ('invalid-speed', {'provider_options': {'openai': {'speed': 5}}}),
            ('wrong-key-provider', {'provider_keys': {'deepgram': key('invalid-provider')}}),
            ('invalid-key-bytes', {'provider_keys': {'openai': 'e2e-invalid key'}}),
        ]
        for label, extra in invalids:
            status, body, _ = tts(label, extra)
            check(label + ' rejected before provider dispatch', status == 400 and not marked_events(prefix + ' ' + label))
        status, jobs, _ = call('/api/v1/jobs?limit=100')
        check('invalid options create no speech job', status == 200 and not any(prefix in j['first_line'] for j in jobs['items']))
        status, _, _ = call('/api/v1/tts', raw=b'{"text":"' + b'a' * 327_681 + b'"}')
        check('gateway rejects oversized body with 413', status == 413)

        saved_key = key('saved')
        request_key = key('request')
        save(saved_key)
        status, listed, raw = call('/api/v1/provider-keys')
        check('saved key list is write-only metadata', status == 200 and any(k['provider'] == 'openai' and k['configured'] for k in listed['items']) and saved_key.encode() not in raw)
        if args.state_dir:
            rows = sql("SELECT secret_cipher FROM waveform_provider_keys WHERE provider='openai'")
            check('saved key is encrypted in PostgreSQL', bool(rows.strip()) and saved_key not in rows)
        status, result, _ = tts('saved-precedence')
        selected = marked_events(prefix + ' saved-precedence')
        check('saved key overrides shared provider key', status == 200 and len(selected) == 1 and selected[0]['key'] == saved_key)

        body_extra = {'provider_keys': {'openai': request_key}, 'provider_options': {'openai': {'model': 'gpt-4o-mini-tts', 'instructions': 'Read with calm precision.', 'speed': 0.25, 'voice': 'coral'}}}
        status, original, _ = tts('request-precedence', body_extra)
        selected = marked_events(prefix + ' request-precedence')
        check('request key overrides saved key and precise controls reach OpenAI', status == 200 and len(selected) == 1 and selected[0]['key'] == request_key and all(selected[0]['body'][k] == v for k, v in body_extra['provider_options']['openai'].items()))
        status, replayed, _ = tts('request-precedence', body_extra)
        check('identical completed request replays without another provider call', status == 200 and replayed['request_id'] == original['request_id'] and len(marked_events(prefix + ' request-precedence')) == 1)
        changed = {**body_extra, 'provider_options': {'openai': {**body_extra['provider_options']['openai'], 'speed': 0.3}}}
        status, failure, _ = tts('request-precedence', changed)
        check('changed provider control conflicts under same idempotency key', status == 409 and failure['error']['code'] == 'idempotency_key_reused' and len(marked_events(prefix + ' request-precedence')) == 1)
        changed = {**body_extra, 'provider_keys': {'openai': key('different-request')}}
        status, failure, _ = tts('request-precedence', changed)
        check('changed request credential conflicts under same idempotency key', status == 409 and failure['error']['code'] == 'idempotency_key_reused' and len(marked_events(prefix + ' request-precedence')) == 1)
        if args.state_dir:
            rows = sql("SELECT secret_cipher FROM waveform_provider_keys WHERE provider='openai'")
            check('request key never overwrites saved encrypted key', request_key not in rows and rows.strip())
        status, _, _ = tts('saved-after-request')
        check('saved credential remains after request-only override', status == 200 and marked_events(prefix + ' saved-after-request')[0]['key'] == saved_key)

        invalid_key = key('rejected', 'fail-invalid_api_key')
        status, failure, raw = tts('key-rejected', {'provider_keys': {'openai': invalid_key}})
        selected = marked_events(prefix + ' key-rejected')
        check('rejected BYOK returns safe provider failure without fallback or shared retry', status == 502 and failure['error']['provider'] == 'openai' and failure['error']['reason'] == 'invalid_api_key' and failure['error']['provider_status'] == 401 and len(selected) == 1 and selected[0]['key'] == invalid_key and b'e2e-private-message-never-expose' not in raw and invalid_key.encode() not in raw)

        status, _, _ = call('/api/v1/provider-keys/openai', {}, 'DELETE')
        check('saved key removal succeeds', status == 204)
        saved_created = False
        status, _, _ = tts('shared-after-delete')
        check('removing saved key restores shared credential', status == 200 and marked_events(prefix + ' shared-after-delete')[0]['key'] == 'e2e-shared-openai')

        fallback_keys = {'openai': key('tts-fallback-openai', 'fail-rate_limited'), 'elevenlabs': key('tts-fallback-eleven')}
        status, result, _ = tts('fallback-enabled', {'auto_fallback': True, 'provider_order': ['openai', 'elevenlabs'], 'provider_keys': fallback_keys})
        selected = marked_events(prefix + ' fallback-enabled')
        check('explicit TTS fallback tries next provider after one first-provider attempt', status == 200 and result['provider'] == 'elevenlabs' and [x['provider'] for x in selected] == ['openai', 'elevenlabs'])

        stt_keys = {'deepgram': key('stt-deepgram', 'fail-invalid_api_key'), 'openai': key('stt-openai')}
        status, result, _ = call('/api/v1/stt', {'file_url': original['file_url'], 'provider_order': ['deepgram', 'openai'], 'provider_keys': stt_keys}, headers={'idempotency-key': prefix + '-stt', 'x-request-id': str(uuid.uuid4())})
        stt_events = [x for x in events() if x.get('key') in stt_keys.values()]
        check('STT automatically falls back with provider-scoped request keys', status == 200 and result['provider'] == 'openai' and result.get('transcript') and [x['provider'] for x in stt_events] == ['deepgram', 'openai'])

        status, jobs, raw = call('/api/v1/jobs?limit=100')
        check('job history contains no request or saved API key', status == 200 and all(value.encode() not in raw for value in secrets))
        status, listed, raw = call('/api/v1/provider-keys')
        check('request-only keys are not listed or saved', status == 200 and not any(k['provider'] == 'openai' for k in listed['items']) and all(value.encode() not in raw for value in secrets))
        if args.state_dir:
            stored = '\n'.join(sql(f'SELECT row_to_json(t)::text FROM {table} t') for table in ['waveform_jobs', 'waveform_idempotency_records', 'waveform_provider_keys'])
            check('database jobs, idempotency records and saved-key rows contain no plaintext key', all(value not in stored for value in secrets))
        if args.cli:
            with tempfile.TemporaryDirectory(prefix='waveform-cli-e2e-') as directory:
                local = Path(directory)
                environment = {k: v for k, v in os.environ.items() if k in ['PATH', 'HOME', 'TMPDIR', 'LANG']}
                environment.update({'SILICON_HOME': str(local / 'home'), 'WAVEFORM_AUTO_UPDATE': 'false'})
                def cli(*arguments):
                    return subprocess.run([str(args.cli.resolve()), '--url', args.backend, '--json', *arguments],
                                          cwd=local, env=environment, capture_output=True, text=True, timeout=60)
                login = cli('login', 'oac_waveform_e2e')
                check('real CLI logs in using isolated local credentials', login.returncode == 0)
                precise_key = key('cli-precise')
                keyfile = local / 'provider.key'
                keyfile.write_text(precise_key + '\n')
                keyfile.chmod(0o600)
                controls = {'elevenlabs': {'stability': 0, 'style': 0, 'seed': 0, 'use_speaker_boost': False, 'speed': 0.7}}
                optionsfile = local / 'options.json'
                optionsfile.write_text(json.dumps(controls))
                arguments = ['tts', prefix + ' cli-precise', '--org', 'tos', '--actor', '00000000-0000-4000-8000-000000000123',
                             '--provider', 'elevenlabs', '--provider-options-file', str(optionsfile),
                             '--provider-key-file', 'elevenlabs=' + str(keyfile), '--idempotency', prefix + '-cli-precise']
                first = cli(*arguments)
                selected = marked_events(prefix + ' cli-precise')
                check('CLI file-based BYOK and zero/false ElevenLabs controls reach real adapter', first.returncode == 0 and len(selected) == 1 and
                      selected[0]['key'] == precise_key and selected[0]['body']['seed'] == 0 and
                      all(selected[0]['body']['voice_settings'][k] == v for k, v in controls['elevenlabs'].items() if k != 'seed') and
                      precise_key not in first.stdout + first.stderr)
                replay = cli(*arguments)
                check('CLI repeats idempotent generation without another provider call', replay.returncode == 0 and
                      json.loads(replay.stdout)['request_id'] == json.loads(first.stdout)['request_id'] and len(marked_events(prefix + ' cli-precise')) == 1)
                rejected_key = key('cli-rejected', 'fail-quota_exceeded')
                keyfile.write_text(rejected_key)
                rejected = cli('tts', prefix + ' cli-rejected', '--org', 'tos', '--actor', '00000000-0000-4000-8000-000000000123',
                               '--provider', 'openai', '--provider-key-file', 'openai=' + str(keyfile), '--idempotency', prefix + '-cli-rejected')
                output = rejected.stdout + rejected.stderr
                check('CLI reports provider failure and reason without exposing key or raw upstream message', rejected.returncode != 0 and
                      'provider_failed' in output and 'quota' in output and rejected_key not in output and
                      'e2e-private-message-never-expose' not in output and len(marked_events(prefix + ' cli-rejected')) == 1)
        report = {'status': 'passed', 'scope': 'real gateway, Rust API, PostgreSQL, ffmpeg, SDK storage/delegation with loopback dummy upstreams', 'checks': checks, 'marker': prefix}
        if args.report:
            args.report.write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({'status': 'passed', 'checks': len(checks), 'marker': prefix}))
    finally:
        if saved_created:
            call('/api/v1/provider-keys/openai', {}, 'DELETE')


if __name__ == '__main__':
    main()
