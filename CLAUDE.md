# CLAUDE.md: OZY Developer Platform

Free developer APIs in Rust, live at **https://dev.itsozy.lol**. Read this first, then
[INFRASTRUCTURE.md](INFRASTRUCTURE.md) for accounts and deployment details. Never commit or print
secrets (`.env*`, `.vercel/` and `work/` are gitignored).

## Architecture

```
Browser / API client
   │  HTTPS (Vercel edge, TLS, rewrites every path to /api/entry)
   ▼
api/entry.rs  ── official vercel_runtime 2.4 (Axum), Fluid Compute, iad1, maxDuration 60 s
   │  ozy_api::router(App)
   ├─ web.rs       server-rendered pages + /assets/{style.css,app.js} + /openapi.json
   ├─ account.rs   register/login/sessions, API keys, usage, logs, settings, account deletion
   ├─ services.rs  stateless APIs (scrape, preview, intelligence, performance, url, email, pdf, ocr, screenshot)
   ├─ jobs.rs      monitors, saved extractions, crawls, webhook relay, scheduler tick
   └─ lib.rs       App state, auth middleware, quotas, security headers, error envelope
        │                         │                                   │
        ▼                         ▼                                   ▼
 Supabase PostgreSQL       ozy-core::net::SafeHttp            Optional ozy-worker (self-hosted)
 (schema `ozy`, iad1)      SSRF-safe pinned egress            Tesseract OCR + headless Chromium
        ▲                                                     behind a validating egress proxy
        │ pg_cron (every minute) → pg_net POST /api/v1/internal/tick (Bearer secret from Vault)
```

## Workspace

| Path | Purpose |
|---|---|
| `api/entry.rs` | Vercel function binary (`entry`). Falls back to `unconfigured_router` if env is missing |
| `src/local.rs` | `ozy-local`: local server on 127.0.0.1:3917 (ports 3000/3100 are taken by other apps on this PC) |
| `src/ops.rs` | `ozy-ops`: `migrate`, `status`, `schedule <origin>`, `unschedule` |
| `crates/ozy-core` | `net.rs` SafeHttp (validation, DNS pinning, redirects, robots cache, pacing, size caps), `tls.rs` certificate inspection |
| `crates/ozyscrape` | OzyScrape engine: parsing, CSS + XPath subset, rules, fingerprints, adaptive relocation, metadata, Markdown, `crawl.rs` checkpointed batches |
| `crates/ozy-auth` | Argon2id passwords, tokens, SHA-256 key hashing, HMAC signatures (OZY + GitHub style), AES-256-GCM |
| `crates/ozy-db` | Pool (Supabase session mode), migrations, atomic `take_budget` rate limiter |
| `crates/ozy-api` | Router, handlers, catalog (single source for docs/OpenAPI/playground), assets, integration tests |
| `crates/ozy-worker` | Optional worker: `preprocess.rs` (grayscale/Otsu/resize), `ocr.rs` (feature `ocr`), `browser.rs` (feature `browser`), `proxy.rs` (egress filter), `Dockerfile` |
| `migrations/` | `0001_platform.sql`, `0002_jobs.sql` (sqlx migrations, embedded at compile time) |
| `bench/` | Benchmark harness (Rust runner is `crates/ozyscrape/examples/compare.rs`); results in `bench/results.json` |
| `.github/workflows/ci.yml` | fmt + clippy + tests; worker built and tested with `--features ocr,browser` on Ubuntu |

## Conventions

- Rust 2024, `rustfmt.toml` (max_width 140). `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` must be clean.
- Handlers return `ozy_api::Result<T>`; errors are `Failure(StatusCode, code, message)` and render as
  `{"error":{"code","message"}}`. Never leak internal errors; `From<sqlx::Error>` logs and returns 503 `database_unavailable`.
- **All untrusted outbound HTTP must go through `app.http` (SafeHttp)** or `ozy_core::net::validate_url` + `resolve_public`.
  Never call `reqwest` directly with user input. Worker calls are operator-configured and exempt.
- Use `domain_budget(&app, url)` before user-triggered fetches (1 request/s per host, shared across instances).
- Do not hold `ozyscrape::Document` (non-`Send`) across `.await`; parse inside a block, as in `services::extract`.
- New API → add the handler, a route in `lib.rs::router`, a permission name in `account::create_key`'s allowed list,
  a `catalog::APIS` entry (docs, playground and OpenAPI come from it), and tests.
