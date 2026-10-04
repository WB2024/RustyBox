// The Xbox 360 itself, over Aurora's FTP: set it up, see what is on it, compare with a library,
// send games, and browse its files.
import { api, el, esc, fmtBytes, fmtTime, toast } from "../util.js";
import { pickFolder } from "../picker.js";

export const title = "Console";

const GAME_KINDS = ["iso", "god"];

export async function render(root, ctx) {
  let consoles = await api("/api/consoles");
  const libs = (await api("/api/libraries")).filter((l) => GAME_KINDS.includes(l.kind) && !l.paths.every((p) => p.remote_url));
  if (!consoles.length) return setup(root, ctx);

  let cur = consoles.find((c) => String(c.id) === sessionStorage.getItem("console")) || consoles[0];
  let tab = sessionStorage.getItem("console-tab") || "games";
  let main = libs.find((l) => String(l.id) === sessionStorage.getItem("console-lib")) || libs.find((l) => l.kind === "god") || libs[0];
  const $ = (s) => root.querySelector(s);

  function head() {
    root.innerHTML = `
      <div class="page-head row"><div class="grow"><h1>🕹️ ${esc(cur.name)}</h1><div class="muted mono">ftp://${esc(cur.host)}:${cur.port} · ${cur.game_paths.map(esc).join(", ")}</div></div>
        ${consoles.length > 1 ? `<select id="c-pick">${consoles.map((c) => `<option value="${c.id}" ${c === cur ? "selected" : ""}>${esc(c.name)}</option>`).join("")}</select>` : ""}
        <button class="btn" id="c-test" title="Checks every operation RustyBox needs, in a temporary folder it removes">Self-test</button><button class="btn" id="c-folder" title="Games on a USB stick or in a download folder">Send from a folder…</button><button class="btn" id="c-edit">Edit</button><button class="btn" id="c-add">Add another</button></div>
      <div class="row tabs" style="margin-bottom:12px"><button class="btn ${tab === "games" ? "primary" : ""}" data-tab="games">Games</button><button class="btn ${tab === "browse" ? "primary" : ""}" data-tab="browse">Browse files</button></div>
      <div id="c-body"></div>`;
    $("#c-pick")?.addEventListener("change", (e) => { sessionStorage.setItem("console", e.target.value); cur = consoles.find((c) => String(c.id) === e.target.value); head(); show(); });
    $("#c-folder").onclick = sendFromFolder;
    $("#c-test").onclick = async () => {
      if (!confirm(`Test ${cur.name}? RustyBox will make a small temporary folder called RustyBoxTest on the console, send, read, rename and delete a few files in it, and remove it again. Nothing else is touched.`)) return;
      try { const { job } = await api(`/api/consoles/${cur.id}/selftest`, { method: "POST" }); ctx.openJob(job); } catch (e) { toast(e.message, "err"); }
    };
    $("#c-edit").onclick = () => editDialog(cur, async () => { consoles = await api("/api/consoles"); cur = consoles.find((c) => c.id === cur.id) || consoles[0]; head(); show(); }, ctx);
    $("#c-add").onclick = () => editDialog(null, async (saved) => { consoles = await api("/api/consoles"); cur = consoles.find((c) => c.id === saved.id) || cur; head(); show(); }, ctx);
    root.querySelectorAll("[data-tab]").forEach((b) => (b.onclick = () => { tab = b.dataset.tab; sessionStorage.setItem("console-tab", tab); head(); show(); }));
  }

  function show() { (tab === "browse" ? browse : games)($("#c-body")); }

  // ── Games ──
  async function games(out) {
    out.innerHTML = `<span class="muted">Loading…</span>`;
    const g = await api(`/api/consoles/${cur.id}/games`);
    const scanBar = `<div class="card"><div class="row"><div class="grow"><b>${g.scanned ? `${g.games.length} game folder${g.games.length === 1 ? "" : "s"} on the console` : "Not scanned yet"}</b>
        <div class="muted">${g.scanned ? "Last scanned " + fmtTime(g.scanned) : "Scan to see what is on the console (it must be on, with Aurora's FTP switched on)."}</div></div>
        <label class="field" style="min-width:230px">Compare with<select id="g-lib">${libs.map((l) => `<option value="${l.id}" ${l === main ? "selected" : ""}>${esc(l.name)} (${l.items})</option>`).join("")}</select></label>
        <button class="btn primary" id="g-scan">${g.scanned ? "Rescan" : "Scan the console"}</button></div></div>`;
    if (!g.scanned) { out.innerHTML = scanBar; wireScan(out); return; }
    let c;
    try { c = main ? await api(`/api/consoles/${cur.id}/compare?library=${main.id}`) : null; } catch (e) { c = null; }
    const row = (r, side) => `<tr><td><input type="checkbox" class="sel" data-side="${side}" data-i="${r.i}"></td><td>${esc(r.name)}${r.disc > 1 ? ` <span class="badge">disc ${r.disc}</span>` : ""}</td><td class="mono">${esc(r.title_id)}</td><td class="mono muted">${esc(r.where)}</td><td class="num">${fmtBytes(r.size)}</td></tr>`;
    const table = (rows, side) => rows.length ? `<table style="margin-top:8px"><thead><tr><th style="width:28px"><input type="checkbox" data-all="${side}"></th><th>Game</th><th>Title ID</th><th>Where</th><th class="num">Size</th></tr></thead><tbody>${rows.map((r, i) => row({ ...r, i }, side)).join("")}</tbody></table>` : `<div class="muted" style="margin-top:8px">Nothing.</div>`;
    const lib = (c?.only_library || []).map((r) => ({ ...r, where: `${r.kind.toUpperCase()} ${r.relpath}` }));
    const both = (c?.both || []).map((r) => ({ ...r, where: `${r.kind.toUpperCase()} ${r.relpath}` }));
    const onlyC = (c?.only_console || []).map((r) => ({ ...r, where: r.path, item: r }));
    const all = g.games.map((r) => ({ ...r, where: r.path }));
    out.innerHTML = scanBar + (c ? `
      <div class="row" style="margin:6px 0 12px;flex-wrap:wrap"><span class="badge ok">${both.length} on both</span><span class="badge accent">${lib.length} missing from the console</span><span class="badge accent">${onlyC.length} only on the console</span><div class="grow"></div>
        <button class="btn small primary" data-act="sync" ${lib.length ? "" : "disabled"}>Send everything missing…</button></div>
      <div class="card"><div class="row"><h2 class="grow" style="margin:0">Missing from the console <span class="muted">(${lib.length})</span></h2><button class="btn small primary" data-act="send">Send selected…</button></div>${table(lib, "lib")}</div>
      <div class="card"><div class="row"><h2 class="grow" style="margin:0">On both <span class="muted">(${both.length})</span></h2><button class="btn small" data-act="replace">Replace the console's copy…</button></div>${table(both, "both")}</div>
      <div class="card"><div class="row"><h2 class="grow" style="margin:0">Only on the console <span class="muted">(${onlyC.length})</span></h2><button class="btn small" data-act="fetch">Copy to ${esc(main.name)}…</button><button class="btn small danger" data-act="delete">Delete from the console…</button></div>${table(onlyC, "console")}</div>`
      : `<div class="card"><h2>Everything on the console <span class="muted">(${all.length})</span></h2>${table(all, "console-all")}<p class="muted">Add a GOD or ISO library to compare it with the console.</p></div>`);
    wireScan(out);
    if (!c) return;
    const lists = { lib, both, console: onlyC };
    out.querySelectorAll("[data-all]").forEach((x) => (x.onchange = () => out.querySelectorAll(`.sel[data-side="${x.dataset.all}"]`).forEach((s) => (s.checked = x.checked))));
    const chosen = (side) => [...out.querySelectorAll(`.sel[data-side="${side}"]:checked`)].map((s) => lists[side][+s.dataset.i]);
    const refs = (rows) => rows.map((r) => ({ library_id: r.library_id, item_id: r.item_id }));
    out.querySelectorAll("[data-act]").forEach((b) => (b.onclick = async () => {
      const act = b.dataset.act;
      if (act === "sync") { return sendDialog({ items: refs(lib) }, `Send everything missing (${lib.length})`, false); }
      const side = { send: "lib", replace: "both", fetch: "console", delete: "console" }[act];
      const picked = chosen(side);
      if (!picked.length) return toast("Tick the games first", "err");
      if (act === "send") return sendDialog({ items: refs(picked) }, `Send ${picked.length} game${picked.length === 1 ? "" : "s"} to ${cur.name}`, false);
      if (act === "replace") return sendDialog({ items: refs(picked) }, `Replace ${picked.length} game${picked.length === 1 ? "" : "s"} on ${cur.name}`, true);
      if (act === "delete") return deleteDialog(picked.map((r) => r.path));
      if (act === "fetch") return fetchDialog(picked);
    }));
  }

  function wireScan(out) {
    out.querySelector("#g-lib")?.addEventListener("change", (e) => { sessionStorage.setItem("console-lib", e.target.value); main = libs.find((l) => String(l.id) === e.target.value); games(out); });
    out.querySelector("#g-scan").onclick = async () => { try { const { job } = await api(`/api/consoles/${cur.id}/scan`, { method: "POST" }); ctx.openJob(job); } catch (e) { toast(e.message, "err"); } };
  }

  // Games in a folder on the server (a mounted USB stick, a download folder), without importing first.
  async function sendFromFolder() {
    const path = await pickFolder();
    if (!path) return;
    let r;
    try { r = await api("/api/import/scan", { body: { source: { kind: "folder", path } } }); } catch (e) { return toast(e.message, "err"); }
    const games = r.candidates.filter((c) => c.content_kind === "game" && c.kind !== "file");
    if (!games.length) return toast("No ISOs or Games on Demand folders found there", "err");
    const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:90vh"><header>Send from ${esc(path)}</header><div class="body" style="padding-top:14px">
      <table><thead><tr><th style="width:28px"><input type="checkbox" id="ff-all"></th><th>Game</th><th>Type</th><th class="num">Size</th></tr></thead><tbody>${games.map((g, i) => `<tr><td><input type="checkbox" class="ff" data-i="${i}"></td><td>${esc(g.name)}${g.health ? ` <span class="badge err">damaged</span>` : ""}<div class="mono muted">${esc(g.id)}</div></td><td><span class="badge">${g.kind.toUpperCase()}</span></td><td class="num">${fmtBytes(g.size)}</td></tr>`).join("")}</tbody></table></div>
      <footer><button class="btn" id="ff-no">Cancel</button><button class="btn primary" id="ff-go">Choose where and how…</button></footer></div></div>`);
    document.body.append(modal);
    modal.querySelector("#ff-all").onchange = (e) => modal.querySelectorAll(".ff").forEach((c) => (c.checked = e.target.checked));
    modal.querySelector("#ff-no").onclick = () => modal.remove();
    modal.querySelector("#ff-go").onclick = () => {
      const sel = [...modal.querySelectorAll(".ff:checked")].map((c) => games[+c.dataset.i].id);
      if (!sel.length) return toast("Tick the games first", "err");
      modal.remove();
      sendDialog({ folder: { path, select: sel } }, `Send ${sel.length} game${sel.length === 1 ? "" : "s"} to ${cur.name}`, false);
    };
  }

  // ── Sending ──
  async function sendDialog(src, heading, replace) {
    const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:92vh"><header>${esc(heading)}</header><div class="body" id="sd" style="padding-top:14px">
      <div class="card"><div class="row" style="align-items:flex-end;flex-wrap:wrap">
        <label class="field grow">Games folder on the console<select id="sd-dest">${cur.game_paths.map((p) => `<option>${esc(p)}</option>`).join("")}</select></label>
        <label class="field">Folder layout<select id="sd-layout"><option value="name_titleid">Game Name / TitleID</option><option value="titleid">TitleID</option></select></label></div>
        <label class="check" style="margin-top:10px"><input type="checkbox" id="sd-rep" ${replace ? "checked" : ""}><span>Replace the game if it is already on the console<small>Removes the old copy (found by title ID, whatever its folder is called) and puts this one in. Saved games, add-ons and updates are kept. The plan lists exactly what is removed.</small></span></label></div>
      <div class="card"><h2>Plan</h2><div id="sd-plan" class="muted">Checking the console…</div></div></div>
      <footer><span class="muted grow" id="sd-sum"></span><button class="btn" id="sd-no">Close</button><button class="btn primary" id="sd-go" disabled>Start</button></footer></div></div>`);
    document.body.append(modal);
    const q = (s) => modal.querySelector(s);
    const settings = await api("/api/settings").catch(() => ({}));
    if (settings.god_layout) q("#sd-layout").value = settings.god_layout;
    const body = () => ({ ...src, dest: q("#sd-dest").value, layout: q("#sd-layout").value, replace: q("#sd-rep").checked });
    let seq = 0;
    async function refresh() {
      const my = ++seq; q("#sd-go").disabled = true; q("#sd-plan").innerHTML = `<span class="muted">Checking the console…</span>`;
      try {
        const p = await api(`/api/consoles/${cur.id}/send/plan`, { body: body() });
        if (my !== seq) return;
        q("#sd-plan").innerHTML = (p.problems || []).map((x) => `<div class="notice err" style="margin-bottom:8px">${esc(x)}</div>`).join("") +
          `<table><thead><tr><th>Game</th><th>What will happen</th><th class="num">Size</th></tr></thead><tbody>${p.entries.map((e) => `<tr><td>${esc(e.name)}<div class="mono muted">${esc(e.title_id || "")}</div></td>
            <td>${e.steps.map((s, i) => `<div${/^Remove /.test(s) ? ` style="color:var(--err)"` : ""}>${i + 1}. ${esc(s)}</div>`).join("")}<div class="mono muted" style="word-break:break-all">→ ${esc(e.output)}</div>
            ${e.problems.map((x) => `<div style="color:var(--err)">⛔ ${esc(x)}</div>`).join("")}${e.warnings.map((x) => `<div style="color:var(--warn)">⚠ ${esc(x)}</div>`).join("")}</td><td class="num">${fmtBytes(e.bytes)}</td></tr>`).join("")}</tbody></table>`;
        q("#sd-sum").textContent = `${fmtBytes(p.entries.reduce((n, e) => n + e.bytes, 0))} to send. RustyBox can't see the console's free space: make sure there is room.`;
        q("#sd-go").disabled = !p.ok;
      } catch (e) { if (my === seq) q("#sd-plan").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; }
    }
    ["#sd-dest", "#sd-layout", "#sd-rep"].forEach((s) => (q(s).onchange = refresh));
    q("#sd-no").onclick = () => modal.remove();
    q("#sd-go").onclick = async () => {
      q("#sd-go").disabled = true;
      try { const { job } = await api(`/api/consoles/${cur.id}/send/start`, { body: body() }); modal.remove(); toast("Started", "ok"); ctx.openJob(job); }
      catch (e) { q("#sd-plan").insertAdjacentHTML("afterbegin", `<div class="notice err" style="margin-bottom:8px">${esc(e.message).replace(/\n/g, "<br>")}</div>`); q("#sd-go").disabled = false; }
    };
    refresh();
  }

  async function deleteDialog(paths) {
    const pre = [];
    for (const path of paths) { try { pre.push(await api(`/api/consoles/${cur.id}/delete`, { body: { path } })); } catch (e) { return toast(e.message, "err"); } }
    const modal = el(`<div class="modal-bg"><div class="modal"><header>Delete from ${esc(cur.name)}?</header><div class="body" style="padding-top:14px">
      <div class="notice err">This <b>deletes</b> ${pre.length === 1 ? "this" : "these"} from the console (${fmtBytes(pre.reduce((n, p) => n + p.bytes, 0))}, ${pre.reduce((n, p) => n + p.files, 0)} files). It can't be undone.</div>
      <table style="margin-top:10px"><tbody>${pre.map((p) => `<tr><td class="mono" style="word-break:break-all">${esc(p.path)}</td><td class="num">${fmtBytes(p.bytes)}</td></tr>`).join("")}</tbody></table></div>
      <footer><button class="btn" id="dl-no">Cancel</button><button class="btn danger" id="dl-yes">Delete</button></footer></div></div>`);
    document.body.append(modal);
    modal.querySelector("#dl-no").onclick = () => modal.remove();
    modal.querySelector("#dl-yes").onclick = async () => {
      try { let last; for (const p of pre) last = await api(`/api/consoles/${cur.id}/delete`, { body: { path: p.path, confirm: true } }); modal.remove(); ctx.openJob(last.job); }
      catch (e) { toast(e.message, "err"); }
    };
  }

  async function fetchDialog(picked) {
    const dest = main.paths.find((p) => p.writable && p.health.online);
    if (!dest) return toast(`${main.name} has no writable folder here`, "err");
    if (!confirm(`Copy ${picked.length} game folder${picked.length === 1 ? "" : "s"} from the console into ${main.name}?`)) return;
    try { let last; for (const r of picked) last = await api(`/api/consoles/${cur.id}/fetch`, { body: { path: r.path, library_id: main.id, path_id: dest.id, rel: r.path.split("/").slice(-2).join("/") } }); ctx.openJob(last.job); }
    catch (e) { toast(e.message, "err"); }
  }

  // ── Browse ──
  async function browse(out, path = sessionStorage.getItem("console-path") || "/") {
    out.innerHTML = `<span class="muted">Connecting…</span>`;
    let d;
    try { d = await api(`/api/consoles/${cur.id}/ls?path=${encodeURIComponent(path)}`); } catch (e) { out.innerHTML = `<div class="notice err">${esc(e.message)}</div>`; return; }
    sessionStorage.setItem("console-path", d.path);
    const inDrive = d.path.split("/").filter(Boolean).length >= 1;
    out.innerHTML = `<div class="card"><div class="row"><div class="grow mono" style="word-break:break-all">${esc(d.path)}</div>
      ${d.parent !== null ? `<button class="btn small" data-go="${esc(d.parent)}">⬆️ Up</button>` : ""}<button class="btn small" id="b-new" ${inDrive ? "" : "disabled"}>New folder…</button></div>
      <table style="margin-top:8px"><tbody>${d.entries.map((e) => `<tr><td>${e.is_dir ? `📁 <a href="#" data-go="${esc(d.path.replace(/\/$/, ""))}/${esc(e.name)}">${esc(e.name)}</a>` : `📄 ${esc(e.name)}`}</td><td class="num">${e.is_dir ? "" : fmtBytes(e.size)}</td>
        <td class="num">${inDrive ? `<button class="btn small" data-ren="${esc(e.name)}">Rename</button> <button class="btn small danger" data-del="${esc(e.name)}">Delete</button>` : ""}</td></tr>`).join("") || `<tr><td class="muted">Empty.</td></tr>`}</tbody></table></div>`;
    out.querySelectorAll("[data-go]").forEach((a) => (a.onclick = (ev) => { ev.preventDefault(); browse(out, a.dataset.go); }));
    const full = (n) => `${d.path.replace(/\/$/, "")}/${n}`;
    out.querySelector("#b-new")?.addEventListener("click", async () => {
      const n = prompt("Name of the new folder"); if (!n) return;
      try { await api(`/api/consoles/${cur.id}/mkdir`, { body: { path: full(n) } }); browse(out, d.path); } catch (e) { toast(e.message, "err"); }
    });
    out.querySelectorAll("[data-ren]").forEach((b) => (b.onclick = async () => {
      const n = prompt("New name", b.dataset.ren); if (!n || n === b.dataset.ren) return;
      try { await api(`/api/consoles/${cur.id}/rename`, { body: { from: full(b.dataset.ren), to: full(n) } }); browse(out, d.path); } catch (e) { toast(e.message, "err"); }
    }));
    out.querySelectorAll("[data-del]").forEach((b) => (b.onclick = () => deleteDialog([full(b.dataset.del)])));
  }

  head(); show();
}

