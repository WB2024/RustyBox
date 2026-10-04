// Title updates: the ones in a library, finding more on XboxUnity, and installing them on the console.
import { api, el, esc, fmtBytes, toast } from "../util.js";

export const title = "Updates";

const COMPAT = {
  compatible: `<span class="badge ok" title="This update is for one of the game's known discs">fits</span>`,
  incompatible: `<span class="badge err" title="Known discs exist and none matches: it may be for another release">other release?</span>`,
  unknown: `<span class="badge" title="Not enough information to say">unknown</span>`,
};

export async function render(root, ctx) {
  const [libs, consoles] = await Promise.all([api("/api/libraries"), api("/api/consoles")]);
  const tuLibs = libs.filter((l) => l.kind === "patches");
  const $ = (s) => root.querySelector(s);
  let lib = tuLibs.find((l) => String(l.id) === sessionStorage.getItem("tu-lib")) || tuLibs[0];

  root.innerHTML = `
    <div class="page-head"><h1>🩹 Title updates</h1><div class="muted">Keep title updates in a library, find more on XboxUnity, and install them on the console where Aurora and the games look for them.</div></div>
    ${tuLibs.length ? `<div class="card"><div class="row"><label class="field grow" style="max-width:360px">Title updates library<select id="u-lib">${tuLibs.map((l) => `<option value="${l.id}" ${l === lib ? "selected" : ""}>${esc(l.name)}</option>`).join("")}</select></label><button class="btn" id="u-refresh">Rescan</button></div></div>
      <div class="card"><h2>In your library</h2><div id="u-local" class="muted">Reading…</div></div>`
      : `<div class="notice">Create a library of kind <b>Title updates</b> first (Libraries → New library), pointing at a folder for them. Then update files you download or already have show up here.</div>`}
    <div class="card"><h2>Find updates on XboxUnity</h2>
      <div class="row"><input type="search" id="x-q" placeholder="Game name or title ID" class="grow" autocomplete="off"><button class="btn primary" id="x-go">Search</button></div><div id="x-out" style="margin-top:10px"></div></div>
    <div class="card"><div class="row"><h2 class="grow" style="margin:0">On the console</h2>${consoles.length ? `<select id="o-c">${consoles.map((c) => `<option value="${c.id}">${esc(c.name)}</option>`).join("")}</select><button class="btn" id="o-go">Check the console</button>` : ""}</div>
      <div id="o-out" class="muted" style="margin-top:8px">${consoles.length ? "Shows which games have updates installed. (Scan the console on the Console page first.)" : "Add your console first (Console in the sidebar)."}</div></div>`;

  let locals = [];
  async function loadLocal() {
    if (!lib) return;
    const out = $("#u-local");
    out.innerHTML = `<span class="muted">Reading…</span>`;
    try { locals = (await api(`/api/updates/local?library_id=${lib.id}`)).updates; } catch (e) { out.innerHTML = `<div class="notice err">${esc(e.message)}</div>`; return; }
    out.innerHTML = locals.length ? `<div class="row" style="margin-bottom:6px"><span class="muted grow">${locals.length} update${locals.length === 1 ? "" : "s"}</span><button class="btn small primary" id="l-install" ${consoles.length ? "" : "disabled"}>Install selected on the console…</button></div>
      <table><thead><tr><th style="width:28px"><input type="checkbox" id="l-all"></th><th>Game</th><th>TU</th><th>Media ID</th><th>File</th><th class="num">Size</th></tr></thead><tbody>${locals.map((u, i) => `<tr><td><input type="checkbox" class="ls" data-i="${i}"></td><td>${esc(u.game || u.tu.name)} ${COMPAT[u.compat]}<div class="mono muted">${esc(u.tu.title_id)}</div></td><td>${u.tu.tu_version ? "TU" + u.tu.tu_version : "?"}</td><td class="mono">${esc(u.tu.media_id)}</td><td class="mono muted" style="word-break:break-all">${esc(u.tu.relpath)}</td><td class="num">${fmtBytes(u.tu.size)}</td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">No title updates found in ${esc(lib.name)}. Download some below, or copy update files into its folder and rescan.</div>`;
    $("#l-all")?.addEventListener("change", (e) => out.querySelectorAll(".ls").forEach((c) => (c.checked = e.target.checked)));
    $("#l-install")?.addEventListener("click", () => {
      const picked = [...out.querySelectorAll(".ls:checked")].map((c) => locals[+c.dataset.i]);
      if (!picked.length) return toast("Tick the updates first", "err");
      installDialog(picked.map((u) => ({ library_id: lib.id, path_id: u.path_id, relpath: u.tu.relpath })));
    });
  }
  $("#u-lib")?.addEventListener("change", (e) => { sessionStorage.setItem("tu-lib", e.target.value); lib = tuLibs.find((l) => String(l.id) === e.target.value); loadLocal(); });
  $("#u-refresh")?.addEventListener("click", loadLocal);

  async function installDialog(files) {
    const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:90vh"><header>Install on the console</header><div class="body" style="padding-top:14px">
      <label class="field">Console<select id="ui-c">${consoles.map((c) => `<option value="${c.id}">${esc(c.name)}</option>`).join("")}</select></label><div id="ui-plan" style="margin-top:12px" class="muted">Checking…</div></div>
      <footer><button class="btn" id="ui-no">Close</button><button class="btn primary" id="ui-go" disabled>Install</button></footer></div></div>`);
    document.body.append(modal);
    const m = (s) => modal.querySelector(s);
    const body = () => ({ console_id: +m("#ui-c").value, files });
    async function plan() {
      m("#ui-go").disabled = true;
      try {
        const p = await api("/api/updates/install/plan", { body: body() });
        m("#ui-plan").innerHTML = `<table><tbody>${p.entries.map((e) => `<tr><td>${esc(e.game || e.name)} ${e.compat ? COMPAT[e.compat] : ""}<div class="mono muted">${esc(e.title_id)} ${e.tu_version ? "· TU" + e.tu_version : ""}</div>${e.problems.map((x) => `<div style="color:var(--err)">⛔ ${esc(x)}</div>`).join("")}${e.warnings.map((x) => `<div style="color:var(--warn)">⚠ ${esc(x)}</div>`).join("")}</td><td class="mono" style="word-break:break-all">${esc(e.target)}</td></tr>`).join("")}</tbody></table>`;
        m("#ui-go").disabled = !p.ok;
      } catch (e) { m("#ui-plan").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; }
    }
    m("#ui-c").onchange = plan; m("#ui-no").onclick = () => modal.remove();
    m("#ui-go").onclick = async () => { try { const { job } = await api("/api/updates/install/start", { body: body() }); modal.remove(); ctx.openJob(job); } catch (e) { toast(e.message, "err"); } };
    plan();
  }

  // ── XboxUnity ──
  async function search() {
    const out = $("#x-out"), q = $("#x-q").value.trim();
    if (q.length < 2) return toast("Type at least two letters", "err");
    out.innerHTML = `<span class="muted">Searching…</span>`;
    try {
      const r = /^[0-9a-f]{8}$/i.test(q) ? { titles: [{ title_id: q.toUpperCase(), name: "Title " + q.toUpperCase(), updates: "?" }] } : await api(`/api/updates/unity/search?q=${encodeURIComponent(q)}`);
      out.innerHTML = r.titles.length ? `<table><tbody>${r.titles.map((t) => `<tr><td>${esc(t.name)} <span class="mono muted">${esc(t.title_id)}</span></td><td class="num">${t.updates} update${t.updates === 1 ? "" : "s"}</td><td class="num"><button class="btn small" data-t="${esc(t.title_id)}">Show</button></td></tr>`).join("")}</tbody></table>` : `<div class="muted">No games with updates match that.</div>`;
      out.querySelectorAll("[data-t]").forEach((b) => (b.onclick = () => showTitle(b.dataset.t)));
    } catch (e) { out.innerHTML = `<div class="notice err">${esc(e.message)}</div>`; }
  }
  async function showTitle(tid) {
    const out = $("#x-out");
    out.innerHTML = `<span class="muted">Loading ${esc(tid)}…</span>`;
    let t;
    try { t = await api(`/api/updates/unity/title?titleid=${tid}`); } catch (e) { out.innerHTML = `<div class="notice err">${esc(e.message)}</div>`; return; }
    out.innerHTML = `<div class="row"><b class="grow">${esc(t.name || t.title_id)} <span class="mono muted">${esc(t.title_id)}</span></b><button class="btn small" id="x-back">Back</button><button class="btn small primary" id="x-dl" ${lib ? "" : "disabled"}>Download selected to ${lib ? esc(lib.name) : "a library"}…</button></div>
      ${t.i_have ? "" : `<div class="muted" style="margin:6px 0">You don't have this game in a library, so fit can only be checked against the known discs.</div>`}
      <table style="margin-top:8px"><thead><tr><th style="width:28px"></th><th>TU</th><th>Media ID</th><th>Date</th><th class="num">Size</th><th>Fits</th></tr></thead><tbody>${t.updates.map((u, i) => `<tr><td><input type="checkbox" class="xs" data-i="${i}" ${u.compat === "compatible" && u.mine ? "checked" : ""}></td><td>TU${esc(u.update.version)}</td><td class="mono">${esc(u.update.media_id)}${u.mine ? ` <span class="badge accent">my disc</span>` : ""}</td><td>${esc(u.update.date)}</td><td class="num">${u.update.size_kb} KB</td><td>${COMPAT[u.compat]}</td></tr>`).join("")}</tbody></table>`;
    $("#x-back").onclick = search;
    $("#x-dl").onclick = async () => {
      const ids = [...out.querySelectorAll(".xs:checked")].map((c) => t.updates[+c.dataset.i].update.tuid);
      if (!ids.length) return toast("Tick the updates first", "err");
      const p = lib.paths.find((x) => x.writable && !x.remote_url);
      if (!p) return toast(`${lib.name} has no writable folder here`, "err");
      try { const { job } = await api("/api/updates/unity/download", { body: { title_id: t.title_id, tuids: ids, library_id: lib.id, path_id: p.id } }); ctx.openJob(job); } catch (e) { toast(e.message, "err"); }
    };
  }
  $("#x-go").onclick = search;
  $("#x-q").onkeydown = (e) => { if (e.key === "Enter") search(); };

  // ── On the console ──
  $("#o-go")?.addEventListener("click", async () => {
    const out = $("#o-out"); out.innerHTML = `<span class="muted">Asking the console…</span>`;
    try {
      const r = await api(`/api/updates/console?console_id=${$("#o-c").value}`);
      const have = new Set(locals.map((u) => u.tu.title_id));
      out.innerHTML = `<table><thead><tr><th>Game</th><th>Installed updates</th><th>In your library</th></tr></thead><tbody>${r.games.map((g) => `<tr><td>${esc(g.name || g.title_id)}<div class="mono muted">${esc(g.title_id)}</div></td><td>${g.installed.length ? g.installed.map((n) => `<span class="mono">${esc(n)}</span>`).join("<br>") : `<span class="muted">none</span>`}</td><td>${have.has(g.title_id) ? `<span class="badge ok">available</span>` : `<span class="muted">—</span>`}</td></tr>`).join("") || `<tr><td class="muted">No games found.</td></tr>`}</tbody></table>`;
    } catch (e) { out.innerHTML = `<div class="notice err">${esc(e.message)}</div>`; }
  });

  loadLocal();
}
