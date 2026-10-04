// Tidy a library's folders into the standard layout: the same for a library on this server and
// for the Xbox's drive, so the two end up as mirror images of each other.
import { api, el, esc, toast } from "./util.js";

export async function tidyDialog(lib, ctx) {
  const plan = await api(`/api/libraries/${lib.id}/tidy/plan`, { body: {} }).catch((e) => ({ error: e.message }));
  const todo = plan.moves?.filter((m) => !m.problem) || [];
  const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:90vh"><header>Tidy ${esc(lib.name)}</header><div class="body" style="padding-top:14px">
    ${plan.error ? `<div class="notice err">${esc(plan.error)}</div>` : `
    <p class="muted">Renames each game's folder to the standard layout (<b>${plan.layout === "titleid" ? "Title ID" : "Game Name / Title ID"}</b>; change it in Settings), and moves add-ons and title updates into the folders set aside for them. Use the same layout on your library and on the Xbox's drive and a sync leaves them as mirror images. Folders are renamed in place: nothing is copied, no file is ever overwritten, and only the old folders this empties are removed. An add-on or update joins the folder of its game when one is already there.</p>
    ${plan.moves.length ? `<table><thead><tr><th>Game</th><th>From</th><th>To</th></tr></thead><tbody>${plan.moves.map((m) => `<tr><td>${esc(m.name)}<div class="mono muted">${esc(m.title_id)}</div></td>
      <td class="mono" style="word-break:break-all">${esc(m.from)}</td><td class="mono" style="word-break:break-all">${m.to_path_id !== m.path_id ? `<span class="badge">${esc(m.to_path_label)}</span> ` : ""}${esc(m.to)}${m.content_kind !== "game" ? ` <span class="badge">${m.content_kind === "dlc" ? "add-on" : "title update"}</span>` : ""}${m.merge ? `<div class="muted">joins the folder that is already there; no file is overwritten</div>` : ""}${m.problem ? `<div style="color:var(--err)">⛔ ${esc(m.problem)}</div>` : ""}</td></tr>`).join("")}</tbody></table>`
      : `<div class="notice">Everything is already tidy.</div>`}
    <div class="muted" style="margin-top:8px">${plan.unchanged} already right${plan.skipped ? ` · ${plan.skipped} skipped (title ID unreadable)` : ""}</div>`}
    </div><footer><span class="grow"></span><button class="btn" id="td-cancel">Close</button><button class="btn primary" id="td-go" ${todo.length ? "" : "disabled"}>Rename ${todo.length} folder${todo.length === 1 ? "" : "s"}</button></footer></div></div>`);
  document.body.append(modal);
  modal.querySelector("#td-cancel").onclick = () => modal.remove();
  modal.querySelector("#td-go").onclick = async () => {
    try { const { job } = await api(`/api/libraries/${lib.id}/tidy/start`, { body: {} }); modal.remove(); ctx.openJob(job); }
    catch (e) { toast(e.message, "err"); }
  };
}
