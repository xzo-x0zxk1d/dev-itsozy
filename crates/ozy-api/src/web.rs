//! Server-rendered pages. Dynamic dashboard data loads from the JSON APIs via
//! /assets/app.js (CSP forbids inline scripts).
use crate::catalog::{APIS, ApiSpec, COMMON_ERRORS, Runtime};
use crate::*;
use axum::{extract::Path, response::Html};

const STYLE: &str = include_str!("../assets/style.css");
const SCRIPT: &str = include_str!("../assets/app.js");
const CHANGELOG: &[(&str, &str, &[&str])] = &[(
    "0.1.0",
    "2026-10-18",
    &[
        "First public release of the OZY Developer Platform on dev.itsozy.lol.",
        "12 APIs implemented in Rust; 8 run fully on Vercel, 2 partly need the optional worker, 2 need the worker.",
        "OzyScrape engine: CSS + XPath subset, adaptive selectors with confidence and margin, Markdown, checkpointed crawls.",
        "Accounts with Argon2id passwords, hashed API keys with per-API permissions, quotas and request logs.",
        "SSRF-hardened egress: validated and pinned DNS, private and metadata ranges blocked, redirects re-validated.",
        "Optional self-hosted worker for Tesseract OCR and Chromium screenshots/rendering.",
    ],
)];

pub async fn stylesheet() -> Response {
    asset(STYLE, "text/css; charset=utf-8")
}
pub async fn javascript() -> Response {
    asset(SCRIPT, "text/javascript; charset=utf-8")
}
fn asset(body: &'static str, kind: &'static str) -> Response {
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, axum::http::HeaderValue::from_static(kind));
    response
}

pub fn esc(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

const LOGO: &str = r#"<svg class="logo-mark" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 2 22 7v10l-10 5L2 17V7z" fill="none" stroke="currentColor" stroke-width="1.8"/><circle cx="12" cy="12" r="3.2" fill="currentColor"/></svg>"#;

fn icon(name: &str) -> &'static str {
    match name {
        "ocr" => {
            r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7V4h3M17 4h3v3M20 17v3h-3M7 20H4v-3M8 12h8M8 9h5M8 15h6"/></svg>"#
        }
        "monitor" => r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 12h4l2-6 4 12 2-6h6"/></svg>"#,
        "scrape" => r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m8 7-5 5 5 5M16 7l5 5-5 5M14 4l-4 16"/></svg>"#,
        "screenshot" => {
            r#"<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="6" width="18" height="14" rx="2"/><circle cx="12" cy="13" r="3.5"/><path d="M9 6l1.5-2h3L15 6"/></svg>"#
        }
        "intelligence" => {
            r#"<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="11" cy="11" r="7"/><path d="m20 20-4-4M11 8v6M8 11h6"/></svg>"#
        }
        "pdf" => r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 2h9l5 5v15H6zM14 2v6h6M9 13h6M9 17h6"/></svg>"#,
        "preview" => {
            r#"<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M7 13h5M7 16h8"/></svg>"#
        }
        "performance" => r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 14l4-4M4 18a9 9 0 1 1 16 0"/></svg>"#,
        "url-check" => {
            r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3 4 6v6c0 5 3.5 8 8 9 4.5-1 8-4 8-9V6z"/><path d="m9 12 2 2 4-4"/></svg>"#
        }
        "email" => {
            r#"<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="5" width="18" height="14" rx="2"/><path d="m3 7 9 6 9-6"/></svg>"#
        }
        "extract" => r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 6h16M4 12h10M4 18h7M17 15v6M14 18h6"/></svg>"#,
        "webhooks" => {
            r#"<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="6" cy="17" r="2.5"/><circle cx="18" cy="17" r="2.5"/><circle cx="12" cy="6" r="2.5"/><path d="M10.5 8 7 15M13.5 8l3.5 7M8.5 17h7"/></svg>"#
        }
        _ => r#"<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="8"/></svg>"#,
    }
}

fn layout(title: &str, page: &str, description: &str, body: &str) -> Html<String> {
    Html(format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · OZY Developer Platform</title>
<meta name="description" content="{description}">
<meta name="theme-color" content="#0a0a0b">
<link rel="stylesheet" href="/assets/style.css">
<script src="/assets/app.js" defer></script>
</head>
<body data-page="{page}">
<a class="skip" href="#main">Skip to content</a>
<header class="site-header">
  <div class="wrap header-row">
    <a class="brand" href="/">{LOGO}<span>OZY</span><span class="brand-sub">developers</span></a>
    <button class="nav-toggle" type="button" aria-expanded="false" aria-controls="site-nav">Menu</button>
    <nav id="site-nav" class="site-nav" aria-label="Primary">
      <a href="/apis">APIs</a><a href="/docs">Docs</a><a href="/playground">Playground</a><a href="/status">Status</a><a href="/changelog">Changelog</a>
      <span class="nav-auth" data-auth="anon"><a href="/login">Sign in</a><a class="btn btn-small" href="/register">Get a free key</a></span>
      <span class="nav-auth" data-auth="user" hidden><a class="btn btn-small" href="/dashboard">Dashboard</a></span>
    </nav>
  </div>
</header>
<main id="main">
{body}
</main>
<footer class="site-footer">
  <div class="wrap footer-row">
    <span>{LOGO} OZY Developer Platform · Rust on Vercel</span>
    <span><a href="/docs/responsible-use">Responsible use</a> · <a href="/openapi.json">OpenAPI</a> · <a href="/status">Status</a></span>
  </div>
</footer>
</body>
</html>"##,
        title = esc(title),
        description = esc(description),
    ))
}

