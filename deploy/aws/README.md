# Standalone Waveform on AWS

This deployment uses one `t4g.small` ARM64 EC2 instance in `us-east-1`, a fixed Elastic IP, and direct Caddy HTTPS.
It has no load balancer, target group, autoscaling group, or public database port. The server has 2 vCPUs, 2 GiB RAM,
32 GiB encrypted gp3 storage, and 2 GiB swap. AWS Systems Manager provides administration; SSH is not exposed.

API: https://backend.waveform.teamofsilicons.com · Website: https://waveform.teamofsilicons.com

Production runs the API natively under systemd (see [`deploy/native`](../native/README.md)); use that directory for
releases. `install.py` here builds a host from scratch with every service in a container (for example after
recreating the CloudFormation stack) and refuses a host that already runs the native API. The current resource IDs,
image references and secret ARN are recorded in `deployment.json`, which holds no credentials. The CloudFormation
definition is `standalone.json`.

## Services

Systemd manages `waveform-postgres`, `waveform-api`, `waveform-frontend` and `waveform-proxy`. Caddy routes each
hostname to its service; no application container publishes a host port. The website is the Next.js web built from
`web/` (a standalone Next.js server on port 4325). It signs Carbons in with Silicon Accounts and keeps each session in
a sealed cookie, so it keeps no state on the host and a restart signs nobody out. It runs unprivileged and read-only
with a 256 MiB memory limit.

Build the API image from the repository root and the website image from `web/` for `linux/arm64`. Push each to the
application ECR repository with a distinct immutable tag, then pass `--image` and `--frontend-image` to the installer.
The installer waits for both applications before activating the proxy. Later runs keep the installed website image
when `--frontend-image` is omitted, and keep the PostgreSQL and Caddy image digests. Back up the database before an
application release and keep the previous image references for rollback; schema migrations are forward-only.

## Configuration and activation

Secrets live in AWS Secrets Manager at `silicon-waveform/production`. The EC2 role may read only this secret and pull
only this application's ECR images, and may write database backups only to its dedicated backup prefix. Host
configuration is in `/etc/waveform` (mode 0700), with the env files mode 0600. Secrets are fetched on the host, never
placed in an image, CloudFormation user data, SSM command text or this repository.

The secret holds, as JSON:

| Key | What it is |
| --- | --- |
| `WAVEFORM_APP_SECRET` | Waveform's app secret at Silicon Accounts (`sa_app_…`). The website receives it too, through `/etc/waveform/web.env`. |
| `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET` | The `whsec_…` secret Silicon Accounts signs webhook deliveries with. |
| `WAVEFORM_PROOF_ISSUERS` | Apps allowed to call Waveform for an account, per scope (for example `waveform.tts=interface;waveform.stt=interface`). |
| `WAVEFORM_ENCRYPTION_KEY`, `WAVEFORM_IDEMPOTENCY_DIGEST_KEY` | Keep these across deployments: saved provider keys, Briefcase access and idempotency records depend on them. |
| `WAVEFORM_GEMINI_API_KEY`, `WAVEFORM_ELEVENLABS_API_KEY`, `WAVEFORM_OPENAI_API_KEY`, `WAVEFORM_DEEPGRAM_API_KEY` | The speech providers. |
| `WAVEFORM_POSTGRES_PASSWORD`, `POSTGRES_PASSWORD` | The application role's and the PostgreSQL superuser's passwords. |
| `WAVEFORM_POSTMARK_TOKEN` | Optional: bug-report mail. |

Settings of the earlier sign-in system that may still be in the record are ignored by the installer. Update only the
intended fields of the secret, using a private JSON file and `--secret-string file://...`; never expand secret values
into shell arguments. The local `.env` and `.local-deploy` directory are excluded from Git and Docker.

`install.py` fetches the secret, generates the private runtime configuration (a copy of the repository's
`.env.example` must be beside it as `defaults.env`), installs the systemd services and pins the container images by
digest. Without `--start-api` it prepares PostgreSQL and HTTPS while the public API answers 503:

```sh
python3 /opt/waveform-deploy/install.py \
  --secret-arn '<SecretArn from deployment.json>' \
  --image '<API image digest>' \
  --frontend-image '<website image digest>' \
  --backup-bucket '<BackupBucket from deployment.json>' \
  --region us-east-1 \
  --start-api
```

Activation first proves Silicon Accounts accepts Waveform's app credentials (introspecting a token that does not
exist must answer `{"active": false}`), waits for the API's readiness route, then switches the HTTPS proxy from 503 to
the application. Afterwards check `/health/live`, `/health/ready`, `/api/v1/capabilities` and that
`/api/v1/me` refuses a request without a token, at the public hostname. For an authenticated check, sign in with the
CLI (`waveform login`) and run `waveform me`.

Silicon Accounts posts account events to `https://backend.waveform.teamofsilicons.com/webhooks/accounts`. Register it
with `PUT /v1/apps/waveform/webhook` and `{"url": "…/webhooks/accounts", "events": null}` (app credentials), and store
the `whsec_…` secret the first save shows as `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET`. `"events": null` picks every update:
Waveform needs `custodian_change`, which the recommended picks of `silicon-apps webhook waveform set` leave out. The
exact commands are in [`docs/migration/cutover.md`](../../docs/migration/cutover.md).

Use `ssm.py --instance <InstanceId> --script <file>` to dispatch a shell script, and `ssm.py --instance <InstanceId>
--status <CommandId>` to read its result. Send only scripts without embedded credentials: SSM records command text and
output.

## Runtime and storage

The API runs as the image's unprivileged user with a read-only filesystem, dropped capabilities, bounded memory and
request concurrency, and a temporary filesystem. Only Caddy publishes ports 80/443. Containers cannot use the host's
IMDS credentials: IMDSv2 is required with a response hop limit of one.

PostgreSQL stores its data in `/var/lib/waveform/postgres` and accepts TLS connections only. Waveform connects with
`sslmode=verify-full` and a private CA; its database owner has no PostgreSQL superuser privileges. The application
applies its forward-only schema migrations at startup. The internal server certificate expires after 825 days; renew it
under the retained private CA before then and restart PostgreSQL. Caddy renews the public HTTPS certificates.

`waveform-backup.timer` runs a daily compressed `pg_dump` to the private encrypted backup bucket. Dumps expire after 14
days. Verify a first backup manually with `systemctl start waveform-backup` and an S3 listing, and rehearse a restore
in a separate database before relying on backups. The root EBS volume is retained on instance termination, and the
backup bucket is retained on stack deletion.

This is a small, single-server deployment: API or PostgreSQL restarts cause brief downtime and there is no failover.
Generated speech lives in each account's Briefcase files, or for up to the idempotency retention in Waveform's own
database when Briefcase could not take it. Silicon Accounts and Briefcase are external dependencies.

## Updates and rollback

Build the current source for `linux/arm64` and push an immutable ECR tag. Never reuse a tag; keep the previous image
digest. Re-run the installer with the new image and `--start-api`, preserving the secret, database and Caddy data.
Check readiness and authentication after each update. For an application rollback restore the previous image
reference; never roll migrations backward.

Namecheap holds the A record `backend.waveform` (TTL 300) pointing to the Elastic IP. Keep that IP fixed when
updating the application.

References: [Caddy automatic HTTPS](https://caddyserver.com/docs/automatic-https),
[PostgreSQL TLS configuration](https://www.postgresql.org/docs/current/ssl-tcp.html),
[AWS instance metadata](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/ec2-instance-metadata.html).
