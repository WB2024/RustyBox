// Shared by Wanted and Discover: the interactive release search (results with verdicts and a Grab button).
import { api, el, esc, fmtBytes, toast } from "./util.js";

export const ago = (t) => { if (!t) return ""; const d = Math.floor((Date.now() / 1000 - t) / 86400); return d < 1 ? "today" : d < 30 ? `${d}d` : d < 365 ? `${Math.floor(d / 30)}mo` : `${Math.floor(d / 365)}y`; };
export const since = (t) => { const a = ago(t); return a === "today" ? "today" : a + " ago"; };

export async function searchDialog(w, done) {
  const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:92vh;width:min(1100px,96vw)"><header>Search: ${esc(w.name)}</header><div class="body" style="padding-top:14px"><div id="sr-tabs" class="row" style="gap:6px;margin-bottom:10px;display:none"><button class="btn small primary" data-t="ix">Indexers</button><button class="btn small" data-t="tf">My torrent files</button></div><div id="sr-ctl"></div><div id="sr"><span class="muted">Asking your indexers…</span></div></div>
    <footer><span class="muted grow" id="sr-sum"></span><label class="check"><input type="checkbox" id="sr-rej"><span>Show rejected</span></label><button class="btn" id="sr-no">Close</button></footer></div></div>`);
  document.body.append(modal);
  const q = (s) => modal.querySelector(s);
  q("#sr-no").onclick = () => modal.remove();
  let data, tab = "ix", fdata, tstat;
  const draw = () => {
    if (tab === "tf") return drawFiles();
    q("#sr-ctl").innerHTML = "";
    const showRej = q("#sr-rej").checked;
    const rows = data.results.filter((j) => showRej || !j.verdict.rejected.length);
    q("#sr-sum").textContent = `${data.results.filter((j) => !j.verdict.rejected.length).length} acceptable of ${data.results.length} · ${data.searched} indexer${data.searched === 1 ? "" : "s"} searched${data.errors.length ? " · " + data.errors.length + " failed" : ""}`;
    q("#sr").innerHTML = `${data.errors.map((e) => `<div class="notice err" style="margin-bottom:6px">${esc(e)}</div>`).join("")}${rows.length ? `<table><thead><tr><th>Release</th><th>Region</th><th>Format</th><th class="num">Size</th><th>Age</th><th class="num">Score</th><th></th></tr></thead><tbody>${rows.map((j, i) => {
      const rej = j.verdict.rejected;
      return `<tr style="${rej.length ? "opacity:.7" : ""}"><td style="word-break:break-all">${esc(j.title)}<div class="muted" style="font-size:12px">${esc(j.indexer)}${j.protocol === "torrent" ? ` · <span class="badge">torrent</span> ${j.seeders != null ? `${j.seeders} seeder${j.seeders === 1 ? "" : "s"}` : "seeders unknown"}` : ` · <span class="badge">usenet</span>`}${j.grabs != null ? ` · ${j.grabs} grabs` : ""}</div>${rej.map((x) => `<div style="color:var(--err);font-size:12.5px">⛔ ${esc(x)}</div>`).join("")}</td>
        <td>${esc(j.verdict.parsed.region)}</td><td>${esc(j.verdict.parsed.format.toUpperCase())}${j.verdict.parsed.disc ? ` disc ${j.verdict.parsed.disc}` : ""}</td><td class="num">${fmtBytes(j.size)}</td><td>${ago(j.posted) || "?"}</td><td class="num">${rej.length ? "—" : j.verdict.score}</td>
        <td><button class="btn small ${rej.length ? "" : "primary"}" data-g="${i}">Grab</button></td></tr>`;
    }).join("")}</tbody></table>` : `<div class="muted">Nothing found${showRej ? "" : " that is acceptable (tick Show rejected to see why)"}.</div>`}`;
    q("#sr").querySelectorAll("[data-g]").forEach((b) => (b.onclick = async () => {
      const j = rows[+b.dataset.g];
      if (j.verdict.rejected.length && !confirm(`This release was rejected:\n${j.verdict.rejected.join("\n")}\n\nGrab it anyway?`)) return;
      try { await api(`/api/wanted/${w.id}/grab`, { body: { guid: j.guid, indexer_id: j.indexer_id, force: !!j.verdict.rejected.length } }); toast(`Sent to ${j.protocol === "torrent" ? "qBittorrent" : "SABnzbd"}: ${j.title}`, "ok"); modal.remove(); done(); }
      catch (e) { toast(e.message, "err"); }
    }));
  };
  const drawFiles = () => {
    const showRej = q("#sr-rej").checked;
    if (!q("#sr-ctl").querySelector("#tf-go")) {
      q("#sr-ctl").innerHTML = `<div class="row" style="gap:8px;margin-bottom:6px;flex-wrap:wrap"><select id="tf-dir"><option value="">All folders</option>${tstat.dirs.map((d) => `<option value="${d.index}">${esc(d.path.split("/").slice(-2).join("/"))} (${d.count})</option>`).join("")}</select><button class="btn" id="tf-pick">Choose torrent files…</button><span class="muted" id="tf-sel"></span></div>
        <div class="row" style="gap:8px;margin-bottom:10px;flex-wrap:wrap"><input id="tf-filter" class="grow" style="min-width:220px" placeholder="Or torrent file name contains… (e.g. xbox 360). Blank = the Xbox 360 ones"><button class="btn primary" id="tf-go">Search</button></div>`;
      q("#tf-go").onclick = runFiles;
      q("#tf-filter").onkeydown = (e) => { if (e.key === "Enter") runFiles(); };
      q("#tf-dir").onchange = () => { sel = loadSel(); showSel(); };
      q("#tf-pick").onclick = pickFiles;
      sel = loadSel(); showSel();
    }
    if (!fdata) { q("#sr").innerHTML = `<div class="muted">Search inside the files listed in your .torrent files. Pick a folder, or type part of a torrent file's name to use just that one.</div>`; q("#sr-sum").textContent = ""; return; }
    const rows = fdata.results.filter((j) => showRej || !j.verdict.rejected.length);
    q("#sr-sum").textContent = `${fdata.results.filter((j) => !j.verdict.rejected.length).length} acceptable of ${fdata.results.length} · ${fdata.files.toLocaleString()} files in ${fdata.torrents} torrent${fdata.torrents === 1 ? "" : "s"} searched${fdata.capped ? " · list cut short" : ""}`;
    q("#sr").innerHTML = `${fdata.notes.map((e) => `<div class="notice" style="margin-bottom:6px">${esc(e)}</div>`).join("")}${rows.length ? `<table><thead><tr><th>File</th><th>Region</th><th>Format</th><th class="num">Size</th><th class="num">Score</th><th></th></tr></thead><tbody>${rows.map((j, i) => {
      const rej = j.verdict.rejected;
      return `<tr style="${rej.length ? "opacity:.7" : ""}"><td style="word-break:break-all">${esc(j.title)}<div class="muted" style="font-size:12px">in ${esc(j.torrent_name)} · ${esc(j.torrent_file)}</div>${rej.map((x) => `<div style="color:var(--err);font-size:12.5px">⛔ ${esc(x)}</div>`).join("")}</td>
        <td>${esc(j.verdict.parsed.region)}</td><td>${esc(j.verdict.parsed.format.toUpperCase())}</td><td class="num">${fmtBytes(j.size)}</td><td class="num">${rej.length ? "—" : j.verdict.score}</td>
        <td><button class="btn small ${rej.length ? "" : "primary"}" data-f="${i}">Grab</button></td></tr>`;
    }).join("")}</tbody></table>` : `<div class="muted">Nothing found${showRej ? "" : " that is acceptable (tick Show rejected to see why)"}.</div>`}`;
    q("#sr").querySelectorAll("[data-f]").forEach((b) => (b.onclick = async () => {
      const j = rows[+b.dataset.f];
      if (j.verdict.rejected.length && !confirm(`This file was rejected:\n${j.verdict.rejected.join("\n")}\n\nGrab it anyway?`)) return;
      try { await api(`/api/wanted/${w.id}/grab-file`, { body: { dir: j.dir, torrent_file: j.torrent_file, index: j.index, force: !!j.verdict.rejected.length } }); toast(`Sent to qBittorrent: ${j.title}`, "ok"); modal.remove(); done(); }
      catch (e) { toast(e.message, "err"); }
    }));
  };
  // Which torrent files to search in, remembered per folder.
  let sel = new Set();
  const dirKey = () => { const v = q("#tf-dir")?.value; return v === "" || v == null ? null : "rb-tf-" + tstat.dirs.find((d) => String(d.index) === v)?.path; };
  const loadSel = () => { const k = dirKey(); if (!k) return new Set(); try { return new Set(JSON.parse(localStorage.getItem(k) || "[]")); } catch { return new Set(); } };
  const saveSel = () => { const k = dirKey(); if (k) try { localStorage.setItem(k, JSON.stringify([...sel])); } catch { /* private mode */ } };
  const showSel = () => {
    const one = dirKey() !== null;
    q("#tf-pick").disabled = !one;
    q("#tf-filter").disabled = sel.size > 0;
    q("#tf-sel").innerHTML = !one ? "Pick one folder to choose its torrent files" : sel.size ? `${sel.size} torrent file${sel.size === 1 ? "" : "s"} chosen: <span class="mono">${esc([...sel].slice(0, 2).map((x) => x.split("/").pop()).join(", "))}${sel.size > 2 ? "…" : ""}</span> <button class="btn small" id="tf-clear">Clear</button>` : "none chosen: all torrents matching the name (or the Xbox 360 ones) are searched";
    const c = q("#tf-clear"); if (c) c.onclick = () => { sel = new Set(); saveSel(); showSel(); };
  };
  const pickFiles = () => {
    const dir = q("#tf-dir").value;
    const m = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:90vh;width:min(820px,96vw)"><header>Torrent files to search in</header><div class="body" style="padding-top:12px"><div class="row" style="gap:8px;margin-bottom:8px"><input id="pk-q" class="grow" placeholder="Filter the list (words in any order)"><button class="btn small" id="pk-all">Tick shown</button><button class="btn small" id="pk-none">Clear</button></div><div id="pk-list" style="max-height:52vh;overflow:auto"><span class="muted">Loading…</span></div></div><footer><span class="muted grow" id="pk-sum"></span><button class="btn primary" id="pk-done">Done</button></footer></div></div>`);
    document.body.append(m);
    const p = (x) => m.querySelector(x);
    let shown = [], timer;
    const sum = () => { p("#pk-sum").textContent = `${sel.size} chosen`; };
    const load = async () => {
      try {
        const r = await api(`/api/torrents/files?dir=${dir}&limit=1000&q=${encodeURIComponent(p("#pk-q").value)}`);
        shown = r.files.map((f) => f.file);
        p("#pk-list").innerHTML = shown.length ? shown.map((f, i) => `<label class="check" style="display:flex;padding:3px 0"><input type="checkbox" data-i="${i}" ${sel.has(f) ? "checked" : ""}><span style="word-break:break-all">${esc(f)}</span></label>`).join("") + (r.total > shown.length ? `<div class="muted" style="margin-top:6px">Showing the first ${shown.length} of ${r.total}: type in the filter to narrow it.</div>` : "") : `<div class="muted">No torrent files match.</div>`;
        p("#pk-list").querySelectorAll("input").forEach((c) => (c.onchange = () => { const f = shown[+c.dataset.i]; if (c.checked) sel.add(f); else sel.delete(f); sum(); }));
      } catch (e) { p("#pk-list").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; }
      sum();
    };
    p("#pk-q").oninput = () => { clearTimeout(timer); timer = setTimeout(load, 250); };
    p("#pk-all").onclick = () => { shown.forEach((f) => sel.add(f)); load(); };
    p("#pk-none").onclick = () => { sel = new Set(); load(); };
    p("#pk-done").onclick = () => { m.remove(); saveSel(); showSel(); };
    load();
  };
  const runFiles = async () => {
    q("#sr").innerHTML = `<span class="muted">Reading your torrent files…</span>`;
    const dir = q("#tf-dir").value, filter = q("#tf-filter").value.trim();
    try { fdata = await api(`/api/wanted/${w.id}/search-files`, { body: { dir: dir === "" ? null : +dir, filter, files: dir !== "" ? [...sel] : [] } }); drawFiles(); }
    catch (e) { q("#sr").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; }
  };
  q("#sr-tabs").querySelectorAll("[data-t]").forEach((b) => (b.onclick = () => {
    tab = b.dataset.t;
    q("#sr-tabs").querySelectorAll("[data-t]").forEach((x) => x.classList.toggle("primary", x === b));
    if (tab === "tf") { q("#sr-ctl").innerHTML = ""; drawFiles(); } else if (data) draw(); else q("#sr").innerHTML = `<span class="muted">Asking your indexers…</span>`;
  }));
  api("/api/torrents/status").then((t) => { if (t.qbit && t.dirs.length) { tstat = t; q("#sr-tabs").style.display = "flex"; } }).catch(() => {});
  q("#sr-rej").onchange = () => { if (tab === "tf") drawFiles(); else if (data) draw(); };
  try { data = await api(`/api/wanted/${w.id}/search`, { method: "POST" }); if (tab === "ix") draw(); }
  catch (e) { if (tab === "ix") q("#sr").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; }
}
