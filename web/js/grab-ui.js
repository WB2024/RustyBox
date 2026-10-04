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
      q("#sr-ctl").innerHTML = `<div class="row" style="gap:8px;margin-bottom:10px;flex-wrap:wrap"><select id="tf-dir"><option value="">All folders</option>${tstat.dirs.map((d) => `<option value="${d.index}">${esc(d.path)} (${d.count})</option>`).join("")}</select><input id="tf-filter" class="grow" style="min-width:220px" placeholder="Torrent file name contains… (e.g. redump), blank = Xbox 360 torrents"><button class="btn primary" id="tf-go">Search</button></div>`;
      q("#tf-go").onclick = runFiles;
      q("#tf-filter").onkeydown = (e) => { if (e.key === "Enter") runFiles(); };
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
  const runFiles = async () => {
    q("#sr").innerHTML = `<span class="muted">Reading your torrent files…</span>`;
    const dir = q("#tf-dir").value, filter = q("#tf-filter").value.trim();
    try { fdata = await api(`/api/wanted/${w.id}/search-files`, { body: { dir: dir === "" ? null : +dir, filter } }); drawFiles(); }
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
