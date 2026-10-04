import { api, esc, fmtBytes, toast } from "../util.js";
import { pickFolder, pickLibraryFolder } from "../picker.js";
import { mountPlanner } from "../import.js";

export const title = "Import";

export async function render(root, ctx) {
  const libs = await api("/api/libraries");
  const folders = libs.flatMap((l) => l.paths.map((p) => ({ libraryId: l.id, pathId: p.id, label: `${l.name} / ${p.label || p.path}`, remote: !!p.remote_url, online: p.health.online })));
  let source = null, candidates = [], planner = null;
  const selected = new Set();

  root.innerHTML = `
    <div class="page-head"><h1>Import</h1>
      <div class="muted">Bring games in from any folder (on this server or on a drive attached to another computer): copy, move, link, convert ISOs to GOD, and put them on the Xbox's drive in one go.</div></div>
    <div class="card"><h2>1. Where are the games?</h2>
      <div class="row" style="gap:20px;align-items:flex-start">
        <div><label class="check"><input type="radio" name="src" value="folder" checked><span><b>A folder on this server</b></span></label>
          <div class="row" style="margin:6px 0 0 24px"><button class="btn small" id="src-pick">Choose a folder…</button><span class="mono muted" id="src-path"></span></div></div>
        <div><label class="check"><input type="radio" name="src" value="drive"><span><b>A folder in a library folder or drive</b></span></label>
          <div class="row" style="margin:6px 0 0 24px"><select id="src-lib" ${folders.length ? "" : "disabled"}>${folders.map((f) => `<option value="${f.libraryId}:${f.pathId}">${f.remote ? "🖥 " : ""}${esc(f.label)}${f.online ? "" : " (offline)"}</option>`).join("")}</select>
            <input type="text" id="src-rel" placeholder="folder inside it (optional)" spellcheck="false"><button class="btn small" id="src-browse">Browse…</button></div></div>
      </div>
      <div class="row" style="margin-top:12px"><button class="btn primary" id="src-scan">Look inside</button><span class="muted" id="src-status"></span></div></div>
    <div class="card hidden" id="cands-card"><div class="row"><h2 class="grow" style="margin:0">2. What to import</h2>
      <label class="check"><input type="checkbox" id="c-only"><span>Include add-ons and other content</span></label><button class="btn small" id="c-all">Select all</button><button class="btn small" id="c-none">None</button></div>
      <div id="cands" style="margin-top:10px"></div></div>
    <div id="planner" class="hidden"></div>`;
  const $ = (s) => root.querySelector(s);
  const mode = () => root.querySelector("input[name=src]:checked").value;

  $("#src-pick").onclick = async () => { const p = await pickFolder(); if (p) { source = { kind: "folder", path: p }; $("#src-path").textContent = p; root.querySelector("input[name=src][value=folder]").checked = true; } };
  $("#src-browse").onclick = async () => {
    const [lib, path] = $("#src-lib").value.split(":").map(Number);
    const rel = await pickLibraryFolder(lib, path, $("#src-rel").value.trim().replace(/^\/+|\/+$/g, ""));
    if (rel === null) return;
    $("#src-rel").value = rel;
    root.querySelector("input[name=src][value=drive]").checked = true;
  };
  const sourceNow = () => {
    if (mode() === "folder") return source;
    const [libraryId, pathId] = $("#src-lib").value.split(":").map(Number);
    return { kind: "drive", library_id: libraryId, path_id: pathId, rel: $("#src-rel").value.trim().replace(/^\/+|\/+$/g, "") };
  };

  function drawCands() {
    const all = $("#c-only").checked;
    const shown = candidates.filter((c) => all || c.content_kind === "game");
    $("#cands").innerHTML = shown.length ? `<table><thead><tr><th style="width:28px"></th><th>Game</th><th>Type</th><th>Title ID</th><th class="num">Size</th><th>Where</th></tr></thead><tbody>
      ${shown.map((c) => `<tr><td><input type="checkbox" class="cs" data-id="${esc(c.id)}" ${selected.has(c.id) ? "checked" : ""}></td>
        <td>${esc(c.name)}${c.discs > 1 ? ` <span class="badge">disc ${c.disc ?? "?"}/${c.discs}</span>` : ""}${c.content_kind !== "game" ? ` <span class="badge">${esc(c.content_kind)}</span>` : ""}
          ${c.health ? ` <span class="badge err" title="${esc(c.health)}">damaged</span><div class="muted" style="font-size:12.5px">${esc(c.health)}</div>` : ""}</td>
        <td><span class="badge">${c.kind.toUpperCase()}</span></td><td class="mono">${esc(c.title_id || "")}</td><td class="num">${fmtBytes(c.size)}</td><td class="mono muted">${esc(c.id)}</td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">No ISOs or Games on Demand folders found here.</div>`;
    root.querySelectorAll(".cs").forEach((c) => (c.onchange = () => { c.checked ? selected.add(c.dataset.id) : selected.delete(c.dataset.id); planner?.refresh(); }));
  }
  $("#c-only").onchange = drawCands;
  $("#c-all").onclick = () => { candidates.filter((c) => $("#c-only").checked || c.content_kind === "game").forEach((c) => selected.add(c.id)); drawCands(); planner?.refresh(); };
  $("#c-none").onclick = () => { selected.clear(); drawCands(); planner?.refresh(); };

  $("#src-scan").onclick = async () => {
    const s = sourceNow();
    if (!s) return toast("Choose a folder first", "err");
    $("#src-status").textContent = "Looking inside… (a big drive can take a little while)";
    try {
      const r = await api("/api/import/scan", { body: { source: s } });
      candidates = r.candidates; selected.clear();
      $("#src-status").textContent = `${candidates.length} item${candidates.length === 1 ? "" : "s"} found in ${r.label}`;
      $("#cands-card").classList.remove("hidden"); $("#planner").classList.remove("hidden");
      drawCands();
      if (!planner) planner = await mountPlanner($("#planner"), { sourceBody: () => (selected.size ? { source: sourceNow(), select: [...selected] } : null), onStarted: (job) => ctx.openJob(job) });
      else planner.refresh();
    } catch (e) { $("#src-status").textContent = ""; toast(e.message, "err"); }
  };
}
