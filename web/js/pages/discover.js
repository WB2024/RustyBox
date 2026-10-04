// Discover: browse every Xbox 360 game on IGDB, see similar games, and send one to the wanted list
// or straight to a Usenet download (which then converts to GOD and lands in your library).
import { api, el, esc, toast } from "../util.js";
import { searchDialog } from "../grab-ui.js";

export const title = "Discover";

const YEARS = [["", "Any year"], ["2005-2006", "2005–2006"], ["2007-2008", "2007–2008"], ["2009-2010", "2009–2010"], ["2011-2012", "2011–2012"], ["2013-2014", "2013–2014"], ["2015-2018", "2015 and later"], ["1990-2004", "Before 2005"]];
const SORTS = [["popular", "Most popular"], ["rating", "Best rated"], ["newest", "Newest"], ["oldest", "Oldest"], ["name", "Name (A–Z)"]];
const year = (t) => (t ? new Date(t * 1000).getUTCFullYear() : "");

function badge(s) {
  if (!s || s.state === "none") return "";
  if (s.state === "library") return `<span class="badge ok">✓ In your library</span>`;
  if (s.state === "downloading") return `<span class="badge accent">Downloading${s.progress ? " " + Math.round(s.progress) + "%" : ""}</span>`;
  return `<span class="badge">On your wanted list</span>`;
}

