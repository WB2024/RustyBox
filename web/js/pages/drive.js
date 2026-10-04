import { api, el, esc, fmtBytes, toast } from "../util.js";
import { openSend } from "../import.js";
import { compareView } from "../compare-view.js";
import { tidyDialog } from "../tidy.js";
import { misplacedDialog } from "../misplaced.js";
import * as browserDrive from "../browser-agent.js";

export const title = "Xbox drive";

// Folders worth having on an Xbox drive, with the role each takes.
const QUICK = [
  ["Content", "content", "The console's own folder for DLC, updates and saves"],
  ["Trainers", "trainers", "Trainers for Aurora"],
  ["Homebrew", "homebrew", "Homebrew apps"],
  ["Emulators", "emulators", "Emulators and their ROMs"],
];

const copyText = (t) => navigator.clipboard?.writeText(t).then(() => toast("Copied", "ok"), () => {});

export async function render(root, ctx) {
  const libs = await api("/api/libraries");
  const drives = libs.filter((l) => ["iso", "god"].includes(l.kind) && l.paths.some((p) => p.remote_url)).map((l) => {
    const remote = l.paths.filter((p) => p.remote_url);
    return { lib: l, path: remote.find((p) => !p.role) || remote[0], remote };
  });
  if (!drives.length) return setup(root, ctx);

  const mains = libs.filter((l) => ["iso", "god"].includes(l.kind) && !l.paths.every((p) => p.remote_url));
  let drive = drives.find((d) => String(d.lib.id) === sessionStorage.getItem("xbox-drive")) || drives[0];
  let main = mains.find((l) => String(l.id) === sessionStorage.getItem("xbox-main")) || mains.find((l) => l.kind === "god") || mains[0];
  const $ = (s) => root.querySelector(s);

  async function draw() {
    const h = drive.path.health;
    const isBrowser = (drive.path.remote_url || "").includes("/browser-agent/");
    const mine = isBrowser && browserDrive.drives().find((d) => d.state === "needs-permission");
    const used = h.total ? Math.round(((h.total - h.free) / h.total) * 100) : 0;
    root.innerHTML = `
      <div class="page-head row"><div class="grow"><h1>💽 ${esc(drive.lib.name)}</h1>
        <div class="muted">${h.online ? `<span class="badge ok">online</span>` : `<span class="badge err">offline</span>`} ${h.remote_name ? "attached to " + esc(h.remote_name) + " · " : ""}${esc(h.fs_type || "")}${h.max_file ? " · single files must be under 4 GB" : ""}${h.share_read_only ? " · shared read-only" : ""}</div></div>
        ${drives.length > 1 ? `<select id="d-pick">${drives.map((d) => `<option value="${d.lib.id}" ${d === drive ? "selected" : ""}>${esc(d.lib.name)}</option>`).join("")}</select>` : ""}
        <button class="btn primary" id="d-add" ${drive.path.writable && h.online ? "" : "disabled"}>Add games…</button><button class="btn" id="d-tidy" ${drive.path.writable && h.online ? "" : "disabled"}>Tidy folders…</button><button class="btn" id="d-fix" ${drive.path.writable && h.online ? "" : "disabled"}>Fix misplaced items…</button><button class="btn" id="d-scan">Rescan the drive</button><button class="btn" id="d-browser" title="Share the drive from this browser instead of through an agent program">Use this browser…</button><a class="btn" href="#library/${drive.lib.id}">Open as a library</a></div>
      ${h.online ? `<div class="card">${h.total >= 2 ** 49 ? `<div class="row"><b>Free space isn't known</b><span class="muted">a browser can't see how much room the drive has; a copy stops with a message if it fills up</span></div>` : `<div class="row"><b>${fmtBytes(h.free)} free</b><span class="muted">of ${fmtBytes(h.total)}</span><div class="grow"></div><span class="muted">${used}% used</span></div>
          <div class="bar" style="margin-top:8px"><i style="width:${used}%"></i></div>`}
          ${drive.path.writable ? "" : `<div class="notice warn" style="margin-top:10px">This drive is read-only here${h.share_read_only ? " (the agent was started with --read-only)" : ": turn on Writable for its folder on the library page"}, so games can't be copied to it.</div>`}</div>`
        : isBrowser
          ? `<div class="notice err">The drive isn't reachable: ${esc(h.problem || "")}<br>It is shared from a web browser: open RustyBox in that browser (on the computer the drive is plugged into) and keep a RustyBox page open. ${mine ? `<button class="btn small" id="d-reconnect">Reconnect</button>` : ""} Its games are kept in the list below the last time it was scanned.</div>`
          : `<div class="notice err">The drive isn't reachable: ${esc(h.problem || "")}<br>Is it plugged in, and is the agent running on that computer? Its games are kept in the list below the last time it was scanned.</div>`}
      <div class="card"><div class="row"><label class="field grow" style="max-width:420px">Compare with<select id="d-main">${mains.map((l) => `<option value="${l.id}" ${l === main ? "selected" : ""}>${esc(l.name)} (${l.items} ${l.kind.toUpperCase()})</option>`).join("")}</select></label>
</div></div>
      <details class="card" id="d-folders" ${drive.remote.length > 1 ? "open" : ""}><summary><b>Folders on this drive</b> <span class="muted">(${drive.remote.length}) · what each folder on the drive holds</span></summary>
        <table style="margin-top:8px"><tbody>${drive.remote.map((p) => `<tr><td class="mono">${esc(p.remote_subdir || "(the whole drive)")}</td>
          <td><select data-role="${p.id}"><option value="" ${!p.role ? "selected" : ""}>Games (and anything else)</option>${p.role === "dlc" ? `<option value="dlc" selected>Add-ons (DLC), old setting</option>` : ""}${p.role === "updates" ? `<option value="updates" selected>Title updates, old setting</option>` : ""}<option value="content" ${p.role === "content" ? "selected" : ""}>Console content (DLC, updates, saves)</option><option value="trainers" ${p.role === "trainers" ? "selected" : ""}>Trainers</option><option value="mods" ${p.role === "mods" ? "selected" : ""}>Mods</option><option value="homebrew" ${p.role === "homebrew" ? "selected" : ""}>Homebrew</option><option value="emulators" ${p.role === "emulators" ? "selected" : ""}>Emulators</option></select></td>
          <td class="num">${drive.remote.length > 1 ? `<button class="btn small" data-drop="${p.id}" title="Stop using this folder (its files stay on the drive)">Remove</button>` : ""}</td></tr>`).join("")}</tbody></table>
        <div class="row" style="margin-top:8px"><button class="btn small" id="d-addfolder" ${drive.path.writable ? "" : "disabled"}>Add a folder…</button>${QUICK.filter(([sub, role]) => !drive.remote.some((p) => p.role === role)).map(([sub, role, what]) => `<button class="btn small" data-quick="${sub}|${role}" ${drive.path.writable ? "" : "disabled"} title="${what}">＋ ${sub}</button>`).join("")}</div>
        <div class="muted" style="margin-top:6px;font-size:12.5px"><b>Content</b> is the console's own folder (DLC, updates and saves, in the layout the console needs), so set it to <i>Console content</i>: Tidy never renames anything in it, and <i>Fix misplaced items…</i> moves stray add-ons into it.</div></details>
      <div id="d-out"></div>`;
    $("#d-pick")?.addEventListener("change", (e) => { sessionStorage.setItem("xbox-drive", e.target.value); drive = drives.find((d) => String(d.lib.id) === e.target.value); draw(); });
    $("#d-main")?.addEventListener("change", (e) => { sessionStorage.setItem("xbox-main", e.target.value); main = mains.find((l) => String(l.id) === e.target.value); compare(); });
    $("#d-add").onclick = addGames;
    $("#d-addfolder").onclick = addFolder;
    root.querySelectorAll("[data-quick]").forEach((b) => (b.onclick = async () => {
      const [sub, role] = b.dataset.quick.split("|");
      try {
        await api(`/api/libraries/${drive.lib.id}/paths`, { body: { label: sub, writable: true, role, remote: { same_as: drive.path.id, subdir: sub, create: true } } });
        toast(`Added ${sub}`, "ok");
        location.reload();
      } catch (e) { toast(e.message, "err"); }
    }));
    root.querySelectorAll("[data-role]").forEach((sel) => (sel.onchange = async () => {
      try { await api(`/api/libraries/${drive.lib.id}/paths/${sel.dataset.role}`, { method: "PUT", body: { role: sel.value || "games" } }); toast(sel.value === "content" ? "Saved. Tidy will leave this folder alone." : "Saved.", "ok"); }
      catch (e) { toast(e.message, "err"); }
    }));
    root.querySelectorAll("[data-drop]").forEach((b) => (b.onclick = async () => {
      if (!confirm("Stop using this folder? Its files stay on the drive; RustyBox just forgets them until you add it again.")) return;
      try { await api(`/api/libraries/${drive.lib.id}/paths/${b.dataset.drop}`, { method: "DELETE" }); location.reload(); }
      catch (e) { toast(e.message, "err"); }
    }));
    $("#d-tidy").onclick = () => tidyDialog(drive.lib, ctx);
    $("#d-fix").onclick = () => misplacedDialog(drive.lib, ctx);
    $("#d-browser").onclick = async () => {
      if (!browserDrive.supported()) { alert(`Sharing a drive from the browser needs a secure page in Chrome, Edge or Brave. Open RustyBox at ${browserDrive.secureAddress()} (the browser warns about the certificate once), then try again.`); return; }
      try {
        const handle = await browserDrive.choose(!!drive.path.writable);
        if (!confirm(`Share the folder "${handle.name}" from this browser as the drive "${drive.lib.name}"?\n\nIts folders (${drive.remote.map((p) => p.remote_subdir || "the whole drive").join(", ")}) stay as they are; only how RustyBox reaches the drive changes. Keep a RustyBox page open while you use it.`)) return;
        const shared = await browserDrive.share({ handle, name: drive.lib.name, writable: !!drive.path.writable, fsType: "fat32" });
        await api(`/api/libraries/${drive.lib.id}/reconnect`, { body: { url: shared.url, token: shared.token } });
        toast("Connected from this browser", "ok");
        location.reload();
      } catch (e) { if (e.name !== "AbortError") toast(e.message, "err"); }
    };
    if ($("#d-reconnect")) $("#d-reconnect").onclick = async () => { try { await browserDrive.reconnect(mine.id); toast("Reconnected", "ok"); location.reload(); } catch (e) { toast(e.message, "err"); } };
    $("#d-scan").onclick = async () => { try { const { job } = await api(`/api/libraries/${drive.lib.id}/scan`, { method: "POST" }); ctx.openJob(job); } catch (e) { toast(e.message, "err"); } };
    if (main) compare(); else $("#d-out").innerHTML = `<div class="notice">Add a GOD or ISO library of your own first, then you can compare it with the drive.</div>`;
  }

  function compare() {
    compareView($("#d-out"), {
      A: main, B: drive.lib, bPathId: drive.path.id, ctx,
    });
  }

  // Another folder on the same drive, with a job: games, add-ons or title updates.
  async function addFolder() {
    const modal = el(`<div class="modal-bg"><div class="modal"><header>Add a folder on ${esc(drive.lib.name)}</header><div class="body" style="padding-top:14px;display:flex;flex-direction:column;gap:12px">
      <label class="field">Folder name on the drive<input type="text" id="af-sub" placeholder="DLC" spellcheck="false" autocomplete="off"></label>
      <label class="field">What it holds<select id="af-role"><option value="content" selected>Console content (DLC, updates, saves)</option><option value="trainers">Trainers</option><option value="mods">Mods</option><option value="homebrew">Homebrew</option><option value="emulators">Emulators</option><option value="">Games (and anything else)</option></select></label>
      <label class="check"><input type="checkbox" id="af-make" checked><span>Create it if it isn't there yet</span></label>
      <div class="notice err hidden" id="af-err"></div></div>
      <footer><button class="btn" id="af-no">Cancel</button><button class="btn primary" id="af-go">Add folder</button></footer></div></div>`);
    document.body.append(modal);
    const q = (s) => modal.querySelector(s);
    q("#af-no").onclick = () => modal.remove();
    q("#af-go").onclick = async () => {
      const sub = q("#af-sub").value.trim().replace(/^\/+|\/+$/g, "");
      if (!sub) { q("#af-err").textContent = "Enter the folder's name"; q("#af-err").classList.remove("hidden"); return; }
      try {
        await api(`/api/libraries/${drive.lib.id}/paths`, { body: { label: sub, writable: true, role: q("#af-role").value, remote: { same_as: drive.path.id, subdir: sub, create: q("#af-make").checked } } });
        const { job } = await api(`/api/libraries/${drive.lib.id}/scan`, { method: "POST" });
        modal.remove(); ctx.openJob(job);
      } catch (e) { q("#af-err").textContent = e.message; q("#af-err").classList.remove("hidden"); }
    };
  }

  // Pick any games from any library to put on the drive.
  async function addGames() {
    const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:92vh"><header>Add games to ${esc(drive.lib.name)}</header>
      <div class="body" style="padding-top:14px"><div class="row"><label class="field">From<select id="ag-lib">${mains.map((l) => `<option value="${l.id}" ${l === main ? "selected" : ""}>${esc(l.name)}</option>`).join("")}</select></label>
        <label class="field grow">Search<input type="text" id="ag-q" placeholder="Game name or title ID" autocomplete="off"></label></div>
        <div id="ag-list" style="margin-top:10px"></div></div>
      <footer><span class="muted grow" id="ag-sum"></span><button class="btn" id="ag-no">Cancel</button><button class="btn primary" id="ag-go" disabled>Choose where and how…</button></footer></div></div>`);
    document.body.append(modal);
    const q = (s) => modal.querySelector(s);
    const picked = new Map();
    let items = [];
    const sum = () => { q("#ag-sum").textContent = picked.size ? `${picked.size} selected` : ""; q("#ag-go").disabled = !picked.size; };
    async function load() {
      const r = await api(`/api/libraries/${q("#ag-lib").value}/items?q=${encodeURIComponent(q("#ag-q").value)}&limit=300`).catch((e) => ({ items: [], error: e.message }));
      items = r.items.filter((i) => i.content_kind === "game");
      q("#ag-list").innerHTML = items.length ? `<table><tbody>${items.map((i, n) => `<tr><td style="width:28px"><input type="checkbox" class="ag" data-n="${n}" ${picked.has(i.id) ? "checked" : ""}></td>
        <td>${esc(i.game_name || i.name)}${i.discs > 1 ? ` <span class="badge">disc ${i.disc ?? "?"}/${i.discs}</span>` : ""}${i.health ? ` <span class="badge err">damaged</span>` : ""}<div class="mono muted">${esc(i.relpath)}</div></td><td class="num">${fmtBytes(i.size)}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">${esc(r.error || "No games found.")}</div>`;
      q("#ag-list").querySelectorAll(".ag").forEach((c) => (c.onchange = () => { const it = items[+c.dataset.n]; c.checked ? picked.set(it.id, it) : picked.delete(it.id); sum(); }));
    }
    let t; q("#ag-q").oninput = () => { clearTimeout(t); t = setTimeout(load, 250); };
    q("#ag-lib").onchange = () => { picked.clear(); sum(); load(); };
    q("#ag-no").onclick = () => modal.remove();
    q("#ag-go").onclick = () => {
      const lib = +q("#ag-lib").value;
      modal.remove();
      openSend({ items: [...picked.values()].map((i) => ({ library_id: lib, item_id: i.id })), title: `Add ${picked.size} game${picked.size === 1 ? "" : "s"} to ${drive.lib.name}`, defaultDest: { libraryId: drive.lib.id, pathId: drive.path.id }, onStarted: ctx.openJob });
    };
    sum(); load();
  }

  await draw();
}