fn runtime_badge(runtime: Runtime) -> String {
    format!(r#"<span class="badge badge-{}">{}</span>"#, runtime.key(), runtime.label())
}

fn api_card(api: &ApiSpec) -> String {
    format!(
        r#"<a class="card api-card" href="/apis/{slug}">
  <div class="card-top"><span class="icon">{icon}</span><span class="num">{num:02}</span></div>
  <h3>{name}</h3><p>{summary}</p>
  <div class="card-foot"><code class="endpoint"><b class="m-{method_l}">{method}</b> {path}</code>{badge}</div>
</a>"#,
        slug = api.slug,
        icon = icon(api.slug),
        num = api.number,
        name = esc(api.name),
        summary = esc(api.summary),
        method_l = api.method.to_lowercase(),
        method = api.method,
        path = esc(api.path),
        badge = runtime_badge(api.runtime),
    )
}

pub fn curl(api: &ApiSpec, origin: &str) -> String {
    if api.method == "GET" {
        format!(
            "curl \"{origin}{}{}\" \\\n  -H \"Authorization: Bearer $OZY_KEY\"",
            api.path, api.example_request
        )
    } else {
        format!(
            "curl -X {} {origin}{} \\\n  -H \"Authorization: Bearer $OZY_KEY\" \\\n  -H \"Content-Type: application/json\" \\\n  -d '{}'",
            api.method, api.path, api.example_request
        )
    }
}

fn pretty(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| raw.to_owned())
}

fn landing(app: &App) -> Html<String> {
    let cards: String = APIS.iter().map(api_card).collect();
    let vercel = APIS.iter().filter(|a| a.runtime == Runtime::Vercel).count();
    let first = &APIS[2];
    let body = format!(
        r#"<section class="hero">
  <div class="wrap hero-grid">
    <div>
      <p class="eyebrow"><span class="dot"></span> 12 APIs · free tier · written in Rust</p>
      <h1>Developer APIs that are <span class="grad">fast, honest and free</span>.</h1>
      <p class="lead">Scraping with adaptive selectors, link previews, website intelligence, monitoring, PDF parsing, URL and email checks, a signed webhook relay and more. One API key, a generous free quota, and no AI guesswork: every result is deterministic and explains itself.</p>
      <div class="cta-row"><a class="btn" href="/register">Get a free API key</a><a class="btn btn-ghost" href="/apis">Browse the APIs →</a></div>
      <dl class="stats"><div><dt>{vercel}</dt><dd>APIs fully on Vercel</dd></div><div><dt>100</dt><dd>free units / day</dd></div><div><dt>0</dt><dd>external AI or paid APIs</dd></div></dl>
    </div>
    <div class="terminal" aria-label="Example request">
      <div class="terminal-bar"><span></span><span></span><span></span><em>Web Scraper</em></div>
      <pre><code>{curl}</code></pre>
    </div>
  </div>
</section>
<section class="wrap section">
  <div class="section-head"><h2>Every API</h2><p>Each one is versioned under <code>/api/v1/</code>, documented with parameters, examples, error codes and limits, and testable in the playground.</p></div>
  <div class="grid">{cards}</div>
</section>
<section class="wrap section features">
  <div class="card"><h3>OzyScrape engine</h3><p>Our own Rust extraction engine. When a selector breaks, adaptive rules relocate the element by structural similarity and report confidence. Uncertain matches return no values rather than a guess.</p><a href="/docs#ozyscrape">How adaptive selectors work →</a></div>
  <div class="card"><h3>Secure by default</h3><p>Outbound requests are validated, DNS answers are pinned, and private, loopback and metadata ranges are blocked. Every redirect is re-checked, and robots.txt and per-domain pacing are respected.</p><a href="/docs#security">Security model →</a></div>
  <div class="card"><h3>Transparent limits</h3><p>Free means quotas, not surprises: 20 units a minute and 100 a day per account. Responses state what was measured and what wasn't.</p><a href="/docs#limits">Rate limits →</a></div>
</section>"#,
        curl = esc(&curl(first, &app.origin)),
    );
    layout(
        "Free developer APIs in Rust",
        "home",
        "Free, fast developer APIs written in Rust: scraping, previews, intelligence, monitoring, PDF parsing and more.",
        &body,
    )
}