- SQL: runtime `sqlx::query*` with binds (no compile-time macros, so builds need no database). Schema lives in `ozy.*`.
  Add migrations as new files `migrations/000N_*.sql`. Never edit applied ones; checksums are verified.
- Frontend: no inline scripts or styles (CSP: `script-src 'self'; style-src 'self'`). Insert text via `textContent`;
  set styles via CSSOM (`el.style.*`). Pages are server-rendered strings in `web.rs`; escape dynamic text with `esc()`.
- Honesty rule: never report a worker-dependent feature as working when `OZY_WORKER_URL` is unset, and never invent benchmark numbers.

## Database schema (`ozy` schema, RLS enabled, no anon/authenticated grants)

| Table | Key columns |
|---|---|
| `users` | id, email (unique, lowercase), password_hash (Argon2id), display_name, email_verified |
| `sessions` | token_hash (SHA-256), user_id, expires_at (7 days) |
| `api_keys` | id, user_id, name, prefix, token_hash, permissions text[], revoked_at, last_used_at |
| `budgets` | (key, bucket) → used; fixed-window counters for quotas and auth rate limits |
| `request_logs` | user_id, key_id, endpoint, status, duration_ms, units (30-day retention) |
| `fingerprints` | (user_id, origin, identifier) → adaptive selector fingerprint JSON |
| `cache` | (user_id, key) → cached scrape result, expires_at |
| `monitors` / `monitor_checks` | url, interval, selector, webhook_url + encrypted secret, content_hash, last_available; check history |
| `extractions` | saved definitions, rules jsonb, refresh schedule, last_result/error, fetch/refresh counters |
| `crawls` | config jsonb, checkpoint jsonb (pending/visited/pages), state queued/running/completed/failed/cancelled |
| `webhooks` / `webhook_events` / `webhook_attempts` | relay config + encrypted secret; events (dedupe on external_id); per-attempt log |
| `notifications` | per-user events (monitor up/down/changed, webhook failures) |
| `domain_leases` | cross-instance per-host pacing |
| `system_state` | `last_tick` summary for the status page |

## API surface

Public: `GET /api/v1/health`, `GET /api/v1/status`, `GET /api/v1/catalog`, `GET /openapi.json`,
`POST /api/v1/auth/register|login`, `GET /api/v1/auth/session` (always 200, `{signed_in}`), `POST /api/v1/hooks/{id}` (signed inbound webhooks),
`GET|POST /api/v1/internal/tick` (Bearer `CRON_SECRET`).

Metered (API key `Authorization: Bearer ozy_…` or session; permission in brackets):

| # | Endpoint | Runtime |
|---|---|---|
| 1 | `POST /api/v1/ocr/captcha` [ocr] | worker |
| 2 | `POST/GET /api/v1/monitor`, `GET/DELETE /api/v1/monitor/{id}` [monitor] | Vercel |
| 3 | `POST /api/v1/scrape` [scrape]; `POST /api/v1/crawls`, `GET/DELETE /api/v1/crawls/{id}`, `POST /api/v1/crawls/{id}/resume` [crawls] | Vercel (+worker for `render`) |
| 4 | `POST /api/v1/screenshot` [screenshot] | worker |
| 5 | `GET /api/v1/intelligence?url=` [intelligence] | Vercel |
| 6 | `POST /api/v1/pdf/parse` (JSON base64/url or multipart) [pdf] | Vercel (+worker for scanned-page OCR) |
| 7 | `GET /api/v1/preview?url=` [preview] | Vercel |
| 8 | `POST /api/v1/performance` [performance] | Vercel (+worker for `browser:true`) |
| 9 | `POST /api/v1/url/check` [url] | Vercel |
| 10 | `POST /api/v1/email/validate` [email] | Vercel |
| 11 | `POST /api/v1/extract/create`, `GET /api/v1/extract`, `GET/DELETE /api/v1/extract/{id}` [extract] | Vercel |
| 12 | `POST/GET /api/v1/webhooks`, `DELETE /api/v1/webhooks/{id}`, `GET /api/v1/webhooks/{id}/deliveries` [webhooks] | Vercel |

