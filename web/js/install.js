// Install a file or folder from a library onto the console.
import { api, el, esc, toast } from "./util.js";

export async function installLocal(libraryId, item, ctx) {
  const consoles = await api("/api/consoles");
  if (!consoles.length) { toast("Add your console first (Console in the sidebar)", "err"); location.hash = "console"; return; }
  const modal = el(`<div class="modal-bg"><div class="modal" style="height:auto;max-height:90vh"><header>Install ${esc(item.name)} on the console</header><div class="body" style="padding-top:14px;display:flex;flex-direction:column;gap:10px">
    <label class="field">Console<select id="il-c">${consoles.map((c) => `<option value="${c.id}">${esc(c.name)}</option>`).join("")}</select></label>
    <label class="field">Folder on the console<input type="text" id="il-dest" placeholder="Hdd1/Arisen Studio/Mods/Name" spellcheck="false"><small class="muted">Filled in for you from the kind of library. Change it if you like; a zip is unpacked into it.</small></label>
    <div id="il-plan" class="muted">Working out where the files go…</div></div>
    <footer><button class="btn" id="il-no">Close</button><button class="btn primary" id="il-go" disabled>Install</button></footer></div></div>`);
  document.body.append(modal);
  const m = (s) => modal.querySelector(s);
  let first = true;
  const body = () => ({ console_id: +m("#il-c").value, local: { library_id: libraryId, item_id: item.id, dest: m("#il-dest").value.trim() || null } });
  async function plan() {
    m("#il-go").disabled = true;
    try {
      const r = await api("/api/content/install/plan", { body: body() });
      if (first || !m("#il-dest").value) m("#il-dest").value = r.dest || "";
      first = false;
      const p = r.plan;
      m("#il-plan").innerHTML = `${p.problems.map((x) => `<div class="notice err">${esc(x)}</div>`).join("")}<table><tbody>${p.files.map((f) => `<tr><td>${esc(f.name)}<div class="muted" style="font-size:12.5px">from ${esc(f.from)}</div></td><td class="mono" style="word-break:break-all">${f.targets.map(esc).join("<br>")}</td></tr>`).join("")}</tbody></table>${p.warnings.map((x) => `<div style="color:var(--warn);margin-top:6px">⚠ ${esc(x)}</div>`).join("")}`;
      m("#il-go").disabled = !p.ok;
    } catch (e) { m("#il-plan").innerHTML = `<div class="notice err">${esc(e.message).replace(/\n/g, "<br>")}</div>`; }
  }
  let t; m("#il-dest").oninput = () => { clearTimeout(t); t = setTimeout(plan, 350); };
  m("#il-c").onchange = plan; m("#il-no").onclick = () => modal.remove();
  m("#il-go").onclick = async () => { try { const { job } = await api("/api/content/install/start", { body: body() }); modal.remove(); ctx.openJob(job); } catch (e) { toast(e.message, "err"); } };
  plan();
}
