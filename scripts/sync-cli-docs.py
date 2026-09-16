#!/usr/bin/env python3
"""Keep the shipped CLI manuals identical to current product guides."""
import argparse
from pathlib import Path
root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--check', action='store_true')
args = parser.parse_args()
for target in (root / 'cli/docs').glob('*.md'):
    source = root / 'docs' / target.name
    if args.check:
        if source.read_bytes() != target.read_bytes():
            raise SystemExit(f'Outdated bundled guide: {target.name}; run scripts/sync-cli-docs.py')
    else:
        target.write_bytes(source.read_bytes())
