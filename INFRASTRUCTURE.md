# OZY infrastructure

Verified 2026-10-18. Never put credentials in this file. All values below are identifiers, not secrets.

## Summary

| Component | Status |
|---|---|
| Namecheap (registrar + DNS) | Configured: one record added |
| Cloudflare | Not used (Namecheap BasicDNS is authoritative) |
| Vercel project | Configured, Git-connected, production deploys from `main` |
| Domain `dev.itsozy.lol` | Verified (Vercel: configured correctly, ownership verified) |
| HTTPS | Verified (valid certificate served by Vercel) |
| Database | Supabase PostgreSQL (Free plan): production and preview, migrated |
| Scheduler | Supabase pg_cron every minute (verified firing) + Vercel daily cron fallback |
| Repository | https://github.com/xzo-x0zxk1d/dev-itsozy (public) |
| CI | GitHub Actions `.github/workflows/ci.yml` (fmt, clippy, tests, worker with OCR + browser) |
| Worker | Not deployed (optional; no hosting authorized). Worker-only APIs return 503 `worker_unavailable` |
| Email delivery | Not configured (no verification or password-reset emails) |

## DNS

- Namecheap BasicDNS is authoritative for `itsozy.lol` (dns1/dns2.registrar-servers.com).
- Added host record: `dev CNAME 030d5bedb4ab1c61.vercel-dns-017.com.` (Automatic TTL).
- Existing apex A records, the `www` CNAME and the email-forwarding records were left unchanged.

## Vercel

- Project `dev-itsozy`, ID `prj_GRmOXsnKlfcrgun8O9T1HbPo5tAq`.
- Scope `ko8382slayerbeast-9839s-projects`, team ID `team_cDcXi9XM4nzdGeQOhds8dCCl`, Hobby plan.
- Runtime: official `vercel_runtime` 2.4 (Rust, Axum) built from `api/entry.rs`; Fluid Compute; region iad1; `maxDuration` 60 s.
  `vercel.json` rewrites every path to `/api/entry` and defines a daily cron (`17 4 * * *`) for `/api/v1/internal/tick`.
- Git integration: pushes to `main` deploy production (Rust build takes about 4 minutes); other branches deploy previews.
- Production domain: https://dev.itsozy.lol. The local `.vercel/project.json` (gitignored) links `D:\dev.itsozy` to the project.
- Environment variables (names only):
  - From the Supabase integration: `POSTGRES_URL`, `POSTGRES_URL_NON_POOLING`, `POSTGRES_PRISMA_URL`, `POSTGRES_HOST`,
    `POSTGRES_USER`, `POSTGRES_PASSWORD`, `POSTGRES_DATABASE`, `SUPABASE_URL`, `SUPABASE_ANON_KEY`,
    `SUPABASE_SERVICE_ROLE_KEY`, `SUPABASE_SECRET_KEY`, `SUPABASE_PUBLISHABLE_KEY`, `SUPABASE_JWT_SECRET`, `NEXT_PUBLIC_SUPABASE_*`.
  - Platform (added 2026-10-18 with `vercel env add --sensitive`): `OZY_ENCRYPTION_KEY` and `CRON_SECRET`
    (different values for production and preview), and `PUBLIC_ORIGIN=https://dev.itsozy.lol` (production only).
  - Not set: `OZY_WORKER_URL`, `OZY_WORKER_SECRET` (no worker deployed).
- Sensitive variables cannot be read back. To rotate one, generate a new value, `vercel env add NAME production --sensitive --force`,
  and redeploy. For `CRON_SECRET`, also re-run `ozy-ops schedule` (see Scheduler).

## Database (Supabase, Free plan, us-east-1 / iad1)

- Integration `icfg_guEbVhiPbthabX02ExanBgNp`.
- Production `ozy-developer-db` (`store_IgAP6YKLP7sTXod8`), connected to Vercel **Production and Development**.
  Locally, `.env.development.local` points at it, so treat it as production.
- Preview `ozy-developer-preview` (`store_X3CfOd8j0qbJ15mN`), connected to Vercel Preview; used by local development and integration tests (`.env.preview.local`).
- Schema `ozy`, migrations `0001_platform` and `0002_jobs` applied to **both** databases. Row-level security is enabled
  and the `anon`/`authenticated` roles have no grants, so Supabase's REST API exposes nothing.
- Connections use the Supavisor **session-mode** pooler (port 5432). `ozy-db` rewrites the integration's transaction-mode
  URL (6543) automatically, because sqlx's named prepared statements fail on transaction pooling.

## Scheduler

- Supabase `pg_cron` job `ozy-tick`, schedule `* * * * *`, runs `pg_net` `POST https://dev.itsozy.lol/api/v1/internal/tick`
  with `Authorization: Bearer <secret>`. The secret is stored in Supabase Vault as `ozy_cron_secret` and is not in the job text.
- Installed with `ozy-ops --env-file .env.development.local schedule https://dev.itsozy.lol` (with the production `CRON_SECRET` in the environment).
- Verified: `/api/v1/status` reports `scheduler.last_tick` advancing every minute (about 100–180 ms per tick).
- Fallback: the Vercel cron in `vercel.json` (Hobby plan allows daily jobs) calls the same endpoint with the same `CRON_SECRET`.

## Verified deployment

- Production deployment of commit `c1b85c5`: `/api/v1/health` returned `database: true`, every page returned 200, and the security headers
  (CSP, HSTS, X-Frame-Options, x-request-id) were present.
- Production smoke test (every API through https, test account removed afterwards): all Vercel-hosted APIs passed;
  OCR and screenshot returned `503 worker_unavailable` as designed; the SSRF probe was blocked with 403.
  Median server-side duration was 36 ms (p95 144 ms, excluding a 4 s polite crawl).
- The latest deployment URL and commit are shown by `vercel list dev-itsozy --scope ko8382slayerbeast-9839s-projects`.

## How automatic deployments work

1. `git push origin main` → GitHub → Vercel builds `api/entry.rs` with the Rust runtime → production at dev.itsozy.lol.
2. Any other branch → a preview deployment on the preview database (preview URLs use Vercel deployment protection).
3. Migrations are not automatic. Run `ozy-ops migrate` against preview, then production, before pushing code that depends on them.
4. GitHub Actions runs CI on every push and pull request; it does not deploy.

## Access for future work

- On this PC, the Vercel CLI (scope above) and Git Credential Manager (GitHub) are already authenticated, so no dashboard visits are needed.
- The Codex Vercel connector cannot access this team scope; use the CLI.
- Another machine or agent must authenticate its own GitHub/Vercel CLI. Never copy cookies, tokens or browser sessions.
- No Namecheap or Cloudflare changes are needed for further development.

## Optional worker deployment (not done)

`crates/ozy-worker/Dockerfile` builds a container with Tesseract and Chromium. It needs a Docker host with at least 1 GiB of RAM.
Choosing and paying for (or approving a free) host is the owner's decision; Render was explicitly declined. After deploying:
`vercel env add OZY_WORKER_URL production` and `vercel env add OZY_WORKER_SECRET production --sensitive`, then redeploy.

## References

- https://vercel.com/docs/functions/runtimes/rust
- https://vercel.com/docs/cron-jobs/usage-and-pricing
- https://supabase.com/docs/guides/cron
- https://supabase.com/docs/guides/database/connecting-to-postgres#connection-pooler
