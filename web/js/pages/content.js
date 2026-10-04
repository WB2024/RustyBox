// Mods, homebrew, trainers, cheats, saves and patches from the Arisen Studio database, with
// install to the console.
import { api, el, esc, fmtTime, toast } from "../util.js";

export const title = "Content";

const KINDS = [
  ["trainers", "Trainers", "Run alongside a game from Aurora."],
  ["mods", "Mods", "Game mods and menus."],
  ["homebrew", "Homebrew", "Apps, emulators and utilities."],
  ["saves", "Saves", "Save games for the Xbox 360."],
  ["cheats", "Cheats", "Cheat lists (data: nothing to install)."],
  ["patches", "Patches", "Patch files for the Xenia emulator."],
];

export async function render(root, ctx) {
  const [status, consoles, libs] = await Promise.all([api("/api/content/status"), api("/api/consoles"), api("/api/libraries")]);
  let kind = sessionStorage.getItem("content-kind") || "trainers";
  let q = "", owned = sessionStorage.getItem("content-owned") === "1", offset = 0;
  const $ = (s) => root.querySelector(s);
  const total = Object.values(status.counts).reduce((a, b) => a + b, 0);

  root.innerHTML = `
    <div class="page-head row"><div class="grow"><h1>🧩 Content</h1><div class="muted">${total ? `${total.toLocaleString()} items from the Arisen Studio database · updated ${status.fetched ? fmtTime(status.fetched) : "never"}` : "The database hasn't been downloaded yet."}${status.mock ? " (sample data in mock mode)" : ""}</div></div>
      <button class="btn ${total ? "" : "primary"}" id="c-refresh">${total ? "Update the database" : "Download the database"}</button></div>
    <div class="row tabs" style="margin-bottom:10px;flex-wrap:wrap">${KINDS.map(([k, l]) => `<button class="btn ${k === kind ? "primary" : ""}" data-kind="${k}">${l} <span class="muted">${status.counts[k] ?? 0}</span></button>`).join("")}</div>
    <div class="card"><div class="row"><label class="field grow">Search<input type="text" id="c-q" placeholder="Name, game or title ID" autocomplete="off"></label>
      <label class="check" style="padding-top:18px"><input type="checkbox" id="c-owned" ${owned ? "checked" : ""}><span>Only games I have</span></label></div>
      <div class="muted" id="c-blurb" style="margin-top:6px"></div></div>
    <div id="c-list"></div>`;

  $("#c-refresh").onclick = async () => { try { const { job } = await api("/api/content/refresh", { method: "POST" }); ctx.openJob(job); } catch (e) { toast(e.message, "err"); } };
  root.querySelectorAll("[data-kind]").forEach((b) => (b.onclick = () => { kind = b.dataset.kind; sessionStorage.setItem("content-kind", kind); render(root, ctx); }));
  let t; $("#c-q").oninput = (e) => { clearTimeout(t); t = setTimeout(() => { q = e.target.value; offset = 0; load(); }, 250); };
  $("#c-owned").onchange = (e) => { owned = e.target.checked; sessionStorage.setItem("content-owned", owned ? "1" : "0"); offset = 0; load(); };
  $("#c-blurb").textContent = KINDS.find((k) => k[0] === kind)[2];

  async function load(more = false) {
    const out = $("#c-list");
    if (!total) { out.innerHTML = `<div class="notice">Press <b>Download the database</b> first.</div>`; return; }
    if (!more) out.innerHTML = `<span class="muted">Loading…</span>`;
    const r = await api(`/api/content?kind=${kind}&q=${encodeURIComponent(q)}&owned=${owned}&limit=60&offset=${offset}`).catch((e) => ({ error: e.message }));
    if (r.error) { out.innerHTML = `<div class="notice err">${esc(r.error)}</div>`; return; }
    const rows = r.items.map((i) => `<tr data-id="${esc(i.id)}"><td><b>${esc(i.name)}</b>${i.version ? ` <span class="badge">v${esc(i.version)}</span>` : ""}${i.mode ? ` <span class="badge">${esc(i.mode)}</span>` : ""}
        <div class="muted" style="font-size:12.5px">${esc(i.game || i.category_title)}${i.title_id ? ` · <span class="mono">${esc(i.title_id)}</span>` : ""}${i.author ? ` · by ${esc(i.author)}` : ""}</div>
        ${i.description ? `<div class="muted" style="font-size:12.5px;white-space:pre-line;max-height:3.4em;overflow:hidden">${esc(i.description)}</div>` : ""}
        ${i.cheats.length ? `<div class="muted" style="font-size:12.5px">${i.cheats.length} cheats: ${esc(i.cheats.slice(0, 6).join(", "))}${i.cheats.length > 6 ? "…" : ""}</div>` : ""}</td>
      <td class="num" style="white-space:nowrap">${i.files.some((f) => f.install_paths.length) ? `<button class="btn small primary" data-act="install">Install…</button> ` : ""}${i.files.length ? `<button class="btn small" data-act="keep">Keep a copy…</button>` : ""}</td></tr>`).join("");
    const head = more ? "" : `<div class="muted" style="margin:8px 2px">${r.total} item${r.total === 1 ? "" : "s"}</div>`;
    if (!more) out.innerHTML = head + `<div class="card"><table><tbody id="c-rows">${rows || `<tr><td class="muted">Nothing matches.</td></tr>`}</tbody></table>${r.total > offset + r.items.length ? `<div style="margin-top:8px"><button class="btn" id="c-more">Show more</button></div>` : ""}</div>`;
    else { $("#c-rows").insertAdjacentHTML("beforeend", rows); $("#c-more")?.remove(); if (r.total > offset + r.items.length) $("#c-list .card").insertAdjacentHTML("beforeend", `<div style="margin-top:8px"><button class="btn" id="c-more">Show more</button></div>`); }
    $("#c-more")?.addEventListener("click", () => { offset += 60; load(true); });
    out.querySelectorAll("tr[data-id]").forEach((tr) => {
      tr.querySelectorAll("[data-act]").forEach((b) => (b.onclick = () => (b.dataset.act === "install" ? installDialog(tr.dataset.id) : keepDialog(tr.dataset.id))));
    });
  }

  async function installDialog(id) {
    if (!consoles.length) { toast("Add your console first (Console in the sidebar)", "err"); location.hash = "console"; return; }
    const modal = el(`<div class="modal-bg"><div class="modal" style="height:auto;max-height:90vh"><header>Install on the console</header><div class="body" style="padding-top:14px">
      <label class="field">Console<select id="in-c">${consoles.map((c) => `<option value="${c.id}">${esc(c.name)}</option>`).join("")}</select></label>
      <div id="in-plan" style="margin-top:12px" class="muted">Working out where the files go…</div></div>
      <footer><button class="btn" id="in-no">Close</button><button class="btn primary" id="in-go" disabled>Install</button></footer></div></div>`);
    document.body.append(modal);
    const m = (s) => modal.querySelector(s);
    const body = () => ({ console_id: +m("#in-c").value, id });
    async function plan() {
      m("#in-go").disabled = true; m("#in-plan").innerHTML = `<span class="muted">Working out where the files go…</span>`;
      try {
        const r = await api("/api/content/install/plan", { body: body() });
        const p = r.plan;
        m("#in-plan").innerHTML = `<b>${esc(p.title)}</b>${p.problems.map((x) => `<div class="notice err" style="margin-top:8px">${esc(x)}</div>`).join("")}
          <table style="margin-top:8px"><tbody>${p.files.map((f) => `<tr><td>${esc(f.name)}<div class="muted" style="font-size:12.5px">from ${esc(f.from)}</div></td><td class="mono" style="word-break:break-all">${f.targets.map(esc).join("<br>")}</td></tr>`).join("")}</tbody></table>
          ${p.warnings.map((x) => `<div style="color:var(--warn);margin-top:6px">⚠ ${esc(x)}</div>`).join("")}`;
        m("#in-go").disabled = !p.ok;
      } catch (e) { m("#in-plan").innerHTML = `<div class="notice err">${esc(e.message).replace(/\n/g, "<br>")}</div>`; }
    }
    m("#in-c").onchange = plan; m("#in-no").onclick = () => modal.remove();
    m("#in-go").onclick = async () => { try { const { job } = await api("/api/content/install/start", { body: body() }); modal.remove(); ctx.openJob(job); } catch (e) { toast(e.message, "err"); } };
    plan();
  }

  function keepDialog(id) {
    const kindLibs = libs.filter((l) => l.kind === kind || l.kind === "custom");
    const choices = kindLibs.flatMap((l) => l.paths.filter((p) => p.writable && !p.remote_url).map((p) => ({ l, p })));
    if (!choices.length) { toast(`Create a ${kind} library with a writable folder first (Libraries)`, "err"); return; }
    const modal = el(`<div class="modal-bg"><div class="modal"><header>Keep a copy in a library</header><div class="body" style="padding-top:14px">
      <label class="field">Library folder<select id="kp">${choices.map((c, i) => `<option value="${i}">${esc(c.l.name)} / ${esc(c.p.label || c.p.path)}</option>`).join("")}</select></label>
      <p class="muted">The files are downloaded as they are (zips stay zipped) into <span class="mono">TitleID / Name /</span>. You can install them from the library later.</p></div>
      <footer><button class="btn" id="kp-no">Cancel</button><button class="btn primary" id="kp-go">Download</button></footer></div></div>`);
    document.body.append(modal);
    modal.querySelector("#kp-no").onclick = () => modal.remove();
    modal.querySelector("#kp-go").onclick = async () => {
      const c = choices[+modal.querySelector("#kp").value];
      try { const { job } = await api("/api/content/download", { body: { id, library_id: c.l.id, path_id: c.p.id } }); modal.remove(); ctx.openJob(job); } catch (e) { toast(e.message, "err"); }
    };
  }

  load();
}
