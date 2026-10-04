import { api, el, esc, fmtBytes, toast } from "../util.js";
import { chooseFolder } from "../picker.js";

export const title = "Libraries";

const SECTIONS = [
  ["Games", "Disc images and Games on Demand folders", ["iso", "god"]],
  ["Add-ons, updates and saves", "Things that go with games", ["dlc", "patches", "saves", "cheats"]],
  ["Tools and extras", "Mods, trainers, homebrew and emulators", ["mods", "trainers", "homebrew", "emulators"]],
  ["Other", "Anything else you keep together", []],
];

const card = (l) => `
  <div class="lib-card" data-id="${l.id}" tabindex="0" role="link" aria-label="${esc(l.name)}"><div class="meta">
    <div class="row"><span style="font-size:26px">${l.icon || "📁"}</span><div class="grow"><div class="t">${esc(l.name)}</div>
      <div class="muted">${esc(l.kind_label || l.kind)}</div></div></div>
    <div style="margin:10px 0"><b>${l.items.toLocaleString()}</b> item${l.items === 1 ? "" : "s"} · ${fmtBytes(l.bytes)}</div>
    ${l.paths.map((p, i) => `<div class="row" style="gap:6px;margin-top:4px"><span class="mono grow" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${esc(p.path)}">${p.remote_url ? "🖥 " : ""}${esc(p.label || p.path)}</span>${i === 0 && l.paths.length > 1 ? `<span class="badge" title="Primary folder">primary</span>` : ""}${healthBadge(p.health)}</div>`).join("") || `<div class="muted">No folders yet</div>`}
  </div></div>`;

const healthBadge = (h) => h.online ? `<span class="badge ok">online</span>` : `<span class="badge err" title="${esc(h.problem || "")}">offline</span>`;

export async function render(root, ctx) {
  const [libs, kinds] = await Promise.all([api("/api/libraries"), api("/api/library-kinds")]);
  root.innerHTML = `
    <div class="page-head row"><div class="grow"><h1>Libraries</h1>
      <div class="muted">Each library gathers folders from any disk or share into one place.</div></div>
      <button class="btn primary" id="new">＋ New library</button></div>
    ${libs.some((l) => l.paths.some((p) => p.remote_url)) ? "" : `<div class="notice" style="margin-bottom:14px">Have an Xbox hard drive plugged into another computer? <a href="#drive">Connect it from the Xbox drive page</a> to compare it with your libraries and copy games across.</div>`}
    ${libs.length ? SECTIONS.map(([name, hint, kindsIn]) => {
      const mine = libs.filter((l) => kindsIn.includes(l.kind) || (kindsIn.length === 0 && !SECTIONS.slice(0, -1).some((x) => x[2].includes(l.kind))));
      return mine.length ? `<h2 style="margin:18px 0 2px">${name}</h2><div class="muted" style="margin-bottom:10px;font-size:12.5px">${hint}</div>
        <div class="lib-grid" style="grid-template-columns:repeat(auto-fill,minmax(320px,1fr))">${mine.map(card).join("")}</div>` : "";
    }).join("")
      : `<div class="card"><div class="empty"><span class="ico">📚</span><b>No libraries yet</b><p>Create one, give it a type (ISO games, GOD games, mods…) and add the folders it should cover.</p></div></div>`}`;
  root.querySelectorAll(".lib-card").forEach((c) => {
    const go = () => (location.hash = `library/${c.dataset.id}`);
    c.onclick = go;
    c.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); go(); } };
  });
  root.querySelector("#new").onclick = () => newLibrary(kinds, (id) => (location.hash = `library/${id}`));
}

function newLibrary(kinds, done) {
  const paths = [];
  const modal = el(`
    <div class="modal-bg"><div class="modal">
      <header>New library</header>
      <div class="body" style="display:flex;flex-direction:column;gap:12px;padding-top:14px">
        <label class="field">Name<input type="text" id="nl-name" placeholder="e.g. ISO games"></label>
        <label class="field">Type<select id="nl-kind">${kinds.map((k) => `<option value="${k.id}">${k.icon} ${esc(k.label)} — ${esc(k.blurb)}</option>`).join("")}</select></label>
        <div><div class="muted" style="margin-bottom:6px">Folders (add as many as you like, from any disk)</div>
          <div id="nl-paths"></div><button class="btn small" id="nl-add">＋ Add folder</button></div>
        <div class="notice err hidden" id="nl-err"></div>
      </div>
      <footer><button class="btn" id="nl-cancel">Cancel</button><button class="btn primary" id="nl-ok">Create</button></footer>
    </div></div>`);
  document.body.append(modal);
  const q = (s) => modal.querySelector(s);
  const draw = () => {
    q("#nl-paths").innerHTML = paths.map((p, i) => `<div class="row" style="margin-bottom:6px"><span class="mono grow">${p.remote ? "🖥 " + esc(p.label) + " (" + esc(p.remote.url) + ")" : esc(p.path)}</span>
      <label class="check"><input type="checkbox" data-i="${i}" ${p.writable ? "checked" : ""}> writable</label><button class="btn small" data-rm="${i}">✕</button></div>`).join("");
    q("#nl-paths").querySelectorAll("input[data-i]").forEach((c) => (c.onchange = () => (paths[c.dataset.i].writable = c.checked)));
    q("#nl-paths").querySelectorAll("[data-rm]").forEach((b) => (b.onclick = () => { paths.splice(+b.dataset.rm, 1); draw(); }));
  };
  q("#nl-add").onclick = async () => { const p = await chooseFolder(); if (p) { paths.push(p); draw(); } };
  q("#nl-cancel").onclick = () => modal.remove();
  q("#nl-ok").onclick = async () => {
    try {
      const r = await api("/api/libraries", { body: { name: q("#nl-name").value, kind: q("#nl-kind").value, paths } });
      modal.remove();
      toast("Library created", "ok");
      done(r.id);
    } catch (e) { const b = q("#nl-err"); b.textContent = e.message; b.classList.remove("hidden"); }
  };
}