fn directory() -> Html<String> {
    let rows: String = APIS
        .iter()
        .map(|a| {
            format!(
                r#"<tr><td><span class="icon icon-sm">{}</span></td><td><a href="/apis/{}">{}</a><div class="muted small">{}</div></td><td><code class="endpoint"><b class="m-{}">{}</b> {}</code></td><td>{}</td><td class="num-cell">{}</td></tr>"#,
                icon(a.slug),
                a.slug,
                esc(a.name),
                esc(a.summary),
                a.method.to_lowercase(),
                a.method,
                esc(a.path),
                runtime_badge(a.runtime),
                a.cost
            )
        })
        .collect();
    let body = format!(
        r#"<section class="wrap section page-head"><h1>API directory</h1><p class="lead">Twelve APIs, one key. Units per call are shown on the right. Your free daily allowance is 100 units.</p>
<div class="legend">{} {} {}</div></section>
<section class="wrap"><div class="table-wrap"><table class="table"><thead><tr><th></th><th>API</th><th>Endpoint</th><th>Runtime</th><th>Units</th></tr></thead><tbody>{rows}</tbody></table></div></section>"#,
        runtime_badge(Runtime::Vercel),
        runtime_badge(Runtime::Hybrid),
        runtime_badge(Runtime::Worker)
    );
    layout("API directory", "apis", "All OZY developer APIs.", &body)
}

fn api_page(api: &ApiSpec, app: &App) -> Html<String> {
    let params: String = api
        .params
        .iter()
        .map(|p| {
            format!(
                "<tr><td><code>{}</code></td><td><span class=\"type\">{}</span></td><td>{}</td><td>{}</td></tr>",
                esc(p.name),
                esc(p.kind),
                if p.required {
                    "<span class=\"req\">required</span>"
                } else {
                    "optional"
                },
                esc(p.description)
            )
        })
        .collect();
    let errors: String = api
        .errors
        .iter()
        .chain(COMMON_ERRORS.iter())
        .map(|(status, code, text)| {
            format!(
                "<tr><td>{status}</td><td><code>{}</code></td><td>{}</td></tr>",
                esc(code),
                esc(text)
            )
        })
        .collect();
    let related: String = if api.related.is_empty() {
        String::new()
    } else {
        let items: String = api
            .related
            .iter()
            .map(|(m, path, text)| {
                format!(
                    "<li><code class=\"endpoint\"><b class=\"m-{}\">{m}</b> {}</code> <span>{}</span></li>",
                    m.to_lowercase(),
                    esc(path),
                    esc(text)
                )
            })
            .collect();
        format!("<h2>Related endpoints</h2><ul class=\"related\">{items}</ul>")
    };
    let body = format!(
        r#"<section class="wrap doc-layout">
<aside class="doc-nav" aria-label="APIs">{nav}</aside>
<article class="doc">
  <p class="eyebrow">API {num:02} · <span>{badge}</span></p>
  <h1>{name}</h1>
  <p class="lead">{description}</p>
  <div class="endpoint-box"><code class="endpoint big"><b class="m-{method_l}">{method}</b> {origin}{path}</code><button class="btn btn-small btn-ghost" data-copy="{origin}{path}" type="button">Copy</button></div>
  <div class="callout callout-{runtime_key}"><strong>{runtime_label}.</strong> {runtime_note}</div>
  <h2>Authentication</h2>
  <p>Send <code>Authorization: Bearer ozy_…</code> with an API key that includes the <code>{permission}</code> permission (or <code>*</code>). Signed-in dashboard sessions work in the playground. Each call costs <strong>{cost} unit{plural}</strong>.</p>
  <h2>Parameters</h2>
  <div class="table-wrap"><table class="table"><thead><tr><th>Name</th><th>Type</th><th></th><th>Description</th></tr></thead><tbody>{params}</tbody></table></div>
  <h2>Example request</h2>
  <pre class="code"><button class="copy" data-copy-target type="button">Copy</button><code>{curl}</code></pre>
  <h2>Example response <span class="muted small">(illustrative)</span></h2>
  <pre class="code"><code>{response}</code></pre>
  {related}
  <h2>Errors</h2>
  <p>Errors use one envelope: <code>{{"error": {{"code": "…", "message": "…"}}}}</code>.</p>
  <div class="table-wrap"><table class="table"><thead><tr><th>Status</th><th>Code</th><th>Meaning</th></tr></thead><tbody>{errors}</tbody></table></div>
  <h2>Rate limits</h2>
  <p>20 units per minute and 100 units per day per account; outbound fetches to the same host are spaced at least one second apart. Exceeding a limit returns <code>429</code>.</p>
  <div class="cta-row"><a class="btn" href="/playground?api={slug}">Try it in the playground</a></div>
</article>
</section>"#,
        nav = APIS
            .iter()
            .map(|a| format!(
                "<a href=\"/apis/{}\"{}>{}</a>",
                a.slug,
                if a.slug == api.slug { " aria-current=\"page\"" } else { "" },
                esc(a.name)
            ))
            .collect::<String>(),
        num = api.number,
        badge = runtime_badge(api.runtime),
        name = esc(api.name),
        description = esc(api.description),
        method_l = api.method.to_lowercase(),
        method = api.method,
        origin = esc(&app.origin),
        path = esc(api.path),
        runtime_key = api.runtime.key(),
        runtime_label = api.runtime.label(),
        runtime_note = esc(api.runtime_note),
        permission = api.permission,
        cost = api.cost,
        plural = if api.cost == 1 { "" } else { "s" },
        curl = esc(&curl(api, &app.origin)),
        response = esc(&pretty(api.example_response)),
        slug = api.slug,
    );
    layout(api.name, "api", api.summary, &body)
}