// No drive agent connected yet: explain what to do, and let the user add it right here.
function setup(root, ctx) {
  const cmd = "rustybox agent --root /media/you/XBOXDRIVE";
  const ufw = "sudo ufw allow from 192.168.1.0/24 to any port 8099 proto tcp";
  root.innerHTML = `
    <div class="page-head"><h1>💽 Xbox drive</h1><div class="muted">Manage the Xbox's external hard drive from here: see what is on it, what's missing compared with your library, copy games across and remove them.</div></div>
    <div class="card" style="max-width:760px" id="s-browser"></div>
    <div class="card" style="max-width:760px"><h2>Or connect it with the agent program</h2>
      <p class="muted">RustyBox runs on your server, so it can't see a drive plugged into another computer. A small <b>agent</b> on that computer shares the drive's folder with RustyBox.</p>
      <ol style="line-height:1.9;padding-left:20px">
        <li>Plug the drive into the computer and note where it is mounted (for example <span class="mono">/media/you/XBOXDRIVE</span>).</li>
        <li>On that computer run: <span class="mono">${esc(cmd)}</span> <button class="btn small" data-copy="${esc(cmd)}">Copy</button><br><span class="muted">It prints an address and a token.</span></li>
        <li>Let your server through that computer's firewall: <span class="mono">${esc(ufw)}</span> <button class="btn small" data-copy="${esc(ufw)}">Copy</button></li>
        <li>Enter the address and token here:</li></ol>
      <div class="row" style="flex-direction:column;align-items:stretch;gap:10px;max-width:520px">
        <label class="field">Address<input type="text" id="s-url" placeholder="http://192.168.1.48:8099" spellcheck="false"></label>
        <label class="field">Token<input type="password" id="s-token" autocomplete="new-password"></label>
        <label class="field">Folder on the drive that holds the games<input type="text" id="s-sub" value="Games" spellcheck="false"></label>
        <label class="check"><input type="checkbox" id="s-w" checked><span>Allow RustyBox to copy games onto the drive</span></label>
        <div class="row"><button class="btn" id="s-test">Test connection</button><button class="btn primary" id="s-add">Connect the drive</button><span id="s-info" class="muted"></span></div>
        <div class="notice err hidden" id="s-err"></div></div>
      <p class="muted" style="margin-top:14px">More detail, including keeping the agent running in the background: <span class="mono">docs/drives-and-import.md</span>.</p></div>`;
  const $ = (s) => root.querySelector(s);
  browserCard($("#s-browser"), ctx);
  root.querySelectorAll("[data-copy]").forEach((b) => (b.onclick = () => copyText(b.dataset.copy)));
  const remote = () => ({ url: $("#s-url").value, token: $("#s-token").value, subdir: $("#s-sub").value.trim() || null });
  const fail = (m) => { $("#s-err").textContent = m; $("#s-err").classList.toggle("hidden", !m); };
  $("#s-test").onclick = async () => {
    fail(""); $("#s-info").textContent = "Testing…";
    try { const i = await api("/api/remote/test", { body: remote() }); $("#s-info").innerHTML = `<span style="color:var(--ok)">Connected to <b>${esc(i.name)}</b></span> · ${esc(i.fs_type)} · ${fmtBytes(i.free)} free${i.read_only ? " · read-only" : ""}`; }
    catch (e) { $("#s-info").textContent = ""; fail(e.message); }
  };
  $("#s-add").onclick = async () => {
    fail("");
    try {
      const r = await api("/api/libraries", { body: { name: "Xbox drive", kind: "god", paths: [{ remote: remote(), writable: $("#s-w").checked }] } });
      const { job } = await api(`/api/libraries/${r.id}/scan`, { method: "POST" });
      toast("Connected. Reading the drive…", "ok");
      ctx.openJob(job);
    } catch (e) { fail(e.message); }
  };
}