// ── Set up or edit a console ──
function form(c) {
  const v = c || { name: "My Xbox 360", host: "", port: 21, user: "xbox", password: "", game_paths: ["Hdd1/Games"], aurora_path: "Hdd1/Aurora", scan_depth: 4 };
  return `<label class="field">Name<input type="text" id="k-name" value="${esc(v.name)}"></label>
    <div class="row"><label class="field grow">Console IP address<input type="text" id="k-host" value="${esc(v.host)}" placeholder="192.168.1.50" spellcheck="false"></label>
      <label class="field" style="width:90px">Port<input type="number" id="k-port" value="${v.port}"></label></div>
    <div class="row"><label class="field grow">FTP user<input type="text" id="k-user" value="${esc(v.user)}"></label>
      <label class="field grow">FTP password<input type="password" id="k-pass" autocomplete="new-password" placeholder="${c ? (c.password_set ? "(unchanged)" : "") : "xbox"}"></label></div>
    <label class="field">Games folders on the console (one per line)<textarea id="k-paths" rows="2" spellcheck="false">${esc(v.game_paths.join("\n"))}</textarea><small class="muted">What Aurora scans for games, such as <b>Hdd1/Games</b> or <b>Usb0/Games</b>.</small></label>
    <label class="field">Aurora's folder<input type="text" id="k-aurora" value="${esc(v.aurora_path)}"></label>`;
}