Session only (API keys get 403 `session_required`): `/api/v1/account` (GET/PATCH/DELETE), `/api/v1/account/password`,
`/api/v1/keys`, `/api/v1/keys/{id}`, `/api/v1/usage`, `/api/v1/logs`, `/api/v1/notifications`, `/api/v1/auth/logout`.

Quotas: 20 units/min, 100 units/day per account, 2000/day platform-wide (OCR/screenshot/PDF cost 10).
Override with `OZY_UNITS_PER_MINUTE`, `OZY_UNITS_PER_DAY`, `OZY_PLATFORM_UNITS_PER_DAY`.

Pages: `/`, `/apis`, `/apis/{slug}`, `/playground`, `/login`, `/register`, `/dashboard`, `/dashboard/{keys,usage,logs,settings}`,
`/status`, `/docs`, `/docs/responsible-use`, `/changelog`.

## Commands (Git Bash; PowerShell: `. ./work/rust-env.ps1`)

```bash
export RUSTUP_HOME=/d/dev.itsozy/.tools/rustup CARGO_HOME=/d/dev.itsozy/.tools/cargo PATH=/d/dev.itsozy/.tools/cargo/bin:$PATH
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                        # ~25 unit tests + 2 integration tests (about 3 min from this PC to us-east-1)
cargo test -p ozy-worker --features browser   # worker; `ocr` needs libtesseract (runs in CI)
cargo run --bin ozy-local                     # http://127.0.0.1:3917, uses .env + .env.preview.local (preview DB)
cargo run --bin ozy-ops -- --env-file .env.preview.local migrate      # preview DB
cargo run --bin ozy-ops -- --env-file .env.development.local migrate  # PRODUCTION DB (Production+Development share it)
cargo run --bin ozy-ops -- --env-file .env.development.local status
```

Integration tests need repo-root `.env` (local `OZY_ENCRYPTION_KEY`, `CRON_SECRET`) and `.env.preview.local`
(`vercel env pull .env.preview.local --environment=preview`); without them they print SKIPPED and pass.
They create and delete their own users in the preview database and use the toscrape.com scraping sandboxes
(example.com asks not to be used for tests).

Worker locally (Chrome installed on this PC):

```bash
OZY_WORKER_SECRET=<32+ chars> CHROME_PATH="C:\Program Files\Google\Chrome\Application\chrome.exe" cargo run -p ozy-worker --features browser
# then set OZY_WORKER_URL=http://127.0.0.1:8080 and the same OZY_WORKER_SECRET in .env and run ozy-local
```

## Deployment

- `git push origin main` deploys production through the Vercel GitHub integration (Rust build takes about 4 minutes). Other branches deploy previews.
- Check: `vercel list dev-itsozy --scope ko8382slayerbeast-9839s-projects`, `vercel inspect <url> --logs --scope ko8382slayerbeast-9839s-projects`.
- After deploying, smoke-test `https://dev.itsozy.lol/api/v1/health` and `/api/v1/status`.
- Migrations are **not** run on deploy. Run `ozy-ops migrate` against preview, then production, before pushing code that needs them.
- The Vercel CLI and Git Credential Manager on this PC are already authenticated. Another machine must log in itself; never copy tokens.

## Environment variables (names only)

Vercel (Supabase integration): `POSTGRES_URL` (pooler 6543, auto-rewritten to session mode 5432), `POSTGRES_URL_NON_POOLING`,
`POSTGRES_HOST`, `POSTGRES_USER`, `POSTGRES_PASSWORD`, `POSTGRES_DATABASE`, `POSTGRES_PRISMA_URL`, `SUPABASE_*`, `NEXT_PUBLIC_SUPABASE_*`.
Platform: `OZY_ENCRYPTION_KEY` (sensitive; production and preview differ), `CRON_SECRET` (sensitive; production and preview differ),
`PUBLIC_ORIGIN` (production only; previews fall back to `https://$VERCEL_URL`). Optional: `OZY_WORKER_URL`, `OZY_WORKER_SECRET`,
`OZY_UNITS_PER_MINUTE`, `OZY_UNITS_PER_DAY`, `OZY_PLATFORM_UNITS_PER_DAY`, `DATABASE_URL` (overrides `POSTGRES_URL`).
Worker: `OZY_WORKER_SECRET`, `BIND`, `CHROME_PATH`, `OZY_CHROME_NO_SANDBOX` (containers only), `OZY_WORKER_CONCURRENCY`, `TESSDATA_PREFIX`.

