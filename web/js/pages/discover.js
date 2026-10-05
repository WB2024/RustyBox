// Discover: browse every Xbox 360 game on IGDB with real filters (genre, theme, game mode,
// perspective, series, franchise, developer, engine, keyword, age rating, years, ratings,
// multiplayer), open a game for everything IGDB knows, and send it to the wanted list or straight
// to a download (which then converts to GOD and lands in your library).
import { api, el, esc, toast } from "../util.js";
import { searchDialog } from "../grab-ui.js";

export const title = "Discover";

const FIXED = [
  ["genre", "Genre"], ["theme", "Theme"], ["mode", "Game mode"], ["persp", "Perspective"], ["age", "Age rating"],
];
const SEARCHED = [
  ["series", "Series", "Grand Theft Auto, Halo…"], ["franchise", "Franchise", "Gears of War…"],
  ["company", "Developer or publisher", "Rockstar, Bungie…"], ["engine", "Game engine", "Unreal Engine 3…"], ["keyword", "Keyword", "zombies, open world…"],
];
const KIND_LABEL = Object.fromEntries([...FIXED.map(([k, l]) => [k, l]), ...SEARCHED.map(([k, l]) => [k, l])]);
const SORTS = [["popular", "Most popular"], ["rating", "Best rated by players"], ["critics", "Best rated by critics"], ["newest", "Newest"], ["oldest", "Oldest"], ["name", "Name (A–Z)"]];
const YEAR_MIN = 2005, YEAR_MAX = 2016;
const MODE_ICON = { "Single player": "🧍", "Co-operative": "👥", "Split screen": "🪟", "Multiplayer": "🌐", "Massively Multiplayer Online (MMO)": "🌍", "Battle Royale": "👑" };
const year = (t) => (t ? new Date(t * 1000).getUTCFullYear() : "");
const dateOf = (t) => (t ? new Date(t * 1000).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" }) : "");
const hours = (s) => (s ? (s / 3600 >= 10 ? Math.round(s / 3600) : Math.round(s / 360) / 10) + " h" : "");

function badge(s) {
  if (!s || s.state === "none") return "";
  if (s.state === "library") return `<span class="badge ok">✓ In your library</span>`;
  if (s.state === "downloading") return `<span class="badge accent">Downloading${s.progress ? " " + Math.round(s.progress) + "%" : ""}</span>`;
  return `<span class="badge">On your wanted list</span>`;
}

const blank = () => ({ q: "", sort: "popular", years: "", min_rating: "", max_votes: "", known: false, online_coop: false, local_coop: false, online: false, hide_owned: false, hide_wanted: false, sel: {}, all: {} });

export async function render(root, ctx) {
  let f = blank();
  let view = "grid", open = false;
  try {
    const s = JSON.parse(sessionStorage.getItem("discover2") || "{}");
    f = { ...blank(), ...(s.f || {}) }; view = s.view || "grid"; open = !!s.open;
  } catch { /* start fresh */ }
  let page = 1, timer, token = 0;
  const $ = (s) => root.querySelector(s);

  let facets;
  try { facets = await api("/api/discover/facets"); } catch (e) {
    root.innerHTML = `<div class="page-head"><h1>🧭 Discover</h1></div><div class="notice err">${esc(e.message)}</div>`;
    return;
  }
  const byName = (kind, name) => (facets[kind] || []).find((x) => x.name.toLowerCase() === name.toLowerCase());
  const counts = {};

  root.innerHTML = `
    <div class="page-head"><h1>🧭 Discover</h1><div class="muted">Every Xbox 360 game on IGDB. Filter by genre, theme, game mode, series, developer and more, open a game for everything IGDB knows, then add it to your wanted list or download it straight away.</div></div>
    <div class="card"><div class="row" style="gap:10px;flex-wrap:wrap">
      <input type="search" id="d-q" class="grow" style="min-width:220px" placeholder="Search by name" autocomplete="off">
      <select id="d-sort" aria-label="Sort">${SORTS.map(([v, l]) => `<option value="${v}">${l}</option>`).join("")}</select>
      <button class="btn" id="d-filters" aria-expanded="false">⚙ Filters <span class="badge" id="d-fcount">0</span></button>
      <button class="btn" id="d-random" title="Open a random game that fits the filters">🎲 Surprise me</button>
      <div class="seg" role="group" aria-label="View"><button data-view="grid">▦ Covers</button><button data-view="list">☰ List</button></div></div>
      <div class="fchips" id="d-shelves" style="margin-top:10px"></div></div>
    <div class="card hidden" id="d-panel"></div>
    <div class="row" style="margin:10px 2px;gap:8px;flex-wrap:wrap;align-items:center"><div class="fchips grow" id="d-active"></div><span class="muted" id="d-total"></span></div>
    <div id="d-grid"></div>
    <div style="text-align:center;margin:14px 0"><button class="btn hidden" id="d-more">Show more</button><span class="muted" id="d-status"></span></div>
    <div class="muted" style="font-size:12.5px">Game information and artwork from <a href="https://www.igdb.com" target="_blank" rel="noopener">IGDB.com</a>.</div>`;

  const save = () => { try { sessionStorage.setItem("discover2", JSON.stringify({ f, view, open })); } catch { /* private mode */ } };
  const ids = (kind) => Object.keys(f.sel[kind] || {});

  const query = () => {
    const p = new URLSearchParams();
    if (f.q) p.set("q", f.q);
    p.set("sort", f.sort);
    for (const kind of Object.keys(f.sel)) if (ids(kind).length) p.set(kind, ids(kind).join(","));
    const all = Object.keys(f.all).filter((k) => f.all[k] && ids(k).length > 1);
    if (all.length) p.set("all", all.join(","));
    if (f.years) p.set("years", f.years);
    if (f.min_rating) p.set("min_rating", f.min_rating);
    if (f.max_votes) p.set("max_votes", f.max_votes);
    if (f.known) p.set("known", "1");
    if (f.online_coop) p.set("online_coop", "1");
    if (f.local_coop) p.set("local_coop", "1");
    if (f.online) p.set("online", "1");
    const hide = [f.hide_owned && "owned", f.hide_wanted && "wanted"].filter(Boolean);
    if (hide.length) p.set("hide", hide.join(","));
    return p;
  };

  // ── What is filtered now ──
  function activeChips() {
    const out = [];
    for (const kind of Object.keys(f.sel)) for (const [id, name] of Object.entries(f.sel[kind])) out.push([`${kind}:${id}`, `${KIND_LABEL[kind] || kind}: ${name}`]);
    if (f.years) out.push(["years", `Released ${f.years.replace("-", "–")}`]);
    if (f.min_rating) out.push(["min_rating", `Rated ${f.min_rating}+`]);
    if (f.max_votes) out.push(["max_votes", `Under ${f.max_votes} ratings`]);
    if (f.known) out.push(["known", "Well known"]);
    if (f.online_coop) out.push(["online_coop", "Online co-op"]);
    if (f.local_coop) out.push(["local_coop", "Local co-op / split screen"]);
    if (f.online) out.push(["online", "Online multiplayer"]);
    if (f.hide_owned) out.push(["hide_owned", "Hiding what I own"]);
    if (f.hide_wanted) out.push(["hide_wanted", "Hiding my wanted list"]);
    return out;
  }
  function drawActive() {
    const chips = activeChips();
    $("#d-fcount").textContent = chips.length;
    $("#d-fcount").classList.toggle("hidden", !chips.length);
    $("#d-active").innerHTML = chips.map(([k, l]) => `<span class="tag on" data-x="${esc(k)}">${esc(l)} ✕</span>`).join("") + (chips.length > 1 ? `<button class="btn small" id="d-clear">Clear all</button>` : "");
    $("#d-active").querySelectorAll("[data-x]").forEach((c) => (c.onclick = () => {
      const k = c.dataset.x;
      if (k.includes(":")) { const [kind, id] = k.split(":"); delete f.sel[kind][id]; } else if (typeof f[k] === "boolean") f[k] = false; else f[k] = "";
      changed();
    }));
    const c = $("#d-clear"); if (c) c.onclick = () => { const keep = { q: f.q, sort: f.sort }; f = { ...blank(), ...keep }; changed(); };
  }

  // ── The filter panel ──
  function drawPanel() {
    const group = ([kind, label]) => {
      const list = facets[kind] || [];
      const many = ids(kind).length > 1;
      return `<div class="dgroup"><div class="row" style="gap:6px;align-items:baseline"><b>${label}</b>${many ? `<label class="check muted" style="font-size:12px"><input type="checkbox" data-all="${kind}" ${f.all[kind] ? "checked" : ""}><span>must have all</span></label>` : ""}</div>
        <div class="fchips" style="margin-top:6px">${list.map((x) => {
          const n = counts[kind]?.[x.id];
          if (n === 0) return "";
          return `<span class="tag ${f.sel[kind]?.[x.id] ? "on" : ""}" data-pick="${kind}:${x.id}" data-name="${esc(x.name)}">${esc(x.name)}${n != null ? ` <small>${n}</small>` : ""}</span>`;
        }).join("")}</div></div>`;
    };
    const search = ([kind, label, ph]) => `<div class="dgroup"><b>${label}</b>
      <div style="position:relative;margin-top:6px"><input type="search" data-suggest="${kind}" placeholder="${esc(ph)}" autocomplete="off" style="width:100%"><div class="dsuggest hidden" data-box="${kind}"></div></div>
      <div class="fchips" style="margin-top:6px">${Object.entries(f.sel[kind] || {}).map(([id, name]) => `<span class="tag on" data-pick="${kind}:${id}" data-name="${esc(name)}">${esc(name)} ✕</span>`).join("")}</div></div>`;
    const yearOpts = (cur, any) => `<option value="">${any}</option>${Array.from({ length: YEAR_MAX - YEAR_MIN + 1 }, (_, i) => YEAR_MIN + i).map((y) => `<option ${String(y) === String(cur) ? "selected" : ""}>${y}</option>`).join("")}`;
    const [y1, y2] = f.years ? f.years.split("-") : ["", ""];
    $("#d-panel").innerHTML = `
      <div class="dgrid">${FIXED.map(group).join("")}</div>
      <hr style="border:0;border-top:1px solid var(--line);margin:14px 0">
      <div class="dgrid">${SEARCHED.map(search).join("")}
        <div class="dgroup"><b>Released</b><div class="row" style="gap:8px;margin-top:6px"><select id="d-y1" aria-label="From year">${yearOpts(y1, "From")}</select><span class="muted">to</span><select id="d-y2" aria-label="To year">${yearOpts(y2 || y1, "To")}</select></div>
          <b style="display:block;margin-top:12px">Rating</b><div class="row" style="gap:8px;margin-top:6px"><select id="d-minr" aria-label="Minimum rating">${[["", "Any rating"], ["60", "60 and up"], ["70", "70 and up"], ["80", "80 and up"], ["90", "90 and up"]].map(([v, l]) => `<option value="${v}" ${v === String(f.min_rating) ? "selected" : ""}>${l}</option>`).join("")}</select></div>
          <label class="check" style="margin-top:8px"><input type="checkbox" id="d-known" ${f.known ? "checked" : ""}><span>Only games lots of people rated</span></label>
          <label class="check" style="margin-top:6px"><input type="checkbox" id="d-gems" ${f.max_votes ? "checked" : ""}><span>Hidden gems: few ratings (use with a rating)</span></label></div>
        <div class="dgroup"><b>Multiplayer</b>
          <label class="check" style="margin-top:6px"><input type="checkbox" id="d-oc" ${f.online_coop ? "checked" : ""}><span>Online co-op</span></label>
          <label class="check" style="margin-top:6px"><input type="checkbox" id="d-lc" ${f.local_coop ? "checked" : ""}><span>Local co-op or split screen</span></label>
          <label class="check" style="margin-top:6px"><input type="checkbox" id="d-on" ${f.online ? "checked" : ""}><span>Online multiplayer</span></label>
          <b style="display:block;margin-top:12px">Hide</b>
          <label class="check" style="margin-top:6px"><input type="checkbox" id="d-ho" ${f.hide_owned ? "checked" : ""}><span>Games I already have</span></label>
          <label class="check" style="margin-top:6px"><input type="checkbox" id="d-hw" ${f.hide_wanted ? "checked" : ""}><span>Games on my wanted list</span></label></div></div>`;
    const p = $("#d-panel");
    p.querySelectorAll("[data-pick]").forEach((c) => (c.onclick = () => {
      const [kind, id] = c.dataset.pick.split(":");
      f.sel[kind] ||= {};
      if (f.sel[kind][id]) delete f.sel[kind][id]; else f.sel[kind][id] = c.dataset.name;
      changed();
    }));
    p.querySelectorAll("[data-all]").forEach((c) => (c.onchange = () => { f.all[c.dataset.all] = c.checked; changed(false); }));
    const years = () => {
      const a = $("#d-y1").value, b = $("#d-y2").value;
      if (!a && !b) f.years = "";
      else { const x = +(a || b), y = +(b || a); f.years = `${Math.min(x, y)}-${Math.max(x, y)}`; }
      changed(false);
    };
    $("#d-y1").onchange = years; $("#d-y2").onchange = years;
    $("#d-minr").onchange = (e) => { f.min_rating = e.target.value; changed(false); };
    const flag = (sel, key, fn) => { $(sel).onchange = (e) => { f[key] = fn ? fn(e.target.checked) : e.target.checked; changed(false); }; };
    flag("#d-known", "known"); flag("#d-gems", "max_votes", (c) => (c ? "60" : "")); flag("#d-oc", "online_coop"); flag("#d-lc", "local_coop"); flag("#d-on", "online"); flag("#d-ho", "hide_owned"); flag("#d-hw", "hide_wanted");
    p.querySelectorAll("[data-suggest]").forEach((inp) => {
      const kind = inp.dataset.suggest, box = p.querySelector(`[data-box="${kind}"]`);
      let t;
      inp.oninput = () => {
        clearTimeout(t);
        const text = inp.value.trim();
        if (text.length < 2) { box.classList.add("hidden"); return; }
        t = setTimeout(async () => {
          try {
            const r = await api(`/api/discover/suggest?kind=${kind}&q=${encodeURIComponent(text)}`);
            box.innerHTML = r.results.length ? r.results.map((x) => `<div data-id="${x.id}" data-name="${esc(x.name)}">${esc(x.name)}</div>`).join("") : `<div class="muted">No match</div>`;
            box.classList.remove("hidden");
            box.querySelectorAll("[data-id]").forEach((o) => (o.onmousedown = () => { (f.sel[kind] ||= {})[o.dataset.id] = o.dataset.name; changed(); }));
          } catch { box.classList.add("hidden"); }
        }, 250);
      };
      inp.onblur = () => setTimeout(() => box.classList.add("hidden"), 200);
    });
  }

  // How many games each value has, filled in as the answers arrive.
  async function loadCounts() {
    for (const [kind] of FIXED) {
      try { counts[kind] = (await api(`/api/discover/facets/${kind}/counts`)).counts; if (open) drawPanel(); } catch { /* the lists work without numbers */ }
    }
  }

  // ── Shelves: one click filter sets ──
  function pick(kind, name) { const x = byName(kind, name); return x ? { sel: { [kind]: { [x.id]: x.name } }, sort: "rating", known: true } : {}; }
  const SHELVES = [
    ["🔥 Most popular", () => ({})],
    ["🏆 Critics' picks", () => ({ sort: "critics", known: true })],
    ["💎 Hidden gems", () => ({ sort: "rating", min_rating: "78", max_votes: "60" })],
    ["👥 Couch co-op", () => ({ local_coop: true, sort: "rating", known: true })],
    ["🌐 Online co-op", () => ({ online_coop: true, sort: "rating", known: true })],
    ["🧟 Horror", () => pick("theme", "Horror")],
    ["🌍 Open world", () => pick("theme", "Open world")],
    ["🏎 Racing", () => pick("genre", "Racing")],
    ["🥊 Fighting", () => pick("genre", "Fighting")],
    ["🧒 Family friendly", () => { const e = byName("age", "ESRB E"), e10 = byName("age", "ESRB E10+"); return { sel: { age: Object.fromEntries([e, e10].filter(Boolean).map((x) => [x.id, x.name])) }, sort: "rating", known: true }; }],
    ["📅 Newest", () => ({ sort: "newest" })],
    ["🕹 Classics 2005–2007", () => ({ years: "2005-2007", sort: "rating", known: true })],
  ];
  $("#d-shelves").innerHTML = SHELVES.map(([l], i) => `<span class="tag" data-shelf="${i}">${esc(l)}</span>`).join("");
  $("#d-shelves").querySelectorAll("[data-shelf]").forEach((c) => (c.onclick = () => { f = { ...blank(), ...SHELVES[+c.dataset.shelf][1]() }; changed(); }));

  // ── Results ──
  const modeIcons = (g) => (g.modes || []).map((m) => `<span title="${esc(m)}">${MODE_ICON[m] || "🎮"}</span>`).join("");
  function cardHtml(g) {
    if (view === "list") {
      return `<div class="drow" data-id="${g.id}">${g.cover ? `<img src="${esc(g.cover)}" alt="" loading="lazy">` : `<div class="noimg"></div>`}
        <div class="grow"><div style="font-weight:600">${esc(g.name)} <span class="muted" style="font-weight:400">${year(g.year)}</span></div><div class="muted" style="font-size:12.5px">${esc([...g.genres, ...(g.themes || [])].slice(0, 5).join(" · "))}${g.developers.length ? " · " + esc(g.developers[0]) : ""}</div></div>
        <div class="muted" title="Game modes">${modeIcons(g)}</div><div style="min-width:44px;text-align:right">${g.rating ? `<b>${Math.round(g.rating)}</b>` : ""}</div><div class="status">${badge(g.status)}</div></div>`;
    }
    return `<div class="card dcard" data-id="${g.id}" style="margin:0;padding:8px;cursor:pointer;display:flex;flex-direction:column;gap:6px">
      <div style="position:relative">${g.cover ? `<img src="${esc(g.cover)}" alt="" loading="lazy" style="width:100%;aspect-ratio:3/4;object-fit:cover;border-radius:6px;display:block;background:var(--line)">` : `<div style="width:100%;aspect-ratio:3/4;background:var(--line);border-radius:6px"></div>`}
        ${g.rating ? `<span class="badge" style="position:absolute;top:6px;right:6px;background:rgba(0,0,0,.72);color:#fff" title="Player rating">${Math.round(g.rating)}</span>` : ""}
        ${g.critics ? `<span class="badge" style="position:absolute;top:6px;left:6px;background:rgba(0,0,0,.72);color:#fff" title="Critics' score">★ ${Math.round(g.critics)}</span>` : ""}</div>
      <div style="font-weight:600;font-size:13.5px;line-height:1.25">${esc(g.name)}</div>
      <div class="muted" style="font-size:12px">${year(g.year)}${g.genres.length ? " · " + esc(g.genres.slice(0, 2).join(", ")) : ""}</div>
      <div style="font-size:13px;letter-spacing:2px">${modeIcons(g)}</div>
      <div class="status">${badge(g.status)}</div></div>`;
  }
  function layout() {
    const grid = $("#d-grid");
    grid.className = view === "grid" ? "dgridcards" : "dlist";
    root.querySelectorAll("[data-view]").forEach((b) => b.classList.toggle("on", b.dataset.view === view));
  }

  async function load(append = false) {
    const my = ++token;
    $("#d-status").textContent = "Loading…"; $("#d-more").classList.add("hidden");
    try {
      const p = query(); p.set("page", page);
      const r = await api(`/api/discover?${p}`);
      if (my !== token) return;
      const grid = $("#d-grid");
      if (!append) grid.innerHTML = "";
      grid.insertAdjacentHTML("beforeend", r.games.map(cardHtml).join(""));
      grid.querySelectorAll("[data-id]:not([data-bound])").forEach((c) => { c.dataset.bound = 1; c.onclick = () => detail(+c.dataset.id, refreshStatuses); });
      if (r.total != null) $("#d-total").textContent = `${r.total.toLocaleString()} game${r.total === 1 ? "" : "s"}${r.relevance ? " · ordered by how well they match" : ""}`;
      $("#d-status").textContent = grid.children.length ? "" : "No Xbox 360 games match that. Try removing a filter.";
      $("#d-more").classList.toggle("hidden", !r.has_more);
    } catch (e) { if (my === token) $("#d-status").textContent = e.message; }
  }
  // After a filter changes: save, redraw what shows the filters, and fetch the first page.
  function changed(panel = true) {
    page = 1; save(); drawActive();
    if (panel && open) drawPanel();
    $("#d-sort").value = f.sort; $("#d-q").value = f.q;
    load(false);
  }

  // Refresh just the status badges of what's on screen (after adding or grabbing something).
  async function refreshStatuses() {
    const cards = [...root.querySelectorAll("#d-grid [data-id]")];
    if (!cards.length) return;
    const pages = Math.max(1, Math.ceil(cards.length / 24));
    const byId = new Map();
    for (let p = 1; p <= pages; p++) { const qs = query(); qs.set("page", p); try { (await api(`/api/discover?${qs}`)).games.forEach((g) => byId.set(g.id, g)); } catch { break; } }
    cards.forEach((c) => { const g = byId.get(+c.dataset.id); if (g) c.querySelector(".status").innerHTML = badge(g.status); });
  }

  $("#d-q").value = f.q; $("#d-sort").value = f.sort;
  $("#d-q").oninput = (e) => { f.q = e.target.value.trim(); clearTimeout(timer); timer = setTimeout(() => changed(false), 350); };
  $("#d-sort").onchange = (e) => { f.sort = e.target.value; changed(false); };
  $("#d-more").onclick = () => { page++; load(true); };
  root.querySelectorAll("[data-view]").forEach((b) => (b.onclick = () => { view = b.dataset.view; save(); layout(); $("#d-grid").innerHTML = ""; page = 1; load(false); }));
  $("#d-filters").onclick = () => { open = !open; $("#d-panel").classList.toggle("hidden", !open); $("#d-filters").setAttribute("aria-expanded", String(open)); if (open) drawPanel(); save(); };
  $("#d-random").onclick = async () => {
    try { const r = await api(`/api/discover/random?${query()}`); if (r.id) detail(r.id, refreshStatuses); else toast("Nothing matches those filters", "info"); } catch (e) { toast(e.message, "err"); }
  };
  if (open) { $("#d-panel").classList.remove("hidden"); $("#d-filters").setAttribute("aria-expanded", "true"); drawPanel(); }
  layout(); drawActive(); load(); loadCounts();

  // ── A game's page ──
  // Choosing something on a game's page filters the list to just that.
  function filterTo(kind, id, name) {
    f = { ...blank(), sort: f.sort === "name" ? "name" : "popular", sel: { [kind]: { [id]: name } } };
    document.querySelectorAll(".dmodal").forEach((m) => m.remove());
    changed();
    window.scrollTo({ top: 0, behavior: "smooth" });
  }

  function simCard(x) {
    return `<div data-sim="${x.id}" style="width:110px;flex:none;cursor:pointer">${x.cover ? `<img src="${esc(x.cover)}" alt="" style="width:110px;aspect-ratio:3/4;object-fit:cover;border-radius:6px;display:block">` : ""}<div style="font-size:12.5px;line-height:1.2;margin-top:4px">${esc(x.name)}</div><div class="muted" style="font-size:11.5px">${year(x.year)}${x.rating ? ` · ${Math.round(x.rating)}` : ""}</div><div class="status" style="margin-top:2px">${badge(x.status)}</div></div>`;
  }

  async function detail(id, done) {
    const modal = el(`<div class="modal-bg dmodal"><div class="modal wide" style="height:auto;max-height:94vh;width:min(1040px,96vw);overflow:auto"><header id="dt-h">Loading…</header><div class="body" id="dt" style="padding-top:14px"><span class="muted">Loading…</span></div>
      <footer><span class="muted grow" id="dt-msg"></span><button class="btn" id="dt-no">Close</button></footer></div></div>`);
    document.body.append(modal);
    const q = (s) => modal.querySelector(s);
    q("#dt-no").onclick = () => { modal.remove(); done?.(); };
    modal.onclick = (e) => { if (e.target === modal) { modal.remove(); done?.(); } };
    let d, seq = 0;
    const tags = (kind, list) => list.map((x) => `<span class="tag" data-f="${kind}:${x.id}" data-n="${esc(x.name)}">${esc(x.name)}</span>`).join("");
    const row = (label, html) => (html ? `<div class="drowline"><div class="muted">${label}</div><div class="fchips">${html}</div></div>` : "");

    async function show(gid) {
      const my = ++seq;
      q("#dt").innerHTML = `<span class="muted">Loading…</span>`;
      try { d = await api(`/api/discover/${gid}`); } catch (e) { q("#dt").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; return; }
      if (my !== seq) return;
      const g = d.game, s = g.status || { state: "none" };
      q("#dt-h").textContent = g.name;
      const busy = s.state === "library" || s.state === "downloading";
      const mp = d.multiplayer;
      const mpText = mp ? [mp.online_coop && `online co-op${mp.online_max ? ` (up to ${mp.online_max})` : ""}`, !mp.online_coop && mp.online_max > 1 && `online up to ${mp.online_max}`, (mp.offline_coop || mp.offline_max > 1) && `local co-op${mp.offline_max ? ` (up to ${mp.offline_max})` : ""}`, mp.split_screen && "split screen", mp.lan && "LAN", mp.campaign_coop && "co-op campaign", mp.drop_in && "drop-in / drop-out"].filter(Boolean).join(" · ") : "";
      const ttb = d.time_to_beat;
      q("#dt").innerHTML = `
        <div class="row" style="align-items:flex-start;gap:18px;flex-wrap:wrap">
          <div style="width:210px;flex:none">${g.cover ? `<img src="${esc(g.cover)}" alt="" style="width:210px;border-radius:8px;display:block">` : ""}
            <div class="row" style="gap:8px;margin-top:10px">
              <div class="dscore" title="Player rating">${g.rating ? Math.round(g.rating) : "–"}<small>players${d.rating_count ? ` · ${d.rating_count.toLocaleString()}` : ""}</small></div>
              <div class="dscore" title="Critics' score">${d.critics ? Math.round(d.critics) : "–"}<small>critics${d.critics_count ? ` · ${d.critics_count}` : ""}</small></div></div>
            ${ttb ? `<div class="muted" style="margin-top:8px;font-size:12.5px">Time to beat<br>${ttb.hastily ? `Rushed ${hours(ttb.hastily)}<br>` : ""}${ttb.normally ? `Normal ${hours(ttb.normally)}<br>` : ""}${ttb.completely ? `Everything ${hours(ttb.completely)}` : ""}</div>` : ""}</div>
          <div class="grow" style="min-width:300px">
            <div class="row" style="gap:8px;flex-wrap:wrap"><span class="muted">${year(g.year)}</span>${badge(s)}${d.age_ratings.slice(0, 3).map((a) => `<span class="badge" title="${esc(a.reasons.join(", ") || a.org)}">${esc(a.org)} ${esc(a.rating)}</span>`).join("")}</div>
            <div style="margin-top:8px">
              ${row("Genres", tags("genre", d.genres))}${row("Themes", tags("theme", d.themes))}${row("Game modes", tags("mode", d.modes))}${row("Perspective", tags("persp", d.perspectives))}
              ${row("Developers", tags("company", d.developers))}${row("Publishers", tags("company", d.publishers))}${row("Ported by", tags("company", d.porters))}
              ${row("Series", tags("series", d.series))}${row("Franchise", tags("franchise", d.franchises))}${row("Engine", tags("engine", d.engines))}</div>
            ${mpText ? `<div class="muted" style="margin-top:6px">Multiplayer: ${esc(mpText)}</div>` : ""}
            ${d.releases.length ? `<div class="muted" style="margin-top:4px">On Xbox 360: ${d.releases.map((r) => `${esc(r.region)} ${dateOf(r.date)}`).join(" · ")}</div>` : ""}
            <p style="margin:10px 0;white-space:pre-line;max-height:9.5em;overflow:auto">${esc(g.summary || "No description.")}</p>
            ${d.storyline ? `<details><summary class="muted">Storyline</summary><p style="white-space:pre-line">${esc(d.storyline)}</p></details>` : ""}
            <div class="row" style="gap:8px;flex-wrap:wrap;margin-top:10px">
              <button class="btn primary" id="dt-best" ${busy ? "disabled" : ""} title="Search your indexers and send the best release to your download client">${s.state === "library" ? "Already in your library" : s.state === "downloading" ? "Downloading…" : "Find and download"}</button>
              <button class="btn" id="dt-search" ${s.state === "library" ? "disabled" : ""} title="See every release, or look inside your own torrent files, and choose">Search releases…</button>
              <button class="btn" id="dt-want" ${s.state !== "none" ? "disabled" : ""}>${s.state === "none" ? "Add to wanted list" : s.state === "wanted" ? "On your wanted list" : "Added"}</button>
              ${d.url ? `<a class="btn" href="${esc(d.url)}" target="_blank" rel="noopener">IGDB ↗</a>` : ""}</div>
            <div class="muted" style="margin-top:6px;font-size:12.5px">Downloaded games are imported into your library, converted to Games on Demand, if that's how Wanted → Setup is set.</div></div></div>
        ${d.videos.length || d.screenshots.length || d.artworks.length ? `<h3 style="margin:18px 0 8px">Media</h3>` : ""}
        ${d.videos.length ? `<div class="dstrip">${d.videos.slice(0, 6).map((v) => `<a href="https://www.youtube.com/watch?v=${encodeURIComponent(v.id)}" target="_blank" rel="noopener" class="dvideo" title="${esc(v.name)}"><img src="https://img.youtube.com/vi/${encodeURIComponent(v.id)}/mqdefault.jpg" alt="" loading="lazy"><span>▶ ${esc(v.name)}</span></a>`).join("")}</div>` : ""}
        ${d.screenshots.length || d.artworks.length ? `<div class="dstrip" style="margin-top:8px">${[...d.screenshots, ...d.artworks].map((u) => `<img src="${esc(u)}" alt="" loading="lazy" style="height:130px;border-radius:6px;flex:none;cursor:zoom-in" data-shot="${esc(u)}">`).join("")}</div>` : ""}
        ${d.related.length ? `<h3 style="margin:18px 0 8px">Related content</h3><div class="fchips">${d.related.map((r) => `<span class="tag" data-sim="${r.id}"><small>${esc(r.kind)}</small> ${esc(r.name)}</span>`).join("")}</div>` : ""}
        <div id="dt-more"></div>
        ${d.similar.length ? `<h3 style="margin:18px 0 8px">Similar games</h3><div class="dstrip">${d.similar.map(simCard).join("")}</div>` : ""}
        ${d.languages.length ? `<details style="margin-top:16px"><summary class="muted">Languages (${d.languages.length})</summary><table style="margin-top:6px"><tbody>${d.languages.map((l) => `<tr><td>${esc(l.name)}</td><td class="muted">${l.kinds.map(esc).join(", ")}</td></tr>`).join("")}</tbody></table></details>` : ""}
        ${d.age_ratings.some((a) => a.reasons.length) ? `<details style="margin-top:8px"><summary class="muted">Age rating details</summary>${d.age_ratings.map((a) => `<div style="margin-top:4px"><b>${esc(a.org)} ${esc(a.rating)}</b> <span class="muted">${esc(a.reasons.join(", "))}</span></div>`).join("")}</details>` : ""}
        ${d.keywords.length ? `<div class="drowline" style="margin-top:14px"><div class="muted">Keywords</div><div class="fchips">${tags("keyword", d.keywords)}</div></div>` : ""}
        ${d.sites.length ? `<div class="drowline" style="margin-top:8px"><div class="muted">Links</div><div class="fchips">${d.sites.map((x) => `<a class="tag" href="${esc(x.url)}" target="_blank" rel="noopener">${esc(x.kind)} ↗</a>`).join("")}</div></div>` : ""}
        ${d.also_called.length ? `<div class="muted" style="margin-top:8px;font-size:12.5px">Also known as: ${d.also_called.map(esc).join(" · ")}</div>` : ""}`;
      q("#dt").querySelectorAll("[data-sim]").forEach((c) => (c.onclick = () => show(+c.dataset.sim)));
      q("#dt").querySelectorAll("[data-f]").forEach((c) => (c.onclick = () => { const [kind, fid] = c.dataset.f.split(":"); filterTo(kind, fid, c.dataset.n); }));
      q("#dt").querySelectorAll("[data-shot]").forEach((im) => (im.onclick = () => window.open(im.dataset.shot, "_blank", "noopener")));
      const act = async (mode) => {
        q("#dt-msg").textContent = mode === "best" ? "Searching your indexers…" : "Adding…";
        try {
          const r = await api(`/api/discover/${g.id}/grab`, { body: { mode } });
          if (mode === "best") {
            if (r.grabbed) toast(`Sent to the download client: ${r.grabbed}`, "ok");
            else toast("Nothing acceptable found. It's on your wanted list; choose a release by hand with Search releases.", "info");
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
      // More from the same series, franchise and developer, loaded after the page is up.
      const more = new URLSearchParams();
      if (d.series[0]) { more.set("series", d.series[0].id); more.set("series_name", d.series[0].name); }
      if (d.franchises[0]) { more.set("franchise", d.franchises[0].id); more.set("franchise_name", d.franchises[0].name); }
      if (d.developers[0]) { more.set("company", d.developers[0].id); more.set("company_name", d.developers[0].name); }
      if ([...more.keys()].length) {
        try {
          const r = await api(`/api/discover/${g.id}/more?${more}`);
          if (my !== seq || !q("#dt-more")) return;
          q("#dt-more").innerHTML = r.shelves.map((sh) => `<h3 style="margin:18px 0 8px">${sh.kind === "company" ? "More from" : "More from the"} ${esc(sh.name)}${sh.kind === "series" ? " series" : sh.kind === "franchise" ? " franchise" : ""} <button class="btn small" data-all="${sh.kind}:${sh.id}:${esc(sh.name)}">See all</button></h3><div class="dstrip">${sh.games.map(simCard).join("")}</div>`).join("");
          q("#dt-more").querySelectorAll("[data-sim]").forEach((c) => (c.onclick = () => show(+c.dataset.sim)));
          q("#dt-more").querySelectorAll("[data-all]").forEach((c) => (c.onclick = () => { const [k, i, ...n] = c.dataset.all.split(":"); filterTo(k, i, n.join(":")); }));
        } catch { /* the shelves are a bonus */ }
      }
    }
    show(id);
  }
}
