// The part of importing that is the same everywhere: where it goes, how, and the plan.
// Used by the Import page (games found in a folder) and by "Send to…" (games already in libraries).
import { api, el, esc, fmtBytes, toast } from "./util.js";

export const MODES = [
  ["copy", "Copy", "Keeps the original. Works between any two places, including a drive on another computer."],
  ["move", "Move", "Instant on the same disk; otherwise copied, checked, and only then removed from the source."],
  ["hardlink", "Hard link", "The same data in two places, using no extra space. Same disk only, and not for FAT32 or exFAT drives."],
  ["symlink", "Symlink", "A pointer to the original. It only works inside RustyBox and breaks if the original is moved."],
];

const GAME_KINDS = ["iso", "god"];

/** All the folders games can go into, as [{label, libraryId, pathId, kind, remote, writable}]. */
export async function destinations() {
  const libs = await api("/api/libraries");
  return libs.filter((l) => GAME_KINDS.includes(l.kind)).flatMap((l) =>
    l.paths.map((p) => ({ label: `${l.name} / ${p.label || p.path}`, libraryId: l.id, pathId: p.id, kind: l.kind, remote: !!p.remote_url, writable: p.writable, online: p.health.online, fs: p.health.fs_type })));
}

const opt = (d) => `<option value="${d.libraryId}:${d.pathId}">${d.remote ? "🖥 " : ""}${esc(d.label)}${d.writable ? "" : " (read-only)"}${d.online ? "" : " (offline)"}</option>`;
const parseDest = (v) => (v ? { library_id: +v.split(":")[0], path_id: +v.split(":")[1] } : null);

/**
 * Put the options and the live plan into `box`.
 * opts: { sourceBody() -> {source, select?} | null, onStarted(jobId), defaultDest, overwrite }
 * Returns { refresh() } to call when the source selection changes.
 */
export async function mountPlanner(box, opts) {
  const dests = (await destinations()).filter((d) => d.writable);
  box.innerHTML = `
    <div class="card"><h2>Where to put it</h2>
      <div class="row"><label class="field grow">Into<select id="pl-dest">${dests.map(opt).join("") || `<option value="">No writable GOD or ISO folder yet</option>`}</select></label>
        <label class="field" id="pl-layout-wrap">GOD folder layout<select id="pl-layout"><option value="name_titleid">Game Name / TitleID</option><option value="titleid">TitleID only</option></select></label></div>
      <div style="margin-top:10px"><div class="muted" style="margin-bottom:6px">How</div>
        <div id="pl-modes" style="display:grid;gap:6px">${MODES.map(([v, l, b], i) => `<label class="check"><input type="radio" name="pl-mode" value="${v}" ${i === 0 ? "checked" : ""}><span><b>${l}</b><small>${b}</small></span></label>`).join("")}</div></div>
      <div class="row" style="margin-top:10px;align-items:flex-start;gap:24px">
        <label class="check" id="pl-conv-wrap"><input type="checkbox" id="pl-conv"><span>Convert ISOs to GOD on the way in<small>For a GOD library. With Move, the ISO is removed only after the converted game is safely in place.</small></span></label>
        <label class="check"><input type="checkbox" id="pl-over"><span>Replace the game if it is already there<small>Removes the old copy (found by title ID, whatever its folder is called) and puts this one in. Saved games, add-ons and title updates are kept. The plan lists exactly what is removed.</small></span></label></div>
      <div style="margin-top:10px"><label class="check"><input type="checkbox" id="pl-also-on"><span>Also copy to another place<small>For example straight on to the Xbox's drive.</small></span></label>
        <select id="pl-also" class="hidden" style="margin-top:6px;max-width:100%">${dests.map(opt).join("")}</select></div>
    </div>
    <div class="card"><h2>Plan</h2><div id="pl-plan" class="muted">Choose something to import.</div>
      <div class="row" style="margin-top:12px"><span class="muted grow" id="pl-sum"></span><button class="btn primary" id="pl-go" disabled>Start</button></div></div>`;
  const q = (s) => box.querySelector(s);
  if (opts.defaultDest) { const v = `${opts.defaultDest.libraryId}:${opts.defaultDest.pathId}`; if ([...q("#pl-dest").options].some((o) => o.value === v)) q("#pl-dest").value = v; }
  if (opts.replace || opts.overwrite) q("#pl-over").checked = true;
  if (opts.mode) { const r = box.querySelector(`input[name=pl-mode][value=${opts.mode}]`); if (r) r.checked = true; }
  const settings = await api("/api/settings").catch(() => ({}));
  if (settings.god_layout) q("#pl-layout").value = settings.god_layout;

  const current = () => dests.find((d) => `${d.libraryId}:${d.pathId}` === q("#pl-dest").value);
  const body = () => {
    const s = opts.sourceBody();
    if (!s || !current()) return null;
    const mode = box.querySelector("input[name=pl-mode]:checked").value;
    return { ...s, mode, dest: parseDest(q("#pl-dest").value), layout: q("#pl-layout").value, convert_iso: q("#pl-conv").checked, replace: q("#pl-over").checked, also_to: q("#pl-also-on").checked ? parseDest(q("#pl-also").value) : null };
  };
  let seq = 0, timer;
  async function refresh() {
    const isGod = current()?.kind === "god";
    q("#pl-layout-wrap").classList.toggle("hidden", !isGod);
    q("#pl-conv-wrap").classList.toggle("hidden", !isGod);
    if (!isGod) q("#pl-conv").checked = false;
    q("#pl-also").classList.toggle("hidden", !q("#pl-also-on").checked);
    q("#pl-go").disabled = true; q("#pl-sum").textContent = "";
    const b = body();
    if (!b) { q("#pl-plan").innerHTML = `<span class="muted">Choose something to import.</span>`; return; }
    const my = ++seq;
    q("#pl-plan").innerHTML = `<span class="muted">Checking…</span>`;
    try {
      const p = await api("/api/import/plan", { body: b });
      if (my !== seq) return;
      q("#pl-plan").innerHTML = renderPlan(p);
      q("#pl-sum").innerHTML = p.destinations.map((d) => `${esc(d.label)}: ${d.needed ? "about " + fmtBytes(d.needed) + " needed, " : ""}${fmtBytes(d.free)} free${d.fs_type ? " (" + esc(d.fs_type) + ")" : ""}`).join(" · ");
      q("#pl-go").disabled = !p.ok;
    } catch (e) { if (my === seq) q("#pl-plan").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; }
  }
  const later = () => { clearTimeout(timer); timer = setTimeout(refresh, 200); };
  ["#pl-dest", "#pl-layout", "#pl-conv", "#pl-over", "#pl-also-on", "#pl-also"].forEach((s) => (q(s).onchange = later));
  box.querySelectorAll("input[name=pl-mode]").forEach((r) => (r.onchange = later));
  q("#pl-go").onclick = async () => {
    const b = body(); if (!b) return;
    q("#pl-go").disabled = true;
    try { const { job } = await api("/api/import/start", { body: b }); toast("Started", "ok"); opts.onStarted?.(job); }
    catch (e) { q("#pl-plan").insertAdjacentHTML("afterbegin", `<div class="notice err" style="margin-bottom:8px">${esc(e.message).replace(/\n/g, "<br>")}</div>`); q("#pl-go").disabled = false; }
  };
  refresh();
  return { refresh: later };
}

