# Operator environment variables

The repository names no production host, account or key. Scripts and runbooks
that reach production read these variables from the operator's shell. Keep the
values in a private place (a password manager or an untracked `~/.mazimi-ops.env`),
never in this repository.

| Variable | Meaning | Read by |
|---|---|---|
| `MAZIMI_HOST` | Production server address (IP or DNS name) | `scripts/forms/auto/run.sh`; DNS and console runbooks (`docs/mazimi-gabia-domain-setup.md`, `docs/mazimi-temporary-domain-console-setup.md`) |
| `MAZIMI_SSH_USER` | SSH login on that server | `scripts/forms/auto/run.sh` |
| `MAZIMI_SSH_KEY` | Path to the SSH private key for that login | `scripts/forms/auto/run.sh` |
| `MAZIMI_SQL_RO` | Server-side command that runs SQL from stdin with a read-only database role | `scripts/forms/auto/run.sh`, `scripts/forms/auto/*.sql` |
| `MAZIMI_DEPLOY_DIR` | Checkout directory on the server, default `/opt/mazimi` | `scripts/oracle-bootstrap.sh`; `scripts/deploy.sh` rewrites the systemd units to the checkout it runs from |
| `MAZIMI_API_URL` | Public API origin, e.g. `https://api.example.org` | `scripts/forms/auto/verify-prod.sh`, `scripts/forms/import-templates.mjs`, mobile builds (`EXPO_PUBLIC_API_URL`), runbooks |
| `MAZIMI_APP_URL` | Public web app origin | README, team and store guides |

Scripts that need a variable stop with `set MAZIMI_…` when it is missing; none
falls back to a built-in host.

Server-side settings (database, OAuth, crawler identity such as
`CORPORATE_BOT_CONTACT_URL`) live in `.env.production` on the server; see
`.env.production.example`.

## Mobile builds

`apps/mobile/eas.json` carries no API origin. Each build profile names its EAS
environment (`development`, `preview`, `production`); set
`EXPO_PUBLIC_API_URL` to `MAZIMI_API_URL` in each of them once:

```bash
cd apps/mobile
npx eas-cli env:create --environment production --name EXPO_PUBLIC_API_URL \
  --value "$MAZIMI_API_URL" --visibility plaintext
# repeat for --environment preview and --environment development
```

A release build without it stops at start (`lib/apiBaseUrl.ts`) rather than
calling a wrong host.