function read(root, id) {
  const q = (s) => root.querySelector(s);
  const o = { name: q("#k-name").value, host: q("#k-host").value, port: +q("#k-port").value || 21, user: q("#k-user").value || "xbox", password: q("#k-pass").value, game_paths: q("#k-paths").value.split("\n").map((s) => s.trim()).filter(Boolean), aurora_path: q("#k-aurora").value };
  if (id) o.id = id;
  return o;
}

async function testIt(root, id) {
  const out = root.querySelector("#k-info"), err = root.querySelector("#k-err");
  err.classList.add("hidden"); out.textContent = "Connecting…";
  try { const r = await api("/api/consoles/test", { body: { ...read(root, id), id } }); out.innerHTML = `<span style="color:var(--ok)">Connected.</span> Drives: ${r.drives.map(esc).join(", ")}`; return true; }
  catch (e) { out.textContent = ""; err.textContent = e.message; err.classList.remove("hidden"); return false; }
}

function setup(root, ctx) {
  root.innerHTML = `<div class="page-head"><h1>🕹️ Console</h1><div class="muted">Connect your Xbox 360 to see what is on it, compare it with your libraries and send games over your network.</div></div>
    <div class="card" style="max-width:640px"><h2>Connect your console</h2>
      <ol style="line-height:1.8;padding-left:20px"><li>On the console, start <b>Aurora</b> and switch on <b>FTP</b> in its settings (Settings → Network).</li><li>Note its IP address (shown in Aurora's network settings) and enter it below. The default login is <span class="mono">xbox</span> / <span class="mono">xbox</span>.</li></ol>
      <div style="display:flex;flex-direction:column;gap:10px;max-width:520px">${form(null)}
      <div class="row"><button class="btn" id="k-test">Test connection</button><button class="btn primary" id="k-save">Save</button><span class="muted" id="k-info"></span></div><div class="notice err hidden" id="k-err"></div></div></div>`;
  root.querySelector("#k-test").onclick = () => testIt(root, 0);
  root.querySelector("#k-save").onclick = async () => {
    try { await api("/api/consoles", { body: read(root, 0) }); location.reload(); } catch (e) { const er = root.querySelector("#k-err"); er.textContent = e.message; er.classList.remove("hidden"); }
  };
}