export async function render(root, ctx) {
  const saved = JSON.parse(sessionStorage.getItem("discover") || "{}");
  const f = { q: "", genre: "", years: "", sort: "popular", ...saved };
  let page = 1, timer, token = 0;
  const $ = (s) => root.querySelector(s);

  let genres = [];
  try { genres = (await api("/api/discover/genres")).genres; } catch (e) {
    root.innerHTML = `<div class="page-head"><h1>🧭 Discover</h1></div><div class="notice err">${esc(e.message)}</div>`;
    return;
  }
  root.innerHTML = `
    <div class="page-head"><h1>🧭 Discover</h1><div class="muted">Every Xbox 360 game on IGDB. Open one to see its details and similar games, then add it to your wanted list or download it straight away (Usenet or torrent): it is converted to Games on Demand and put in your library.</div></div>
    <div class="card"><div class="row" style="flex-wrap:wrap;gap:10px;align-items:flex-end">
      <label class="field grow" style="min-width:220px">Search<input type="search" id="d-q" placeholder="Name of a game" value="${esc(f.q)}" autocomplete="off"></label>
      <label class="field">Genre<select id="d-genre"><option value="">All genres</option>${genres.map((g) => `<option value="${g.id}" ${String(g.id) === String(f.genre) ? "selected" : ""}>${esc(g.name)}</option>`).join("")}</select></label>
      <label class="field">Released<select id="d-years">${YEARS.map(([v, l]) => `<option value="${v}" ${v === f.years ? "selected" : ""}>${l}</option>`).join("")}</select></label>
      <label class="field">Sort by<select id="d-sort">${SORTS.map(([v, l]) => `<option value="${v}" ${v === f.sort ? "selected" : ""}>${l}</option>`).join("")}</select></label></div>
      <div class="muted" id="d-note" style="margin-top:6px"></div></div>
    <div id="d-grid" style="display:grid;grid-template-columns:repeat(auto-fill,minmax(170px,1fr));gap:12px;margin-top:12px"></div>
    <div style="text-align:center;margin:14px 0"><button class="btn hidden" id="d-more">Show more</button><span class="muted" id="d-status"></span></div>`;

  const save = () => sessionStorage.setItem("discover", JSON.stringify(f));
  const query = () => `/api/discover?${new URLSearchParams({ ...(f.q ? { q: f.q } : {}), ...(f.genre ? { genre: f.genre } : {}), ...(f.years ? { years: f.years } : {}), sort: f.sort, page })}`;

  function cardHtml(g) {
    return `<div class="card dcard" data-id="${g.id}" style="margin:0;padding:8px;cursor:pointer;display:flex;flex-direction:column;gap:6px">
      <div style="position:relative">${g.cover ? `<img src="${esc(g.cover)}" alt="" loading="lazy" style="width:100%;aspect-ratio:3/4;object-fit:cover;border-radius:6px;display:block;background:var(--line)">` : `<div style="width:100%;aspect-ratio:3/4;background:var(--line);border-radius:6px"></div>`}
        ${g.rating ? `<span class="badge" style="position:absolute;top:6px;right:6px;background:rgba(0,0,0,.72);color:#fff">${Math.round(g.rating)}</span>` : ""}</div>
      <div style="font-weight:600;font-size:13.5px;line-height:1.25">${esc(g.name)}</div>
      <div class="muted" style="font-size:12px">${year(g.year)}${g.genres.length ? " · " + esc(g.genres.slice(0, 2).join(", ")) : ""}</div>
      <div class="status">${badge(g.status)}</div></div>`;
  }

  async function load(append = false) {
    const my = ++token;
    $("#d-status").textContent = "Loading…"; $("#d-more").classList.add("hidden");
    try {
      const r = await api(query());
      if (my !== token) return;
      const grid = $("#d-grid");
      if (!append) grid.innerHTML = "";
      grid.insertAdjacentHTML("beforeend", r.games.map(cardHtml).join(""));
      grid.querySelectorAll(".dcard:not([data-bound])").forEach((c) => { c.dataset.bound = 1; c.onclick = () => detail(+c.dataset.id, refreshStatuses); });
      $("#d-note").textContent = r.relevance ? "Results are ordered by how well they match your search." : "";
      $("#d-status").textContent = grid.children.length ? "" : "No Xbox 360 games match that.";
      $("#d-more").classList.toggle("hidden", !r.has_more);
    } catch (e) { if (my === token) $("#d-status").textContent = e.message; }
  }
  const reload = () => { page = 1; save(); load(false); };

  // Refresh just the status badges of what's on screen (after adding or grabbing something).
  async function refreshStatuses() {
    const cards = [...root.querySelectorAll(".dcard")];
    if (!cards.length) return;
    const pages = Math.max(1, Math.ceil(cards.length / 24));
    const saved = page;
    const byId = new Map();
    for (let p = 1; p <= pages; p++) { page = p; try { (await api(query())).games.forEach((g) => byId.set(g.id, g)); } catch { break; } }
    page = saved;
    cards.forEach((c) => { const g = byId.get(+c.dataset.id); if (g) c.querySelector(".status").innerHTML = badge(g.status); });
  }

  $("#d-q").oninput = (e) => { f.q = e.target.value.trim(); clearTimeout(timer); timer = setTimeout(reload, 350); };
  $("#d-genre").onchange = (e) => { f.genre = e.target.value; reload(); };
  $("#d-years").onchange = (e) => { f.years = e.target.value; reload(); };
  $("#d-sort").onchange = (e) => { f.sort = e.target.value; reload(); };
  $("#d-more").onclick = () => { page++; load(true); };
  load();

  // ── A game's page ──
  async function detail(id, done) {
    const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:94vh;width:min(960px,96vw);overflow:auto"><header id="dt-h">Loading…</header><div class="body" id="dt" style="padding-top:14px"><span class="muted">Loading…</span></div>
      <footer><span class="muted grow" id="dt-msg"></span><button class="btn" id="dt-no">Close</button></footer></div></div>`);
    document.body.append(modal);
    const q = (s) => modal.querySelector(s);
    q("#dt-no").onclick = () => { modal.remove(); done?.(); };
    let d;
    async function show(gid) {
      q("#dt").innerHTML = `<span class="muted">Loading…</span>`;
      try { d = await api(`/api/discover/${gid}`); } catch (e) { q("#dt").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; return; }
      const g = d.game, s = g.status || { state: "none" };
      q("#dt-h").textContent = g.name;
      const busy = s.state === "library" || s.state === "downloading";
      q("#dt").innerHTML = `
        <div class="row" style="align-items:flex-start;gap:16px;flex-wrap:wrap">
          <div style="width:200px;flex:none">${g.cover ? `<img src="${esc(g.cover)}" alt="" style="width:200px;border-radius:8px;display:block">` : ""}</div>
          <div class="grow" style="min-width:260px">
            <div class="row" style="gap:8px;flex-wrap:wrap"><span class="muted">${year(g.year)}</span>${g.genres.map((x) => `<span class="badge">${esc(x)}</span>`).join("")}${badge(s)}</div>
            <div class="muted" style="margin-top:6px">${g.developers.length ? "By " + esc(g.developers.join(", ")) : ""}${d.publishers.length ? " · Published by " + esc(d.publishers.join(", ")) : ""}</div>
            <div class="muted">${g.rating ? `Rated ${Math.round(g.rating)} / 100${d.rating_count ? ` by ${d.rating_count.toLocaleString()} people` : ""}` : "Not rated yet"}${d.modes.length ? " · " + esc(d.modes.join(", ")) : ""}${d.perspectives.length ? " · " + esc(d.perspectives.join(", ")) : ""}</div>
            <p style="margin:10px 0;white-space:pre-line;max-height:9.5em;overflow:auto">${esc(g.summary || "No description.")}</p>
            ${d.storyline ? `<details><summary class="muted">Storyline</summary><p style="white-space:pre-line">${esc(d.storyline)}</p></details>` : ""}
            <div class="row" style="gap:8px;flex-wrap:wrap;margin-top:10px">
              <button class="btn primary" id="dt-best" ${busy ? "disabled" : ""} title="Search your indexers and send the best release to your download client (SABnzbd or qBittorrent)">${s.state === "library" ? "Already in your library" : s.state === "downloading" ? "Downloading…" : "Find and download"}</button>
              <button class="btn" id="dt-search" ${s.state === "library" ? "disabled" : ""} title="See every release and choose">Search releases…</button>
              <button class="btn" id="dt-want" ${s.state !== "none" ? "disabled" : ""}>${s.state === "none" ? "Add to wanted list" : s.state === "wanted" ? "On your wanted list" : "Added"}</button>
              ${d.url ? `<a class="btn" href="${esc(d.url)}" target="_blank" rel="noopener">IGDB ↗</a>` : ""}</div>
            <div class="muted" style="margin-top:6px;font-size:12.5px">Downloaded games are imported into your library, converted to Games on Demand, if that's how Wanted → Setup is set.</div></div></div>
        ${d.screenshots.length ? `<div style="display:flex;gap:8px;overflow-x:auto;margin-top:14px;padding-bottom:6px">${d.screenshots.map((u) => `<img src="${esc(u)}" alt="" loading="lazy" style="height:130px;border-radius:6px;flex:none;cursor:zoom-in" data-shot="${esc(u)}">`).join("")}</div>` : ""}
        ${d.similar.length ? `<h3 style="margin:16px 0 8px">Similar games</h3><div style="display:flex;gap:10px;overflow-x:auto;padding-bottom:6px">${d.similar.map((x) => `<div data-sim="${x.id}" style="width:110px;flex:none;cursor:pointer">${x.cover ? `<img src="${esc(x.cover)}" alt="" style="width:110px;aspect-ratio:3/4;object-fit:cover;border-radius:6px;display:block">` : ""}<div style="font-size:12.5px;line-height:1.2;margin-top:4px">${esc(x.name)}</div><div class="status" style="margin-top:2px">${badge(x.status)}</div></div>`).join("")}</div>` : ""}`;
      q("#dt").querySelectorAll("[data-sim]").forEach((c) => (c.onclick = () => show(+c.dataset.sim)));
      q("#dt").querySelectorAll("[data-shot]").forEach((im) => (im.onclick = () => window.open(im.dataset.shot, "_blank", "noopener")));
      const act = async (mode) => {
        q("#dt-msg").textContent = mode === "best" ? "Searching your indexers…" : "Adding…";
        try {
          const r = await api(`/api/discover/${g.id}/grab`, { body: { mode } });
          if (mode === "best") {
            if (r.grabbed) toast(`Sent to the download client: ${r.grabbed}`, "ok");
            else { toast("Nothing acceptable found. It's on your wanted list; choose a release by hand with Search releases.", "info"); }
          } else toast("Added to your wanted list", "ok");
          await show(g.id);
        } catch (e) { toast(e.message, "err"); q("#dt-msg").textContent = ""; }
      };
      q("#dt-best").onclick = () => act("best");
      q("#dt-want").onclick = () => act("add");
      q("#dt-search").onclick = async () => {
        try { const r = await api(`/api/discover/${g.id}/grab`, { body: { mode: "add" } }); searchDialog({ id: r.wanted_id, name: g.name }, () => show(g.id)); } catch (e) { toast(e.message, "err"); }
      };
      q("#dt-msg").textContent = "";
    }
    show(id);
  }
}