fn playground() -> Html<String> {
    let options: String = APIS
        .iter()
        .map(|a| format!("<option value=\"{}\">{:02} · {}</option>", a.slug, a.number, esc(a.name)))
        .collect();
    let body = format!(
        r#"<section class="wrap section page-head"><h1>API playground</h1><p class="lead">Send real requests with your session or an API key. Requests count against your quota.</p></section>
<section class="wrap playground">
  <form class="card pg-form" id="pg-form">
    <label>API<select id="pg-api">{options}</select></label>
    <div class="pg-endpoint"><code class="endpoint" id="pg-endpoint"></code><span id="pg-runtime"></span></div>
    <label>API key <span class="muted small">(optional when signed in; stays in this tab only)</span><input id="pg-key" type="password" autocomplete="off" placeholder="ozy_…" spellcheck="false"></label>
    <label id="pg-query-label">Query<input id="pg-query" type="text" spellcheck="false"></label>
    <label id="pg-body-label">JSON body<textarea id="pg-body" rows="12" spellcheck="false"></textarea></label>
    <div class="cta-row"><button class="btn" type="submit" id="pg-send">Send request</button><a class="btn btn-ghost" id="pg-docs" href="/apis">Docs</a></div>
    <p class="muted small" id="pg-hint"></p>
  </form>
  <div class="card pg-result">
    <div class="pg-meta"><span id="pg-status" class="status-pill">Ready</span><span id="pg-time" class="muted"></span></div>
    <img id="pg-image" alt="Screenshot result" hidden>
    <pre class="code"><code id="pg-output">// Responses appear here.</code></pre>
  </div>
</section>"#
    );
    layout("Playground", "playground", "Interactive OZY API playground.", &body)
}

