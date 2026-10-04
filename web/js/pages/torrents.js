// Torrents you already have as files: pick which of the files inside to download with qBittorrent.
// (Searching torrent indexers for a wanted game is on the Wanted page; both end up in the same place.)
import { api, el, esc, fmtBytes, toast } from "../util.js";
import { pickFolder } from "../picker.js";

export const title = "Torrents";

const PAGE = 100;

export async function render(root, ctx) {
  let st = await api("/api/torrents/status");
  let dir = +(sessionStorage.getItem("torrent-dir") || 0), q = "", shown = PAGE, timer, files = [], total = 0, all = 0;
  const $ = (s) => root.querySelector(s);

  async function paint() {
    root.innerHTML = `
      <div class="page-head row"><div class="grow"><h1>🧲 Torrents</h1>
        <div class="muted">Keep folders of <b>.torrent</b> files here, open one to see every file inside, tick the ones you want, and qBittorrent downloads just those. Finished games are imported into your library like any other download.</div></div></div>
      ${st.qbit ? "" : `<div class="notice warn" style="margin-bottom:12px">qBittorrent isn't set up yet. <a href="#wanted" id="go-setup">Open the download setup</a> to add it (you can still look inside torrent files).</div>`}
      ${st.qbit && !st.import_library ? `<div class="notice warn" style="margin-bottom:12px">No library is chosen for finished downloads, so they will download but not be imported. <a href="#wanted" id="go-setup">Choose one in the download setup</a>.</div>` : ""}
      <div class="card"><div class="row"><h2 class="grow" style="margin:0">Folders of torrent files</h2><button class="btn small primary" id="t-add">＋ Add folder</button></div>
        ${st.dirs.length ? `<table style="margin-top:8px"><tbody>${st.dirs.map((d) => `<tr><td class="mono" style="word-break:break-all">${esc(d.path)}</td><td>${d.online ? `<span class="badge ok">${d.count.toLocaleString()} torrent file${d.count === 1 ? "" : "s"}</span>` : `<span class="badge err">offline</span>`}</td><td class="num"><button class="btn small" data-rm="${d.index}" title="Stop listing this folder (its files stay where they are)">Remove</button></td></tr>`).join("")}</tbody></table>`
          : `<div class="empty"><span class="ico">🧲</span>No folders yet. Add the folder where your .torrent files are.</div>`}</div>
      ${st.dirs.length ? `<div class="card"><div class="row"><h2 class="grow" style="margin:0">Torrent files</h2>
        ${st.dirs.length > 1 ? `<select id="t-dir" aria-label="Folder">${st.dirs.map((d) => `<option value="${d.index}" ${d.index === dir ? "selected" : ""}>${esc(d.path.split("/").slice(-2).join("/"))}</option>`).join("")}</select>` : ""}
        <input type="search" id="t-q" placeholder="Search names (words in any order)…" value="${esc(q)}" style="min-width:260px"></div>
        <div id="t-list" style="margin-top:8px"></div><div class="muted" id="t-count" style="margin-top:6px"></div></div>` : ""}
      <div class="card"><div class="row"><h2 class="grow" style="margin:0">Torrent downloads</h2><a class="btn small" href="#wanted" id="go-act">All activity</a></div><div id="t-act" style="margin-top:8px"></div></div>`;
    $("#go-setup")?.addEventListener("click", () => sessionStorage.setItem("wanted-open-setup", "1"));
    $("#go-act").onclick = () => sessionStorage.setItem("wanted-tab", "activity");
    $("#t-add").onclick = addFolder;
    root.querySelectorAll("[data-rm]").forEach((b) => (b.onclick = async () => {
      if (!confirm("Stop listing this folder? The torrent files stay where they are.")) return;
      await save(st.dirs.filter((d) => d.index !== +b.dataset.rm).map((d) => d.path));
    }));
    $("#t-dir")?.addEventListener("change", (e) => { dir = +e.target.value; sessionStorage.setItem("torrent-dir", dir); shown = PAGE; load(); });
    $("#t-q")?.addEventListener("input", (e) => { q = e.target.value; shown = PAGE; clearTimeout(timer); timer = setTimeout(load, 250); });
    if (st.dirs.length) { if (!st.dirs.some((d) => d.index === dir)) dir = 0; await load(); }
    activity();
  }

  async function save(dirs) {
    try { st = await api("/api/torrents/dirs", { method: "PUT", body: { dirs } }); await paint(); }
    catch (e) { toast(e.message, "err"); }
  }

  async function addFolder() {
    const p = await pickFolder();
    if (p) await save([...st.dirs.map((d) => d.path), p]);
  }

  async function load() {
    const d = st.dirs.find((x) => x.index === dir);
    if (!d?.online) { $("#t-list").innerHTML = `<div class="notice err">That folder isn't available right now.</div>`; return; }
    try {
      const r = await api(`/api/torrents/files?dir=${dir}&limit=${shown}&q=${encodeURIComponent(q)}`);
      files = r.files; total = r.total; all = r.all;
    } catch (e) { $("#t-list").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; return; }
    $("#t-list").innerHTML = files.length ? `<table><thead><tr><th>Torrent file</th><th class="num">File size</th><th></th></tr></thead><tbody>${files.map((f, i) => `<tr><td style="word-break:break-all">${esc(f.file.replace(/\.torrent$/i, ""))}</td><td class="num muted">${fmtBytes(f.size)}</td><td class="num"><button class="btn small primary" data-i="${i}">Choose files…</button></td></tr>`).join("")}</tbody></table>`
      : `<div class="empty"><span class="ico">🔍</span>${q ? "No torrent file matches." : "No .torrent files in this folder."}</div>`;
    $("#t-count").innerHTML = files.length < total ? `Showing ${files.length} of ${total.toLocaleString()} · <a href="#" id="t-more">show more</a>` : `${total.toLocaleString()} of ${all.toLocaleString()} torrent file${all === 1 ? "" : "s"}`;
    $("#t-more")?.addEventListener("click", (e) => { e.preventDefault(); shown += PAGE * 2; load(); });
    root.querySelectorAll("[data-i]").forEach((b) => (b.onclick = () => choose(files[+b.dataset.i].file)));
  }

  async function activity() {
    const r = await api("/api/grab/activity").catch(() => ({ grabs: [] }));
    const mine = r.grabs.filter(({ grab: g }) => g.protocol === "torrent").slice(0, 8);
    const label = { queued: "Queued", downloading: "Downloading", completed: "Downloaded", unpacking: "Unpacking", importing: "Importing", imported: "In your library", failed: "Failed", import_failed: "Import failed" };
    const out = $("#t-act");
    if (!out) return;
    out.innerHTML = mine.length ? `<table><tbody>${mine.map(({ grab: g }) => `<tr><td style="word-break:break-all">${esc(g.title)}${g.error ? `<div style="color:var(--err);font-size:12.5px">${esc(g.error)}</div>` : ""}</td>
      <td style="width:170px">${g.status === "downloading" ? `<div class="bar"><i style="width:${Math.round(g.progress)}%"></i></div>` : ""}</td><td><span class="badge ${{ imported: "ok", failed: "err", import_failed: "err", downloading: "accent" }[g.status] || ""}">${label[g.status] || esc(g.status)}${g.status === "downloading" ? " " + Math.round(g.progress) + "%" : ""}</span></td><td class="num muted">${fmtBytes(g.size)}</td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">Nothing yet. Torrents you send from here (or grab from a search) show up as they download.</div>`;
    if (mine.some(({ grab: g }) => ["queued", "downloading", "completed", "unpacking", "importing"].includes(g.status))) {
      clearTimeout(act);
      act = setTimeout(() => { if (location.hash.startsWith("#torrents")) activity(); }, 5000);
    }
  }
  let act;
  ctx.leave.push(() => { clearTimeout(act); clearTimeout(timer); });

  // The dialog: every file in the torrent, with a tick for each one wanted.
  async function choose(file) {
    let info;
    try { info = await api(`/api/torrents/inspect?dir=${dir}&file=${encodeURIComponent(file)}`); }
    catch (e) { toast(e.message, "err"); return; }
    const t = info.torrent, picked = new Set();
    let filter = "", limit = 300;
    const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:min(86vh,820px)"><header class="row"><div class="grow"><b>${esc(t.name)}</b><div class="muted" style="font-size:12.5px;font-weight:400">${t.files.length.toLocaleString()} file${t.files.length === 1 ? "" : "s"} · ${fmtBytes(t.total)} in all · <span class="mono">${esc(t.info_hash.slice(0, 12))}…</span></div></div></header>
      <div class="body" style="padding-top:12px;display:flex;flex-direction:column;gap:10px;min-height:0">
        ${info.already_in_client ? `<div class="notice warn">This torrent is already in qBittorrent. Change which of its files to fetch there.</div>` : ""}
        <div class="row" style="gap:8px"><input type="search" id="c-q" class="grow" placeholder="Filter the files (words in any order)…" autocomplete="off">
          <button class="btn small" id="c-all" title="Tick everything that matches the filter">Tick all matching</button><button class="btn small" id="c-none">Clear all ticks</button></div>
        <div id="c-list" style="overflow:auto;min-height:0;flex:1;border:1px solid var(--line);border-radius:8px"></div></div>
      <footer><span class="muted grow" id="c-sum"></span><button class="btn" id="c-no">Cancel</button><button class="btn primary" id="c-go" disabled>Download the ticked files</button></footer></div></div>`);
    document.body.append(modal);
    const m = (s) => modal.querySelector(s);
    m("#c-no").onclick = () => modal.remove();
    const matches = () => {
      const words = filter.toLowerCase().split(/\s+/).filter(Boolean);
      return t.files.filter((f) => words.every((w) => f.path.toLowerCase().includes(w)));
    };
    const sum = () => {
      const bytes = [...picked].reduce((a, i) => a + t.files[i].size, 0);
      m("#c-sum").textContent = picked.size ? `${picked.size.toLocaleString()} file${picked.size === 1 ? "" : "s"} ticked · ${fmtBytes(bytes)}` : "Tick the files you want";
      m("#c-go").disabled = !picked.size || info.already_in_client;
      m("#c-go").textContent = picked.size ? `Download ${picked.size.toLocaleString()} file${picked.size === 1 ? "" : "s"} (${fmtBytes(bytes)})` : "Download the ticked files";
    };
    const draw = () => {
      const hits = matches(), slice = hits.slice(0, limit);
      m("#c-list").innerHTML = hits.length
        ? `<table style="margin:0"><tbody>${slice.map((f) => {
          const cut = f.path.lastIndexOf("/");
          return `<tr data-i="${f.index}" style="cursor:pointer"><td style="width:30px"><input type="checkbox" ${picked.has(f.index) ? "checked" : ""} aria-label="${esc(f.path)}"></td>
            <td style="word-break:break-all"><b>${esc(f.path.slice(cut + 1))}</b>${cut > 0 ? `<div class="muted mono" style="font-size:11.5px">${esc(f.path.slice(0, cut))}</div>` : ""}</td><td class="num muted" style="white-space:nowrap">${fmtBytes(f.size)}</td></tr>`;
        }).join("")}</tbody></table>${hits.length > limit ? `<div class="muted" style="padding:8px 12px">Showing ${limit} of ${hits.length.toLocaleString()} matches · <a href="#" id="c-more">show more</a> (or narrow the filter)</div>` : `<div class="muted" style="padding:8px 12px">${hits.length.toLocaleString()} match${hits.length === 1 ? "" : "es"}</div>`}`
        : `<div class="empty">No file matches that.</div>`;
      m("#c-list").querySelectorAll("tr[data-i]").forEach((tr) => (tr.onclick = (e) => {
        const i = +tr.dataset.i, box = tr.querySelector("input");
        if (e.target !== box) box.checked = !box.checked;
        if (box.checked) picked.add(i); else picked.delete(i);
        sum();
      }));
      m("#c-more")?.addEventListener("click", (e) => { e.preventDefault(); limit += 500; draw(); });
    };
    m("#c-q").oninput = (e) => { filter = e.target.value; limit = 300; draw(); };
    m("#c-all").onclick = () => { matches().forEach((f) => picked.add(f.index)); draw(); sum(); };
    m("#c-none").onclick = () => { picked.clear(); draw(); sum(); };
    m("#c-go").onclick = async () => {
      m("#c-go").disabled = true;
      try {
        const r = await api("/api/torrents/send", { body: { dir, file, select: picked.size === t.files.length ? null : [...picked].sort((a, b) => a - b) } });
        toast(`Sent to qBittorrent: ${r.title}`, "ok");
        modal.remove();
        activity();
      } catch (e) { toast(e.message, "err"); m("#c-go").disabled = false; }
    };
    draw(); sum();
    m("#c-q").focus();
  }

  await paint();
}
