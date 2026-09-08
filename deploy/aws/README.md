# Standalone Waveform on AWS

This deployment uses one `t4g.small` ARM64 EC2 instance in `us-east-1`, a fixed
Elastic IP, and direct Caddy HTTPS. It has no load balancer, target group,
autoscaling group, or public database port. The server has 2 vCPUs, 2 GiB RAM,
32 GiB encrypted gp3 storage, and 2 GiB swap. AWS Systems Manager provides
administration; SSH is not exposed.

Public endpoint: https://backend.waveform.teamofsilicons.com

Frontend endpoint: https://waveform.teamofsilicons.com

The SolidJS frontend and its Node session gateway run in a separate, unprivileged
container on the same instance. Caddy routes each hostname directly to its service;
neither application container publishes a host port. The frontend has a 192 MiB
memory limit and keeps sessions in one process. Restarting it signs users out.

Build `frontend/Dockerfile` using `frontend` as its build context for `linux/arm64`.
Push its image to the existing application ECR repository with a distinct immutable
tag, then pass `--frontend-image <image>` to the installer along with the backend
image. The installer waits for both applications before activating the proxy.
Subsequent runs retain the installed frontend image when that option is omitted,
and retain the PostgreSQL and Caddy image digests. Back up the database before
applying an application release. Keep the previous application image references
for rollback; schema migrations are forward-only.

The current resource IDs, immutable application image reference, and secret
ARN are recorded in `deployment.json`. That file contains no credentials.
The CloudFormation definition is `standalone.json`.

## First deployment status

Infrastructure, DNS, a trusted HTTPS certificate, PostgreSQL, and all four
user-supplied provider keys were installed on 2026-09-08. Production IAM accepts the existing `tos>waveform` application credentials
after correcting a deployment import that retained dotenv quotation marks.
The API is active and public liveness, readiness, and capabilities checks return
200. Protected account, jobs, and provider-key routes reject unauthenticated
requests with 401 when the required organization header is present. A malformed
login code returns 400. All three services and the backup timer are active, and
the first database dumps were verified in S3. ElevenLabs also returned
`payment_required` for a short synthesis check. Its account-read permission is
restricted; successful synthesis remains unverified until the billing issue is resolved. Gemini, OpenAI and Deepgram credential checks succeeded.

The initial production readiness check exposed an FFmpeg stdin pipe that was
left open. Closing the pipe before waiting for FFmpeg fixed readiness; a real
FFmpeg regression test now covers the behavior. Validation after the fix passed
141 backend unit tests (5 database-dependent tests excluded from this run),
formatting, and Clippy. Earlier full integration results are recorded separately
in `docs/test-report-2026-09-08.md`. A successful real-user production login and
paid end-to-end provider generation have not been verified in this deployment.

## Configuration and activation

Secrets live in AWS Secrets Manager at `silicon-waveform/production`. The EC2
role may read only this application secret and pull only this application's
ECR images. It may write database backups only to its dedicated backup prefix.
Host configuration is in `/etc/waveform`, mode 0700, with API and PostgreSQL env
files mode 0600. Secrets are fetched on the host, not placed in the image,
CloudFormation user data, SSM command text, or this repository.

Preserve the existing encryption key, idempotency digest key, and both generated
database passwords across deployments. Replacing those independently of their
persisted data breaks authentication/decryption. Update only the intended fields
in the current Secrets Manager JSON, using a private JSON file and
`--secret-string file://...`. Never expand secret values into shell arguments.
The local `.env` and `.local-deploy` directory are excluded from Git and Docker.

`install.py` fetches the secret, generates private runtime configuration, installs
systemd services, and pins the downloaded container images by digest. A copy of
the repository's `.env.example` must be beside it as `defaults.env`. Installation
without `--start-api` prepares PostgreSQL and HTTPS while returning public 503.

To deploy an updated immutable image, upload the current public installer and
defaults through SSM and run on the instance:

```sh
python3 /opt/waveform-deploy/install.py \
  --secret-arn '<SecretArn from deployment.json>' \
  --image '<ImageUri from deployment.json>' \
  --backup-bucket '<BackupBucket from deployment.json>' \
  --region us-east-1 \
  --start-api
```

Activation verifies the IAM application credentials, waits for the API readiness
route, and only then changes the HTTPS proxy from 503 to the application.
Confirm `/health/live`, `/health/ready`, capabilities, and authentication rejection
at the public hostname after every deployment. Use a real IAM SLT for an
authenticated production smoke test when one is available. Login uses an IAM `oac_` short-lived code. The webhook
registration must point to `https://backend.waveform.teamofsilicons.com/webhook/`
and use the same signing secret as the server. Approval of a new IAM webhook
is an IAM control-plane operation requiring the appropriate organization actor.

Use `ssm.py --instance <InstanceId> --script <file>` to dispatch a shell script,
and `ssm.py --instance <InstanceId> --status <CommandId>` to read its result.
Only send scripts without embedded credentials: SSM records command text/output.

## Runtime and storage

Systemd manages `waveform-postgres`, `waveform-api`, and `waveform-proxy`.
The API runs as the image's unprivileged user with a read-only filesystem,
dropped capabilities, bounded memory and request concurrency, and a temporary
filesystem. Only Caddy publishes ports 80/443. Containers cannot use the host's
IMDS credentials: IMDSv2 is required with a response hop limit of one.

PostgreSQL stores its data in `/var/lib/waveform/postgres`. Its network accepts
TLS connections only. Waveform connects with `sslmode=verify-full` and a
private CA; its database owner has no PostgreSQL superuser privileges. The
application performs its existing forward-only schema migrations at startup.
The internal server certificate expires after 825 days; renew it under the
retained private CA before expiration and restart PostgreSQL. Public HTTPS
certificate renewal is handled by Caddy.

`waveform-backup.timer` runs a daily compressed `pg_dump` to
the private encrypted backup bucket. Dumps expire after 14 days. Verify a first
backup manually with `systemctl start waveform-backup` and an AWS S3 listing.
A restore should be rehearsed in a separate database before relying on backups.
The initial root EBS volume is retained on instance termination, and the backup
bucket is retained on stack deletion. Removing the stack does not automatically
erase those data resources.

This is a small, single-server deployment. API or PostgreSQL restarts cause
brief downtime; there is no failover. TTS audio lives in Briefcase, not on this
server. Production IAM and Briefcase remain external dependencies.

## Updates and rollback

Build the current source for `linux/arm64` and push an immutable ECR tag. Do not
reuse a tag; retain the previous image digest. Re-run the installer with the new
image and `--start-api`, preserving the secret, database, and Caddy data. Check
readiness and authentication after each update. For an application rollback,
restore the previous image reference; do not roll migrations backward.

Namecheap CLI created an A record named `backend.waveform` with TTL 300 pointing
to the Elastic IP. The DNS update was checked to preserve every unrelated
record. Keep that IP fixed when updating the application.

Useful references: [Caddy automatic HTTPS](https://caddyserver.com/docs/automatic-https),
[PostgreSQL TLS configuration](https://www.postgresql.org/docs/current/ssl-tcp.html),
[AWS instance metadata](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/ec2-instance-metadata.html).