function editDialog(c, done, ctx) {
  const modal = el(`<div class="modal-bg"><div class="modal"><header>${c ? "Edit " + esc(c.name) : "Add a console"}</header><div class="body" style="padding-top:14px;display:flex;flex-direction:column;gap:10px">${form(c)}
    <div class="row"><button class="btn" id="k-test">Test connection</button><span class="muted" id="k-info"></span></div><div class="notice err hidden" id="k-err"></div></div>
    <footer>${c ? `<button class="btn danger" id="k-del">Remove console</button>` : ""}<span class="grow"></span><button class="btn" id="k-no">Cancel</button><button class="btn primary" id="k-save">Save</button></footer></div></div>`);
  document.body.append(modal);
  const q = (s) => modal.querySelector(s);
  q("#k-no").onclick = () => modal.remove();
  q("#k-test").onclick = () => testIt(modal, c?.id);
  q("#k-save").onclick = async () => {
    try { const saved = c ? await api(`/api/consoles/${c.id}`, { method: "PUT", body: read(modal, c.id) }) : await api("/api/consoles", { body: read(modal, 0) }); modal.remove(); done(saved); }
    catch (e) { q("#k-err").textContent = e.message; q("#k-err").classList.remove("hidden"); }
  };
  q("#k-del")?.addEventListener("click", async () => {
    if (!confirm(`Remove ${c.name} from RustyBox? Nothing on the console is touched.`)) return;
    await api(`/api/consoles/${c.id}`, { method: "DELETE" }); modal.remove(); location.reload();
  });
}