Rotations: changing `CRON_SECRET` requires re-running `ozy-ops schedule https://dev.itsozy.lol` with the new value in the
environment (it updates Supabase Vault). Changing `OZY_ENCRYPTION_KEY` makes stored webhook/monitor secrets unreadable,
so it needs a re-encryption migration first.

## Worker architecture

`crates/ozy-worker` is an Axum service (`GET /health`, `POST /v1/{ocr|screenshot|render|performance}`, Bearer `OZY_WORKER_SECRET`,
concurrency-limited). Chromium runs per job with a fresh profile, `--proxy-server` pointed at an in-process Rust proxy that
validates and pins every destination (public IPv4, ports 80/443), plus `--host-resolver-rules` so Chromium never resolves DNS
itself. Deploy with `crates/ozy-worker/Dockerfile` on any Docker host, then set `OZY_WORKER_URL` and `OZY_WORKER_SECRET` on Vercel.
**No worker is deployed today; no hosting was authorized.** Verified locally on Windows with Chrome: screenshots (desktop
PNG, mobile WebP), JS rendering (quotes.toscrape.com/js: 0 quotes without render, 10 with) and browser metrics. OCR is verified only in CI.

## Feature status (2026-10-18)

| API | Implemented | Tested | Production |
|---|---|---|---|
| 1 OCR | yes (worker, Tesseract) | preprocessing unit tests; OCR in CI only | 503 worker_unavailable (no worker) |
| 2 Monitor | yes | integration (live check, stats, history) | working; pg_cron every minute |
| 3 Scraper + crawls | yes | unit + integration (adaptive, cache, SSRF, crawl) | working; `render` needs worker |
| 4 Screenshot | yes (worker, Chromium) | local worker E2E | 503 worker_unavailable |
| 5 Intelligence | yes | unit + integration (TLS details) | working |
| 6 PDF | yes (pure Rust) | integration (text, metadata, Markdown, corrupt input) | working; scanned OCR needs worker |
| 7 Preview | yes | integration | working |
| 8 Performance | yes | integration (HTTP); local worker (browser) | HTTP timing working; browser needs worker |
| 9 URL safety | yes | unit + integration | working |
| 10 Email | yes | unit + integration (MX/SPF/DMARC, typos, disposable) | working |
| 11 Website to API | yes | integration | working; scheduled refresh via tick |
| 12 Webhook relay | yes | unit + integration (signatures, replay, filter, dedupe, retries log) | working |

## Known issues and limitations

- No email delivery service, so no email verification and no password reset. `email_verified` is always false.
- Supabase **session-mode** pooling is required because sqlx's named prepared statements collide on the transaction pooler.
  Session mode holds one backend per pooled client (pool max 3, idle 15 s), so very high concurrency can hit the
  free-tier pooler's client limit. A fix is `.persistent(false)` on every query plus the transaction pooler (needs testing).
- Each metered request does about 6 sequential DB round trips (key lookup, 3 budget windows, log insert). That's fine in iad1, but the
  three budget upserts could be merged into one statement.
- Egress is IPv4-only by design; IPv6-only sites are refused. `domain_budget` is global per host (1 req/s across all users).
- robots.txt answers other than 200/404/410 are treated as "disallow" (conservative). Crawl-delay above 10 s is refused.
- Monitors hash visible text; pages with timestamps or rotating content flag changes unless `selector` narrows it.
- XPath is a subset (`//tag`, `[@attr='v']`, child `/`). Markdown conversion (html2md) is basic.
- Rules return at most 100 matches each; inline HTML and fetched pages are capped at 2 MiB; PDFs at 3 MiB.
- Benchmarks were measured on this Windows PC (see BENCHMARKS.md). Network throughput against third-party sites was deliberately not stress-tested.

## Next improvements

1. Deploy the worker (needs an approved Docker host), then set `OZY_WORKER_URL`/`OZY_WORKER_SECRET` on Vercel.
2. Email delivery (verification, password reset, failure alerts) once a provider is approved.
3. Merge the quota upserts into one query; cache API-key lookups for a few seconds per instance.
4. Monitor notifications in the dashboard UI (data is already in `/api/v1/notifications`), plus UI forms for monitors/extractions/webhooks.
5. Broader XPath support and table extraction in OzyScrape; per-template fingerprint sharing.
6. Pagination for logs, deliveries and crawl pages.
