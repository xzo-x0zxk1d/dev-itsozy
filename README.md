# OZY Developer Platform

Free developer APIs written in Rust, live at **https://dev.itsozy.lol**.

- 12 versioned APIs under `/api/v1/`: scraping (OzyScrape), link previews, website intelligence,
  monitoring, PDF parsing, URL safety, email validation, website-to-API, a signed webhook relay,
  website performance, OCR and screenshots
- **OzyScrape**: an original Rust extraction engine with adaptive selectors that report confidence
  and refuse uncertain matches
- Accounts, hashed API keys with per-API permissions, quotas, usage analytics and request logs
- Server-rendered docs, playground, dashboard and status page; OpenAPI at `/openapi.json`

Everything runs on the official Vercel Rust runtime with Supabase PostgreSQL. OCR, screenshots,
JavaScript rendering and browser metrics need the optional self-hosted worker in
`crates/ozy-worker`. Without it, those endpoints return `503 worker_unavailable`.

```bash
cargo test --workspace                                   # unit + integration (integration needs .env)
cargo run --bin ozy-local                                # http://127.0.0.1:3917 against the preview DB
cargo run --bin ozy-ops -- --env-file .env.preview.local migrate
```

- Development guide: [CLAUDE.md](CLAUDE.md)
- Infrastructure: [INFRASTRUCTURE.md](INFRASTRUCTURE.md)
- Benchmarks: [BENCHMARKS.md](BENCHMARKS.md)

This repository must never contain credentials.
