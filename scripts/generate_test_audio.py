#!/usr/bin/env python3
"""Generate checked-in test speech once; runtime test requests never call providers.

Uses GEMINI_API_KEY (and DEEPGRAM_API_KEY with --verify), or reads the existing
Waveform AWS secret into memory with --aws-secret. Never logs credentials.
"""
import argparse
import base64
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
MODEL = 'gemini-3.1-flash-tts-preview'


def request_json(url, body, headers):
    request = urllib.request.Request(url, data=body, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=180) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        # Provider error bodies can include request/configuration details.
        raise RuntimeError(f'Provider request rejected (HTTP {error.code})') from None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--voice', action='append', help='Profile slug; repeat or use all. Defaults to kore.')
    parser.add_argument('--output', type=Path, default=ROOT/'src/infrastructure/test-audio')
    parser.add_argument('--aws-secret', action='store_true', help='Read keys using deploy/aws/deployment.json')
    parser.add_argument('--verify', action='store_true', help='Transcribe generated audio with Deepgram for content QA')
    args = parser.parse_args()
    keys = dict(os.environ)
    if args.aws_secret:
        deployment = json.loads((ROOT/'deploy/aws/deployment.json').read_text())
        secret = json.loads(subprocess.check_output([
            'aws', '--profile', deployment['Profile'], '--region', deployment['Region'],
            'secretsmanager', 'get-secret-value', '--secret-id', deployment['SecretArn'],
            '--query', 'SecretString', '--output', 'text',
        ], text=True))
        keys['GEMINI_API_KEY'] = secret['WAVEFORM_GEMINI_API_KEY']
        if args.verify:
            keys['DEEPGRAM_API_KEY'] = secret['WAVEFORM_DEEPGRAM_API_KEY']
    if not keys.get('GEMINI_API_KEY') or (args.verify and not keys.get('DEEPGRAM_API_KEY')):
        parser.error('Required provider credentials are absent')
    source = (ROOT/'src/infrastructure/testing.rs').read_text()
    text = re.search(r'pub const TEST_TTS_TEXT: &str = "(.*?)";', source).group(1)
    profiles = json.loads((ROOT/'src/domain/voice_profiles.json').read_text())
    wanted = set(args.voice or ['kore'])
    if wanted != {'all'} and wanted - {p['id'] for p in profiles}:
        parser.error('Unknown voice profile')
    args.output.mkdir(parents=True, exist_ok=True)
    manifest_path = args.output/'manifest.json'
    manifest = json.loads(manifest_path.read_text()) if manifest_path.exists() else {'text': text, 'model': MODEL, 'clips': {}}
    if manifest['text'] != text or manifest['model'] != MODEL:
        parser.error('Existing manifest uses different text/model; choose a new output directory')
    for profile in profiles:
        slug = profile['id']
        if wanted != {'all'} and slug not in wanted:
            continue
        target = args.output/f'{slug}.mp3'
        if target.exists() and slug in manifest['clips']:
            if hashlib.sha256(target.read_bytes()).hexdigest() != manifest['clips'][slug]['sha256']:
                raise RuntimeError(f'{slug}: existing asset hash differs from manifest')
            print(f'{slug}: already generated', flush=True)
            continue
        response = request_json('https://generativelanguage.googleapis.com/v1beta/interactions',
            json.dumps({'model':MODEL, 'input':text, 'response_format':{'type':'audio'},
                'generation_config':{'speech_config':[{'voice':profile['gemini_voice']}]}}).encode(),
            {'x-goog-api-key':keys['GEMINI_API_KEY'],'Content-Type':'application/json'})
        if response.get('status') != 'completed':
            raise RuntimeError(f'{slug}: generation did not complete')
        audio = [part for step in response.get('steps',[]) for part in step.get('content',[]) if part.get('type') == 'audio'][-1]
        raw = base64.b64decode(audio['data'], validate=True)
        mime = audio.get('mime_type','audio/l16').lower()
        input_args = []
        if mime.split(';')[0] == 'audio/l16':
            input_args = ['-f','s16le','-ar',str(audio.get('sample_rate',24000)),'-ac',str(audio.get('channels',1))]
        elif mime not in ('audio/wav','audio/x-wav','audio/mpeg','audio/mp3'):
            raise RuntimeError(f'{slug}: unexpected audio format')
        with tempfile.TemporaryDirectory(prefix='waveform-demo-') as folder:
            temporary = Path(folder)/'clip.mp3'
            subprocess.run(['ffmpeg','-hide_banner','-loglevel','error',*input_args,'-i','pipe:0',
                '-map_metadata','-1','-ac','1','-ar','44100','-codec:a','libmp3lame','-b:a','96k',
                '-write_xing','0','-id3v2_version','3',str(temporary)],input=raw,check=True)
            subprocess.run(['ffmpeg','-v','error','-i',str(temporary),'-f','null','-'],check=True)
            probe=json.loads(subprocess.check_output(['ffprobe','-v','error','-count_frames','-show_streams','-of','json',str(temporary)]))
            data=temporary.read_bytes()
        frames=int(probe['streams'][0]['nb_read_frames'])
        duration_ms=(frames*1152*1000+44099)//44100
        if not 8000 <= duration_ms <= 45000 or len(data)>1048576:
            raise RuntimeError(f'{slug}: unexpected fixture duration/size')
        entry={'voice':profile['gemini_voice'],'profile_revision':profile['revision'],
            'sha256':hashlib.sha256(data).hexdigest(),'duration_ms':duration_ms,'bytes':len(data),
            'sample_rate':44100,'channels':1,'generated_at':datetime.now(timezone.utc).isoformat()}
        if args.verify:
            transcription=request_json('https://api.deepgram.com/v1/listen?model=nova-3&language=en&smart_format=true&mip_opt_out=true',
                data,{'Authorization':'Token '+keys['DEEPGRAM_API_KEY'],'Content-Type':'audio/mpeg'})
            entry['qa_transcript']=transcription['results']['channels'][0]['alternatives'][0]['transcript']
        target.write_bytes(data)
        manifest['clips'][slug]=entry
        manifest_path.write_text(json.dumps(manifest,indent=2)+'\n')
        print(f'{slug}: {duration_ms/1000:.2f}s, {len(data)} bytes'+(f"; QA: {entry['qa_transcript']}" if args.verify else ''),flush=True)


if __name__ == '__main__':
    main()
