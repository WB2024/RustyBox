// Quick find: jump to a page, a library or a game from anywhere (Ctrl+K, or /).
import { api, el, esc } from "./util.js";

let libsCache = null;

export async function open(pages) {
  if (document.querySelector(".qf")) return;
  const modal = el(`<div class="modal-bg"><div class="modal qf"><input type="search" id="qf-q" placeholder="Go to a page, library or game…" autocomplete="off" spellcheck="false"><div class="qf-list" id="qf-list" role="listbox"></div></div></div>`);
  document.body.append(modal);
  modal.onclick = (e) => { if (e.target === modal) modal.remove(); };
  const input = modal.querySelector("#qf-q"), list = modal.querySelector("#qf-list");
  input.focus();
  let items = [], sel = 0, seq = 0, timer;
  const lib = (libsCache ||= api("/api/libraries").catch(() => []));
  libsCache = null; // only reuse it for this one opening

  const go = (it) => { modal.remove(); location.hash = it.href; };
  const draw = () => {
    let last = "";
    list.innerHTML = items.length ? items.map((it, i) => {
      const head = it.group !== last ? `<div class="qf-head">${esc(it.group)}</div>` : "";
      last = it.group;
      return `${head}<div class="qf-item ${i === sel ? "sel" : ""}" role="option" data-i="${i}">${esc(it.icon || "")} <span>${esc(it.label)}</span><span class="k">${esc(it.hint || "")}</span></div>`;
    }).join("") : `<div class="empty">Nothing found</div>`;
    list.querySelector(".sel")?.scrollIntoView({ block: "nearest" });
  };
  const score = (label, q) => {
    const l = label.toLowerCase();
    if (!q) return 1;
    if (l.startsWith(q)) return 3;
    if (l.includes(q)) return 2;
    let i = 0;
    for (const c of l) if (c === q[i]) i++;
    return i === q.length ? 1 : 0;
  };
  async function search() {
    const q = input.value.trim().toLowerCase(), mine = ++seq;
    const base = pages.map((p) => ({ group: "Pages", icon: p.icon, label: p.label, href: p.id, s: score(p.label, q) })).filter((x) => x.s).sort((a, b) => b.s - a.s).slice(0, q ? 6 : 20);
    // Pages show at once; libraries and games follow as soon as they arrive (checking a drive that is
    // offline can take a few seconds, and typing should not wait for it).
    items = base;
    sel = 0;
    draw();
    const gamesP = q.length >= 2
      ? api(`/api/games?limit=8&q=${encodeURIComponent(q)}`).then((d) => d.games.map((g) => ({ group: "Games", icon: "🎮", label: g.info?.name || g.name || g.title_id, hint: g.title_id, href: `games?q=${encodeURIComponent(g.title_id)}`, s: 1 }))).catch(() => [])
      : Promise.resolve([]);
    const games = await gamesP;
    if (mine !== seq) return;
    items = [...base, ...games];
    draw();
    const libs = ((await lib) || []).map((l) => ({ group: "Libraries", icon: l.icon || "📁", label: l.name, hint: l.kind_label, href: `library/${l.id}`, s: score(l.name, q) })).filter((x) => x.s && q);
    if (mine !== seq) return;
    items = [...base, ...libs.slice(0, 6), ...games];
    draw();
  }
  input.oninput = () => { clearTimeout(timer); timer = setTimeout(search, 120); };
  input.onkeydown = (e) => {
    if (e.key === "ArrowDown") { e.preventDefault(); sel = Math.min(items.length - 1, sel + 1); draw(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); sel = Math.max(0, sel - 1); draw(); }
    else if (e.key === "Enter") { e.preventDefault(); if (items[sel]) go(items[sel]); }
  };
  list.onclick = (e) => { const r = e.target.closest(".qf-item"); if (r) go(items[+r.dataset.i]); };
  search();
}
