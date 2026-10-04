// The conversion dialog: choose where the result goes, see exactly what would be written, then start.
import { api, el, esc, fmtBytes, toast } from "./util.js";

export const OPS = {
  iso_to_god: { label: "Convert to GOD", dest: ["god"], blurb: "Games on Demand folder, ready to copy to the console" },
  god_to_iso: { label: "Convert to ISO", dest: ["iso"], blurb: "A single disc image file" },
  extract: { label: "Unpack to folder", dest: null, blurb: "The game's files in a plain folder" },
  create: { label: "Create ISO from a folder", dest: ["iso"], blurb: "Pack a game folder into a disc image" },
};

/**
 * Open the dialog.
 * opts: { op, ops (choices), items: [{library_id, item_id}], sourceFolder, title, onStarted(jobId) }
 */
export async function openConvert(opts) {
  const [libs, settings] = await Promise.all([api("/api/libraries"), api("/api/settings")]);
  const ops = opts.ops || [opts.op];
  const modal = el(`
    <div class="modal-bg"><div class="modal wide" style="height:auto;max-height:92vh">
      <header>${esc(opts.title || "Convert")}</header>
      <div class="body" style="display:flex;flex-direction:column;gap:12px;padding-top:14px">
        ${ops.length > 1 ? `<label class="field">What to do<select id="cv-op">${ops.map((o) => `<option value="${o}">${esc(OPS[o].label)}: ${esc(OPS[o].blurb)}</option>`).join("")}</select></label>` : ""}
        <div class="row">
          <label class="field grow">Save into library<select id="cv-lib"></select></label>
          <label class="field grow">Folder<select id="cv-path"></select></label>
        </div>
        <div class="row" id="cv-opts">
          <label class="field" id="cv-layout-wrap">GOD folder layout<select id="cv-layout">
            <option value="name_titleid">Game Name / TitleID (tidy)</option><option value="titleid">TitleID only</option></select></label>
          <label class="check"><input type="checkbox" id="cv-over"><span>Replace existing<small>Only the item being replaced is touched, and only after the new one is complete.</small></span></label>
        </div>
        <div id="cv-plan" class="muted">Checking…</div>
      </div>
      <footer><span class="muted grow" id="cv-sum"></span><button class="btn" id="cv-cancel">Cancel</button><button class="btn primary" id="cv-go" disabled>Start</button></footer>
    </div></div>`);
  document.body.append(modal);
  const q = (s) => modal.querySelector(s);
  q("#cv-layout").value = settings.god_layout;
  let op = ops[0], seq = 0, plan = null;

  const writableLibs = () => {
    const kinds = OPS[op].dest;
    return libs.filter((l) => (!kinds || kinds.includes(l.kind)) && l.paths.some((p) => p.writable));
  };
  const drawDest = () => {
    const ls = writableLibs();
    q("#cv-lib").innerHTML = ls.length ? ls.map((l) => `<option value="${l.id}">${esc(l.name)}</option>`).join("") : `<option value="">No suitable library</option>`;
    drawPaths();
  };
  const drawPaths = () => {
    const l = libs.find((x) => x.id === +q("#cv-lib").value);
    const ps = (l?.paths || []).filter((p) => p.writable);
    q("#cv-path").innerHTML = ps.length ? ps.map((p) => `<option value="${p.id}">${esc(p.label || p.path)} — ${p.health.online ? fmtBytes(p.health.free) + " free" : "offline"}</option>`).join("")
      : `<option value="">No writable folder</option>`;
  };
  const request = () => ({
    op,
    items: opts.items || [],
    source_folder: opts.sourceFolder,
    dest_library_id: +q("#cv-lib").value || 0,
    dest_path_id: +q("#cv-path").value || 0,
    layout: q("#cv-layout").value,
    overwrite: q("#cv-over").checked,
  });

  async function refresh() {
    q("#cv-layout-wrap").classList.toggle("hidden", op !== "iso_to_god");
    const my = ++seq;
    const r = request();
    q("#cv-go").disabled = true;
    if (!r.dest_library_id || !r.dest_path_id) {
      q("#cv-plan").innerHTML = `<div class="notice warn">Choose a library with a writable folder. (Tick <b>Writable</b> for a folder on its library page.)</div>`;
      return;
    }
    q("#cv-plan").innerHTML = `<span class="muted">Checking…</span>`;
    try {
      plan = await api("/api/convert/plan", { body: r });
      if (my !== seq) return;
      q("#cv-plan").innerHTML = renderPlan(plan);
      q("#cv-sum").textContent = `${plan.entries.length} item${plan.entries.length === 1 ? "" : "s"} · about ${fmtBytes(plan.total_out)} · ${fmtBytes(plan.free)} free`;
      q("#cv-go").disabled = !plan.ok;
    } catch (e) {
      if (my === seq) q("#cv-plan").innerHTML = `<div class="notice err">${esc(e.message)}</div>`;
    }
  }

  q("#cv-op")?.addEventListener("change", (e) => { op = e.target.value; drawDest(); refresh(); });
  q("#cv-lib").onchange = () => { drawPaths(); refresh(); };
  q("#cv-path").onchange = refresh;
  q("#cv-layout").onchange = refresh;
  q("#cv-over").onchange = refresh;
  q("#cv-cancel").onclick = () => modal.remove();
  q("#cv-go").onclick = async () => {
    q("#cv-go").disabled = true;
    try {
      const { job } = await api("/api/convert/start", { body: request() });
      modal.remove();
      toast("Started", "ok");
      opts.onStarted?.(job);
    } catch (e) {
      q("#cv-plan").insertAdjacentHTML("afterbegin", `<div class="notice err" style="margin-bottom:8px">${esc(e.message).replace(/\n/g, "<br>")}</div>`);
      q("#cv-go").disabled = false;
    }
  };
  drawDest();
  refresh();
}

function renderPlan(p) {
  const top = p.problems.map((x) => `<div class="notice err" style="margin-bottom:8px">${esc(x)}</div>`).join("");
  return `${top}<div class="muted" style="margin-bottom:6px">Into <b>${esc(p.dest_label)}</b> (<span class="mono">${esc(p.dest_root)}</span>)</div>
    <table><thead><tr><th>Game</th><th>Will be written to</th><th class="num">Size</th></tr></thead><tbody>
    ${p.entries.map((e) => `<tr>
      <td>${esc(e.name)}${e.discs > 1 ? ` <span class="badge">disc ${e.disc ?? "?"}/${e.discs}</span>` : ""}
        <div class="mono muted">${esc(e.title_id || "")}</div></td>
      <td><div class="mono" style="word-break:break-all">${esc(e.output)}</div>
        ${e.problems.map((x) => `<div style="color:var(--err)">⛔ ${esc(x)}</div>`).join("")}
        ${e.warnings.map((x) => `<div style="color:var(--warn)">⚠ ${esc(x)}</div>`).join("")}</td>
      <td class="num">${fmtBytes(e.bytes_out)}</td></tr>`).join("")}
    </tbody></table>`;
}