// Share the drive straight from this browser: pick its folder, nothing to install or type.
function browserCard(card, ctx) {
  if (!browserDrive.supported()) {
    const https = location.protocol !== "https:";
    card.innerHTML = `<h2>Connect the drive from this browser</h2>
      <p class="muted">${https
        ? `This only works on a secure page. Open RustyBox at <a href="${esc(browserDrive.secureAddress())}">${esc(browserDrive.secureAddress())}</a> (your browser will warn about the certificate once, because RustyBox made it itself: choose to continue), then come back here.`
        : "This browser can't share a folder. Use Chrome, Edge or Brave."}</p>`;
    return;
  }
  card.innerHTML = `<h2>Connect the drive from this browser</h2>
    <p class="muted">Plug the drive into <b>this computer</b>, then choose its folder. Nothing to install: this page does the file work for RustyBox, so keep a RustyBox page open while you use the drive.</p>
    <div class="row" style="flex-direction:column;align-items:stretch;gap:10px;max-width:520px">
      <label class="check"><input type="checkbox" id="b-w" checked><span>Allow RustyBox to copy games onto the drive (leave off to only look)</span></label>
      <div class="row"><button class="btn primary" id="b-pick">Choose the drive folder…</button><span id="b-folder" class="muted"></span></div>
      <div id="b-more" class="hidden" style="display:flex;flex-direction:column;gap:10px">
        <label class="field">Name<input type="text" id="b-name" spellcheck="false"></label>
        <label class="field">What the drive is formatted as<select id="b-fs"><option value="fat32">FAT32 (the usual for an Xbox drive: files must be under 4 GB)</option><option value="exfat">exFAT</option><option value="ntfs">NTFS</option><option value="other">Something else</option></select></label>
        <label class="field">Folder on the drive that holds the games<input type="text" id="b-sub" value="Games" spellcheck="false"></label>
        <div class="row"><button class="btn primary" id="b-go">Connect the drive</button></div></div>
      <div class="notice err hidden" id="b-err"></div></div>`;
  const $ = (s) => card.querySelector(s);
  let handle;
  const fail = (m) => { $("#b-err").textContent = m; $("#b-err").classList.toggle("hidden", !m); };
  $("#b-pick").onclick = async () => {
    fail("");
    try {
      handle = await browserDrive.choose($("#b-w").checked);
      $("#b-folder").textContent = handle.name;
      $("#b-name").value = $("#b-name").value || handle.name;
      $("#b-more").classList.remove("hidden");
    } catch (e) { if (e.name !== "AbortError") fail(e.message); }
  };
  $("#b-go").onclick = async () => {
    fail("");
    const writable = $("#b-w").checked;
    $("#b-go").disabled = true;
    try {
      const name = $("#b-name").value.trim() || handle.name;
      const shared = await browserDrive.share({ handle, name, writable, fsType: $("#b-fs").value });
      const r = await api("/api/libraries", { body: { name: "Xbox drive", kind: "god", paths: [{ remote: { url: shared.url, token: shared.token, subdir: $("#b-sub").value.trim() || null, create: writable }, writable }] } });
      const { job } = await api(`/api/libraries/${r.id}/scan`, { method: "POST" });
      toast("Connected. Reading the drive…", "ok");
      ctx.openJob(job);
    } catch (e) { fail(e.message); $("#b-go").disabled = false; }
  };
}
