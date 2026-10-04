// Find add-ons, title updates and trainers that are in the wrong folder on an Xbox drive, show
// exactly where each would go, and move them only when asked.
import { api, el, esc, toast } from "./util.js";

const KIND = { dlc: "add-on (DLC)", update: "title update", patches: "title update", trainer: "trainer", god: "game (GOD)", iso: "game (ISO)", saves: "saves", archive: "archive", mixed: "mixed" };
const size = (n) => (n >= 1e9 ? (n / 1e9).toFixed(1) + " GB" : n >= 1e6 ? (n / 1e6).toFixed(1) + " MB" : Math.max(1, Math.round(n / 1e3)) + " KB");

export async function misplacedDialog(lib, ctx) {
  const plan = await api(`/api/libraries/${lib.id}/misplaced/plan`, { body: {} }).catch((e) => ({ error: e.message }));
  const todo = plan.moves?.filter((m) => !m.problem) || [];
  const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:90vh"><header>Fix misplaced items on ${esc(lib.name)}</header><div class="body" style="padding-top:14px">
    ${plan.error ? `<div class="notice err">${esc(plan.error)}</div>` : `
    <p class="muted">${lib.kind === "god" && lib.paths.some((p) => p.role === "content") ? "" : `On a library, each game, add-on, title update and save found in the wrong place is sent to the <b>primary</b> folder of the library for its kind (set it with Make primary on a library's folder). Folders it can't recognise are left alone. `}The console only uses add-ons and title updates that are in <b>Content/0000000000000000/&lt;TitleID&gt;/&lt;type&gt;</b>, so any it finds in Games, or in a stray folder inside Content, are listed here. Trainers kept inside a profile's save folder go to your Trainers folder. Each is moved (never copied or overwritten) and only the folders this empties are removed. Spare copies are never deleted. Saves and games are not touched.</p>
    ${plan.moves.length ? `<table><thead><tr><th>Item</th><th>From</th><th>To</th></tr></thead><tbody>${plan.moves.map((m) => `<tr><td>${esc(m.name)}<div class="mono muted">${esc(m.title_id)} · ${size(m.size)}</div></td>
      <td class="mono" style="word-break:break-all"><span class="badge">${esc(m.from_label)}</span> ${esc(m.from)}</td>
      <td class="mono" style="word-break:break-all">${m.to_label ? `<span class="badge">${esc(m.to_label)}</span> ` : ""}${esc(m.to)} <span class="badge">${KIND[m.kind] || m.kind}</span>${m.problem ? `<div style="color:var(--err)">${m.duplicate ? "⚠" : "⛔"} ${esc(m.problem)}</div>` : ""}</td></tr>`).join("")}</tbody></table>`
      : `<div class="notice">Nothing looks misplaced.</div>`}`}
    </div><footer><span class="grow"></span><button class="btn" id="mp-cancel">Close</button><button class="btn primary" id="mp-go" ${todo.length ? "" : "disabled"}>Move ${todo.length} item${todo.length === 1 ? "" : "s"}</button></footer></div></div>`);
  document.body.append(modal);
  modal.querySelector("#mp-cancel").onclick = () => modal.remove();
  modal.querySelector("#mp-go").onclick = async () => {
    try { const { job } = await api(`/api/libraries/${lib.id}/misplaced/start`, { body: {} }); modal.remove(); ctx.openJob(job); }
    catch (e) { toast(e.message, "err"); }
  };
}
