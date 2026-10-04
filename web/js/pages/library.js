import { api, el, esc, fmtBytes, fmtTime, toast } from "../util.js";
import { chooseFolder } from "../picker.js";
import { followJob } from "../jobs.js";
import { uploadFile } from "../upload.js";
import { openConvert, OPS } from "../convert.js";
import { openSend } from "../import.js";
import { tidyDialog } from "../tidy.js";
import { misplacedDialog } from "../misplaced.js";
import { installLocal } from "../install.js";

export const title = "Library";
let follower = null;

export async function render(root, ctx, idArg) {
  const id = +idArg;
  let lib, q = "", pathFilter = "", timer, sort = "name", dir = "asc", loaded = [];
  const selected = new Set();
  // Leaving the page ends the scan follower, so a scan that finishes later can't redraw (and
  // re-wire the buttons of) a page that is no longer this one.
  ctx.leave.push(() => follower?.close());
  const opsFor = (kind) => (kind === "iso" ? ["iso_to_god", "extract"] : kind === "god" ? ["god_to_iso"] : []);
  const reload = async () => { lib = await api(`/api/libraries/${id}`); };
  await reload();
  document.title = `${lib.name} · RustyBox`;

  const shell = () => {
    root.innerHTML = `
      <div class="page-head row"><div class="grow"><a class="crumb" href="#libraries">← Libraries</a><h1>${lib.icon || ""} ${esc(lib.name)}</h1>
        <div class="muted">${esc(lib.kind_label || lib.kind)} · <span id="lib-count">${lib.items.toLocaleString()} items, ${fmtBytes(lib.bytes)}</span></div></div>
        <details class="menu"><summary class="btn">More ▾</summary><div class="menu-pop">
          ${lib.kind === "god" ? `<button id="tidy">Tidy folders…</button>` : ""}
          <button id="fix">Fix misplaced items…</button>
          <button id="rename">Rename</button>
          <button id="delete" class="danger">Remove library…</button></div></details></div>
      <div id="scan-job"></div>
      <div class="card"><div class="row"><h2 class="grow" style="margin:0">Folders</h2>
        <button class="btn small" id="add-path">＋ Add folder</button><button class="btn small primary" id="scan">Scan now</button></div>
        <table style="margin-top:10px"><thead><tr><th>Folder</th><th>Status</th><th>Space</th><th>Last scan</th><th>Writable</th><th></th></tr></thead><tbody>
        ${lib.paths.map((p, ix) => `<tr><td><b>${p.remote_url ? "🖥 " : ""}${esc(p.label || "")}</b>${ix === 0 ? ` <span class="badge ok" title="Fix misplaced items puts things that belong in this library here">primary</span>` : ` <button class="btn small" data-primary="${p.id}" title="Make this the folder things are put in">Make primary</button>`}<div class="mono muted">${esc(p.remote_url ? p.remote_url : p.path)}</div>
          ${p.health.fs_type ? `<div class="muted" style="font-size:12.5px">${esc(p.health.fs_type)}${p.health.max_file ? " · files must be under 4 GB" : ""}${p.health.share_read_only ? " · shared read-only" : ""}</div>` : ""}</td>
          <td>${p.health.online ? `<span class="badge ok">online</span>` : `<span class="badge err">offline</span><div class="muted">${esc(p.health.problem || "")}</div>`}</td>
          <td class="muted">${p.health.online ? `${fmtBytes(p.health.free)} free of ${fmtBytes(p.health.total)}` : ""}</td>
          <td class="muted">${p.last_scan ? fmtTime(p.last_scan) : "never"}</td>
          <td><input type="checkbox" data-w="${p.id}" ${p.writable ? "checked" : ""}></td>
          <td><button class="btn small" data-rm="${p.id}" title="Remove this folder from the library (files stay where they are)">✕</button></td></tr>`).join("")
          || `<tr><td colspan="6" class="muted">No folders yet. Add one to start.</td></tr>`}
        </tbody></table></div>
      <div class="card" id="drop"><div class="row"><h2 class="grow" style="margin:0">Upload from this computer</h2>
        <select id="up-path">${lib.paths.filter((p) => p.writable && p.health.online).map((p) => `<option value="${p.id}">${esc(p.label || p.path)}</option>`).join("")}</select>
        <input type="file" id="up-files" multiple class="hidden"><button class="btn small" id="up-pick">Choose files…</button></div>
        <div class="muted" id="up-hint" style="margin-top:8px">${lib.paths.some((p) => p.writable && p.health.online) ? "Drop files here, or choose them. Large files are sent in pieces and can resume." : "Turn on <b>Writable</b> for a folder to upload into it."}</div>
        <div id="up-list"></div></div>
      <div class="card"><div class="row"><h2 class="grow" style="margin:0">Items</h2>
        <select id="f-path"><option value="">All folders</option>${lib.paths.map((p) => `<option value="${p.id}">${esc(p.label || p.path)}</option>`).join("")}</select>
        <input type="search" id="f-q" placeholder="Search name or title ID…"></div>
        <div id="actions" class="row hidden" style="margin-top:10px"><span class="muted" id="sel-count"></span>
          ${opsFor(lib.kind).map((o) => `<button class="btn small primary" data-op="${o}">${esc(OPS[o].label)}…</button>`).join("")}
          ${["iso", "god"].includes(lib.kind) ? `<button class="btn small primary" id="sel-send">Send to…</button>` : ""}
          ${lib.kind === "iso" ? `<button class="btn small" id="sel-check">Check ISO…</button>` : ""}
          <button class="btn small" id="sel-clear">Clear</button></div>
        <div id="items" style="margin-top:10px"></div></div>`;
    wire();
    loadItems();
  };

  const PAGE = 200;
  const cols = [["name", "Name"], ["title", "Title ID"], [null, "Folder"], ["size", "Size", "num"], ["mtime", "Modified"]];
  const arrow = (k) => (k === sort ? (dir === "asc" ? " ▲" : " ▼") : "");
  const rowHtml = (i) => `<tr class="${i.available ? "" : "muted"}">${opsFor(lib.kind).length ? `<td><input type="checkbox" class="sel" data-id="${i.id}" ${selected.has(i.id) ? "checked" : ""} ${i.meta === "error" || !i.available ? "disabled" : ""}></td>` : ""}<td>${esc(i.game_name || i.name)}${i.available ? "" : ` <span class="badge err">unavailable</span>`}
          ${i.discs > 1 ? ` <span class="badge">disc ${i.disc ?? "?"}/${i.discs}</span>` : ""}${i.platform === "xbox" ? ` <span class="badge">Original Xbox</span>` : ""}
          ${i.meta === "error" ? ` <span class="badge err" title="${esc(i.meta_error || "")}">can't read</span>` : ""}
          <div class="mono muted">${esc(i.relpath)}</div>${i.meta === "error" ? `<div class="muted" style="font-size:12.5px">${esc(i.meta_error || "")}</div>` : ""}</td>
          <td class="mono">${esc(i.title_id || "")}${i.media_id ? `<div class="muted" title="Media ID">${esc(i.media_id)}</div>` : ""}</td><td class="muted">${esc(i.path_label)}</td><td class="num">${fmtBytes(i.size)}</td><td class="muted">${fmtTime(i.mtime)}</td>
          <td style="white-space:nowrap">${["iso", "god"].includes(lib.kind) ? "" : `<button class="btn small" data-install="${i.id}">Install on console…</button> `}${i.kind === "god" ? "" : `<a class="btn small" href="/api/libraries/${id}/items/${i.id}/download">Download</a>`}</td></tr>`;

  // `more` adds the next page under the ones already shown; otherwise the list starts again.
  async function loadItems(more = false) {
    const offset = more ? loaded.length : 0;
    const d = await api(`/api/libraries/${id}/items?limit=${PAGE}&offset=${offset}&sort=${sort}&dir=${dir}&q=${encodeURIComponent(q)}${pathFilter ? `&path_id=${pathFilter}` : ""}`);
    loaded = more ? loaded.concat(d.items) : d.items;
    const sel = opsFor(lib.kind).length;
    root.querySelector("#items").innerHTML = loaded.length
      ? `<table><thead><tr>${sel ? `<th style="width:28px"><input type="checkbox" id="sel-all" title="Select all shown"></th>` : ""}${cols.map(([k, l, c]) => k ? `<th class="${c || ""}"><button class="th-sort" data-sort="${k}">${l}${arrow(k)}</button></th>` : `<th>${l}</th>`).join("")}<th></th></tr></thead><tbody>
        ${loaded.map(rowHtml).join("")}
        </tbody></table><div class="muted" style="margin-top:8px">${loaded.length < d.total ? `Showing ${loaded.length.toLocaleString()} of ${d.total.toLocaleString()} · <button class="btn small" id="more">Show ${Math.min(PAGE, d.total - loaded.length)} more</button>` : `${d.total.toLocaleString()} item${d.total === 1 ? "" : "s"}`}</div>`
      : `<div class="empty"><span class="ico">📂</span>${q || pathFilter ? "Nothing matches." : "Nothing here yet. Add a folder and scan, or upload files."}</div>`;
    root.querySelectorAll(".th-sort").forEach((b) => (b.onclick = () => {
      if (sort === b.dataset.sort) dir = dir === "asc" ? "desc" : "asc"; else { sort = b.dataset.sort; dir = sort === "size" || sort === "mtime" ? "desc" : "asc"; }
      loadItems();
    }));
    root.querySelector("#more")?.addEventListener("click", () => loadItems(true));
    wireSelection();
    root.querySelectorAll("[data-install]").forEach((b) => (b.onclick = () => installLocal(id, d.items.find((x) => String(x.id) === b.dataset.install), ctx)));
  }

  const updateActions = () => {
    const a = root.querySelector("#actions");
    if (!a) return;
    a.classList.toggle("hidden", !selected.size);
    root.querySelector("#sel-count").textContent = `${selected.size} selected`;
    const chk = root.querySelector("#sel-check");
    if (chk) { chk.disabled = selected.size !== 1; chk.title = selected.size === 1 ? "" : "Select one ISO to check"; }
  };
  function wireSelection() {
    root.querySelectorAll(".sel").forEach((c) => (c.onchange = () => { c.checked ? selected.add(+c.dataset.id) : selected.delete(+c.dataset.id); updateActions(); }));
    const all = root.querySelector("#sel-all");
    if (all) all.onchange = () => root.querySelectorAll(".sel:not(:disabled)").forEach((c) => { c.checked = all.checked; c.checked ? selected.add(+c.dataset.id) : selected.delete(+c.dataset.id); updateActions(); });
    updateActions();
  }

  function startScan() {
    follower?.close();
    return api(`/api/libraries/${id}/scan`, { method: "POST" }).then(({ job }) => {
      follower = followJob(job, root.querySelector("#scan-job"), {
        onEnd: async () => { await reload(); shell(); },
        onResult: (name, d, box) => { if (name === "scan") toast(`Scan finished: ${d.added} added, ${d.updated} changed, ${d.removed} removed`, "ok");
          if (name === "scan") box.innerHTML = `<div class="notice">Added <b>${d.added}</b>, changed <b>${d.updated}</b>, removed <b>${d.removed}</b>${d.offline_paths ? ` · <b>${d.offline_paths}</b> folder(s) offline` : ""}.</div>`; },
      });
    }).catch((e) => toast(e.message, "err"));
  }

  const addUploadRow = (name) => {
    const row = document.createElement("div");
    row.style.marginTop = "8px";
    row.innerHTML = `<div class="row"><span class="grow mono">${esc(name)}</span><span class="muted up-state">waiting</span></div><div class="bar"><i></i></div>`;
    root.querySelector("#up-list").append(row);
    return { set(pct, txt) { row.querySelector("i").style.width = pct + "%"; row.querySelector(".up-state").textContent = txt; }, row };
  };

  async function upload(files) {
    const pathId = root.querySelector("#up-path").value;
    if (!pathId) return toast("Choose a writable folder first", "err");
    let okCount = 0;
    for (const f of files) {
      const rel = f.webkitRelativePath || f.name;
      const ui = addUploadRow(rel);
      const attempt = async (overwrite) => uploadFile(f, { libraryId: id, pathId, rel, overwrite, onProgress: (d, t) => ui.set(t ? (d / t) * 100 : 100, `${fmtBytes(d)} / ${fmtBytes(t)}`) });
      try {
        try { await attempt(false); }
        catch (e) { if (e.code === "EXISTS" && confirm(`"${rel}" already exists. Replace it?`)) await attempt(true); else throw e; }
        ui.set(100, "done");
        ui.row.querySelector("i").parentElement.classList.add("done");
        okCount++;
      } catch (e) { ui.set(0, "failed: " + e.message); ui.row.querySelector(".up-state").style.color = "var(--err)"; }
    }
    if (okCount) { toast(`${okCount} file${okCount > 1 ? "s" : ""} uploaded. Scanning…`, "ok"); startScan(); }
  }

  function checkDialog(itemId) {
    const abgx = ctx.status.tools.find((t) => t.name === "abgx360");
    const writable = lib.paths.some((p) => p.writable);
    const modal = el(`<div class="modal-bg"><div class="modal"><header>Check ISO with abgx360</header><div class="body" style="display:flex;flex-direction:column;gap:12px;padding-top:14px">
      ${abgx?.found ? "" : `<div class="notice err">abgx360 isn't installed here, so ISOs can't be checked.</div>`}
      <label class="check"><input type="radio" name="cm" value="check" checked><span><b>Check only</b><small>Reads the image and reports. Nothing is changed.</small></span></label>
      <label class="check"><input type="radio" name="cm" value="fix" ${writable ? "" : "disabled"}><span><b>Check and fix</b><small>abgx360 may repair the image in place (headers, stealth sectors, video padding). ${writable ? "" : "Turn on <b>Writable</b> for the folder first."}</small></span></label>
      <label class="check" id="bk"><input type="checkbox" id="bk-on" checked><span>Keep a backup copy first<small>Saved next to the ISO as <span class="mono">.rustybox-backup</span>. Needs as much free space as the ISO.</small></span></label>
      <div class="notice err hidden" id="ck-err"></div></div>
      <footer><button class="btn" id="ck-cancel">Cancel</button><button class="btn primary" id="ck-go" ${abgx?.found ? "" : "disabled"}>Start</button></footer></div></div>`);
    document.body.append(modal);
    const q = (x) => modal.querySelector(x);
    const sync = () => q("#bk").classList.toggle("hidden", modal.querySelector('input[name=cm]:checked').value !== "fix");
    modal.querySelectorAll("input[name=cm]").forEach((r) => (r.onchange = sync));
    sync();
    q("#ck-cancel").onclick = () => modal.remove();
    q("#ck-go").onclick = async () => {
      const fix = modal.querySelector("input[name=cm]:checked").value === "fix";
      try {
        const { job } = await api("/api/iso/check", { body: { library_id: id, item_id: itemId, fix, backup: q("#bk-on").checked } });
        modal.remove(); selected.clear(); ctx.openJob(job);
      } catch (e) { q("#ck-err").textContent = e.message; q("#ck-err").classList.remove("hidden"); }
    };
  }

  function wire() {
    const $ = (s) => root.querySelector(s);
    $("#scan").onclick = startScan;
    $("#add-path").onclick = async () => {
      const f = await chooseFolder();
      if (!f) return;
      try { await api(`/api/libraries/${id}/paths`, { body: { path: f.path || "", label: f.label, writable: f.writable, remote: f.remote } }); await reload(); shell(); }
      catch (e) { toast(e.message, "err"); }
    };
    root.querySelectorAll("[data-primary]").forEach((b) => (b.onclick = async () => {
      try { await api(`/api/libraries/${id}/paths/${b.dataset.primary}`, { method: "PUT", body: { primary: true } }); await reload(); shell(); toast("Primary folder changed", "ok"); } catch (e) { toast(e.message, "err"); }
    }));
    root.querySelectorAll("[data-rm]").forEach((b) => (b.onclick = async () => {
      if (!confirm("Remove this folder from the library? Files on disk are not touched.")) return;
      try { await api(`/api/libraries/${id}/paths/${b.dataset.rm}`, { method: "DELETE" }); await reload(); shell(); } catch (e) { toast(e.message, "err"); }
    }));
    root.querySelectorAll("[data-w]").forEach((c) => (c.onchange = async () => {
      try { await api(`/api/libraries/${id}/paths/${c.dataset.w}`, { method: "PUT", body: { writable: c.checked } }); await reload(); shell(); } catch (e) { toast(e.message, "err"); }
    }));
    $("#rename").onclick = async () => {
      const name = prompt("Library name", lib.name);
      if (!name || name === lib.name) return;
      try { await api(`/api/libraries/${id}`, { method: "PUT", body: { name } }); await reload(); shell(); } catch (e) { toast(e.message, "err"); }
    };
    $("#delete").onclick = async () => {
      if (!confirm(`Remove the library "${lib.name}"? This forgets the library and its index. Your files are not touched.`)) return;
      await api(`/api/libraries/${id}`, { method: "DELETE" });
      location.hash = "libraries";
    };
    root.querySelectorAll("[data-op]").forEach((b) => (b.onclick = () => openConvert({
      op: b.dataset.op, items: [...selected].map((item_id) => ({ library_id: id, item_id })),
      title: `${OPS[b.dataset.op].label} — ${selected.size} game${selected.size === 1 ? "" : "s"}`,
      onStarted: (job) => { selected.clear(); ctx.openJob(job); },
    })));
    $("#sel-check")?.addEventListener("click", () => checkDialog([...selected][0]));
    $("#sel-send")?.addEventListener("click", () => openSend({
      items: [...selected].map((item_id) => ({ library_id: id, item_id })), title: `Send ${selected.size} game${selected.size === 1 ? "" : "s"} to…`,
      onStarted: (job) => { selected.clear(); ctx.openJob(job); },
    }));
    $("#tidy")?.addEventListener("click", () => tidyDialog(lib, ctx));
    $("#fix").addEventListener("click", () => misplacedDialog(lib, ctx));
    $("#sel-clear").onclick = () => { selected.clear(); root.querySelectorAll(".sel, #sel-all").forEach((c) => (c.checked = false)); updateActions(); };
    $("#f-q").oninput = (e) => { q = e.target.value; clearTimeout(timer); timer = setTimeout(() => loadItems(), 250); };
    $("#f-path").onchange = (e) => { pathFilter = e.target.value; loadItems(); };
    $("#up-pick").onclick = () => $("#up-files").click();
    $("#up-files").onchange = (e) => { upload([...e.target.files]); e.target.value = ""; };
    const drop = $("#drop");
    drop.ondragover = (e) => { e.preventDefault(); drop.style.borderColor = "var(--accent)"; };
    drop.ondragleave = () => (drop.style.borderColor = "");
    drop.ondrop = (e) => { e.preventDefault(); drop.style.borderColor = ""; upload([...e.dataTransfer.files]); };
  }

  shell();
}
