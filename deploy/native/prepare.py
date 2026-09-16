#!/usr/bin/env python3
"""Prepare a private resolver entry and CA for the existing PostgreSQL service."""
import json
from pathlib import Path
import shutil
import subprocess

record = json.loads(subprocess.check_output(["/usr/bin/docker", "inspect", "waveform-postgres"]))[0]
assert record["State"]["Running"], "PostgreSQL container is not running"
address = record["NetworkSettings"]["Networks"]["waveform"]["IPAddress"]
assert address, "PostgreSQL has no Waveform network address"
runtime = Path("/run/waveform")
runtime.mkdir(exist_ok=True)
hosts = Path("/etc/hosts").read_text()
hosts = "\n".join(line for line in hosts.splitlines() if "waveform-native-postgres" not in line)
(runtime / "hosts").write_text(hosts + f"\n{address} postgres # waveform-native-postgres\n")
(runtime / "hosts").chmod(0o644)
shutil.copyfile("/etc/waveform/postgres-tls/ca.crt", runtime / "db-ca.crt")
(runtime / "db-ca.crt").chmod(0o644)
