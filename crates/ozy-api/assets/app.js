// OZY Developer Platform client. Progressive enhancement only: all business
// logic lives in the Rust APIs. Text is always inserted with textContent.
(() => {
  "use strict";
  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));
  const page = document.body.dataset.page;

  function el(tag, attrs = {}, ...children) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(attrs)) {
      if (key === "class") node.className = value;
      else if (key === "text") node.textContent = value;
      // CSSOM writes are permitted by the CSP; style attributes are not.
      else if (key === "style") node.style.cssText = value;
      else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
      else node.setAttribute(key, value);
    }
    for (const child of children) node.append(child instanceof Node ? child : document.createTextNode(String(child ?? "")));
    return node;
  }

  async function api(path, options = {}) {
    const init = { method: options.method || "GET", credentials: "same-origin", headers: {} };
    if (options.body !== undefined) {
      init.headers["Content-Type"] = "application/json";
      init.body = typeof options.body === "string" ? options.body : JSON.stringify(options.body);
    }
    if (options.key) init.headers.Authorization = "Bearer " + options.key;
    const started = performance.now();
    const response = await fetch(path, init);
    const text = await response.text();
    let data;
    try { data = text ? JSON.parse(text) : null; } catch { data = text; }
    return { ok: response.ok, status: response.status, data, ms: Math.round(performance.now() - started) };
  }

  const errorMessage = (r) => (r.data && r.data.error && r.data.error.message) || "Request failed (HTTP " + r.status + ")";
  const when = (v) => (v ? new Date(v).toLocaleString() : "—");
  function showError(target, message) {
    const box = typeof target === "string" ? $(target) : target;
    if (!box) return;
    box.textContent = message;
    box.hidden = !message;
  }

  // Navigation
  const toggle = $(".nav-toggle");
  if (toggle) toggle.addEventListener("click", () => {
    const nav = $("#site-nav");
    const open = nav.classList.toggle("open");
    toggle.setAttribute("aria-expanded", String(open));
  });
  $$(".site-nav a").forEach((a) => { if (a.getAttribute("href") === location.pathname) a.setAttribute("aria-current", "page"); });

  let account = null;
  const accountReady = api("/api/v1/account").then((r) => {
    if (r.ok) {
      account = r.data;
      $$('[data-auth="anon"]').forEach((n) => (n.hidden = true));
      $$('[data-auth="user"]').forEach((n) => (n.hidden = false));
    }
    return r;
  }).catch(() => ({ ok: false, status: 0 }));

  // Copy helpers
  async function copy(text, button) {
    try {
      await navigator.clipboard.writeText(text);
      if (button) { const old = button.textContent; button.textContent = "Copied"; setTimeout(() => (button.textContent = old), 1400); }
    } catch { /* clipboard unavailable */ }
  }
  document.addEventListener("click", (event) => {
    const button = event.target.closest("[data-copy],[data-copy-target],[data-copy-el]");
    if (!button) return;
    if (button.hasAttribute("data-copy")) copy(button.getAttribute("data-copy"), button);
    else if (button.hasAttribute("data-copy-el")) copy($("#" + button.getAttribute("data-copy-el")).textContent, button);
    else copy(button.parentElement.querySelector("code").textContent, button);
  });

  // Auth
  if (page === "auth") {
    const form = $("#auth-form");
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const fields = Object.fromEntries(new FormData(form));
      if (!fields.email || !fields.password) return showError("#auth-error", "Email and password are required.");
      if (form.dataset.mode === "register" && fields.password.length < 12) return showError("#auth-error", "Use at least 12 characters.");
      const button = $("button[type=submit]", form);
      button.disabled = true;
      showError("#auth-error", "");
      const r = await api("/api/v1/auth/" + form.dataset.mode, { method: "POST", body: fields }).catch(() => ({ ok: false, status: 0 }));
      button.disabled = false;
      if (r.ok) location.assign("/dashboard");
      else showError("#auth-error", errorMessage(r));
    });
  }

  // Dashboard
  async function requireSession() {
    const r = await accountReady;
    if (r.status === 401 || r.status === 403) { location.assign("/login"); return false; }
    if (!r.ok) { showError("#dash-error", errorMessage(r)); return false; }
    return true;
  }

  function stat(value, label) { return el("div", { class: "card stat" }, el("b", { text: value }), el("span", { text: label })); }

  function listInto(target, items, render, empty) {
    const list = $(target);
    if (!list) return;
    list.replaceChildren(...(items.length ? items.map((item) => el("li", {}, ...render(item))) : [el("li", { class: "muted", text: empty })]));
  }

  async function overview() {
    $("#welcome").textContent = "Signed in as " + account.user.email + ".";
    const [usage, notes, monitors, extractions, webhooks] = await Promise.all([
      api("/api/v1/usage"), api("/api/v1/notifications"), api("/api/v1/monitor"), api("/api/v1/extract"), api("/api/v1/webhooks"),
    ]);
    const today = new Date().toISOString().slice(0, 10);
    const daily = (usage.data && usage.data.daily) || [];
    const todayRow = daily.find((d) => String(d.day).startsWith(today));
    const total = daily.reduce((sum, d) => sum + Number(d.requests || 0), 0);
    $("#overview-stats").replaceChildren(
      stat(String(todayRow ? todayRow.units : 0) + " / " + account.limits.units_per_day, "units used today"),
      stat(String(total), "requests (30 days)"),
      stat(String(account.limits.units_per_minute), "units per minute"),
    );
    listInto("#notifications", (notes.data && notes.data.notifications) || [], (n) => [el("span", { class: "state", text: n.kind }), " ", n.message, el("div", { class: "muted small", text: when(n.created_at) })], "No notifications yet.");
    listInto("#list-monitors", (monitors.data && monitors.data.monitors) || [], (m) => [el("span", { class: "state " + (m.last_available ? "state-ok" : "state-bad"), text: m.last_available ? "UP" : "DOWN" }), " ", m.url], "No monitors. POST /api/v1/monitor to create one.");
    listInto("#list-extractions", (extractions.data && extractions.data.extractions) || [], (x) => [x.name, el("div", { class: "muted small", text: x.url })], "No saved extractions.");
    listInto("#list-webhooks", (webhooks.data && webhooks.data.webhooks) || [], (w) => [w.destination, el("div", { class: "muted small", text: w.events + " events · " + w.failed + " failed" })], "No webhook relays.");
  }

  async function keys() {
    const body = $("#keys-body");
    async function load() {
      const r = await api("/api/v1/keys");
      if (!r.ok) return showError("#dash-error", errorMessage(r));
      const items = r.data.keys || [];
      body.replaceChildren(...(items.length ? items.map((k) => el("tr", {},
        el("td", { text: k.name }), el("td", {}, el("code", { text: k.prefix + "…" })),
        el("td", { text: (k.permissions || []).join(", ") }), el("td", { text: when(k.created_at) }),
        el("td", { text: k.revoked_at ? "revoked " + when(k.revoked_at) : when(k.last_used_at) }),
        el("td", {}, k.revoked_at ? "" : el("button", { class: "btn btn-small btn-danger", type: "button", onclick: async () => {
          if (!confirm("Revoke " + k.name + "? Applications using it will stop working.")) return;
          const res = await api("/api/v1/keys/" + k.id, { method: "DELETE" });
          if (res.ok) load(); else showError("#dash-error", errorMessage(res));
        } }, "Revoke")),
      )) : [el("tr", {}, el("td", { colspan: "6", class: "muted", text: "No keys yet." }))]));
    }
    $("#key-form").addEventListener("submit", async (event) => {
      event.preventDefault();
      const form = event.currentTarget;
      let permissions = $$("input[name=perm]:checked", form).map((i) => i.value);
      if (permissions.includes("*")) permissions = ["*"];
      if (!permissions.length) return showError("#dash-error", "Select at least one permission.");
      const r = await api("/api/v1/keys", { method: "POST", body: { name: form.name.value, permissions } });
      if (!r.ok) return showError("#dash-error", errorMessage(r));
      showError("#dash-error", "");
      $("#new-key-value").textContent = r.data.key;
      $("#new-key").hidden = false;
      form.reset();
      load();
    });
    load();
  }

  async function usagePage() {
    const r = await api("/api/v1/usage");
    if (!r.ok) return showError("#dash-error", errorMessage(r));
    const daily = r.data.daily || [];
    const units = daily.reduce((s, d) => s + Number(d.units || 0), 0);
    const errors = daily.reduce((s, d) => s + Number(d.errors || 0), 0);
    const requests = daily.reduce((s, d) => s + Number(d.requests || 0), 0);
    $("#usage-stats").replaceChildren(stat(String(requests), "requests"), stat(String(units), "units"), stat(String(errors), "errors (HTTP ≥ 400)"),
      stat(requests ? Math.round((1 - errors / requests) * 100) + "%" : "—", "success rate"));
    const max = Math.max(1, ...daily.map((d) => Number(d.units || 0)));
    $("#usage-chart").replaceChildren(...(daily.length ? daily.map((d) => el("div", { style: "height:" + Math.max(2, (Number(d.units) / max) * 100) + "%", title: String(d.day).slice(0, 10) + ": " + d.units + " units, " + d.requests + " requests" })) : [el("p", { class: "muted", text: "No usage yet." })]));
    const perKey = r.data.per_key || [];
    $("#usage-keys").replaceChildren(...(perKey.length ? perKey.map((k) => el("tr", {}, el("td", {}, el("code", { text: k.key_id ? String(k.key_id).slice(0, 8) : "session" })), el("td", { text: k.requests }), el("td", { text: k.units }))) : [el("tr", {}, el("td", { colspan: "3", class: "muted", text: "No usage yet." }))]));
  }

  async function logsPage() {
    const r = await api("/api/v1/logs");
    if (!r.ok) return showError("#dash-error", errorMessage(r));
    const logs = r.data.logs || [];
    $("#logs-body").replaceChildren(...(logs.length ? logs.map((l) => el("tr", {},
      el("td", { text: when(l.created_at) }), el("td", {}, el("code", { text: l.endpoint })),
      el("td", {}, el("span", { class: "state " + (l.status < 400 ? "state-ok" : l.status < 500 ? "state-warn" : "state-bad"), text: l.status })),
      el("td", { text: l.duration_ms + " ms" }), el("td", { text: l.units }), el("td", {}, el("code", { text: l.key_id ? String(l.key_id).slice(0, 8) : "session" })),
    )) : [el("tr", {}, el("td", { colspan: "6", class: "muted", text: "No requests yet." }))]));
  }

  async function settingsPage() {
    const nameForm = $("#name-form");
    nameForm.name.value = account.user.name;
    $("#account-email").textContent = account.user.email + (account.user.email_verified ? " · verified" : " · not verified (email delivery is not configured)");
    nameForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      const r = await api("/api/v1/account", { method: "PATCH", body: { name: nameForm.name.value } });
      showError("#dash-error", r.ok ? "" : errorMessage(r));
    });
    const pw = $("#password-form");
    pw.addEventListener("submit", async (event) => {
      event.preventDefault();
      const r = await api("/api/v1/account/password", { method: "POST", body: Object.fromEntries(new FormData(pw)) });
      if (r.ok) location.assign("/login"); else showError("#dash-error", errorMessage(r));
    });
    $("#logout").addEventListener("click", async () => { await api("/api/v1/auth/logout", { method: "POST" }); location.assign("/"); });
  }

  if (page === "dashboard") {
    requireSession().then((ok) => {
      if (!ok) return;
      const section = $(".dash").dataset.section;
      ({ keys, usage: usagePage, logs: logsPage, settings: settingsPage }[section] || overview)().catch((e) => showError("#dash-error", String(e)));
    });
  }

  // Playground
  if (page === "playground") {
    let catalog = [];
    const select = $("#pg-api");
    const keyInput = $("#pg-key");
    try { keyInput.value = sessionStorage.getItem("ozy-playground-key") || ""; } catch { /* storage unavailable */ }
    keyInput.addEventListener("change", () => { try { sessionStorage.setItem("ozy-playground-key", keyInput.value); } catch { /* ignore */ } });

    function current() { return catalog.find((a) => a.slug === select.value); }
    function render() {
      const a = current();
      if (!a) return;
      const endpoint = $("#pg-endpoint");
      endpoint.replaceChildren(el("b", { class: "m-" + a.method.toLowerCase(), text: a.method }), " " + a.path);
      $("#pg-runtime").replaceChildren(el("span", { class: "badge badge-" + a.runtime, text: a.runtime_label }));
      $("#pg-docs").href = "/apis/" + a.slug;
      const isGet = a.method === "GET";
      $("#pg-query-label").hidden = !isGet;
      $("#pg-body-label").hidden = isGet;
      if (isGet) $("#pg-query").value = a.example_request.replace(/^\?/, "");
      else {
        try { $("#pg-body").value = JSON.stringify(JSON.parse(a.example_request), null, 2); } catch { $("#pg-body").value = a.example_request; }
      }
      $("#pg-hint").textContent = "Costs " + a.cost + " unit" + (a.cost === 1 ? "" : "s") + ". " + (a.runtime === "worker" ? "Returns 503 worker_unavailable unless the optional worker is configured." : "");
      history.replaceState(null, "", "/playground?api=" + a.slug);
    }

    api("/api/v1/catalog").then((r) => {
      catalog = (r.data && r.data.apis) || [];
      const wanted = new URLSearchParams(location.search).get("api");
      if (wanted && catalog.some((a) => a.slug === wanted)) select.value = wanted;
      render();
    });
    select.addEventListener("change", render);
    accountReady.then((r) => { if (!r.ok) $("#pg-hint").textContent = "Sign in or paste an API key to send requests."; });

    $("#pg-form").addEventListener("submit", async (event) => {
      event.preventDefault();
      const a = current();
      if (!a) return;
      const status = $("#pg-status");
      const output = $("#pg-output");
      const image = $("#pg-image");
      image.hidden = true;
      let path = a.path;
      const options = { method: a.method, key: keyInput.value.trim() || undefined };
      if (a.method === "GET") {
        path += "?" + $("#pg-query").value.replace(/^\?/, "");
      } else {
        try { options.body = JSON.parse($("#pg-body").value); } catch { status.textContent = "Invalid JSON"; status.className = "status-pill err"; return; }
      }
      const button = $("#pg-send");
      button.disabled = true;
      status.textContent = "Sending…";
      status.className = "status-pill";
      try {
        const r = await api(path, options);
        status.textContent = String(r.status);
        status.className = "status-pill " + (r.ok ? "ok" : "err");
        $("#pg-time").textContent = r.ms + " ms";
        let shown = r.data;
        if (r.ok && shown && typeof shown.image_base64 === "string") {
          image.src = "data:image/" + (shown.format || "png") + ";base64," + shown.image_base64;
          image.hidden = false;
          shown = Object.assign({}, shown, { image_base64: "[" + shown.image_base64.length + " base64 characters shown above]" });
        }
        output.textContent = typeof shown === "string" ? shown : JSON.stringify(shown, null, 2);
      } catch (error) {
        status.textContent = "Network error";
        status.className = "status-pill err";
        output.textContent = String(error);
      } finally {
        button.disabled = false;
      }
    });
  }

  // Status
  if (page === "status") {
    const label = { operational: ["Operational", "state-ok"], partial: ["Partial: worker features unavailable", "state-warn"], requires_worker: ["Requires optional worker (not configured)", "state-warn"], degraded: ["Degraded", "state-bad"] };
    async function load() {
      const r = await api("/api/v1/status");
      if (!r.ok || !r.data) return;
      const s = r.data;
      const tick = s.scheduler.last_tick ? when(s.scheduler.last_tick) : "never";
      $("#status-summary").replaceChildren(
        stat(s.database ? "Healthy" : "Unavailable", "Database (Supabase PostgreSQL)"),
        stat(s.worker.configured ? (s.worker.reachable ? "Reachable" : "Unreachable") : "Not configured", "Optional worker"),
        stat(s.scheduler.fresh_within_10_minutes ? "Running" : "Stale", "Scheduler · last tick " + tick),
        stat("v" + s.version, "Platform version"),
      );
      $("#status-apis").replaceChildren(...s.apis.map((a) => {
        const [text, cls] = label[a.availability] || [a.availability, ""];
        return el("tr", {}, el("td", {}, el("a", { href: "/apis/" + a.slug, text: a.name })), el("td", {}, el("span", { class: "badge badge-" + a.runtime, text: a.runtime })), el("td", {}, el("span", { class: "state " + cls, text: text })));
      }));
      $("#status-updated").textContent = "Checked " + when(s.checked_at) + ". Refreshes every 60 seconds.";
    }
    load();
    setInterval(load, 60000);
  }
})();
