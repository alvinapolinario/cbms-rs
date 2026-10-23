const KEY = "sabong.admin";
const NAV = [
  ["Home", "dashboard"],
  ["Admins", "accounts/admins"],
  ["Tellers", "accounts/tellers"],
  ["TV", "tv"],
  ["TV New", "tv-new"],
  ["Events", "events"],
  ["Matches", "matches"],
  ["Cash Transactions", "cash-transactions"],
  ["Teller Transactions", "teller-transactions"],
  ["Bets", "bets"],
  ["Unclaimed", "unclaim"],
  ["Remittances", "remittances"],
  ["Salaries", "salaries"],
  ["Reports", "reports"],
  ["Logs", "logs"],
  ["Reset", "reset"],
  ["Restore", "restore"],
];

let session = readSession();
let pollTimer = null;
let toastTimer = null;

function readSession() {
  try { return JSON.parse(sessionStorage.getItem(KEY) || "null"); }
  catch { return null; }
}

function writeSession(next) {
  session = next;
  if (next) sessionStorage.setItem(KEY, JSON.stringify(next));
  else sessionStorage.removeItem(KEY);
}

function path() {
  return location.hash.replace(/^#\/?/, "").replace(/\/$/, "");
}

function esc(value) {
  return String(value ?? "").replace(/[&<>"']/g, (ch) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[ch]));
}

function peso(value, digits = 2) {
  const n = Number(value);
  if (!Number.isFinite(n)) return value == null || value === "" ? "—" : String(value);
  return n.toLocaleString("en-PH", { minimumFractionDigits: digits, maximumFractionDigits: digits });
}

function odds(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return "0.0";
  const scaled = n * 10;
  const epsilon = 1e-6 * Math.max(1, Math.abs(scaled));
  const rounded = scaled >= 0 ? Math.floor(scaled + 0.5 + epsilon) : Math.ceil(scaled - 0.5 - epsilon);
  return (rounded / 10).toFixed(1);
}

function badge(value) {
  const text = value == null || value === "" ? "—" : String(value);
  return `<span class="badge ${esc(text).toLowerCase()}">${esc(text)}</span>`;
}

function toast(message, kind) {
  const el = document.getElementById("toast");
  el.hidden = false;
  el.className = "toast" + (kind === "err" ? " err" : "");
  el.textContent = message;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, 3400);
}

function stopPoll() {
  if (pollTimer) clearInterval(pollTimer);
  pollTimer = null;
}

let closeLive = null;
function stopLive() {
  if (closeLive) closeLive();
  closeLive = null;
}

// Same Pusher channels the Laravel screens listen to. A message means the row is already saved,
// so the page reloads that one board instead of waiting for the next timer.
function startLive(refresh) {
  stopLive();
  const url = `ws://${location.hostname}:6001/app/local?protocol=7&client=js&version=7.0.0&flash=false`;
  let socket = null;
  let stopped = false;
  let retry = null;
  let batch = null;
  const channels = ["match-updates", "fight-history", "tv-display"];
  function connect() {
    if (stopped) return;
    socket = new WebSocket(url);
    socket.onmessage = (message) => {
      let frame;
      try { frame = JSON.parse(message.data); } catch { return; }
      if (frame.event === "pusher:ping") {
        socket.send(JSON.stringify({ event: "pusher:pong", data: {} }));
        return;
      }
      if (frame.event === "pusher:connection_established") {
        channels.forEach((channel) => {
          socket.send(JSON.stringify({ event: "pusher:subscribe", data: { channel } }));
        });
        return;
      }
      if (!frame.event || String(frame.event).startsWith("pusher:")) return;
      clearTimeout(batch);
      batch = setTimeout(() => { refresh().catch(() => {}); }, 30);
    };
    socket.onclose = () => {
      if (!stopped) retry = setTimeout(connect, 1500);
    };
  }
  connect();
  closeLive = () => {
    stopped = true;
    clearTimeout(retry);
    clearTimeout(batch);
    if (socket) socket.close();
  };
}

function startPoll(fn, ms) {
  stopPoll();
  pollTimer = setInterval(fn, ms);
}

async function api(urlPath, options = {}) {
  const headers = { Accept: "application/json" };
  if (options.body !== undefined) headers["Content-Type"] = "application/json";
  if (options.auth !== false && session?.token) headers.Authorization = "Bearer " + session.token;
  const res = await fetch("/api" + urlPath, {
    method: options.method || "POST",
    headers,
    body: options.body !== undefined ? JSON.stringify(options.body) : undefined,
  });
  const text = await res.text();
  let data = text;
  if (text) {
    try { data = JSON.parse(text); } catch { data = text; }
  }
  if (!res.ok) {
    const message = (data && (data.message || data.error)) || text || res.statusText;
    if (res.status === 401 && options.auth !== false) {
      writeSession(null);
      location.hash = "#/login";
    }
    throw new Error(message || "Request failed");
  }
  return data;
}

function scalarOk(value) {
  return String(value).trim() === "1";
}

function formData(form) {
  return Object.fromEntries(new FormData(form).entries());
}

function openModal(title, body, onSubmit, submitLabel = "Save") {
  const dialog = document.getElementById("modal");
  dialog.innerHTML = `
    <form>
      <div class="modal-h"><h3>${esc(title)}</h3><button type="button" class="btn ghost" data-close>Close</button></div>
      <div class="modal-b">${body}</div>
      <div class="modal-f"><button class="btn" type="submit">${esc(submitLabel)}</button></div>
    </form>`;
  dialog.showModal();
  dialog.querySelector("[data-close]").onclick = () => dialog.close();
  dialog.querySelector("form").onsubmit = async (event) => {
    event.preventDefault();
    const button = dialog.querySelector("[type=submit]");
    button.disabled = true;
    try {
      await onSubmit(dialog.querySelector("form"));
      dialog.close();
    } catch (err) {
      toast(err.message || "Request failed", "err");
      button.disabled = false;
    }
  };
}

function field(label, name, value, type = "text", extra = "") {
  return `<label class="field">${esc(label)}<input name="${esc(name)}" type="${type}" value="${esc(value ?? "")}" ${extra}></label>`;
}

function selectField(label, name, options, selected) {
  const html = options.map(([value, text]) =>
    `<option value="${esc(value)}" ${String(value) === String(selected) ? "selected" : ""}>${esc(text)}</option>`
  ).join("");
  return `<label class="field">${esc(label)}<select name="${esc(name)}">${html}</select></label>`;
}

function table(columns, rows) {
  const head = columns.map((col) => `<th>${esc(col.label)}</th>`).join("");
  const body = rows.length
    ? rows.map((row) => `<tr>${columns.map((col) => `<td>${col.html ? col.html(row) : esc(row[col.key])}</td>`).join("")}</tr>`).join("")
    : `<tr><td colspan="${columns.length}" class="muted">Nothing here yet.</td></tr>`;
  return `<div class="scroll"><table><thead><tr>${head}</tr></thead><tbody>${body}</tbody></table></div>`;
}

async function eventOptions(select, preferActive) {
  const events = await api("/events-admin/list");
  const list = Array.isArray(events) ? events : [];
  select.innerHTML = list.map((event) =>
    `<option value="${event.event_id}">${esc(event.event_date)} · ${esc(event.event_status)} · ${esc(event.event_name)}</option>`
  ).join("") || `<option value="">No events</option>`;
  if (preferActive) {
    const active = list.find((event) => event.event_status === "Active");
    if (active) select.value = String(active.event_id);
  }
  return list;
}

function shell(title) {
  const links = NAV.map(([label, href]) =>
    `<a class="${path() === href ? "active" : ""} ${href.includes("/") ? "sub" : ""}" href="#/${href}">${esc(label)}</a>`
  ).join("");
  return `
    <div class="shell">
      <aside class="sidebar">
        <div class="brand">SA<span>BONG</span></div>
        <nav class="nav">
          <div class="group">Menu</div>
          ${links}
          <button class="link" id="logout" type="button">Logout</button>
        </nav>
      </aside>
      <section class="main">
        <header class="topbar">
          <button class="btn ghost menu-btn" id="nav-toggle" type="button">Menu</button>
          <h1>${esc(title)}</h1>
          <div class="who">${esc(session?.adminName || "Admin")}${session?.eventName ? " · " + esc(session.eventName) : ""}</div>
        </header>
        <div class="content" id="view"></div>
      </section>
    </div>`;
}

function mount(title, render) {
  stopPoll();
  stopLive();
  document.body.classList.remove("nav-open");
  document.getElementById("modal").close();
  document.getElementById("app").innerHTML = shell(title);
  document.getElementById("logout").onclick = async () => {
    try { await api("/logout"); } catch { /* token may already be gone */ }
    writeSession(null);
    location.hash = "#/login";
  };
  document.getElementById("nav-toggle").onclick = () => document.body.classList.toggle("nav-open");
  document.querySelectorAll(".nav a").forEach((link) => {
    link.addEventListener("click", () => document.body.classList.remove("nav-open"));
  });
  return render(document.getElementById("view"));
}

function loginPage() {
  stopPoll();
  stopLive();
  document.body.classList.remove("nav-open");
  const remembered = localStorage.getItem("sabong.username") || "";
  document.getElementById("app").innerHTML = `
    <div class="login-wrap">
      <form class="login-card" id="login-form">
        <div class="mark">SABONG</div>
        <h2>Welcome back</h2>
        <p>Sign in with an admin account</p>
        ${field("Username", "username", remembered, "text", "required autofocus")}
        ${field("Password", "password", "", "password", "required")}
        <label class="field" style="flex-direction:row;align-items:center;color:var(--text)">
          <input type="checkbox" name="remember" ${remembered ? "checked" : ""} style="width:auto"> Remember me
        </label>
        <button class="btn block" type="submit">Sign in</button>
      </form>
    </div>`;
  document.getElementById("login-form").onsubmit = async (event) => {
    event.preventDefault();
    const form = event.target;
    const button = form.querySelector("button");
    button.disabled = true;
    const data = formData(form);
    try {
      const result = await api("/login-app-admin", {
        auth: false,
        body: { username: data.username, password: data.password },
      });
      if (data.remember) localStorage.setItem("sabong.username", data.username);
      else localStorage.removeItem("sabong.username");
      writeSession({
        token: result.token,
        adminName: result.admin?.admin_name || data.username,
        eventName: result.event?.event_name || "",
      });
      location.hash = "#/dashboard";
    } catch (err) {
      toast(err.message || "Login failed", "err");
      button.disabled = false;
    }
  };
}

async function pageHome(view) {
  view.innerHTML = `<p class="muted">Loading the active event…</p>`;
  let events = [];
  let board = { match: null, matches: [] };
  try { events = await api("/events-admin/list"); } catch (err) { toast(err.message, "err"); }
  try { board = await api("/matches-admin/list"); } catch (err) { toast(err.message, "err"); }
  const list = Array.isArray(events) ? events : [];
  const active = list.find((event) => event.event_status === "Active") || null;
  const fights = Array.isArray(board.matches) ? board.matches : [];
  const current = board.match;
  const percentage = Number(active?.event_percentage || 0);
  const completed = fights.filter((fight) => fight.match_status === "Completed");
  const totalBets = fights.reduce((sum, fight) => sum + Number(fight.meron_total_bet || 0) + Number(fight.wala_total_bet || 0), 0);
  const meronWins = fights.filter((fight) => fight.match_winner === "Meron").length;
  const walaWins = fights.filter((fight) => fight.match_winner === "Wala").length;
  const maxPool = Math.max(1, ...completed.map((fight) => Number(fight.meron_total_bet || 0) + Number(fight.wala_total_bet || 0)));
  const bars = [...completed].sort((a, b) => a.match_number - b.match_number).slice(-12).map((fight) => {
    const meron = Number(fight.meron_total_bet || 0);
    const wala = Number(fight.wala_total_bet || 0);
    return `<div class="bar-row"><span>#${fight.match_number}</span><div class="bar"><i class="m" style="width:${(meron / maxPool) * 100}%"></i><i class="w" style="width:${(wala / maxPool) * 100}%"></i></div><span>${peso(meron + wala)}</span></div>`;
  }).join("") || `<p class="muted">No completed fights yet.</p>`;
  const recent = [...fights].sort((a, b) => b.match_number - a.match_number);
  view.innerHTML = `
    <div class="card" style="margin-bottom:16px">Welcome back, <strong>${esc(session.adminName)}</strong>. Manage fights, cash, and the day's take from here.</div>
    <div class="grid grid-3">
      <div class="card stat bg-accent"><h3>Current event</h3><p>${esc(active?.event_name || "None")}</p></div>
      <div class="card stat bg-ok"><h3>Current fight</h3><p>${current ? "#" + current.match_number : "—"}</p></div>
      <div class="card stat bg-meron"><h3>Total bets</h3><p>${peso(totalBets)}</p></div>
    </div>
    <div class="grid grid-2" style="margin-top:16px">
      <div class="card"><h3>Completed fight pools</h3><p class="muted">Red is Meron, blue is Wala. House cut on this event is ${esc(percentage)}.</p><div class="bars">${bars}</div></div>
      <div class="card">
        <h3>Results</h3>
        <p>Meron wins <strong class="meron-text">${meronWins}</strong> · Wala wins <strong class="wala-text">${walaWins}</strong></p>
        <p>Completed ${completed.length} · Ongoing ${fights.filter((f) => f.match_status === "Ongoing").length} · Draw ${fights.filter((f) => f.match_status === "Draw").length} · Cancelled ${fights.filter((f) => f.match_status === "Cancelled").length}</p>
        <p class="muted">Estimated house take on completed fights: ${peso(completed.reduce((sum, fight) => sum + (Number(fight.meron_total_bet || 0) + Number(fight.wala_total_bet || 0)) * percentage, 0))}</p>
      </div>
    </div>
    <div class="card" style="margin-top:16px">
      <h3>Recent fights</h3>
      ${table([
        { label: "Fight", html: (row) => esc(row.match_number) },
        { label: "Meron", html: (row) => peso(row.meron_total_bet) },
        { label: "Wala", html: (row) => peso(row.wala_total_bet) },
        { label: "Winner", html: (row) => row.match_winner === "Meron" ? `<span class="meron-text">Meron</span>` : row.match_winner === "Wala" ? `<span class="wala-text">Wala</span>` : "—" },
        { label: "Odds", html: (row) => row.match_winner === "Meron" ? odds(row.meron_odds) : row.match_winner === "Wala" ? odds(row.wala_odds) : "—" },
        { label: "Status", html: (row) => badge(row.match_status) },
      ], recent)}
    </div>`;
}

function accountPages(kind) {
  const isAdmin = kind === "admin";
  return async function (view) {
    const title = isAdmin ? "Admins" : "Tellers";
    view.innerHTML = `<div class="row"><button class="btn" id="add">Add ${title.slice(0, -1)}</button></div><div id="list"></div>`;
    async function load() {
      const rows = await api(isAdmin ? "/accounts/admins/list" : "/accounts/tellers/list");
      const list = Array.isArray(rows) ? rows : [];
      const columns = isAdmin
        ? [
          { label: "Name", key: "admin_name" },
          { label: "Username", key: "username" },
          { label: "Active", html: (row) => badge(Number(row.is_active) === 1 ? "Active" : "Inactive") },
          { label: "", html: (row) => `<button class="btn ghost" data-edit="${row.admin_id}">Edit</button> <button class="btn danger" data-del="${row.admin_id}">Delete</button>` },
        ]
        : [
          { label: "Name", key: "teller_name" },
          { label: "Username", key: "username" },
          { label: "Contact", key: "contact_number" },
          { label: "Phone", html: (row) => esc(row.phone_uid || "—") },
          { label: "Active", html: (row) => badge(Number(row.is_active) === 1 ? "Active" : "Inactive") },
          { label: "", html: (row) => `<button class="btn ghost" data-edit="${row.teller_id}">Edit</button> <button class="btn ghost" data-reset="${row.teller_id}">Reset account</button> <button class="btn ghost" data-clear="${row.teller_id}">Clear phone</button> <button class="btn danger" data-del="${row.teller_id}">Delete</button>` },
        ];
      document.getElementById("list").innerHTML = `<div class="card">${table(columns, list)}</div>`;
      document.getElementById("list").onclick = async (event) => {
        const edit = event.target.closest("[data-edit]");
        const del = event.target.closest("[data-del]");
        const clear = event.target.closest("[data-clear]");
        const reset = event.target.closest("[data-reset]");
        try {
          if (edit) await editAccount(isAdmin, list.find((row) => String(isAdmin ? row.admin_id : row.teller_id) === edit.dataset.edit), load);
          if (reset) {
            const teller = list.find((row) => String(row.teller_id) === reset.dataset.reset);
            openModal(
              "Reset account",
              `<p>Unlocks the phone linked to ${esc(teller?.teller_name || "this teller")} and sets a new login password.</p>
               ${field("New password", "password", "", "password", "required autocomplete=\"new-password\"")}`,
              async (form) => {
                const data = formData(form);
                if (!String(data.password || "").trim()) throw new Error("Type a new password.");
                const result = await api("/accounts/tellers/reset-account", { body: { teller_id: Number(reset.dataset.reset), password: data.password } });
                if (!scalarOk(result)) throw new Error("Could not reset the account.");
                toast("Account reset. The phone is unlocked.");
                await load();
              },
              "Reset",
            );
          }
          if (clear && confirm("Clear this teller's phone lock?")) {
            const result = await api(`/accounts/tellers/${clear.dataset.clear}/clear-phone-uid`);
            if (!scalarOk(result)) throw new Error("Could not clear the phone.");
            toast("Phone lock cleared.");
            await load();
          }
          if (del && confirm("Delete this account?")) {
            const result = await api(`${isAdmin ? "/accounts/admins/" : "/accounts/tellers/"}${del.dataset.del}`, { method: "DELETE" });
            if (!scalarOk(result)) throw new Error("Could not delete.");
            toast("Deleted.");
            await load();
          }
        } catch (err) { toast(err.message, "err"); }
      };
    }
    document.getElementById("add").onclick = () => editAccount(isAdmin, null, load);
    try { await load(); } catch (err) { toast(err.message, "err"); }
  };
}

function editAccount(isAdmin, row, reload) {
  const active = row ? Number(row.is_active) === 1 : true;
  const body = isAdmin
    ? `${row ? "" : field("Username", "username", "", "text", "required")}
       ${field("Name", isAdmin ? "admin_name" : "teller_name", row?.admin_name || "", "text", "required")}
       ${field("Password", "password", "", "password", row ? "" : "required")}
       ${selectField("Active", "is_active", [["1", "Active"], ["0", "Inactive"]], active ? "1" : "0")}`
    : `${row ? "" : field("Username", "username", "", "text", "required")}
       ${field("Name", "teller_name", row?.teller_name || "", "text", "required")}
       ${field("Contact", "contact_number", row?.contact_number || "")}
       ${field("Password", "password", "", "password", row ? "" : "required")}
       ${selectField("Active", "is_active", [["1", "Active"], ["0", "Inactive"]], active ? "1" : "0")}`;
  openModal(row ? "Edit" : "Add", body, async (form) => {
    const data = formData(form);
    let result;
    if (!row && isAdmin) {
      result = await api("/accounts/admins/store", { body: { username: data.username, password: data.password, admin_name: data.admin_name } });
    } else if (!row) {
      result = await api("/accounts/tellers/store", { body: { username: data.username, password: data.password, teller_name: data.teller_name, contact_number: data.contact_number } });
    } else if (isAdmin) {
      result = await api("/accounts/admins/update", { body: { uid: row.admin_id, _password: data.password || "", _is_active: data.is_active, _admin_name: data.admin_name } });
    } else {
      result = await api("/accounts/tellers/update", { body: { uid: row.teller_id, _password: data.password || "", _is_active: data.is_active, _teller_name: data.teller_name, _contact_number: data.contact_number } });
    }
    if (String(result).trim() === "-1") throw new Error("That username is already used.");
    if (!scalarOk(result)) throw new Error("Save failed.");
    toast("Saved.");
    await reload();
  });
}

async function pageEvents(view) {
  view.innerHTML = `<div class="row"><button class="btn" id="add">Add event</button></div><div id="list"></div>`;
  async function load() {
    const rows = await api("/events-admin/list");
    const list = Array.isArray(rows) ? rows : [];
    document.getElementById("list").innerHTML = `<div class="card">${table([
      { label: "Date", key: "event_date" },
      { label: "Name", key: "event_name" },
      { label: "Status", html: (row) => badge(row.event_status) },
      { label: "Cut", key: "event_percentage" },
      { label: "Admin cash", html: (row) => peso(row.admin_cash_on_hand) },
      { label: "", html: (row) => `<button class="btn ghost" data-edit="${row.event_id}">Edit</button> <button class="btn ghost" data-tellers="${row.event_id}">Tellers</button> <button class="btn danger" data-del="${row.event_id}">Delete</button>` },
    ], list)}</div>`;
    document.getElementById("list").onclick = async (event) => {
      const row = list.find((item) => String(item.event_id) === (event.target.dataset.edit || event.target.dataset.tellers || event.target.dataset.del));
      try {
        if (event.target.dataset.edit) await editEvent(row, load);
        if (event.target.dataset.tellers) await assignTellers(row);
        if (event.target.dataset.del && confirm("Delete this event?")) {
          const result = await api("/events-admin/delete", { body: { event_id: row.event_id } });
          if (!scalarOk(result)) throw new Error("Could not delete.");
          toast("Deleted.");
          await load();
        }
      } catch (err) { toast(err.message, "err"); }
    };
  }
  document.getElementById("add").onclick = () => editEvent(null, load);
  try { await load(); } catch (err) { toast(err.message, "err"); }
}

function eventForm(row) {
  return `
    ${field("Name", "event_name", row?.event_name || "", "text", "required")}
    ${field("Description", "event_description", row?.event_description || "")}
    ${field("House cut", "event_percentage", row?.event_percentage ?? "0.070", "number", 'step="0.001" min="0" max="1" required')}
    ${field("Date", "event_date", row?.event_date || "", "date", "required")}
    ${selectField("Status", "event_status", [["Active", "Active"], ["Completed", "Completed"]], row?.event_status || "Completed")}
    ${field("Admin cash on hand", "admin_cash_on_hand", row?.admin_cash_on_hand ?? "0", "number", 'step="0.01" required')}
    ${field("Teller opening cash", "teller_initial_cash_on_hand", row?.teller_initial_cash_on_hand ?? "0", "number", 'step="0.01" required')}
    ${field("Prizes", "prizes", row?.prizes ?? "0", "number", 'step="0.01" required')}
    ${field("RD", "rd", row?.rd ?? "0", "number", 'step="0.01" required')}`;
}

function eventBody(data) {
  const get = (name) => data[name];
  return {
    event_name: get("event_name"),
    event_description: get("event_description"),
    event_percentage: Number(get("event_percentage")),
    event_date: get("event_date"),
    event_status: get("event_status"),
    admin_cash_on_hand: Number(get("admin_cash_on_hand")),
    teller_initial_cash_on_hand: Number(get("teller_initial_cash_on_hand")),
    prizes: Number(get("prizes")),
    rd: Number(get("rd")),
  };
}

function editEvent(row, reload) {
  openModal(row ? "Edit event" : "Add event", eventForm(row), async (form) => {
    const fields = eventBody(formData(form), false);
    let result;
    if (!row) {
      result = await api("/events-admin/store", { body: fields });
    } else {
      const body = { uid: row.event_id };
      Object.entries(fields).forEach(([key, value]) => { body["_" + key] = value; });
      result = await api("/events-admin/update", { body });
    }
    if (!scalarOk(result)) throw new Error(typeof result === "object" && result?.message ? result.message : "Save failed.");
    toast("Saved.");
    await reload();
  });
}

async function assignTellers(eventRow) {
  const data = await api("/events-admin/tellers", { body: { event_id: eventRow.event_id } });
  const assigned = new Set((data.assigned_tellers || []).map(String));
  const checks = (data.tellers || []).map((teller) =>
    `<label><input type="checkbox" value="${teller.teller_id}" ${assigned.has(String(teller.teller_id)) ? "checked" : ""}> ${esc(teller.teller_name)}</label>`
  ).join("");
  openModal("Tellers on " + eventRow.event_name, `<div class="check-list">${checks || "<p>No tellers yet.</p>"}</div>`, async (form) => {
    const tellers = [...form.querySelectorAll("input:checked")].map((input) => Number(input.value));
    await api("/events-admin/update-tellers", { body: { event_id: eventRow.event_id, tellers } });
    toast("Tellers updated.");
  });
}

function longDate(value) {
  if (!value) return "";
  const date = new Date(`${String(value).slice(0, 10)}T00:00:00`);
  if (Number.isNaN(date.getTime())) return String(value);
  return date.toLocaleDateString("en-US", { month: "long", day: "numeric", year: "numeric" });
}

function fightResultValue(fight) {
  if (fight.match_status === "Draw") return "Draw";
  if (fight.match_status === "Cancelled") return "Cancelled";
  if (fight.match_winner === "Wala") return "Wala";
  return "Meron";
}

async function pageMatches(view) {
  view.innerHTML = `
    <div class="matches-intro">
      <h2 id="event-name">No Event</h2>
      <p id="event-date"></p>
      <button class="btn lg" id="get-started" type="button" hidden>View Matches</button>
    </div>
    <div id="board" hidden>
      <div class="fight-grid">
        <div>
          <div class="fight-head">
            <h2>Fight # <b class="fight-no" id="match-number"></b></h2>
            <h2>Bet Status <span id="bet-status"></span></h2>
          </div>
          <h3 class="total-bets">Total Bets: <span id="total-bets"></span></h3>
          <div class="grid grid-2">
            <div>
              <div class="card side bg-meron" id="card-meron">
                <h3>Meron</h3>
                <input id="meron-entry" name="meron_entry">
                <div class="amt" id="meron-bet"></div>
                <div class="odds" id="meron-odds"></div>
                <span class="lock-note" id="meron-lock" hidden>Betting Disabled</span>
              </div>
              <div class="side-toggle"><button class="btn lg meron" id="toggle-meron" type="button">Disable Meron Bet</button></div>
              <div class="card action-box">
                <button class="btn lg ok" id="open-bet" type="button">Open Bet</button>
                <button class="btn lg warn" id="close-bet" type="button">Close Bet</button>
              </div>
            </div>
            <div>
              <div class="card side bg-wala" id="card-wala">
                <h3>Wala</h3>
                <input id="wala-entry" name="wala_entry">
                <div class="amt" id="wala-bet"></div>
                <div class="odds" id="wala-odds"></div>
                <span class="lock-note" id="wala-lock" hidden>Betting Disabled</span>
              </div>
              <div class="side-toggle"><button class="btn lg wala" id="toggle-wala" type="button">Disable Wala Bet</button></div>
              <div class="card action-box">
                <div class="grid grid-2">
                  <button class="btn lg meron" id="meron-wins" type="button">Meron Wins</button>
                  <button class="btn lg wala" id="wala-wins" type="button">Wala Wins</button>
                  <button class="btn lg warn" id="cancel-fight" type="button">Cancel Fight</button>
                  <button class="btn lg gray" id="draw-fight" type="button">Draw Fight</button>
                </div>
              </div>
            </div>
          </div>
        </div>
        <div class="card">
          <h3>Fights History</h3>
          <div class="scroll"><table id="fight-history"><thead><tr><th>Fight #</th><th>Winner</th><th>Odds</th><th>Status</th><th></th><th>TV</th></tr></thead><tbody id="history"></tbody></table></div>
        </div>
      </div>
    </div>`;
  let current = null;
  let signature = "";
  let started = false;
  const meronEntry = view.querySelector("#meron-entry");
  const walaEntry = view.querySelector("#wala-entry");
  meronEntry.oninput = () => { meronEntry.dataset.touched = "1"; };
  walaEntry.oninput = () => { walaEntry.dataset.touched = "1"; };

  function sideButton(button, locked, enableLabel, disableLabel, openClass) {
    button.textContent = locked ? enableLabel : disableLabel;
    button.className = "btn lg " + (locked ? "ok" : openClass);
  }

  function historyWinner(fight) {
    if (fight.match_winner === "Meron") return `<span class="meron-text">Meron</span>`;
    if (fight.match_winner === "Wala") return `<span class="wala-text">Wala</span>`;
    return `<span class="muted">${esc(fight.match_winner || "TBD")}</span>`;
  }

  function historyOdds(fight) {
    if (fight.match_winner === "Meron") return odds(fight.meron_odds);
    if (fight.match_winner === "Wala") return odds(fight.wala_odds);
    return "N/A";
  }

  function historyStatus(status) {
    const kind = status === "Completed" ? "ok" : status === "Ongoing" ? "warn" : "danger";
    return `<span class="status-${kind}">${esc(status)}</span>`;
  }

  async function refresh() {
    if (path() !== "matches" || !started) return;
    const epoch = boardEpoch;
    const data = await api("/matches-admin/list");
    if (epoch !== boardEpoch || path() !== "matches") return;
    const match = data.match;
    view.querySelector("#board").hidden = !match;
    if (!match) return;
    if (current?.match_id !== match.match_id) {
      meronEntry.dataset.touched = "";
      walaEntry.dataset.touched = "";
    }
    current = match;
    if (!meronEntry.dataset.touched && document.activeElement !== meronEntry) meronEntry.value = match.meron_entry || "";
    if (!walaEntry.dataset.touched && document.activeElement !== walaEntry) walaEntry.value = match.wala_entry || "";
    view.querySelector("#match-number").textContent = match.match_number;
    view.querySelector("#bet-status").innerHTML = badge(match.match_bet_status);
    view.querySelector("#meron-bet").textContent = peso(match.meron_total_bet);
    view.querySelector("#wala-bet").textContent = peso(match.wala_total_bet);
    view.querySelector("#meron-odds").textContent = odds(match.meron_odds);
    view.querySelector("#wala-odds").textContent = odds(match.wala_odds);
    view.querySelector("#total-bets").textContent = peso(Number(match.meron_total_bet) + Number(match.wala_total_bet));
    const meronLocked = Number(match.meron_bet_status) === 1;
    const walaLocked = Number(match.wala_bet_status) === 1;
    const betsOpen = match.match_bet_status === "Open";
    view.querySelector("#meron-lock").hidden = !meronLocked;
    view.querySelector("#wala-lock").hidden = !walaLocked;
    sideButton(view.querySelector("#toggle-meron"), meronLocked, "Enable Meron Bet", "Disable Meron Bet", "meron");
    sideButton(view.querySelector("#toggle-wala"), walaLocked, "Enable Wala Bet", "Disable Wala Bet", "wala");
    view.querySelector("#toggle-meron").disabled = !betsOpen;
    view.querySelector("#toggle-wala").disabled = !betsOpen;
    applyPendingLocks();
    view.querySelector("#close-bet").disabled = !betsOpen;
    view.querySelector("#open-bet").disabled = betsOpen;
    view.querySelector("#meron-wins").disabled = betsOpen;
    view.querySelector("#wala-wins").disabled = betsOpen;
    view.querySelector("#cancel-fight").disabled = betsOpen;
    view.querySelector("#draw-fight").disabled = betsOpen;
    if (pendingBet) paintBet(pendingBet);
    const nextSig = (data.matches || []).map((fight) => [fight.match_id, fight.match_status, fight.match_winner, fight.meron_odds, fight.wala_odds, fight.is_display].join(":")).join("|");
    if (nextSig !== signature) {
      signature = nextSig;
      view.querySelector("#history").innerHTML = (data.matches || []).map((fight) => {
        const finished = fight.match_status !== "Ongoing";
        const change = finished
          ? `<button class="btn warn change-result" data-fix="${fight.match_id}" data-number="${esc(fight.match_number)}" data-winner="${esc(fightResultValue(fight))}" type="button">Change Result</button>`
          : "";
        const display = finished
          ? `<input type="checkbox" class="toggle-display" data-match-id="${fight.match_id}" ${Number(fight.is_display) === 1 ? "checked" : ""}>`
          : "";
        return `<tr>
          <td>${esc(fight.match_number)}</td>
          <td>${historyWinner(fight)}</td>
          <td>${historyOdds(fight)}</td>
          <td>${historyStatus(fight.match_status)}</td>
          <td>${change}</td>
          <td>${display}</td>
        </tr>`;
      }).join("") || `<tr><td colspan="6" class="muted">No fights yet.</td></tr>`;
    }
  }

  const pendingLock = { meron: null, wala: null };
  let pendingBet = null;
  let boardEpoch = 0;

  function paintBet(status) {
    const open = status === "Open";
    view.querySelector("#bet-status").innerHTML = badge(status);
    view.querySelector("#close-bet").disabled = !open;
    view.querySelector("#open-bet").disabled = open;
    view.querySelector("#toggle-meron").disabled = !open;
    view.querySelector("#toggle-wala").disabled = !open;
    view.querySelector("#meron-wins").disabled = open;
    view.querySelector("#wala-wins").disabled = open;
    view.querySelector("#cancel-fight").disabled = open;
    view.querySelector("#draw-fight").disabled = open;
  }

  function paintSide(side, locked) {
    if (side === "meron") {
      view.querySelector("#meron-lock").hidden = !locked;
      sideButton(view.querySelector("#toggle-meron"), locked, "Enable Meron Bet", "Disable Meron Bet", "meron");
    } else {
      view.querySelector("#wala-lock").hidden = !locked;
      sideButton(view.querySelector("#toggle-wala"), locked, "Enable Wala Bet", "Disable Wala Bet", "wala");
    }
  }

  function applyPendingLocks() {
    if (pendingLock.meron !== null) paintSide("meron", pendingLock.meron);
    if (pendingLock.wala !== null) paintSide("wala", pendingLock.wala);
  }

  async function act(work) {
    try { await work(); await refresh(); }
    catch (err) { toast(err.message, "err"); }
  }

  async function toggleSide(side, urlPath) {
    const button = view.querySelector(side === "meron" ? "#toggle-meron" : "#toggle-wala");
    if (!current || button.disabled || pendingLock[side] !== null) return;
    boardEpoch += 1;
    const was = side === "meron" ? Number(current.meron_bet_status) === 1 : Number(current.wala_bet_status) === 1;
    pendingLock[side] = !was;
    paintSide(side, !was);
    try {
      await api(urlPath, { body: { match_id: current.match_id } });
      boardEpoch += 1;
      current[side === "meron" ? "meron_bet_status" : "wala_bet_status"] = pendingLock[side] ? 1 : 0;
      pendingLock[side] = null;
      refresh().catch(() => {});
    } catch (err) {
      boardEpoch += 1;
      pendingLock[side] = null;
      paintSide(side, was);
      toast(err.message, "err");
    }
  }

  async function entries() {
    return { meron_entry: meronEntry.value, wala_entry: walaEntry.value };
  }

  view.querySelector("#get-started").onclick = async () => {
    const button = view.querySelector("#get-started");
    button.disabled = true;
    try {
      const data = await api("/matches-admin/get-started");
      if (data && data.success === false) throw new Error(data.message || "Failed to get started.");
      started = true;
      button.hidden = true;
      view.querySelector("#board").hidden = false;
      startPoll(() => { refresh().catch(() => {}); }, 2000);
      startLive(() => refresh());
      await refresh();
    } catch (err) {
      toast(err.message || "Failed to get started.", "err");
      button.disabled = false;
    }
  };
  view.querySelector("#toggle-meron").onclick = () => toggleSide("meron", "/matches-admin/toggle-meron");
  view.querySelector("#toggle-wala").onclick = () => toggleSide("wala", "/matches-admin/toggle-wala");
  function setBetStatus(status, prompt) {
    if (!current || pendingBet) return;
    if (!confirm(prompt)) return;
    const matchId = current.match_id;
    const previous = current.match_bet_status;
    pendingBet = status;
    boardEpoch += 1;
    paintBet(status);
    entries().then((fields) => api("/matches-admin/update-bet-status", {
      body: { match_id: matchId, match_bet_status: status, ...fields },
    })).then(() => {
      pendingBet = null;
      if (current && current.match_id === matchId) current.match_bet_status = status;
      refresh().catch(() => {});
    }).catch((err) => {
      pendingBet = null;
      boardEpoch += 1;
      if (current && current.match_id === matchId) current.match_bet_status = previous;
      paintBet(previous);
      toast(err.message, "err");
    });
  }
  view.querySelector("#open-bet").onclick = () => setBetStatus("Open", "You are about to open the bet for this match.");
  view.querySelector("#close-bet").onclick = () => setBetStatus("Closed", "You are about to close the bet for this match.");
  async function settle(status, winner, message) {
    if (!current || !confirm(message)) return;
    await act(() => api("/matches-admin/update-match-status", { body: { match_id: current.match_id, match_status: status, winner } }));
  }
  view.querySelector("#meron-wins").onclick = () => settle("Completed", "Meron", "You are about to set Meron as the winner.");
  view.querySelector("#wala-wins").onclick = () => settle("Completed", "Wala", "You are about to set Wala as the winner.");
  view.querySelector("#cancel-fight").onclick = () => settle("Cancelled", "-", "You are about to cancel this fight.");
  view.querySelector("#draw-fight").onclick = () => settle("Draw", "-", "You are about to draw this fight.");
  view.querySelector("#history").onclick = (event) => {
    const button = event.target.closest("[data-fix]");
    if (!button) return;
    const matchId = Number(button.dataset.fix);
    const matchNumber = button.dataset.number;
    openModal(
      `Change Result for Fight #${matchNumber}`,
      `<p>Select a new winner</p>${selectField("Winner", "winner", [["Meron", "Meron"], ["Wala", "Wala"], ["Draw", "Draw"], ["Cancelled", "Cancelled"]], button.dataset.winner)}`,
      async (form) => {
        const winner = formData(form).winner;
        await api("/matches-admin/update-match-status", {
          body: { match_id: matchId, match_status: "Completed", winner, dont_create: true },
        });
        toast("Result corrected.");
        await refresh();
      },
      "Confirm",
    );
  };
  view.querySelector("#history").onchange = async (event) => {
    const box = event.target.closest(".toggle-display");
    if (!box) return;
    try {
      await api("/matches-admin/update-display", { body: { match_id: Number(box.dataset.matchId), is_display: box.checked ? 1 : 0 } });
      toast("TV display status updated successfully.");
      await refresh();
    } catch (err) {
      box.checked = !box.checked;
      toast(err.message, "err");
    }
  };

  try {
    const events = await api("/events-admin/list");
    const active = (Array.isArray(events) ? events : []).find((event) => event.event_status === "Active") || null;
    view.querySelector("#event-name").textContent = active?.event_name || "No Event";
    if (active?.event_date) {
      view.querySelector("#event-date").textContent = longDate(active.event_date);
      view.querySelector("#get-started").hidden = false;
    }
  } catch (err) {
    toast(err.message, "err");
  }
}

function balls(matches) {
  const ordered = [...(matches || [])].reverse().filter((fight) =>
    fight.match_winner || fight.match_status === "Draw" || fight.match_status === "Cancelled"
  );
  const columns = [];
  let last = "";
  ordered.forEach((fight) => {
    let kind = "cancel";
    let result = "Cancelled";
    if (fight.match_winner === "Meron") { kind = "meron"; result = "Meron"; }
    else if (fight.match_winner === "Wala") { kind = "wala"; result = "Wala"; }
    else if (fight.match_status === "Draw") { kind = "draw"; result = "Draw"; }
    const column = columns[columns.length - 1];
    if (!column || column.items.length >= 5 || result !== last) columns.push({ items: [] });
    columns[columns.length - 1].items.push(`<div class="ball ${kind}">${esc(fight.match_number)}</div>`);
    last = result;
  });
  return `<div class="balls">${columns.map((column) => `<div class="col">${column.items.join("")}</div>`).join("")}</div>`;
}

function stickBallsToRight(board) {
  const strip = board.querySelector(".balls");
  if (!strip) return;
  const follow = board.dataset.ballsFollow !== "0";
  if (follow) strip.scrollLeft = strip.scrollWidth;
  strip.onscroll = () => {
    const atRight = strip.scrollLeft + strip.clientWidth >= strip.scrollWidth - 4;
    board.dataset.ballsFollow = atRight ? "1" : "0";
  };
}

function tvMarkup(match) {
  const closed = match?.match_bet_status === "Closed";
  const meronLocked = Number(match?.meron_bet_status) === 1;
  const walaLocked = Number(match?.wala_bet_status) === 1;
  return `
    <div class="tv-top">
      <h2>FIGHT # <b id="tv-number">${esc(match?.match_number ?? "—")}</b></h2>
      <div class="tv-status ${closed ? "closed" : ""}" id="tv-status">${esc(match?.match_bet_status || "")}</div>
    </div>
    <div class="tv-sides">
      <div class="tv-side ${meronLocked ? "" : "bg-meron"}" id="tv-meron" style="${meronLocked ? "background:#9a8f8f" : ""}">
        <h3>MERON</h3>
        <div class="entry">${esc(match?.meron_entry || "")}</div>
        <div class="amt">${peso(match?.meron_total_bet || 0, 0)}</div>
        <div class="odds">${odds(match?.meron_odds)}</div>
        <div class="lock">${meronLocked ? "DISABLED" : ""}</div>
      </div>
      <div class="tv-side ${walaLocked ? "" : "bg-wala"}" style="${walaLocked ? "background:#9a8f8f" : ""}">
        <h3>WALA</h3>
        <div class="entry">${esc(match?.wala_entry || "")}</div>
        <div class="amt">${peso(match?.wala_total_bet || 0, 0)}</div>
        <div class="odds">${odds(match?.wala_odds)}</div>
        <div class="lock">${walaLocked ? "DISABLED" : ""}</div>
      </div>
    </div>`;
}

function resultOverlay(matches) {
  const shown = (matches || []).find((fight) => Number(fight.is_display) === 1);
  if (!shown) return "";
  let kind = "cancel";
  let name = "Cancelled";
  let word = "";
  if (shown.match_winner === "Meron") { kind = "meron"; name = "Meron"; word = "Wins"; }
  else if (shown.match_winner === "Wala") { kind = "wala"; name = "Wala"; word = "Wins"; }
  else if (shown.match_status === "Draw") { kind = "draw"; name = "Draw"; }
  const winner = kind === "meron" || kind === "wala";
  const shownOdds = shown.match_winner === "Wala" ? odds(shown.wala_odds) : shown.match_winner === "Meron" ? odds(shown.meron_odds) : "";
  const sparks = winner
    ? Array.from({ length: 16 }, (_, i) => `<i class="spark" style="--i:${i}"></i>`).join("")
    : "";
  return `<div class="overlay ${kind}${winner ? " winner" : ""}" role="status">
    <div class="burst" aria-hidden="true">${sparks}</div>
    <div class="declare">
      <p class="fight-label">Fight #${esc(shown.match_number)}</p>
      <h2 class="winner-name">${esc(name)}</h2>
      ${word ? `<p class="winner-word">${esc(word)}</p>` : ""}
      ${shownOdds ? `<p class="winner-odds"><span>${esc(shownOdds)}</span></p>` : ""}
    </div>
  </div>`;
}

async function pageTv(_view, variant) {
  stopPoll();
  stopLive();
  const savedOverlay = localStorage.getItem("sabong.overlay") || "";
  const frame = variant !== "new" ? "" : savedOverlay
    ? `<iframe class="overlay-frame" src="${esc(savedOverlay)}" title="Overlay"></iframe>`
    : `<div class="overlay-empty"><div><p>Video overlay</p><input id="overlay-url" placeholder="https://…" value=""><button class="btn" id="overlay-save" type="button">Use URL</button></div></div>`;
  document.getElementById("app").innerHTML = `
    <div class="tv" id="tv-root">${variant === "new" ? `<div class="tv-new"><div id="tv-frame">${frame}</div><div id="tv-board"></div></div>` : `<div id="tv-board"></div>`}</div>
    ${session ? '<a class="desk-link" href="#/dashboard">Desk</a>' : ""}`;
  const board = document.getElementById("tv-board");
  const save = document.getElementById("overlay-save");
  if (save) save.onclick = () => {
    localStorage.setItem("sabong.overlay", document.getElementById("overlay-url").value.trim());
    pageTv(null, "new");
  };
  async function refresh() {
    if (path() !== (variant === "new" ? "tv-new" : "tv")) return;
    if (document.activeElement && document.activeElement.id === "overlay-url") return;
    let data = { match: null, matches: [] };
    try { data = await api("/tv/board", { method: "GET", auth: false }); }
    catch (err) { board.innerHTML = `<p>${esc(err.message)}</p>`; return; }
    board.innerHTML = tvMarkup(data.match) + `<h3 style="color:#fff;font-weight:500">Fight history</h3>` + balls(data.matches) + resultOverlay(data.matches);
    stickBallsToRight(board);
  }
  await refresh();
  startLive(() => refresh());
  startPoll(() => { refresh().catch(() => {}); }, 1500);
}

async function pageCash(view) {
  view.innerHTML = `
    <div class="row">
      <label class="field grow">Event<select id="event"></select></label>
    </div>
    <div id="cash"></div>`;
  const select = view.querySelector("#event");
  let events = [];
  try { events = await eventOptions(select, true); } catch (err) { toast(err.message, "err"); return; }
  async function load() {
    const eventId = Number(select.value);
    const event = events.find((item) => item.event_id === eventId);
    if (!eventId) return;
    const [summary, board] = await Promise.all([
      api("/teller-transactions/get-data", { body: { event_id: eventId } }),
      api("/cash-transactions/board", { body: { event_id: eventId } }),
    ]);
    const active = event?.event_status === "Active";
    const cashRows = (rows, idKey, amountKey, dateKey, statusKey, kind) => table([
      { label: "", html: (row) => row[statusKey] === "Pending" ? `<button class="btn ok" data-yes="${row[idKey]}" data-kind="${kind}">Approve</button> <button class="btn danger" data-no="${row[idKey]}" data-kind="${kind}">Decline</button>` : "" },
      { label: "Status", html: (row) => badge(row[statusKey]) },
      { label: "#", html: (row) => esc(row[idKey]) },
      { label: "Name", key: "teller_name" },
      { label: "Amount", html: (row) => peso(row[amountKey]) },
      { label: "Date", html: (row) => esc(row[dateKey] || "") },
    ], rows);
    view.querySelector("#cash").innerHTML = `
      <div class="card"><h2 style="margin:0;font-weight:500">Admin cash on hand <strong>${peso(summary.cash_on_hand)}</strong></h2>
        <p class="muted">Collections ${peso(summary.total_collections)} · Fights ${esc(summary.total_fights)}</p></div>
      <div class="card"><h3>Tellers cash on hand</h3>${table([
        { label: "Name", key: "teller_name" },
        { label: "Cash on hand", html: (row) => peso(row.teller_balance) },
        { label: "This fight", html: (row) => peso(row.teller_match_balance) },
      ], board.tellers || [])}</div>
      <div class="row" style="margin:16px 0">
        <button class="btn" id="add-cash" type="button" ${active ? "" : "disabled"}>Add cash to teller</button>
        ${active ? "" : `<span class="muted">Cash can be added only while the event is Active.</span>`}
      </div>
      <div class="card"><h3>Cash-out requests</h3>${cashRows(board.cash_outs || [], "cash_out_id", "cash_out_amount", "cash_out_datetime", "cash_out_status", "out")}</div>
      <div class="card"><h3>Cash-in requests</h3>${cashRows(board.cash_ins || [], "cash_in_id", "cash_in_amount", "cash_in_datetime", "cash_in_status", "in")}</div>`;
    const add = view.querySelector("#add-cash");
    if (add) add.onclick = () => {
      const options = (board.tellers || []).map((teller) => `<option value="${teller.event_teller_id}">${esc(teller.teller_name)}</option>`).join("");
      openModal("Add cash to teller", `<label class="field">Teller<select name="event_teller_id">${options}</select></label>${field("Amount", "cash_in_amount", "", "number", 'step="0.01" required')}`, async (form) => {
        const data = formData(form);
        const result = await api("/cash-transactions/cash-in", { body: { event_teller_id: Number(data.event_teller_id), cash_in_amount: Number(data.cash_in_amount) } });
        if (!scalarOk(result)) throw new Error("Could not add cash.");
        toast("Cash added.");
        await load();
      });
    };
    view.querySelector("#cash").onclick = async (event) => {
      const yes = event.target.dataset.yes;
      const no = event.target.dataset.no;
      const id = yes || no;
      if (!id) return;
      const kind = event.target.dataset.kind;
      const approve = Boolean(yes);
      try {
        if (kind === "in") await api(approve ? "/cash-transactions/cash-in/approve" : "/cash-transactions/cash-in/decline", { body: { cash_in_id: Number(id) } });
        else await api(approve ? "/cash-transactions/cash-out/approve" : "/cash-transactions/cash-out/decline", { body: { cash_out_id: Number(id) } });
        toast(approve ? "Approved." : "Declined.");
        await load();
      } catch (err) { toast(err.message, "err"); }
    };
  }
  select.onchange = () => load().catch((err) => toast(err.message, "err"));
  await load();
}

async function pageTellerTx(view) {
  view.innerHTML = `
    <div class="row">
      <label class="field grow">Event<select id="event"></select></label>
      <label class="field grow">Teller<select id="teller"><option value="">Select an event</option></select></label>
    </div>
    <div id="summary"></div>
    <div id="history"></div>`;
  const eventSelect = view.querySelector("#event");
  const tellerSelect = view.querySelector("#teller");
  try { await eventOptions(eventSelect, true); } catch (err) { toast(err.message, "err"); return; }
  async function loadTellers() {
    const board = await api("/cash-transactions/board", { body: { event_id: Number(eventSelect.value) } });
    const summary = await api("/teller-transactions/get-data", { body: { event_id: Number(eventSelect.value) } });
    tellerSelect.innerHTML = (board.tellers || []).map((teller) => `<option value="${teller.event_teller_id}">${esc(teller.teller_name)}</option>`).join("") || `<option value="">No tellers</option>`;
    view.querySelector("#summary").innerHTML = `<div class="card"><p>Collections <strong>${peso(summary.total_collections)}</strong> · Fights <strong>${esc(summary.total_fights)}</strong> · Admin cash <strong>${peso(summary.cash_on_hand)}</strong></p></div>`;
    await loadHistory();
  }
  async function loadHistory() {
    const id = Number(tellerSelect.value);
    if (!id) { view.querySelector("#history").innerHTML = ""; return; }
    const fights = await api("/teller-transactions/history", { body: { event_teller_id: id } });
    const list = Array.isArray(fights) ? fights : [];
    view.querySelector("#history").innerHTML = list.map((fight) => `
      <div class="card">
        <h3>Fight #${esc(fight.match_number)} ${badge(fight.match_status)} ${fight.match_winner ? esc(fight.match_winner) : ""}</h3>
        <p class="muted">Meron ${peso(fight.total_meron)} · Wala ${peso(fight.total_wala)} · Total ${peso(fight.total_bet)} · Odds ${odds(fight.meron_odds)} / ${odds(fight.wala_odds)}</p>
        ${table([
          { label: "Receipt", key: "bet_receipt_code" },
          { label: "Side", html: (row) => row.bet_side === "Meron" ? '<span class="meron-text">Meron</span>' : '<span class="wala-text">Wala</span>' },
          { label: "Amount", html: (row) => peso(row.bet_amount) },
          { label: "Status", html: (row) => badge(Number(row.bet_status) === 2 ? "Void" : "Live") },
          { label: "Payout", html: (row) => peso(row.bet_payout_amount) },
          { label: "Paid by", key: "teller_name" },
        ], fight.bets || [])}
      </div>`).join("") || `<div class="card muted">No bets for this teller.</div>`;
  }
  eventSelect.onchange = () => loadTellers().catch((err) => toast(err.message, "err"));
  tellerSelect.onchange = () => loadHistory().catch((err) => toast(err.message, "err"));
  await loadTellers();
}

async function pageBets(view, unclaimed) {
  view.innerHTML = `<div class="row"><label class="field grow">Event<select id="event"></select></label><label class="field grow">Search<input id="q" placeholder="Receipt, teller, side"></label></div><div id="stats"></div><div id="list"></div>`;
  const select = view.querySelector("#event");
  try { await eventOptions(select, true); } catch (err) { toast(err.message, "err"); return; }
  let rows = [];
  function draw() {
    const q = view.querySelector("#q").value.trim().toLowerCase();
    const shown = rows.filter((row) => !q || JSON.stringify(row).toLowerCase().includes(q));
    view.querySelector("#list").innerHTML = `<div class="card">${table([
      { label: "Receipt", key: "bet_receipt_code" },
      { label: "Teller", key: "teller_name" },
      { label: "Side", html: (row) => row.bet_side === "Meron" ? '<span class="meron-text">Meron</span>' : `<span class="wala-text">${esc(row.bet_side || "")}</span>` },
      { label: "Amount", html: (row) => peso(row.bet_amount) },
      { label: "Fight", html: (row) => esc(row.match_number || row.match_id || "") },
      { label: "Fight status", html: (row) => badge(row.match_status) },
      { label: "Bet", html: (row) => badge(Number(row.bet_status) === 2 ? "Void" : "Live") },
      { label: "Payout", html: (row) => peso(row.bet_payout_amount) },
      { label: "Paid by", key: "payout_name" },
    ], shown)}</div>`;
  }
  async function load() {
    const id = Number(select.value);
    const data = unclaimed
      ? await api(`/unclaimed/list/${id}`)
      : await api(`/bets/list/${id}`);
    rows = data.bets || [];
    if (unclaimed) {
      view.querySelector("#stats").innerHTML = `<div class="card">Unclaimed winnings <strong>${peso(data.unclaimed)}</strong></div>`;
    } else {
      view.querySelector("#stats").innerHTML = `<div class="grid grid-3">
        <div class="card">Total bets<br><strong>${peso(data.total_bets)}</strong></div>
        <div class="card">Valid completed<br><strong>${peso(data.valid_bets)}</strong></div>
        <div class="card">Claimed<br><strong>${peso(data.claimed)}</strong></div>
        <div class="card">Unclaimed<br><strong>${peso(data.unclaimed)}</strong></div>
        <div class="card">Voided<br><strong>${peso(data.voided_bets)}</strong></div>
        <div class="card">Returned<br><strong>${peso(data.returned_bets)}</strong></div>
      </div>`;
    }
    draw();
  }
  select.onchange = () => load().catch((err) => toast(err.message, "err"));
  view.querySelector("#q").oninput = draw;
  await load();
}

function claimResult(row) {
  if (row.match_status === "Draw") return "Draw refund";
  if (row.match_status === "Cancelled") return "Cancelled refund";
  if (Number(row.bet_is_winner) === 1) {
    const value = row.bet_side === "Wala" ? row.wala_odds : row.meron_odds;
    return `Winner ${odds(value)}`;
  }
  return row.match_status || "";
}

function betTime(value) {
  if (!value) return "";
  const date = new Date(String(value).replace(" ", "T"));
  if (Number.isNaN(date.getTime())) return String(value);
  return date.toLocaleString("en-US", { month: "short", day: "numeric", year: "numeric", hour: "numeric", minute: "2-digit" });
}

async function pageUnclaimed(view) {
  view.innerHTML = `
    <div class="row">
      <label class="field grow">Event<select id="event"></select></label>
      <label class="field">Fight<select id="fight"><option value="">All fights</option></select></label>
      <label class="field grow">Search<input id="q" placeholder="Receipt, teller, side"></label>
    </div>
    <div id="stats"></div>
    <div id="list"></div>`;
  const select = view.querySelector("#event");
  const fightSelect = view.querySelector("#fight");
  try { await eventOptions(select, true); } catch (err) { toast(err.message, "err"); return; }
  let rows = [];
  function draw() {
    const q = view.querySelector("#q").value.trim().toLowerCase();
    const fight = fightSelect.value;
    const shown = rows.filter((row) => {
      if (fight && String(row.match_number) !== fight) return false;
      return !q || JSON.stringify(row).toLowerCase().includes(q);
    });
    view.querySelector("#list").innerHTML = `<div class="card">${table([
      { label: "Receipt", key: "bet_receipt_code" },
      { label: "Fight", html: (row) => esc(row.match_number ?? "") },
      { label: "Side", html: (row) => row.bet_side === "Meron" ? '<span class="meron-text">Meron</span>' : `<span class="wala-text">${esc(row.bet_side || "")}</span>` },
      { label: "Stake", html: (row) => peso(row.bet_amount) },
      { label: "Result", html: (row) => esc(claimResult(row)) },
      { label: "Bet time", html: (row) => esc(betTime(row.bet_datetime)) },
      { label: "Teller", key: "teller_name" },
      { label: "Payout", html: (row) => peso(row.bet_payout_amount) },
    ], shown)}</div>`;
  }
  async function load() {
    const id = Number(select.value);
    const data = await api(`/unclaimed/list/${id}`);
    rows = (data.bets || []).slice().sort((a, b) => Number(a.match_number) - Number(b.match_number) || String(a.bet_datetime).localeCompare(String(b.bet_datetime)));
    const fights = [...new Set(rows.map((row) => row.match_number))].sort((a, b) => Number(a) - Number(b));
    const chosen = fightSelect.value;
    fightSelect.innerHTML = `<option value="">All fights</option>` + fights.map((fight) => `<option value="${esc(fight)}">Fight ${esc(fight)}</option>`).join("");
    if (fights.some((fight) => String(fight) === chosen)) fightSelect.value = chosen;
    view.querySelector("#stats").innerHTML = `<div class="card">Unclaimed amount <strong>${peso(data.unclaimed)}</strong> · ${rows.length} receipt${rows.length === 1 ? "" : "s"}</div>`;
    draw();
  }
  select.onchange = () => load().catch((err) => toast(err.message, "err"));
  fightSelect.onchange = draw;
  view.querySelector("#q").oninput = draw;
  await load();
}

const DENOMS = [1000, 500, 200, 100, 50, 20, 10, 5, 1];

function denomTotal(row, prefix = "denom_") {
  return DENOMS.reduce((sum, value) => sum + Number(row[prefix + value] || 0) * value, 0);
}

async function pageRemittances(view) {
  view.innerHTML = `
    <div class="row">
      <label class="field grow">Event<select id="event"></select></label>
      <button class="btn" id="generate" type="button">Generate empty remittances</button>
    </div>
    <div id="lists"></div>`;
  const select = view.querySelector("#event");
  try { await eventOptions(select, true); } catch (err) { toast(err.message, "err"); return; }
  async function load() {
    const eventId = Number(select.value);
    const [tellers, admins] = await Promise.all([
      api("/remittances/tellers/list", { body: { event_id: eventId } }),
      api("/remittances/admins/list", { body: { event_id: eventId } }),
    ]);
    const tellerRows = Array.isArray(tellers) ? tellers : [];
    const adminRows = Array.isArray(admins) ? admins : [];
    view.querySelector("#lists").innerHTML = `
      <div class="card"><h3>Teller remittances</h3>${table([
        { label: "Teller", html: (row) => esc(row.teller_name || row.event_teller_id) },
        { label: "Counted", html: (row) => peso(denomTotal(row)) },
        { label: "", html: (row) => `<button class="btn ghost" data-edit-teller="${row.teller_remittance_id}">Denominations</button> <button class="btn ghost" data-short-teller="${row.teller_remittance_id}">Shorts</button> <button class="btn danger" data-del="${row.teller_remittance_id}">Delete</button>` },
      ], tellerRows)}</div>
      <div class="card"><h3>Admin remittance</h3>${table([
        { label: "Counted", html: (row) => peso(denomTotal(row)) },
        { label: "Expected cash", html: (row) => peso(row.cash_on_hand) },
        { label: "", html: (row) => `<button class="btn ghost" data-edit-admin="${row.admin_remittance_id}">Denominations</button> <button class="btn ghost" data-short-admin="${row.admin_remittance_id}">Shorts</button>` },
      ], adminRows)}</div>
      <div id="shorts"></div>`;
    view.querySelector("#lists").onclick = async (event) => {
      const tellerId = event.target.dataset.editTeller;
      const adminId = event.target.dataset.editAdmin;
      const shortTeller = event.target.dataset.shortTeller;
      const shortAdmin = event.target.dataset.shortAdmin;
      const del = event.target.dataset.del;
      try {
        if (tellerId) editDenoms(tellerRows.find((row) => String(row.teller_remittance_id) === tellerId), false, load);
        if (adminId) editDenoms(adminRows.find((row) => String(row.admin_remittance_id) === adminId), true, load);
        if (shortTeller) showShorts("teller", Number(shortTeller));
        if (shortAdmin) showShorts("admin", Number(shortAdmin));
        if (del && confirm("Delete this teller remittance?")) {
          const result = await api(`/remittances/${del}`, { method: "DELETE" });
          if (!scalarOk(result)) throw new Error("Could not delete.");
          toast("Deleted.");
          await load();
        }
      } catch (err) { toast(err.message, "err"); }
    };
  }
  async function showShorts(kind, id) {
    const rows = await api(`/remittances/${kind}-shorts/list/${id}`, { method: "GET" });
    const list = Array.isArray(rows) ? rows : [];
    view.querySelector("#shorts").innerHTML = `
      <div class="card">
        <div class="row"><h3 class="grow">${kind === "teller" ? "Teller" : "Admin"} shorts</h3><button class="btn" id="add-short" type="button">Add short</button></div>
        ${table([
          { label: "Amount", html: (row) => peso(row.short_amount) },
          { label: "Remarks", key: "short_remarks" },
          { label: "", html: (row) => `<button class="btn danger" data-del-short="${row[kind === "teller" ? "teller_short_id" : "admin_short_id"]}">Delete</button>` },
        ], list)}
      </div>`;
    view.querySelector("#add-short").onclick = () => openModal("Add short", `${field("Amount", "short_amount", "", "number", 'step="0.01" required')}${field("Remarks", "short_remarks", "", "text", "required")}`, async (form) => {
      const data = formData(form);
      const body = { short_amount: Number(data.short_amount), short_remarks: data.short_remarks };
      if (kind === "teller") body.teller_remittance_id = id;
      else body.admin_remittance_id = id;
      const result = await api(`/remittances/${kind}-shorts/store`, { body });
      if (!scalarOk(result)) throw new Error("Could not save the short.");
      toast("Short saved.");
      await showShorts(kind, id);
    });
    view.querySelector("#shorts").onclick = async (event) => {
      const idToDelete = event.target.dataset.delShort;
      if (!idToDelete || !confirm("Delete this short?")) return;
      try {
        const result = await api(`/remittances/${kind}-shorts/delete/${idToDelete}`, { method: "DELETE" });
        if (!scalarOk(result)) throw new Error("Could not delete.");
        await showShorts(kind, id);
      } catch (err) { toast(err.message, "err"); }
    };
  }
  view.querySelector("#generate").onclick = async () => {
    try {
      const result = await api(`/remittances/generate/${select.value}`);
      if (!scalarOk(result)) throw new Error("Could not generate remittances.");
      toast("Remittance sheets are ready.");
      await load();
    } catch (err) { toast(err.message, "err"); }
  };
  select.onchange = () => load().catch((err) => toast(err.message, "err"));
  await load();
}

function editDenoms(row, admin, reload) {
  const fields = DENOMS.map((value) => field(String(value), admin ? "admin_denom_" + value : "denom_" + value, row["denom_" + value] ?? 0, "number", 'min="0" step="1"')).join("");
  openModal("Denomination count", fields, async (form) => {
    const data = formData(form);
    let result;
    if (admin) {
      const body = { admin_uid: row.admin_remittance_id };
      DENOMS.forEach((value) => { body["admin_denom_" + value] = Number(data["admin_denom_" + value] || 0); });
      result = await api("/remittances/admin/update", { body });
    } else {
      const body = { uid: row.teller_remittance_id };
      DENOMS.forEach((value) => { body["_denom_" + value] = Number(data["denom_" + value] || 0); });
      result = await api("/remittances/teller/update", { body });
    }
    if (!scalarOk(result)) throw new Error("Could not save the count.");
    toast("Count saved.");
    await reload();
  });
}

async function pageSalaries(view) {
  view.innerHTML = `<div class="row"><label class="field grow">Event<select id="event"></select></label><button class="btn" id="add" type="button">Add salary</button></div><div id="list"></div>`;
  const select = view.querySelector("#event");
  try { await eventOptions(select, true); } catch (err) { toast(err.message, "err"); return; }
  function pay(row) {
    return Number(row.daily_rate || 0) + Number(row.overtime_rate || 0) * Number(row.total_overtime || 0) + Number(row.bonus || 0);
  }
  async function load() {
    const rows = await api("/salaries/list", { body: { event_id: Number(select.value) } });
    const list = Array.isArray(rows) ? rows : [];
    document.getElementById("list").innerHTML = `<div class="card">${table([
      { label: "Type", key: "employee_type" },
      { label: "Name", key: "employee_name" },
      { label: "Daily", html: (row) => peso(row.daily_rate) },
      { label: "OT rate", html: (row) => peso(row.overtime_rate) },
      { label: "OT hours", key: "total_overtime" },
      { label: "Bonus", html: (row) => peso(row.bonus) },
      { label: "Pay", html: (row) => peso(pay(row)) },
      { label: "", html: (row) => `<button class="btn ghost" data-edit="${row.salary_id}">Edit</button> <button class="btn danger" data-del="${row.salary_id}">Delete</button>` },
    ], list)}<p>Total pay <strong>${peso(list.reduce((sum, row) => sum + pay(row), 0))}</strong></p></div>`;
    document.getElementById("list").onclick = async (event) => {
      const row = list.find((item) => String(item.salary_id) === (event.target.dataset.edit || event.target.dataset.del));
      try {
        if (event.target.dataset.edit) editSalary(select.value, row, load);
        if (event.target.dataset.del && confirm("Delete this salary?")) {
          const result = await api(`/salaries/${row.salary_id}`, { method: "DELETE" });
          if (!scalarOk(result)) throw new Error("Could not delete.");
          toast("Deleted.");
          await load();
        }
      } catch (err) { toast(err.message, "err"); }
    };
  }
  document.getElementById("add").onclick = () => editSalary(select.value, null, load);
  select.onchange = () => load().catch((err) => toast(err.message, "err"));
  await load();
}

function editSalary(eventId, row, reload) {
  const body = `
    ${selectField("Type", "employee_type", [["Teller", "Teller"], ["Staff", "Staff"], ["Other", "Other"]], row?.employee_type || "Staff")}
    ${field("Name", "employee_name", row?.employee_name || "", "text", "required")}
    ${field("Daily rate", "daily_rate", row?.daily_rate ?? 0, "number", 'step="0.01"')}
    ${field("Overtime rate", "overtime_rate", row?.overtime_rate ?? 0, "number", 'step="0.01"')}
    ${field("Overtime hours", "total_overtime", row?.total_overtime ?? 0, "number", 'step="0.01"')}
    ${field("Bonus", "bonus", row?.bonus ?? 0, "number", 'step="0.01"')}
    ${row ? "" : `<label class="field" style="flex-direction:row;align-items:center;color:var(--text)"><input type="checkbox" name="include_all_tellers" style="width:auto"> One row for every teller on this event</label>`}`;
  openModal(row ? "Edit salary" : "Add salary", body, async (form) => {
    const data = formData(form);
    const payload = {
      employee_type: data.employee_type,
      employee_name: data.employee_name,
      daily_rate: Number(data.daily_rate || 0),
      overtime_rate: Number(data.overtime_rate || 0),
      total_overtime: Number(data.total_overtime || 0),
      bonus: Number(data.bonus || 0),
    };
    let result;
    if (!row) {
      if (form.querySelector("[name=include_all_tellers]")?.checked) payload.include_all_tellers = true;
      result = await api(`/salaries/store/${eventId}`, { body: payload });
    } else {
      result = await api("/salaries/update", { body: {
        uid: row.salary_id,
        _employee_type: payload.employee_type,
        _employee_name: payload.employee_name,
        _daily_rate: payload.daily_rate,
        _overtime_rate: payload.overtime_rate,
        _total_overtime: payload.total_overtime,
        _bonus: payload.bonus,
      } });
    }
    if (!scalarOk(result)) throw new Error("Could not save the salary.");
    toast("Saved.");
    await reload();
  });
}

const REPORT_LABELS = [
  ["total_collection", "Total collection"],
  ["organizer_share", "Organizer share"],
  ["teller_count", "Tellers"],
  ["fight_count", "Fights"],
  ["fight_count_completed", "Completed"],
  ["fight_count_draw", "Draw"],
  ["fight_count_cancelled", "Cancelled"],
  ["bet_count", "Bets"],
  ["bet_count_meron", "Meron bets"],
  ["bet_count_wala", "Wala bets"],
  ["voided_bets", "Voided"],
  ["claimed_bets", "Claimed"],
  ["unclaimed_winnings", "Unclaimed winnings"],
  ["salaries", "Salaries"],
  ["net_for_sharing", "Net for sharing"],
  ["system_share", "System share (14%)"],
  ["operator_share", "Operator share (86%)"],
  ["revolving_fund", "Revolving fund"],
  ["prizes", "Prizes"],
  ["supposed_cash_on_hand", "Supposed cash on hand"],
  ["actual_cash_on_hand", "Actual cash on hand"],
  ["difference", "Difference"],
  ["rd", "RD"],
  ["comission", "Commission %"],
  ["short", "Short"],
];

async function pageReports(view) {
  view.innerHTML = `<div class="row"><label class="field grow">Event<select id="event"></select></label></div><div id="report"></div>`;
  const select = view.querySelector("#event");
  try { await eventOptions(select, true); } catch (err) { toast(err.message, "err"); return; }
  async function load() {
    const data = await api(`/reports/report/${select.value}`);
    const show = (value) => {
      if (typeof value === "number" && Number.isInteger(value)) return esc(value);
      if (typeof value === "number") return peso(value);
      return esc(value);
    };
    view.querySelector("#report").innerHTML = `<div class="grid grid-3">${REPORT_LABELS.map(([key, label]) =>
      `<div class="card"><div class="muted">${esc(label)}</div><strong>${show(data[key])}</strong></div>`
    ).join("")}</div>`;
  }
  select.onchange = () => load().catch((err) => toast(err.message, "err"));
  await load();
}

async function pageLogs(view) {
  view.innerHTML = `<div class="row"><label class="field grow">Search<input id="q" placeholder="Account, type, message"></label></div><div id="list"></div>`;
  let rows = [];
  function draw() {
    const q = view.querySelector("#q").value.trim().toLowerCase();
    const shown = rows.filter((row) => !q || JSON.stringify(row).toLowerCase().includes(q));
    view.querySelector("#list").innerHTML = `<div class="card">${table([
      { label: "When", key: "transaction_datetime" },
      { label: "Account", key: "account_name" },
      { label: "Type", key: "transaction_type" },
      { label: "Message", key: "transaction_message" },
    ], shown)}</div>`;
  }
  try {
    const data = await api("/logs/list");
    rows = data.logs || [];
    draw();
    view.querySelector("#q").oninput = draw;
  } catch (err) { toast(err.message, "err"); }
}

function pageReset(view) {
  view.innerHTML = `
    <div class="card">
      <h3>Reset</h3>
      <p>This clears bets, cash, claims, fights, and logs. It also unlocks every teller phone and removes tellers from events. Teller and admin logins stay. A backup is saved before anything is deleted.</p>
      <label class="field">Type RESET to confirm<input id="confirm" autocomplete="off"></label>
      <button class="btn danger" id="go" type="button">Reset</button>
    </div>`;
  view.querySelector("#go").onclick = async () => {
    const confirmation = view.querySelector("#confirm").value.trim();
    if (confirmation !== "RESET") {
      toast("Type RESET to confirm.", "err");
      return;
    }
    if (!confirm("Reset fights, bets, and teller phone locks?")) return;
    const button = view.querySelector("#go");
    button.disabled = true;
    try {
      const result = await api("/operator/reset", { body: { confirmation } });
      toast(result.message || "Reset complete.");
      writeSession(null);
      location.hash = "#/login";
    } catch (err) {
      toast(err.message, "err");
      button.disabled = false;
    }
  };
}

function pageNotice(view, title, body) {
  view.innerHTML = `<div class="card"><h2>${esc(title)}</h2><p>${body}</p></div>`;
}

const PAGES = {
  dashboard: ["Home", pageHome],
  "accounts/admins": ["Admins", accountPages("admin")],
  "accounts/tellers": ["Tellers", accountPages("teller")],
  events: ["Events", pageEvents],
  matches: ["Matches", pageMatches],
  "cash-transactions": ["Cash Transactions", pageCash],
  "teller-transactions": ["Teller Transactions", pageTellerTx],
  bets: ["Bets", (view) => pageBets(view, false)],
  unclaim: ["Unclaimed", pageUnclaimed],
  unclaimed: ["Unclaimed", pageUnclaimed],
  remittances: ["Remittances", pageRemittances],
  salaries: ["Salaries", pageSalaries],
  reports: ["Reports", pageReports],
  logs: ["Logs", pageLogs],
  reset: ["Reset", pageReset],
  restore: ["Restore", (view) => pageNotice(view, "Restore", "Database restore stays on the Laravel operator page. It reloads a dump taken by Reset.")],
};

function render() {
  const here = path() || "dashboard";
  if (here === "unclaimed") {
    location.replace("#/unclaim");
    return;
  }
  if (here === "login" || here === "logout") {
    if (here === "logout") writeSession(null);
    loginPage();
    return;
  }
  if (here === "tv" || here === "tv-new") {
    pageTv(null, here === "tv-new" ? "new" : "classic").catch((err) => toast(err.message, "err"));
    return;
  }
  if (!session?.token) {
    loginPage();
    return;
  }
  const page = PAGES[here] || PAGES.dashboard;
  mount(page[0], page[1]).catch((err) => toast(err.message, "err"));
}

window.addEventListener("hashchange", render);
if (!location.hash) location.hash = session?.token ? "#/dashboard" : "#/login";
else render();
