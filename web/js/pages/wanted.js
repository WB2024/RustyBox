// Wanted games: find them on Usenet or as torrents, download them with SABnzbd or qBittorrent, and import them into libraries.
// Radarr-style, for Xbox 360 games, with IGDB as the metadata.
import { api, el, esc, fmtBytes, fmtTime, toast } from "../util.js";
import { ago, since, searchDialog } from "../grab-ui.js";

export const title = "Wanted";

const STATUS = {
  queued: ["Queued", ""],
  unpacking: ["Unpacking", "accent"],
  downloading: ["Downloading", "accent"],
  completed: ["Downloaded", "ok"],
  importing: ["Importing", "accent"],
  imported: ["In your library", "ok"],
  failed: ["Failed", "err"],
  import_failed: ["Import failed", "err"],
};

export async function render(root, ctx) {
  let tab = sessionStorage.getItem("wanted-tab") || "wanted";
  let timer = null;
  const $ = (s) => root.querySelector(s);

  root.innerHTML = `
    <div class="page-head row"><div class="grow"><h1>🔎 Wanted</h1><div class="muted">Games you want, found through your Usenet and torrent indexers, downloaded by SABnzbd or qBittorrent and imported into your libraries. Torrent files you already have are on the <a href="#torrents">Torrents</a> page.</div></div>
      <button class="btn" id="w-setup">Setup</button><button class="btn" id="w-all" title="Search every monitored game now">Search all</button><button class="btn primary" id="w-add">Add a game</button></div>
    <div id="w-banner"></div>
    <div class="row tabs" style="margin-bottom:12px"><button class="btn" data-tab="wanted">Wanted</button><button class="btn" data-tab="activity">Activity</button><button class="btn" data-tab="blocklist">Blocklist</button></div>
    <div id="w-body"></div>`;
  const paint = () => root.querySelectorAll("[data-tab]").forEach((b) => b.classList.toggle("primary", b.dataset.tab === tab));
  root.querySelectorAll("[data-tab]").forEach((b) => (b.onclick = () => { tab = b.dataset.tab; sessionStorage.setItem("wanted-tab", tab); paint(); show(); }));
  $("#w-setup").onclick = () => setupDialog(show);
  $("#w-add").onclick = () => addDialog(show);
  $("#w-all").onclick = async () => { try { const { job } = await api("/api/wanted/search-all", { method: "POST" }); ctx.openJob(job); } catch (e) { toast(e.message, "err"); } };

  async function show() { paint(); clearTimeout(timer); await ({ wanted: wantedTab, activity: activityTab, blocklist: blocklistTab })[tab]($("#w-body")); }

  // ── Wanted ──
  async function wantedTab(out) {
    const r = await api("/api/wanted");
    $("#w-banner").innerHTML = r.ready ? "" : `<div class="notice warn" style="margin-bottom:12px">${!r.indexers ? "No usable indexers yet. " : ""}${!r.sab && !r.qbit ? "Neither SABnzbd nor qBittorrent is set up yet. " : ""}Open <button class="btn small" id="w-setup2">Setup</button> to add them. You can still build your wanted list.</div>`;
    $("#w-setup2")?.addEventListener("click", () => setupDialog(show));
    if (!r.items.length) { out.innerHTML = `<div class="card muted">Nothing wanted yet. Press <b>Add a game</b> and search IGDB.</div>`; return; }
    out.innerHTML = `<div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(330px,1fr));gap:12px">${r.items.map(({ wanted: w, have, grab }) => {
      const g = grab && ["queued", "downloading", "completed", "unpacking", "importing", "failed", "import_failed"].includes(grab.status) ? grab : null;
      const badge = have ? `<span class="badge ok">In your library</span>` : g ? `<span class="badge ${STATUS[g.status][1]}">${STATUS[g.status][0]}${g.status === "downloading" ? " " + Math.round(g.progress) + "%" : ""}</span>` : `<span class="badge accent">Wanted</span>`;
      return `<div class="card" data-id="${w.id}" style="margin:0;display:flex;gap:12px">
        <div style="width:84px;flex:none">${w.cover ? `<img src="/api/covers/${esc(w.cover)}" style="width:84px;border-radius:6px;display:block" alt="">` : `<div style="width:84px;height:112px;background:var(--bg2,#eee);border-radius:6px"></div>`}</div>
        <div class="grow" style="min-width:0"><div class="row"><b class="grow" style="overflow:hidden;text-overflow:ellipsis">${esc(w.name)}</b>${badge}</div>
          <div class="muted" style="font-size:12.5px">${w.release ? new Date(w.release * 1000).getFullYear() : ""}${w.monitored ? "" : " · not monitored"}</div>
          ${g ? `<div class="muted" style="font-size:12.5px;margin-top:4px;word-break:break-all">${esc(g.title)}${g.error ? `<div style="color:var(--err)">${esc(g.error)}</div>` : ""}</div>${g.status === "downloading" ? `<div class="bar" style="margin-top:6px"><i style="width:${Math.round(g.progress)}%"></i></div>` : ""}` : w.last_search ? `<div class="muted" style="font-size:12.5px;margin-top:4px">Searched ${since(w.last_search)}: ${esc(w.last_result || "")}</div>` : ""}
          <div class="row" style="margin-top:8px;flex-wrap:wrap;gap:6px">
            ${have ? "" : `<button class="btn small primary" data-act="search">Search…</button><button class="btn small" data-act="auto" title="Grab the best acceptable release">Grab best</button>`}
            <button class="btn small" data-act="monitor">${w.monitored ? "Unmonitor" : "Monitor"}</button><button class="btn small danger" data-act="remove">Remove</button></div></div></div>`;
    }).join("")}</div>`;
    out.querySelectorAll("[data-id]").forEach((card) => {
      const id = +card.dataset.id, item = r.items.find((i) => i.wanted.id === id);
      card.querySelectorAll("[data-act]").forEach((b) => (b.onclick = async () => {
        const act = b.dataset.act;
        try {
          if (act === "search") return searchDialog(item.wanted, show);
          if (act === "auto") { b.disabled = true; const x = await api(`/api/wanted/${id}/auto`, { method: "POST" }); toast(x.grabbed ? `Sent to the download client: ${x.grabbed}` : "Nothing acceptable found. Use Search… to look at the results", x.grabbed ? "ok" : "info"); return show(); }
          if (act === "monitor") { await api(`/api/wanted/${id}`, { method: "PUT", body: { monitored: !item.wanted.monitored } }); return show(); }
          if (act === "remove") { if (!confirm(`Remove ${item.wanted.name} from the wanted list?`)) return; await api(`/api/wanted/${id}`, { method: "DELETE" }); return show(); }
        } catch (e) { toast(e.message, "err"); b.disabled = false; }
      }));
    });
  }

  // ── Add a game ──
  function addDialog(done) {
    const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:90vh"><header>Add a game</header><div class="body" style="padding-top:14px">
      <div class="row"><input type="search" id="ad-q" class="grow" placeholder="Search IGDB for an Xbox 360 game" autocomplete="off"><button class="btn primary" id="ad-go">Search</button></div><div id="ad-out" style="margin-top:10px"></div></div>
      <footer><button class="btn" id="ad-no">Close</button></footer></div></div>`);
    document.body.append(modal);
    const q = (s) => modal.querySelector(s);
    q("#ad-no").onclick = () => modal.remove();
    async function go() {
      const t = q("#ad-q").value.trim(); if (t.length < 2) return;
      q("#ad-out").innerHTML = `<span class="muted">Searching…</span>`;
      try {
        const r = await api(`/api/wanted/lookup?q=${encodeURIComponent(t)}`);
        q("#ad-out").innerHTML = r.games.length ? r.games.map((g, i) => `<div class="row" style="padding:8px 0;border-bottom:1px solid var(--line,#ddd);align-items:flex-start;gap:12px">
          ${g.thumb ? `<img src="${esc(g.thumb)}" style="width:56px;border-radius:4px" alt="">` : ""}<div class="grow"><b>${esc(g.candidate.name)}</b> <span class="muted">${g.candidate.release ? new Date(g.candidate.release * 1000).getFullYear() : ""}</span>
          <div class="muted" style="font-size:12.5px">${esc((g.candidate.genres || []).join(", "))}${g.candidate.developers?.length ? " · " + esc(g.candidate.developers[0]) : ""}</div><div class="muted" style="font-size:12.5px;max-height:3.2em;overflow:hidden">${esc(g.candidate.summary || "")}</div></div>
          <button class="btn small primary" data-i="${i}" ${g.wanted ? "disabled" : ""}>${g.wanted ? "On the list" : "Add"}</button></div>`).join("") : `<div class="muted">No Xbox 360 games match that.</div>`;
        q("#ad-out").querySelectorAll("[data-i]").forEach((b) => (b.onclick = async () => {
          const g = r.games[+b.dataset.i];
          try { await api("/api/wanted", { body: { igdb_id: g.candidate.id, candidate: g.candidate } }); toast(`${g.candidate.name} added`, "ok"); b.textContent = "Added"; b.disabled = true; done(); } catch (e) { toast(e.message, "err"); }
        }));
      } catch (e) { q("#ad-out").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; }
    }
    q("#ad-go").onclick = go; q("#ad-q").onkeydown = (e) => { if (e.key === "Enter") go(); }; q("#ad-q").focus();
  }

  // ── Activity ──
  async function activityTab(out) {
    const r = await api("/api/grab/activity");
    out.innerHTML = r.grabs.length ? `<div class="card"><table><thead><tr><th>Game</th><th>Release</th><th>Status</th><th class="num">Size</th><th>When</th><th></th></tr></thead><tbody>${r.grabs.map(({ grab: g, game }) => `<tr data-id="${g.id}">
      <td><b>${esc(game || "?")}</b></td><td style="word-break:break-all">${esc(g.title)}<div class="muted" style="font-size:12px"><span class="badge">${g.protocol === "torrent" ? "torrent" : "usenet"}</span> ${esc(g.indexer)}${g.error ? `<div style="color:var(--err)">${esc(g.error)}</div>` : ""}</div></td>
      <td><span class="badge ${STATUS[g.status]?.[1] || ""}">${STATUS[g.status]?.[0] || esc(g.status)}${g.status === "downloading" ? " " + Math.round(g.progress) + "%" : ""}</span></td><td class="num">${fmtBytes(g.size)}</td><td class="muted">${fmtTime(g.added)}</td>
      <td class="num" style="white-space:nowrap">${g.extra_job ? `<a class="btn small" href="#jobs/${g.extra_job}" title="Sending it on to the console">Console job</a> ` : ""}${["completed", "import_failed"].includes(g.status) ? `<button class="btn small primary" data-act="import">Import</button> ` : ""}<button class="btn small danger" data-act="remove">${["queued", "downloading"].includes(g.status) ? "Cancel" : "Remove"}</button></td></tr>`).join("")}</tbody></table></div>` : `<div class="card muted">No downloads yet.</div>`;
    out.querySelectorAll("tr[data-id]").forEach((tr) => tr.querySelectorAll("[data-act]").forEach((b) => (b.onclick = async () => {
      try {
        if (b.dataset.act === "import") { const { job } = await api(`/api/grab/activity/${tr.dataset.id}/import`, { method: "POST" }); return ctx.openJob(job); }
        const bad = confirm("Cancel/remove this download?\n\nPress OK to also put the release on the blocklist (a bad release), or Cancel to keep things as they are.") ;
        if (!bad) return;
        await api(`/api/grab/activity/${tr.dataset.id}?blocklist=true`, { method: "DELETE" }); show();
      } catch (e) { toast(e.message, "err"); }
    })));
    if (r.grabs.some((x) => ["queued", "downloading", "importing", "unpacking", "completed"].includes(x.grab.status))) timer = setTimeout(() => { if (location.hash === "#wanted" && tab === "activity") show(); }, 5000);
  }

  // ── Blocklist ──
  async function blocklistTab(out) {
    const r = await api("/api/grab/blocklist");
    out.innerHTML = r.blocked.length ? `<div class="card"><div class="muted" style="margin-bottom:6px">Releases that failed (or you rejected) are never grabbed again.</div><table><tbody>${r.blocked.map((b) => `<tr><td style="word-break:break-all">${esc(b.title)}<div class="muted" style="font-size:12px">${esc(b.reason || "")} · ${esc(b.indexer || "")}</div></td><td class="muted">${fmtTime(b.added)}</td><td class="num"><button class="btn small" data-un="${b.id}">Allow again</button></td></tr>`).join("")}</tbody></table></div>` : `<div class="card muted">Nothing is blocked.</div>`;
    out.querySelectorAll("[data-un]").forEach((b) => (b.onclick = async () => { await api(`/api/grab/blocklist/${b.dataset.un}`, { method: "DELETE" }); show(); }));
  }

  show();
  if (sessionStorage.getItem("wanted-open-setup")) { sessionStorage.removeItem("wanted-open-setup"); setupDialog(show); }
}

// ── Setup ──────────────────────────────────────────────────────────────────────
async function setupDialog(done) {
  const [cfg, libs, consoles] = await Promise.all([api("/api/grab/config"), api("/api/libraries"), api("/api/consoles")]);
  const folders = libs.filter((l) => ["iso", "god"].includes(l.kind)).flatMap((l) => l.paths.filter((p) => p.writable && !p.remote_url).map((p) => ({ l, p })));
  const drives = libs.filter((l) => ["iso", "god"].includes(l.kind)).flatMap((l) => l.paths.filter((p) => p.writable).map((p) => ({ l, p })));
  const pr = cfg.profile;
  const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:94vh;width:min(920px,96vw)"><header>Setup</header><div class="body" style="padding-top:14px;display:flex;flex-direction:column;gap:14px">
    <div class="card" style="margin:0"><h2>Indexers</h2><div class="muted">Newznab (Usenet) and Torznab (torrent) indexers searched for games. Only those carrying the Xbox 360 category are used. Torrent indexers need qBittorrent set up below.</div><div id="s-ix" style="margin-top:8px"></div>
      <div class="row" style="margin-top:8px;flex-wrap:wrap;gap:8px"><select id="ix-proto" title="What this indexer finds"><option value="usenet">Usenet</option><option value="torrent">Torrent</option></select><input id="ix-name" placeholder="Name" style="width:130px"><input id="ix-url" placeholder="https://api.nzbgeek.info" class="grow"><input id="ix-key" type="password" placeholder="API key" autocomplete="new-password" style="width:170px"><button class="btn" id="ix-add">Add</button></div>
      <div class="row" style="margin-top:8px;flex-wrap:wrap;gap:8px"><span class="muted">Or import everything from Prowlarr (Usenet and torrent indexers):</span><input id="pw-url" placeholder="http://192.168.1.110:9696" class="grow"><input id="pw-key" type="password" placeholder="Prowlarr API key" autocomplete="new-password" style="width:190px"><button class="btn" id="pw-go">Import</button></div><div class="muted" id="ix-msg" style="margin-top:6px"></div></div>
    <div class="card" style="margin:0"><h2>SABnzbd</h2><div class="row" style="flex-wrap:wrap;gap:8px"><input id="sb-url" value="${esc(cfg.sab.url)}" placeholder="http://192.168.1.110:8085" class="grow"><input id="sb-key" type="password" placeholder="${cfg.sab.api_key_set ? "API key saved. Leave blank to keep it" : "API key"}" autocomplete="new-password" style="width:210px"><input id="sb-cat" value="${esc(cfg.sab.category)}" placeholder="category" style="width:120px"></div>
      <div class="row" style="margin-top:8px"><button class="btn" id="sb-test">Test connection</button><button class="btn" id="sb-cat-make" title="Create the category in SABnzbd">Create the category</button><span class="muted" id="sb-msg"></span></div></div>
    <div class="card" style="margin:0"><h2>qBittorrent</h2><div class="muted">For torrents from the torrent indexers and from folders of .torrent files (the Torrents page). Switch on qBittorrent's Web UI (Tools → Options → Web UI).</div>
      <div class="row" style="flex-wrap:wrap;gap:8px;margin-top:8px"><input id="qb-url" value="${esc(cfg.qbit.url)}" placeholder="http://192.168.1.110:8080" class="grow"><input id="qb-user" value="${esc(cfg.qbit.username)}" placeholder="user name" autocomplete="off" style="width:130px"><input id="qb-pass" type="password" placeholder="${cfg.qbit.password_set ? "password saved. Leave blank to keep it" : "password"}" autocomplete="new-password" style="width:210px"><input id="qb-cat" value="${esc(cfg.qbit.category)}" placeholder="category" style="width:120px"></div>
      <div class="row" style="margin-top:8px"><button class="btn" id="qb-test">Test connection</button><button class="btn" id="qb-cat-make" title="Create the category in qBittorrent">Create the category</button><span class="muted" id="qb-msg"></span></div>
      <div class="row" style="flex-wrap:wrap;gap:14px;margin-top:10px">
        <label class="field">When a torrent finishes, put it in the library by<select id="qb-mode"><option value="hardlink" ${cfg.torrent_import_mode === "hardlink" ? "selected" : ""}>Hardlink (instant; keeps seeding; copies if the disks differ)</option><option value="copy" ${cfg.torrent_import_mode === "copy" ? "selected" : ""}>Copy (keeps seeding)</option><option value="move" ${cfg.torrent_import_mode === "move" ? "selected" : ""}>Move (stops seeding)</option></select></label>
        <label class="field">Then<select id="qb-after"><option value="keep" ${cfg.torrent_after_import === "keep" ? "selected" : ""}>Leave it in qBittorrent (keep seeding)</option><option value="remove" ${cfg.torrent_after_import === "remove" ? "selected" : ""}>Remove it from qBittorrent, with its files</option></select></label></div>
      <div class="muted" style="margin-top:6px">Zipped disc images (Redump's collection is one zip per game) are unpacked automatically before importing. qBittorrent reports folders as it sees them: use the path mapping below if it runs in a container.</div></div>
    <div class="card" style="margin:0"><h2>What to look for</h2><div class="row" style="flex-wrap:wrap;gap:14px">
      <label class="field">Preferred region<select id="pf-region">${["usa", "free", "pal", "japan"].map((r) => `<option value="${r}" ${pr.regions[0] === r ? "selected" : ""}>${{ usa: "USA / NTSC", free: "Region free", pal: "PAL / Europe", japan: "Japan" }[r]}</option>`).join("")}</select></label>
      <label class="check" style="padding-top:18px"><input type="checkbox" id="pf-any" ${pr.any_region ? "checked" : ""}><span>Accept other regions too</span></label>
      <label class="check" style="padding-top:18px"><input type="checkbox" id="pf-iso" ${pr.formats.includes("iso") ? "checked" : ""}><span>Disc images (ISO)</span></label><label class="check" style="padding-top:18px"><input type="checkbox" id="pf-god" ${pr.formats.includes("god") ? "checked" : ""}><span>Games on Demand</span></label><label class="check" style="padding-top:18px"><input type="checkbox" id="pf-pkg" ${pr.formats.includes("package") ? "checked" : ""}><span>Arcade (XBLA) packages</span></label>
      <label class="check" style="padding-top:18px"><input type="checkbox" id="pf-godfirst" ${pr.formats[0] === "god" ? "checked" : ""}><span>Prefer Games on Demand</span></label></div>
      <div class="row" style="flex-wrap:wrap;gap:14px;margin-top:8px"><label class="field">Smallest disc image (MB)<input type="number" id="pf-min" value="${pr.min_iso_mb}" style="width:110px"></label><label class="field">Largest (MB)<input type="number" id="pf-max" value="${pr.max_iso_mb}" style="width:110px"></label>
      <label class="field">Grab automatically at score<input type="number" id="pf-score" value="${pr.min_score}" style="width:90px"></label><label class="field grow">Reject words (comma separated)<input id="pf-rej" value="${esc(pr.reject_words.join(", "))}" placeholder="jtag, rgh"></label></div></div>
    <div class="card" style="margin:0"><h2>Finished downloads</h2><div class="row" style="flex-wrap:wrap;gap:14px">
      <label class="field grow">Import into<select id="im-lib"><option value="">(choose a library folder)</option>${folders.map(({ l, p }) => `<option value="${l.id}:${p.id}" ${cfg.import_library && cfg.import_library[0] === l.id && cfg.import_library[1] === p.id ? "selected" : ""}>${esc(l.name)} / ${esc(p.label || p.path)} (${l.kind.toUpperCase()})</option>`).join("")}</select></label>
      <label class="field">How<select id="im-mode"><option value="move" ${cfg.import_mode === "move" ? "selected" : ""}>Move (remove the download)</option><option value="copy" ${cfg.import_mode === "copy" ? "selected" : ""}>Copy</option></select></label>
      <label class="check" style="padding-top:18px"><input type="checkbox" id="im-conv" ${cfg.convert_iso ? "checked" : ""}><span>Convert ISOs to GOD (GOD libraries)</span></label></div>
      <div class="row" style="flex-wrap:wrap;gap:14px;margin-top:10px">
        <label class="field grow">Then also copy to (the Xbox's drive, say)<select id="im-drive"><option value="">Nowhere</option>${drives.filter(({ p }) => !p.role || (cfg.also_drive && cfg.also_drive[1] === p.id)).map(({ l, p }) => `<option value="${l.id}:${p.id}" ${cfg.also_drive && cfg.also_drive[0] === l.id && cfg.also_drive[1] === p.id ? "selected" : ""}>${p.remote_url ? "🖥 " : ""}${esc(l.name)} / ${esc(p.remote_url ? (p.remote_subdir ? "the " + p.remote_subdir + " folder" : "the whole drive") + (p.label ? " (" + p.label + ")" : "") : (p.label || p.path))}</option>`).join("")}</select></label>
        <label class="field grow">Then send to a console (FTP)<select id="im-console"><option value="">Nowhere</option>${consoles.map((c) => `<option value="${c.id}" ${cfg.also_console?.console_id === c.id ? "selected" : ""}>${esc(c.name)}</option>`).join("")}</select></label>
        <label class="field grow">…into its games folder<select id="im-cdest">${[...new Set(consoles.flatMap((c) => c.game_paths))].map((g) => `<option ${cfg.also_console?.dest === g ? "selected" : ""}>${esc(g)}</option>`).join("")}</select></label></div>
      <div class="muted" style="margin-top:4px">So one press can mean: download, convert to GOD, put in your library, copy to the drive and send to the console.</div>
      <div style="margin-top:8px"><div class="muted">SABnzbd reports finished folders as it sees them; map that to the same place as RustyBox sees it (one per line: <span class="mono">/downloads → /data/downloads</span>).</div><textarea id="im-maps" rows="2" spellcheck="false" style="width:100%;margin-top:4px" placeholder="/downloads → /data/downloads">${esc(cfg.path_maps.map((m) => `${m.from} → ${m.to}`).join("\n"))}</textarea></div></div>
    <div class="card" style="margin:0"><h2>Automation</h2><div class="row" style="flex-wrap:wrap;gap:14px"><label class="field">Search for wanted games every (hours, 0 = only when asked)<input type="number" id="au-h" min="0" value="${cfg.search_every_hours}" style="width:90px"></label>
      <label class="check" style="padding-top:18px"><input type="checkbox" id="au-grab" ${cfg.auto_grab ? "checked" : ""}><span>Grab the best release automatically</span></label><label class="check" style="padding-top:18px"><input type="checkbox" id="au-imp" ${cfg.auto_import ? "checked" : ""}><span>Import finished downloads automatically</span></label></div></div>
    </div><footer><button class="btn" id="s-no">Close</button><button class="btn primary" id="s-save">Save</button></footer></div></div>`);
  document.body.append(modal);
  const q = (s) => modal.querySelector(s);
  const note = (id, t, ok) => { const r = q(id); r.textContent = t; r.style.color = ok ? "var(--ok)" : "var(--err)"; };
  let indexers = cfg.indexers;
  const drawIx = () => {
    q("#s-ix").innerHTML = indexers.length ? `<table><tbody>${indexers.map((i) => `<tr><td><b>${esc(i.name)}</b> <span class="badge">${i.protocol === "torrent" ? "torrent" : "usenet"}</span><div class="muted mono" style="font-size:12px;word-break:break-all">${esc(i.url)}</div></td>
      <td>${i.has_xbox360 === true ? `<span class="badge ok">Xbox 360</span>` : i.has_xbox360 === false ? `<span class="badge err">no Xbox 360 category</span>` : `<span class="badge">untested</span>`}</td>
      <td class="num" style="white-space:nowrap"><label class="check" style="display:inline-flex"><input type="checkbox" data-en="${i.id}" ${i.enabled ? "checked" : ""}><span>on</span></label> <button class="btn small" data-test="${i.id}">Test</button> <button class="btn small danger" data-del="${i.id}">Remove</button></td></tr>`).join("")}</tbody></table>` : `<div class="muted">No indexers yet.</div>`;
    q("#s-ix").querySelectorAll("[data-test]").forEach((b) => (b.onclick = async () => {
      note("#ix-msg", "Testing…", true);
      try { const r = await api("/api/grab/indexers/test", { body: { id: +b.dataset.test } }); note("#ix-msg", r.has_xbox360 ? "Works, and carries Xbox 360 games." : "Works, but has no Xbox 360 category, so it won't be searched.", r.has_xbox360); indexers = (await api("/api/grab/config")).indexers; drawIx(); } catch (e) { note("#ix-msg", e.message, false); }
    }));
    q("#s-ix").querySelectorAll("[data-del]").forEach((b) => (b.onclick = async () => { if (!confirm("Remove this indexer from RustyBox?")) return; await api(`/api/grab/indexers/${b.dataset.del}`, { method: "DELETE" }); indexers = indexers.filter((x) => x.id !== +b.dataset.del); drawIx(); }));
    q("#s-ix").querySelectorAll("[data-en]").forEach((c) => (c.onchange = async () => { const i = indexers.find((x) => x.id === +c.dataset.en); await api(`/api/grab/indexers/${i.id}`, { method: "PUT", body: { ...i, enabled: c.checked, api_key: "" } }); i.enabled = c.checked; }));
  };
  drawIx();
  q("#ix-add").onclick = async () => {
    try { const ix = await api("/api/grab/indexers", { body: { name: q("#ix-name").value, url: q("#ix-url").value, api_key: q("#ix-key").value, protocol: q("#ix-proto").value } }); indexers.push(ix); q("#ix-name").value = q("#ix-url").value = q("#ix-key").value = ""; drawIx(); note("#ix-msg", "Added. Press Test to check it.", true); } catch (e) { note("#ix-msg", e.message, false); }
  };
  q("#pw-go").onclick = async () => {
    note("#ix-msg", "Asking Prowlarr…", true);
    try { const r = await api("/api/grab/prowlarr", { body: { url: q("#pw-url").value, api_key: q("#pw-key").value } }); indexers = r.indexers; drawIx(); note("#ix-msg", r.added.length ? `Added ${r.added.join(", ")}` : "Nothing new to add.", true); } catch (e) { note("#ix-msg", e.message, false); }
  };
  const sab = () => ({ url: q("#sb-url").value, api_key: q("#sb-key").value });
  q("#sb-test").onclick = async () => {
    note("#sb-msg", "Connecting…", true);
    try { const r = await api("/api/grab/sab/test", { body: sab() }); note("#sb-msg", `Connected to SABnzbd ${r.version}. Categories: ${r.categories.join(", ")}${r.category_exists ? "" : " (the one you chose doesn't exist yet: press Create the category)"}`, true); } catch (e) { note("#sb-msg", e.message, false); }
  };
  q("#sb-cat-make").onclick = async () => {
    try { await api("/api/grab/config", { method: "PUT", body: { sab: { ...sab(), category: q("#sb-cat").value } } }); await api("/api/grab/sab/category", { body: { name: q("#sb-cat").value } }); note("#sb-msg", "Category created in SABnzbd.", true); } catch (e) { note("#sb-msg", e.message, false); }
  };
  const qb = () => ({ url: q("#qb-url").value, username: q("#qb-user").value, password: q("#qb-pass").value });
  q("#qb-test").onclick = async () => {
    note("#qb-msg", "Connecting…", true);
    try { const r = await api("/api/grab/qbit/test", { body: qb() }); note("#qb-msg", `Connected to qBittorrent ${r.version}. Categories: ${r.categories.join(", ") || "none"}${r.category_exists ? "" : " (the one you chose doesn't exist yet: press Create the category)"}`, true); } catch (e) { note("#qb-msg", e.message, false); }
  };
  q("#qb-cat-make").onclick = async () => {
    try { await api("/api/grab/config", { method: "PUT", body: { qbit: { ...qb(), category: q("#qb-cat").value } } }); await api("/api/grab/qbit/category", { body: { name: q("#qb-cat").value } }); note("#qb-msg", "Category created in qBittorrent.", true); } catch (e) { note("#qb-msg", e.message, false); }
  };
  q("#s-no").onclick = () => { modal.remove(); done(); };
  q("#s-save").onclick = async () => {
    const pref = q("#pf-region").value;
    const regions = [pref, ...["usa", "free", "pal", "unknown", "japan"].filter((r) => r !== pref)];
    const formats = [q("#pf-iso").checked && "iso", q("#pf-god").checked && "god", q("#pf-pkg").checked && "package"].filter(Boolean);
    if (q("#pf-godfirst").checked) formats.sort((a, b) => (a === "god" ? -1 : b === "god" ? 1 : 0));
    const [lib, pth] = (q("#im-lib").value || ":").split(":");
    const maps = q("#im-maps").value.split("\n").map((l) => l.split(/→|->/).map((x) => x.trim())).filter((x) => x[0] && x[1]).map(([from, to]) => ({ from, to }));
    try {
      await api("/api/grab/config", { method: "PUT", body: {
        sab: { ...sab(), category: q("#sb-cat").value },
        qbit: { ...qb(), category: q("#qb-cat").value }, torrent_import_mode: q("#qb-mode").value, torrent_after_import: q("#qb-after").value,
        profile: { regions, any_region: q("#pf-any").checked, formats, min_iso_mb: +q("#pf-min").value, max_iso_mb: +q("#pf-max").value, min_score: +q("#pf-score").value, prefer_proper: true, reject_words: q("#pf-rej").value.split(",").map((x) => x.trim()).filter(Boolean) },
        import_library: lib ? [+lib, +pth] : null,
        also_drive: q("#im-drive").value ? q("#im-drive").value.split(":").map(Number) : null,
        also_console: q("#im-console").value ? { console_id: +q("#im-console").value, dest: q("#im-cdest").value || null } : null, import_mode: q("#im-mode").value, convert_iso: q("#im-conv").checked, path_maps: maps,
        search_every_hours: +q("#au-h").value, auto_grab: q("#au-grab").checked, auto_import: q("#au-imp").checked,
      } });
      toast("Saved", "ok"); modal.remove(); done();
    } catch (e) { toast(e.message, "err"); }
  };
}
