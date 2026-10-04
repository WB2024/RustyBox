import { api, el, esc, fmtBytes, toast } from "../util.js";

export const title = "Games";

const FILTERS = [
  ["", "All", () => true],
  ["duplicates", "Duplicates", (g) => g.duplicate],
  ["both", "ISO and GOD", (g) => g.has_iso && g.has_god],
  ["iso_only", "ISO only", (g) => g.has_iso && !g.has_god],
  ["god_only", "GOD only", (g) => g.has_god && !g.has_iso],
  ["unknown", "Unknown title", (g) => !g.name && !g.info],
];
const SORTS = [
  ["name", "Name", (a, b) => label(a).localeCompare(label(b), undefined, { sensitivity: "base" })],
  ["size", "Largest first", (a, b) => b.bytes - a.bytes],
  ["year", "Newest first", (a, b) => (b.info?.release || 0) - (a.info?.release || 0)],
];
const label = (g) => g.info?.name || g.name || g.title_id;
const PAGE = 150;

const discText = (c) => (c.discs > 1 ? ` · disc ${c.disc ?? "?"} of ${c.discs}` : "");
const year = (t) => (t ? new Date(t * 1000).getUTCFullYear() : "");
const coverSrc = (info) => (info?.cover ? `/api/covers/${encodeURIComponent(info.cover)}` : "");

const thumb = (g, size = 44) => g.info?.cover
  ? `<img src="${coverSrc(g.info)}" alt="" loading="lazy" style="width:${size}px;height:${Math.round(size * 1.4)}px;object-fit:cover;border-radius:4px;border:1px solid var(--line);display:block">`
  : `<div style="width:${size}px;height:${Math.round(size * 1.4)}px;border-radius:4px;border:1px dashed var(--line);display:grid;place-items:center;color:var(--muted)">🎮</div>`;

