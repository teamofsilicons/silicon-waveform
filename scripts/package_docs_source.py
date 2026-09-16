#!/usr/bin/env python3
"""Package an explicit source allowlist; never include local state or credentials."""
from pathlib import Path
import gzip
import hashlib
import io
import tarfile

root = Path(__file__).resolve().parent.parent
files = [root / 'rust-toolchain.toml']
for crate in ('cli', 'client'):
    base = root / crate
    files += [base / name for name in ('Cargo.toml', 'Cargo.lock', 'README.md', 'LICENSE')]
    for folder in ('src', 'docs'):
        if (base / folder).exists():
            files += [p for p in (base / folder).rglob('*') if p.is_file()]
archive = io.BytesIO()
with tarfile.open(fileobj=archive, mode='w', format=tarfile.PAX_FORMAT) as tar:
    for file in sorted(files):
        info = tar.gettarinfo(str(file), 'waveform/' + file.relative_to(root).as_posix())
        info.uid = info.gid = info.mtime = 0
        info.uname = info.gname = ''
        info.mode = 0o644
        with file.open('rb') as source:
            tar.addfile(info, source)
output = root / 'docs-site/waveform-source.tar.gz'
output.write_bytes(gzip.compress(archive.getvalue(), mtime=0))
output.with_suffix('.gz.sha256').write_text(hashlib.sha256(output.read_bytes()).hexdigest() + '  waveform-source.tar.gz\n')
print(f'Packaged {len(files)} source files ({output.stat().st_size} bytes).')
