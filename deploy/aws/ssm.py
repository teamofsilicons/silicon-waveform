#!/usr/bin/env python3
"""Dispatch a shell script to the standalone Waveform host, or read its status."""
import argparse
import json
from pathlib import Path
import subprocess

p=argparse.ArgumentParser()
p.add_argument('--profile',default='silicon-production')
p.add_argument('--region',default='us-east-1')
p.add_argument('--instance',required=True)
p.add_argument('--script',type=Path)
p.add_argument('--status',metavar='COMMAND_ID')
p.add_argument('--comment',default='Waveform deployment maintenance')
a=p.parse_args()
base=['aws','--profile',a.profile,'--region',a.region,'ssm']
if bool(a.script)==bool(a.status):p.error('provide exactly one of --script or --status')
if a.status:
 command=[*base,'get-command-invocation','--command-id',a.status,'--instance-id',a.instance,'--output','json']
else:
 command=[*base,'send-command','--instance-ids',a.instance,'--document-name','AWS-RunShellScript','--comment',a.comment,'--parameters',json.dumps({'commands':[a.script.read_text()],'executionTimeout':['1200']}),'--query','Command.CommandId','--output','text']
subprocess.run(command,check=True)