export async function render(root, ctx) {
  const P = ctx.params;
  let q = P.get("q") || "", only = P.get("only") || "", sort = P.get("sort") || "name", view = P.get("view") || "list", shown = PAGE, timer, all = [];
  if (!FILTERS.some((f) => f[0] === only)) only = "";
  if (!SORTS.some((x) => x[0] === sort)) sort = "name";
  const igdb = (await api("/api/igdb/status")).configured;
  root.innerHTML = `
    <div class="page-head row"><div class="grow"><h1>Games</h1>
      <div class="muted">Every ISO and GOD game across all your libraries, matched by title ID, with every place a copy lives.</div></div>
      <button class="btn" id="g-fetch" ${igdb ? "" : "disabled"} title="${igdb ? "Look up cover art and details for games that don't have them yet" : "Add your IGDB credentials in Settings first"}">Fetch game info</button></div>
    ${igdb ? "" : `<div class="notice" style="margin-bottom:12px">Cover art and details come from IGDB. <a href="#settings">Add your Twitch credentials in Settings</a> to turn them on.</div>`}
    <div class="card sticky-bar"><div class="row">
      <input type="search" id="g-q" class="grow" placeholder="Search name, title ID or file…" value="${esc(q)}" aria-label="Search games">
      <select id="g-sort" aria-label="Sort">${SORTS.map(([v, l]) => `<option value="${v}" ${v === sort ? "selected" : ""}>${l}</option>`).join("")}</select>
      <div class="seg" role="group" aria-label="View"><button data-view="list" class="${view === "list" ? "on" : ""}">☰ List</button><button data-view="grid" class="${view === "grid" ? "on" : ""}">▦ Covers</button></div></div>
      <div class="fchips" id="g-chips" style="margin-top:10px"></div></div>
    <div class="card"><div id="g-list"></div><div class="muted" id="g-count" style="margin-top:8px"></div></div>
    ${igdb ? `<div class="muted" style="font-size:12.5px;margin-top:8px">Game information and cover art from <a href="https://www.igdb.com" target="_blank" rel="noopener">IGDB.com</a>.</div>` : ""}`;

  const places = (g) => {
    const seen = new Map();
    for (const c of g.copies) {
      const k = `${c.library_id}|${c.kind}`;
      if (!seen.has(k)) seen.set(k, { kind: c.kind, lib: c.library_name, n: 0, off: false });
      const p = seen.get(k); p.n++; p.off ||= !c.available;
    }
    return [...seen.values()];
  };
  const placeBadges = (g) => places(g).map((p) => `<span class="badge" title="${esc(p.lib)}${p.off ? " (unavailable)" : ""}">${p.kind.toUpperCase()} · ${esc(p.lib)}${p.n > 1 ? ` ×${p.n}` : ""}${p.off ? " ⚠" : ""}</span>`).join(" ");
  const discs = (g) => Math.max(0, ...g.copies.map((c) => c.discs || 0));

  function detail(g) {
    const i = g.info;
    return `<div class="row" style="align-items:flex-start;gap:16px;padding:8px 0">
      <div>${thumb(g, 110)}</div>
      <div class="grow" style="min-width:260px">
        ${i ? `<div><b>${esc(i.name)}</b> ${i.release ? `<span class="muted">(${year(i.release)})</span>` : ""}${i.status === "manual" ? ` <span class="badge">chosen by hand</span>` : ""}</div>
          <div class="muted" style="margin:2px 0 6px">${[i.genres.join(", "), i.developers.length ? "by " + i.developers.join(", ") : "", i.publishers.length ? "published by " + i.publishers.slice(0, 2).join(", ") : "", i.rating ? "rated " + Math.round(i.rating) + "/100" : ""].filter(Boolean).map(esc).join(" · ")}</div>
          ${i.summary ? `<div style="max-width:70ch">${esc(i.summary)}</div>` : ""}
          ${i.url ? `<div style="margin-top:6px"><a href="${esc(i.url)}" target="_blank" rel="noopener">View on IGDB</a></div>` : ""}`
          : `<div class="muted">${igdb ? "No information yet." : "Information comes from IGDB (see Settings)."}</div>`}
        <div class="row" style="margin-top:8px;gap:8px">
          ${igdb ? `<button class="btn small" data-act="again" data-t="${g.title_id}">Look up again</button><button class="btn small" data-act="choose" data-t="${g.title_id}" data-n="${esc(g.name || "")}">${i ? "Choose a different match…" : "Choose a match…"}</button>` : ""}
          ${i ? `<button class="btn small" data-act="clear" data-t="${g.title_id}">Remove info</button>` : ""}</div>
      </div>
      <div style="min-width:320px;flex:1">
        ${g.copies.map((c) => `<div class="row" style="padding:3px 0;gap:10px">
          <span class="badge">${c.kind.toUpperCase()}</span>
          <a href="#library/${c.library_id}">${esc(c.library_name)}</a><span class="muted">${esc(c.path_label)}</span>
          <span class="mono grow" style="overflow:hidden;text-overflow:ellipsis">${esc(c.relpath)}${esc(discText(c))}</span>
          ${c.available ? "" : `<span class="badge err">unavailable</span>`}<span class="muted">${fmtBytes(c.size)}</span></div>`).join("")}
      </div></div>`;
  }

  let games = [], openIds = new Set();
  const bindActs = (scope) => scope.querySelectorAll("[data-act]").forEach((b) => (b.onclick = (e) => { e.stopPropagation(); act(b.dataset.act, b.dataset.t, b.dataset.n); }));

  function sync() {
    const p = new URLSearchParams();
    if (q) p.set("q", q);
    if (only) p.set("only", only);
    if (sort !== "name") p.set("sort", sort);
    if (view !== "list") p.set("view", view);
    // replaceState doesn't fire hashchange, so the page isn't rebuilt under the user's cursor.
    history.replaceState(null, "", `#games${p.toString() ? "?" + p : ""}`);
  }

  function chips() {
    root.querySelector("#g-chips").innerHTML = FILTERS.map(([v, l, fn]) => {
      const n = all.filter(fn).length;
      return v && !n && v !== only ? "" : `<button class="fchip ${v === only ? "on" : ""}" data-only="${v}">${l}<small>${n}</small></button>`;
    }).join("");
    root.querySelectorAll("[data-only]").forEach((b) => (b.onclick = () => { only = b.dataset.only; shown = PAGE; sync(); draw(); chips(); }));
  }

  function visible() {
    const fn = FILTERS.find((f) => f[0] === only)[2], cmp = SORTS.find((x) => x[0] === sort)[2];
    return all.filter(fn).sort(cmp);
  }

  function draw() {
    games = visible();
    const list = root.querySelector("#g-list"), slice = games.slice(0, shown);
    if (!games.length) {
      list.innerHTML = `<div class="empty"><span class="ico">🎮</span>${q || only ? "Nothing matches." : "No games found yet. Add ISO or GOD libraries and scan them."}</div>`;
    } else if (view === "grid") {
      list.innerHTML = `<div class="ggrid">${slice.map((g, i) => `<div class="gcard" data-i="${i}" tabindex="0" role="button" aria-label="${esc(label(g))}"><div class="art">${g.info?.cover ? `<img src="${coverSrc(g.info)}" alt="" loading="lazy">` : "🎮"}</div>
        <div class="meta"><div class="t">${esc(label(g))}</div><div class="muted" style="font-size:12px">${g.info?.release ? year(g.info.release) + " · " : ""}${fmtBytes(g.bytes)}</div>
        <div style="margin-top:4px">${g.duplicate ? `<span class="badge accent">duplicate</span> ` : ""}${places(g).map((p) => `<span class="badge">${p.kind.toUpperCase()}</span>`).join(" ")}</div></div></div>`).join("")}</div>`;
      list.querySelectorAll(".gcard").forEach((c) => {
        const open = () => popup(games[+c.dataset.i]);
        c.onclick = open;
        c.onkeydown = (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); open(); } };
      });
    } else {
      list.innerHTML = `<table><thead><tr><th style="width:56px"></th><th>Game</th><th>Title ID</th><th>Where</th><th class="num">Size</th></tr></thead><tbody>
        ${slice.map((g, i) => `
          <tr class="g-row" data-i="${i}" style="cursor:pointer" tabindex="0" aria-expanded="${openIds.has(g.title_id)}"><td>${thumb(g)}</td>
            <td>${g.info ? esc(g.info.name) : g.name ? esc(g.name) : `<span class="muted">Unknown title</span>`}${g.info?.release ? ` <span class="muted">(${year(g.info.release)})</span>` : ""}
              ${g.duplicate ? ` <span class="badge accent" title="The same disc is in one library more than once">duplicate</span>` : ""}
              ${discs(g) > 1 ? ` <span class="badge">${discs(g)} discs</span>` : ""}
              ${g.info?.genres?.length ? `<div class="muted" style="font-size:12.5px">${esc(g.info.genres.slice(0, 3).join(", "))}</div>` : ""}</td>
            <td class="mono">${esc(g.title_id)}</td>
            <td>${placeBadges(g)}</td>
            <td class="num">${fmtBytes(g.bytes)}</td></tr>
          <tr class="g-detail ${openIds.has(g.title_id) ? "" : "hidden"}" data-i="${i}"><td colspan="5" style="background:var(--panel2)">${detail(g)}</td></tr>`).join("")}
        </tbody></table>`;
      const toggle = (r) => {
        const d = list.querySelector(`.g-detail[data-i="${r.dataset.i}"]`), t = games[+r.dataset.i].title_id;
        d.classList.toggle("hidden");
        if (d.classList.contains("hidden")) openIds.delete(t); else openIds.add(t);
        r.setAttribute("aria-expanded", String(!d.classList.contains("hidden")));
      };
      list.querySelectorAll(".g-row").forEach((r) => {
        r.onclick = () => toggle(r);
        r.onkeydown = (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(r); } };
      });
      bindActs(list);
    }
    root.querySelector("#g-count").innerHTML = `${slice.length < games.length ? `Showing ${slice.length} of ${games.length} · <a href="#" id="g-more">show ${Math.min(PAGE, games.length - slice.length)} more</a>` : `${games.length} game${games.length === 1 ? "" : "s"}`}${all.length !== games.length ? ` (of ${all.length})` : ""}`;
    root.querySelector("#g-more")?.addEventListener("click", (e) => { e.preventDefault(); shown += PAGE; draw(); });
  }

  // A game's details in a dialog, for the cover view.
  function popup(g) {
    const m = el(`<div class="modal-bg"><div class="modal wide" style="height:auto"><header>${esc(label(g))}</header><div class="body" style="padding-top:12px">${detail(g)}</div><footer><button class="btn" id="gp-close">Close</button></footer></div></div>`);
    document.body.append(m);
    m.onclick = (e) => { if (e.target === m) m.remove(); };
    m.querySelector("#gp-close").onclick = () => m.remove();
    m.querySelectorAll("[data-act]").forEach((b) => (b.onclick = (e) => { e.stopPropagation(); m.remove(); act(b.dataset.act, b.dataset.t, b.dataset.n); }));
  }

  async function load() {
    const d = await api(`/api/games?q=${encodeURIComponent(q)}`);
    all = d.games;
    chips();
    draw();
  }

  async function act(what, tid, name) {
    try {
      if (what === "again") { toast("Looking it up…"); await api(`/api/games/${tid}/info/refresh`, { method: "POST" }); }
      if (what === "clear") await api(`/api/games/${tid}/info`, { method: "DELETE" });
      if (what === "choose") return chooseMatch(tid, name);
      await load();
    } catch (e) { toast(e.message, "err"); }
  }

  function chooseMatch(tid, name) {
    const modal = el(`<div class="modal-bg"><div class="modal"><header>Choose the matching game</header>
      <div class="body" style="padding-top:14px"><div class="row"><input type="search" id="cm-q" class="grow" value="${esc(name)}"><button class="btn primary" id="cm-go">Search</button></div>
      <div id="cm-res" style="margin-top:10px" class="muted">Searching…</div></div>
      <footer><button class="btn" id="cm-close">Cancel</button></footer></div></div>`);
    document.body.append(modal);
    const q2 = (s) => modal.querySelector(s);
    const search = async () => {
      q2("#cm-res").textContent = "Searching…";
      try {
        const r = await api("/api/igdb/search", { body: { q: q2("#cm-q").value } });
        q2("#cm-res").innerHTML = r.length ? r.map((c) => `<div class="row" style="padding:6px 0;border-bottom:1px solid var(--line);gap:12px">
          ${c.cover_url ? `<img src="${esc(c.cover_url)}" alt="" style="width:42px;height:56px;object-fit:cover;border-radius:4px">` : `<div style="width:42px;height:56px"></div>`}
          <div class="grow"><b>${esc(c.name)}</b> <span class="muted">${year(c.release)}</span><div class="muted" style="font-size:12.5px">${esc([c.developers.join(", "), c.genres.join(", ")].filter(Boolean).join(" · "))}</div></div>
          <button class="btn small primary" data-id="${c.id}">Use this</button></div>`).join("") : `<div class="muted">No Xbox 360 games found for that name.</div>`;
        modal.querySelectorAll("[data-id]").forEach((b) => (b.onclick = async () => {
          try { await api(`/api/games/${tid}/info`, { method: "PUT", body: { igdb_id: +b.dataset.id } }); modal.remove(); await load(); toast("Saved", "ok"); }
          catch (e) { toast(e.message, "err"); }
        }));
      } catch (e) { q2("#cm-res").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; }
    };
    q2("#cm-go").onclick = search;
    q2("#cm-q").onkeydown = (e) => { if (e.key === "Enter") search(); };
    q2("#cm-close").onclick = () => modal.remove();
    q2("#cm-q").focus();
    q2("#cm-q").select(); // typing replaces the game's name rather than going in front of it
    search();
  }

  root.querySelector("#g-q").oninput = (e) => { q = e.target.value; shown = PAGE; clearTimeout(timer); timer = setTimeout(() => { sync(); load(); }, 250); };
  root.querySelector("#g-sort").onchange = (e) => { sort = e.target.value; sync(); draw(); };
  root.querySelectorAll("[data-view]").forEach((b) => (b.onclick = () => {
    view = b.dataset.view; sync();
    root.querySelectorAll("[data-view]").forEach((x) => x.classList.toggle("on", x === b));
    draw();
  }));
  root.querySelector("#g-fetch").onclick = async () => {
    try { const { job } = await api("/api/igdb/fetch", { body: { retry_unmatched: true } }); ctx.openJob(job); }
    catch (e) { toast(e.message, "err"); }
  };
  await load();
}
