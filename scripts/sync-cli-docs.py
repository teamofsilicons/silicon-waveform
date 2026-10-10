#!/usr/bin/env python3
"""Keep the shipped CLI manuals and package READMEs identical to the product guides in docs/.

Pairs (copy, source):
  cli/docs/<name>.md  <- docs/<name>.md   (bundled guides: `waveform docs <topic>`)
  cli/README.md       <- docs/cli.md      (`waveform --help` long help and `waveform docs cli`)
  client/README.md    <- docs/client.md   (the crate's README on crates.io)
"""
import argparse
from pathlib import Path

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument('--check', action='store_true', help='fail when a copy differs from its source')
args = parser.parse_args()

pairs = [(target, root / 'docs' / target.name) for target in sorted((root / 'cli/docs').glob('*.md'))]
pairs += [(root / 'cli/README.md', root / 'docs/cli.md'), (root / 'client/README.md', root / 'docs/client.md')]

for target, source in pairs:
    if not source.is_file():
        raise SystemExit(f'Bundled guide {target.relative_to(root)} has no source {source.relative_to(root)}')
    if args.check:
        if source.read_bytes() != target.read_bytes():
            raise SystemExit(f'Outdated copy: {target.relative_to(root)} differs from {source.relative_to(root)}; run scripts/sync-cli-docs.py')
    else:
        target.write_bytes(source.read_bytes())