fn auth_page(register: bool) -> Html<String> {
    let (title, action, alt) = if register {
        (
            "Create your free account",
            "Create account",
            r#"Already registered? <a href="/login">Sign in</a>"#,
        )
    } else {
        ("Sign in", "Sign in", r#"New here? <a href="/register">Create a free account</a>"#)
    };
    let name = if register {
        r#"<label>Name<input name="name" autocomplete="name" maxlength="80" placeholder="Ada Lovelace"></label>"#
    } else {
        ""
    };
    let body = format!(
        r#"<section class="auth-wrap"><form class="card auth-card" id="auth-form" data-mode="{mode}" novalidate>
  <a class="brand" href="/">{LOGO}<span>OZY</span></a>
  <h1>{title}</h1>
  {name}
  <label>Email<input name="email" type="email" autocomplete="email" required></label>
  <label>Password<input name="password" type="password" autocomplete="{autocomplete}" minlength="12" maxlength="128" required>{hint}</label>
  <button class="btn btn-block" type="submit">{action}</button>
  <p class="form-error" role="alert" id="auth-error" hidden></p>
  <p class="muted small">{alt}</p>
  <p class="muted small">Email verification is not enabled yet: this deployment has no email delivery service configured.</p>
</form></section>"#,
        mode = if register { "register" } else { "login" },
        autocomplete = if register { "new-password" } else { "current-password" },
        hint = if register {
            r#"<span class="muted small">At least 12 characters.</span>"#
        } else {
            ""
        },
    );
    layout(title, "auth", "OZY account access.", &body)
}

fn dashboard(section: &str) -> Html<String> {
    let tabs = [
        ("", "Overview"),
        ("keys", "API keys"),
        ("usage", "Usage"),
        ("logs", "Request logs"),
        ("settings", "Settings"),
    ];
    let nav: String = tabs
        .iter()
        .map(|(slug, label)| {
            let href = if slug.is_empty() {
                "/dashboard".to_owned()
            } else {
                format!("/dashboard/{slug}")
            };
            format!(
                "<a href=\"{href}\"{}>{label}</a>",
                if *slug == section { " aria-current=\"page\"" } else { "" }
            )
        })
        .collect();
    let content = match section {
        "keys" => {
            r#"<h1>API keys</h1><p class="muted">Keys are shown once, stored only as SHA-256 hashes, and can be revoked at any time. Up to 10 active keys.</p>
<form class="card inline-form" id="key-form"><label>Name<input name="name" required maxlength="64" placeholder="Production server"></label>
<fieldset class="perm-grid"><legend>Permissions</legend><label class="check"><input type="checkbox" name="perm" value="*" checked> All APIs</label>
<label class="check"><input type="checkbox" name="perm" value="scrape"> scrape</label><label class="check"><input type="checkbox" name="perm" value="preview"> preview</label>
<label class="check"><input type="checkbox" name="perm" value="intelligence"> intelligence</label><label class="check"><input type="checkbox" name="perm" value="performance"> performance</label>
<label class="check"><input type="checkbox" name="perm" value="url"> url</label><label class="check"><input type="checkbox" name="perm" value="email"> email</label>
<label class="check"><input type="checkbox" name="perm" value="ocr"> ocr</label><label class="check"><input type="checkbox" name="perm" value="screenshot"> screenshot</label>
<label class="check"><input type="checkbox" name="perm" value="pdf"> pdf</label><label class="check"><input type="checkbox" name="perm" value="monitor"> monitor</label>
<label class="check"><input type="checkbox" name="perm" value="extract"> extract</label><label class="check"><input type="checkbox" name="perm" value="crawls"> crawls</label>
<label class="check"><input type="checkbox" name="perm" value="webhooks"> webhooks</label></fieldset>
<button class="btn" type="submit">Create key</button></form>
<div class="card secret-box" id="new-key" hidden><strong>Copy your new key now. It won't be shown again.</strong><pre class="code"><code id="new-key-value"></code></pre><button class="btn btn-small" type="button" data-copy-el="new-key-value">Copy key</button></div>
<div class="table-wrap"><table class="table"><thead><tr><th>Name</th><th>Prefix</th><th>Permissions</th><th>Created</th><th>Last used</th><th></th></tr></thead><tbody id="keys-body"><tr><td colspan="6" class="muted">Loading…</td></tr></tbody></table></div>"#
        }
        "usage" => {
            r#"<h1>Usage</h1><p class="muted">Last 30 days. Units: 1 per standard call, 10 for OCR, screenshots and PDF parsing.</p>
<div class="stat-row" id="usage-stats"></div><div class="card"><h3>Daily units</h3><div class="bars" id="usage-chart" role="img" aria-label="Daily units chart"></div></div>
<div class="table-wrap"><table class="table"><thead><tr><th>Key</th><th>Requests</th><th>Units</th></tr></thead><tbody id="usage-keys"></tbody></table></div>"#
        }
        "logs" => {
            r#"<h1>Request logs</h1><p class="muted">Your 100 most recent metered requests. Request bodies are never stored.</p>
<div class="table-wrap"><table class="table"><thead><tr><th>Time</th><th>Endpoint</th><th>Status</th><th>Duration</th><th>Units</th><th>Key</th></tr></thead><tbody id="logs-body"><tr><td colspan="6" class="muted">Loading…</td></tr></tbody></table></div>"#
        }
        "settings" => {
            r#"<h1>Account settings</h1>
<form class="card inline-form" id="name-form"><h3>Profile</h3><label>Display name<input name="name" maxlength="80" required></label><p class="muted small" id="account-email"></p><button class="btn" type="submit">Save</button></form>
<form class="card inline-form" id="password-form"><h3>Change password</h3><label>Current password<input name="current_password" type="password" autocomplete="current-password" required></label>
<label>New password<input name="new_password" type="password" autocomplete="new-password" minlength="12" maxlength="128" required></label><p class="muted small">Changing your password signs out every session.</p><button class="btn" type="submit">Update password</button></form>
<div class="card"><h3>Sign out</h3><button class="btn btn-ghost" type="button" id="logout">Sign out of this browser</button></div>
<form class="card inline-form" id="delete-form"><h3>Delete account</h3><p class="muted small">Permanently deletes your account, API keys, logs, monitors, saved extractions, crawls and webhook relays. This cannot be undone.</p><label>Confirm with your password<input name="password" type="password" autocomplete="current-password" required></label><button class="btn btn-danger" type="submit">Delete my account</button></form>"#
        }
        _ => {
            r#"<h1>Overview</h1><p class="muted" id="welcome">Loading your account…</p>
<div class="stat-row" id="overview-stats"></div>
<div class="grid grid-2"><div class="card"><h3>Quick start</h3><ol class="steps"><li><a href="/dashboard/keys">Create an API key</a></li><li>Call any API with <code>Authorization: Bearer ozy_…</code></li><li>Watch usage and logs here</li></ol></div>
<div class="card"><h3>Notifications</h3><ul class="notes" id="notifications"><li class="muted">Loading…</li></ul></div></div>
<div class="grid grid-3"><div class="card"><h3>Monitors</h3><ul class="notes" id="list-monitors"></ul></div><div class="card"><h3>Saved extractions</h3><ul class="notes" id="list-extractions"></ul></div><div class="card"><h3>Webhook relays</h3><ul class="notes" id="list-webhooks"></ul></div></div>"#
        }
    };
    let body = format!(
        r#"<section class="wrap dash" data-section="{section}"><aside class="dash-nav" aria-label="Dashboard">{nav}</aside><div class="dash-main"><div class="form-error" role="alert" id="dash-error" hidden></div>{content}</div></section>"#
    );
    layout("Dashboard", "dashboard", "Your OZY developer dashboard.", &body)
}

fn status_page() -> Html<String> {
    let body = r#"<section class="wrap section page-head"><h1>System status</h1><p class="lead">Live checks of this deployment. APIs that need the optional worker are reported honestly when it is not configured.</p></section>
<section class="wrap"><div class="stat-row" id="status-summary"><div class="card stat"><span class="muted">Loading…</span></div></div>
<div class="table-wrap"><table class="table"><thead><tr><th>API</th><th>Runtime</th><th>Availability</th></tr></thead><tbody id="status-apis"></tbody></table></div>
<p class="muted small" id="status-updated"></p></section>"#;
    layout("Status", "status", "OZY platform status.", body)
}

fn docs_page(app: &App) -> Html<String> {
    let body = format!(
        r##"<section class="wrap doc-layout"><aside class="doc-nav" aria-label="Documentation">
<a href="#start">Getting started</a><a href="#auth">Authentication</a><a href="#limits">Rate limits</a><a href="#errors">Errors</a><a href="#ozyscrape">OzyScrape</a><a href="#security">Security model</a><a href="#worker">Worker-backed APIs</a><a href="#scheduler">Scheduling</a><a href="/docs/responsible-use">Responsible use</a><a href="/openapi.json">OpenAPI</a></aside>
<article class="doc">
<h1>Documentation</h1>
<h2 id="start">Getting started</h2>
<ol class="steps"><li><a href="/register">Create a free account</a>.</li><li>Create a key under <a href="/dashboard/keys">Dashboard → API keys</a>.</li><li>Call an API:</li></ol>
<pre class="code"><code>curl "{origin}/api/v1/preview?url=https://example.com" \
  -H "Authorization: Bearer $OZY_KEY"</code></pre>
<h2 id="auth">Authentication</h2>
<p>API keys are bearer tokens beginning with <code>ozy_</code>. They are stored only as SHA-256 hashes and can be scoped to specific APIs. Account management endpoints (<code>/api/v1/keys</code>, <code>/api/v1/account</code>, <code>/api/v1/usage</code>, <code>/api/v1/logs</code>) require a signed-in browser session, not a key. Sessions use <code>HttpOnly</code>, <code>Secure</code>, <code>SameSite=Strict</code> cookies; state-changing session requests must come from this origin.</p>
<h2 id="limits">Rate limits and quotas</h2>
<div class="table-wrap"><table class="table"><tbody>
<tr><td>Per account</td><td>20 units / minute, 100 units / day</td></tr>
<tr><td>Unit cost</td><td>1 per call; 10 for OCR, screenshots and PDF parsing</td></tr>
<tr><td>Per host</td><td>outbound requests to one host at least 1 s apart; robots.txt crawl-delay honoured (max 10 s)</td></tr>
<tr><td>Sign-in / registration</td><td>15 attempts per 15 minutes per IP and per email</td></tr>
<tr><td>Resources</td><td>10 API keys, 10 monitors, 10 saved extractions, 10 webhook relays, 2 active crawls</td></tr>
<tr><td>Payloads</td><td>request bodies 3 MiB; fetched pages 2 MiB; webhook payloads 256 KiB</td></tr>
</tbody></table></div>
<h2 id="errors">Errors</h2>
<p>All errors share <code>{{"error": {{"code": "…", "message": "…"}}}}</code> and a matching HTTP status. Messages never include stack traces or internal addresses. Every response carries an <code>x-request-id</code>.</p>
<h2 id="ozyscrape">OzyScrape and adaptive selectors</h2>
<p>OzyScrape is an original Rust engine built on html5ever (via <code>scraper</code>). Rules accept CSS selectors or an XPath subset (<code>//tag</code>, <code>//tag[@attr='v']</code>, <code>/</code> child steps). Unsupported XPath fails explicitly rather than being reinterpreted.</p>
<p>With <code>"adaptive": true</code>, each exact match stores a fingerprint: tag, text, attributes, classes, six ancestors, neighbouring siblings, sibling pattern and position. If the selector later matches nothing, every element with the same tag is scored with a weighted similarity. A result is returned as <code>recovered</code> only when confidence ≥ 0.65 and it beats the runner-up by ≥ 0.07. Otherwise it is <code>uncertain</code> with an empty value list, and both numbers are reported.</p>
<p>Scrapling inspired the capability set. OzyScrape is an independent Rust implementation, and no Scrapling or Python code runs here. Benchmarks and methodology are in the repository's BENCHMARKS.md.</p>
<h2 id="security">Security model</h2>
<ul class="bullets"><li>Only <code>http</code>/<code>https</code> on ports 80/443; no credentials in URLs.</li><li>Every DNS answer must be a public IPv4 address. The validated address is pinned into the connection, which defeats DNS rebinding.</li><li>Private, loopback, link-local, CGNAT, benchmark, documentation, multicast and cloud metadata ranges are blocked.</li><li>Redirects are followed manually (max 5), and each hop is re-validated.</li><li>Response bodies are streamed with a 2 MiB cap; the total time limit is 35 s.</li><li>Webhook destinations must be public HTTPS; redirects are never followed for deliveries.</li><li>Passwords use Argon2id. Webhook secrets are encrypted with AES-256-GCM.</li></ul>
<h2 id="worker">Worker-backed APIs</h2>
<p>Tesseract OCR and Chromium cannot run reliably inside Vercel functions. Those features use an optional, separately deployed Rust worker (<code>crates/ozy-worker</code>). The browser's traffic goes through the worker's own SSRF-filtering proxy. When no worker is configured, the affected endpoints return <code>503 worker_unavailable</code>, and the <a href="/status">status page</a> says so.</p>
<h2 id="scheduler">Scheduling</h2>
<p>Monitors, saved-extraction refreshes, webhook retries and queued crawls advance when the authenticated scheduler endpoint runs. Supabase pg_cron calls it every minute when configured, and a Vercel daily cron is the fallback. Check freshness on the status page.</p>
</article></section>"##,
        origin = esc(&app.origin),
    );
    layout("Documentation", "docs", "OZY platform documentation.", &body)
}

fn responsible_use() -> Html<String> {
    let body = r#"<section class="wrap section doc narrow"><h1>Responsible use</h1>
<p class="lead">OzyScrape (user agent <code>OzyScrape/0.1</code>) fetches public pages on behalf of OZY developers.</p>
<ul class="bullets"><li>It obeys robots.txt for the <code>OzyScrape</code> agent and honours crawl-delay.</li><li>It spaces requests to each host at least one second apart and backs off on 429/503.</li><li>It does not solve CAPTCHAs, spoof browser fingerprints or bypass access controls.</li><li>Use it only for data you are permitted to access, and comply with each site's terms and applicable law.</li><li>The OCR endpoint is for your own test images and authorised testing environments only.</li></ul>
<p>Site owners can block OzyScrape in robots.txt (<code>User-agent: OzyScrape</code> / <code>Disallow: /</code>).</p></section>"#;
    layout("Responsible use", "docs", "How OzyScrape behaves.", body)
}

fn changelog() -> Html<String> {
    let entries: String = CHANGELOG
        .iter()
        .map(|(version, date, items)| {
            format!(
                "<article class=\"card change\"><div class=\"change-head\"><h2>v{version}</h2><time datetime=\"{date}\">{date}</time></div><ul class=\"bullets\">{}</ul></article>",
                items.iter().map(|i| format!("<li>{}</li>", esc(i))).collect::<String>()
            )
        })
        .collect();
    layout(
        "Changelog",
        "changelog",
        "OZY platform changelog.",
        &format!("<section class=\"wrap section narrow\"><h1>Changelog</h1>{entries}</section>"),
    )
}

fn not_found() -> Response {
    let body = r#"<section class="wrap section narrow center"><p class="eyebrow">404</p><h1>Page not found</h1><p class="lead">That page doesn't exist.</p><a class="btn" href="/">Go home</a></section>"#;
    (StatusCode::NOT_FOUND, layout("Not found", "404", "Not found", body)).into_response()
}

pub async fn page(State(app): State<App>, path: Option<Path<String>>) -> Response {
    let path = path.map(|Path(p)| p).unwrap_or_default();
    let path = path.trim_end_matches('/');
    match path {
        "" => landing(&app).into_response(),
        "apis" => directory().into_response(),
        "playground" => playground().into_response(),
        "login" => auth_page(false).into_response(),
        "register" => auth_page(true).into_response(),
        "dashboard" => dashboard("").into_response(),
        "status" => status_page().into_response(),
        "docs" => docs_page(&app).into_response(),
        "docs/responsible-use" => responsible_use().into_response(),
        "changelog" => changelog().into_response(),
        _ => {
            if let Some(slug) = path.strip_prefix("apis/")
                && let Some(api) = catalog::find(slug)
            {
                return api_page(api, &app).into_response();
            }
            if let Some(section) = path.strip_prefix("dashboard/")
                && matches!(section, "keys" | "usage" | "logs" | "settings")
            {
                return dashboard(section).into_response();
            }
            not_found()
        }
    }
}

fn catalog_entries() -> Vec<Value> {
    APIS.iter()
        .map(|a| {
            json!({"number":a.number,"slug":a.slug,"name":a.name,"method":a.method,"path":a.path,"summary":a.summary,
            "permission":a.permission,"cost":a.cost,"runtime":a.runtime.key(),"runtime_label":a.runtime.label(),
            "example_request":a.example_request})
        })
        .collect()
}

pub async fn catalog_json() -> Json<Value> {
    Json(json!({"apis":catalog_entries()}))
}

pub async fn status(State(app): State<App>) -> Json<Value> {
    let database = sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&app.db).await.is_ok();
    let last_tick: Option<(Value, chrono::DateTime<chrono::Utc>)> = if database {
        sqlx::query_as("SELECT value,updated_at FROM ozy.system_state WHERE key='last_tick'")
            .fetch_optional(&app.db)
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    let worker = services::worker_health(&app).await;
    let worker_ok = worker["reachable"] == true;
    let capability = |name: &str| worker["capabilities"][name] == true;
    let apis: Vec<Value> = APIS
        .iter()
        .map(|a| {
            let availability = match a.runtime {
                _ if !database => "degraded",
                Runtime::Vercel => "operational",
                Runtime::Hybrid if worker_ok => "operational",
                Runtime::Hybrid => "partial",
                Runtime::Worker if worker_ok && capability(if a.slug == "ocr" { "ocr" } else { "browser" }) => "operational",
                Runtime::Worker => "requires_worker",
            };
            json!({"slug":a.slug,"name":a.name,"runtime":a.runtime.key(),"availability":availability})
        })
        .collect();
    let scheduler_fresh = last_tick
        .as_ref()
        .is_some_and(|(_, at)| chrono::Utc::now() - *at < chrono::Duration::minutes(10));
    Json(json!({"database":database,"worker":worker,
        "scheduler":{"last_tick":last_tick.as_ref().map(|(_, at)| at),"summary":last_tick.map(|(v, _)| v),"fresh_within_10_minutes":scheduler_fresh},
        "apis":apis,"version":env!("CARGO_PKG_VERSION"),"checked_at":chrono::Utc::now()}))
}

pub async fn openapi(State(app): State<App>) -> Json<Value> {
    let mut paths = serde_json::Map::new();
    for api in APIS {
        let parameters: Vec<Value> = if api.method == "GET" {
            api.params
                .iter()
                .map(|p| json!({"name":p.name,"in":"query","required":p.required,"description":p.description,"schema":{"type":"string"}}))
                .collect()
        } else {
            vec![]
        };
        let mut operation = json!({
            "operationId": api.slug.replace('-', "_"),
            "summary": api.name,
            "description": format!("{}\n\nRuntime: {}. {}", api.description, api.runtime.label(), api.runtime_note),
            "tags": [api.runtime.label()],
            "security": [{"bearer": []}],
            "x-ozy-permission": api.permission,
            "x-ozy-units": api.cost,
            "parameters": parameters,
            "responses": {
                "200": {"description":"Success","content":{"application/json":{"example":serde_json::from_str::<Value>(api.example_response).unwrap_or(Value::Null)}}},
                "default": {"description":"Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/Error"}}}}
            }
        });
        if api.method != "GET" {
            operation["requestBody"] = json!({"required":true,"content":{"application/json":{
                "schema":{"type":"object","properties":api.params.iter().filter(|p| !p.name.contains('.') && !p.name.contains(' ')).map(|p| (p.name.trim_end_matches("[]").to_owned(), json!({"description":format!("{} ({})", p.description, p.kind)}))).collect::<serde_json::Map<_, _>>()},
                "example":serde_json::from_str::<Value>(api.example_request).unwrap_or(Value::Null)}}});
        }
        paths.insert(api.path.to_owned(), json!({ api.method.to_lowercase(): operation }));
    }
    Json(json!({
        "openapi": "3.1.0",
        "info": {"title":"OZY Developer Platform","version":env!("CARGO_PKG_VERSION"),"description":"Free developer APIs written in Rust. Example responses are illustrative."},
        "servers": [{"url": app.origin}],
        "components": {
            "securitySchemes": {"bearer": {"type":"http","scheme":"bearer","description":"API key beginning with ozy_"}},
            "schemas": {"Error": {"type":"object","properties":{"error":{"type":"object","properties":{"code":{"type":"string"},"message":{"type":"string"}}}}}}
        },
        "paths": paths
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escaping_and_catalog_integrity() {
        assert_eq!(esc("<a href='x'>&\"</a>"), "&lt;a href=&#39;x&#39;&gt;&amp;&quot;&lt;/a&gt;");
        assert_eq!(APIS.len(), 12);
        let mut slugs: Vec<_> = APIS.iter().map(|a| a.slug).collect();
        slugs.dedup();
        assert_eq!(slugs.len(), 12);
        for api in APIS {
            assert!(api.path.starts_with("/api/v1/"), "{}", api.slug);
            assert!(
                serde_json::from_str::<Value>(api.example_response).is_ok(),
                "example response for {} must be JSON",
                api.slug
            );
            if api.method != "GET" {
                assert!(
                    serde_json::from_str::<Value>(api.example_request).is_ok(),
                    "example request for {} must be JSON",
                    api.slug
                );
            }
        }
    }
}
