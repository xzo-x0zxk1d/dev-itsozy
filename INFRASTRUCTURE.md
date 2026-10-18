# OZY infrastructure

Verified 2026-10-18. Never put credentials in this file.

## Accounts and routing
- Namecheap BasicDNS is authoritative: dns1.registrar-servers.com and dns2.registrar-servers.com.
- Only new host record: `dev CNAME 030d5bedb4ab1c61.vercel-dns-017.com.` (Automatic TTL).
- Existing apex A records, www CNAME and mail forwarding were preserved. Cloudflare is not required.
- Vercel project: `dev-itsozy`, ID `prj_GRmOXsnKlfcrgun8O9T1HbPo5tAq`.
- Scope: `ko8382slayerbeast-9839s-projects`, ID `team_cDcXi9XM4nzdGeQOhds8dCCl`, Hobby plan.
- Domain: https://dev.itsozy.lol; Vercel reports configured-correctly, ownership verified, no DNS conflicts.
- Google public DNS confirms the new CNAME. Local ISP resolver initially retained NXDOMAIN.
- TLS certificate validated by curl without bypassing certificate checks (using the verified Vercel address while local DNS was stale).
- GitHub: https://github.com/xzo-x0zxk1d/dev-itsozy (public, dedicated repository).
- Vercel GitHub integration connected; `main` triggers production; other branches trigger previews.
- First Git deployment: `af3fbe1e6c931737f66abc6853b4ca2c66b13d6c`, READY.
- Verified bootstrap: https://dev-itsozy-gjr637lnc-ko8382slayerbeast-9839s-projects.vercel.app
- `/api/v1/health` returned database=true, runtime=rust over production HTTPS.
- Official vercel_runtime 2.4.1 with Axum; Fluid Compute, iad1 region.

## Storage
- Supabase existing integration `icfg_guEbVhiPbthabX02ExanBgNp` on explicit Free ($0/month) plan.
- Production: `ozy-developer-db`, resource `store_IgAP6YKLP7sTXod8`; available and connected to production/development.
- Preview: `ozy-developer-preview`, resource `store_X3CfOd8j0qbJ15mN`; available and connected to preview.
- Both are PostgreSQL in iad1. Production SQL connectivity is verified by bootstrap SELECT 1.
- Connection variables are injected by the integration. Secret values never belong in Git or handoff documents.
- Names available: POSTGRES_URL, POSTGRES_URL_NON_POOLING, POSTGRES_PRISMA_URL, POSTGRES_HOST,
  POSTGRES_DATABASE, POSTGRES_USER, POSTGRES_PASSWORD, SUPABASE_URL and provider-managed Supabase keys.
- Local .env files and .vercel are ignored. D: is local development storage, never production persistence.

## Remaining infrastructure dependencies before full release
- Rust OCR/browser/PDF worker: optional and self-hostable, per owner's updated instruction.
  No Render dependency, account setup or deployment is required or authorized. No worker is deployed.
  Worker-dependent APIs must return an explicit 503 worker_unavailable until configured.
- Vercel Container Registry has documented storage charges; no container resource was enabled there.
- Frequent monitoring scheduler: to configure through Supabase pg_cron + pg_net calling the Rust service;
  Vercel Hobby cron alone only supports daily execution. No frequent scheduler is verified yet.
- Email sending service is not configured. Do not claim email verification until real delivery is tested.
- Database schema/migrations, transactional persistence tests, preview smoke tests and final production smoke tests remain.
- Application implementation is in progress; bootstrap is not the finished platform.

## Continuing deployment
From D:\dev.itsozy, `vercel project inspect --non-interactive` must identify the project above.
Push reviewed changes to main for production or a feature branch for previews. No registrar reconnection is needed.
The Vercel connector cannot access this team scope, but the installed Vercel CLI credentials work.
A different Claude environment must authenticate its own GitHub/Vercel CLI if it needs direct deployment;
do not transfer cookies, browser sessions or tokens. Git push uses normal Git Credential Manager authentication.

## Official references
- https://vercel.com/docs/functions/runtimes/rust
- https://vercel.com/docs/domains/working-with-domains/add-a-domain
- https://vercel.com/docs/cron-jobs/usage-and-pricing
- https://supabase.com/docs/guides/cron
- https://render.com/docs/free