function renderPlan(p) {
  const top = p.problems.map((x) => `<div class="notice err" style="margin-bottom:8px">${esc(x)}</div>`).join("");
  return `${top}<table><thead><tr><th>Game</th><th>What will happen</th><th class="num">Size</th></tr></thead><tbody>
    ${p.entries.map((e) => `<tr><td>${esc(e.name)}${e.discs > 1 ? ` <span class="badge">disc ${e.disc ?? "?"}/${e.discs}</span>` : ""}<div class="mono muted">${esc(e.title_id || "")}</div></td>
      <td>${e.steps.map((s, i) => `<div${/^Remove /.test(s) ? ` style="color:var(--err)"` : ""}>${i + 1}. ${esc(s)}</div>`).join("")}
        ${e.outputs.map((o) => `<div class="mono muted" style="word-break:break-all">→ ${esc(o)}</div>`).join("")}
        ${e.problems.map((x) => `<div style="color:var(--err)">⛔ ${esc(x)}</div>`).join("")}
        ${e.warnings.map((x) => `<div style="color:var(--warn)">⚠ ${esc(x)}</div>`).join("")}</td>
      <td class="num">${fmtBytes(e.bytes_in)}</td></tr>`).join("")}</tbody></table>`;
}

/** A dialog for sending games that are already in libraries somewhere else. */
export async function openSend({ items, title, defaultDest, overwrite, replace, mode, onStarted }) {
  const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:92vh"><header>${esc(title || "Send to…")}</header>
    <div class="body" id="sd-body" style="padding-top:14px"></div><footer><button class="btn" id="sd-close">Close</button></footer></div></div>`);
  document.body.append(modal);
  modal.querySelector("#sd-close").onclick = () => modal.remove();
  await mountPlanner(modal.querySelector("#sd-body"), {
    sourceBody: () => ({ source: { kind: "items", items } }), defaultDest, overwrite, replace, mode,
    onStarted: (job) => { modal.remove(); onStarted?.(job); },
  });
}
