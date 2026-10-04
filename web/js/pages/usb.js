// USB sticks: which are safe to use, formatting, and building a Bad Avatar stick.
import { api, el, esc, fmtBytes, toast } from "../util.js";

export const title = "USB";

export async function render(root, ctx) {
  const [d, src, bk] = await Promise.all([api("/api/usb/devices"), api("/api/usb/badavatar/source"), api("/api/usb/backups")]);
  const $ = (s) => root.querySelector(s);
  const packageDir = src.dir;

  root.innerHTML = `
    <div class="page-head row"><div class="grow"><h1>🔌 USB</h1><div class="muted">Make a Bad Avatar stick for the console, or format a stick as FAT32. Only removable USB sticks are ever offered: hard disks, system disks and anything RustyBox itself lives on are left alone.</div></div><button class="btn" id="u-refresh">Refresh</button></div>
    ${d.mock ? `<div class="notice">Mock mode: these are pretend sticks (folders), so nothing real is touched.</div>` : ""}
    ${d.note ? `<div class="notice warn">${esc(d.note)}</div>` : ""}
    <div class="card"><h2>Exploit package</h2>
      <div class="row" style="flex-wrap:wrap;gap:6px"><span class="badge ${src.ok ? "ok" : "err"}">${src.ok ? "package found" : "package missing"}</span>
        <span class="badge ${src.has_payload ? "ok" : "err"}" title="BadUpdatePayload/default.xex">${src.has_payload ? "payload ✓" : "payload missing"}</span>
        <span class="badge ${src.has_content ? "ok" : "warn"}" title="the avatar profile the exploit loads">${src.has_content ? "Content ✓" : "no Content"}</span>
        <span class="badge ${src.has_aurora ? "ok" : ""}" title="Apps/Aurora">${src.has_aurora ? "Aurora ✓" : "no Aurora (optional)"}</span></div>
      <div class="muted" style="margin-top:6px">${src.ok ? `${src.files} files, ${fmtBytes(src.bytes)} in <span class="mono">${esc(packageDir)}</span> on the server` : esc(src.message)}</div>
      <div class="row" style="margin-top:10px;flex-wrap:wrap">
        <button class="btn" id="pk-folder">Add the package folder…</button>
        <button class="btn ${src.ok && !src.has_payload ? "primary" : ""}" id="pk-fetch" title="Downloads the pinned XeUnshackle release from GitHub">Fetch XeUnshackle (v1.03)</button>
        <button class="btn" id="pk-payload">Add my own payload (default.xex)…</button>
        <button class="btn" id="pk-aurora">Add an Aurora folder…</button>
        ${src.ok ? `<button class="btn danger" id="pk-remove">Remove the package</button>` : ""}<span class="muted" id="pk-status"></span></div>
      <input type="file" id="pk-in-folder" webkitdirectory multiple hidden><input type="file" id="pk-in-payload" accept=".xex" hidden><input type="file" id="pk-in-aurora" webkitdirectory multiple hidden>
      <p class="muted" style="margin-top:8px">RustyBox never ships the exploit files. Pick the unzipped <b>ABadAvatar</b> folder (it holds <span class="mono">BadUpdatePayload/</span> and <span class="mono">Content/</span>), then add a <b>payload</b> (the <b>Fetch XeUnshackle</b> button does it): the exploit runs <span class="mono">BadUpdatePayload/default.xex</span> and the package doesn't include one (use XeUnshackle or FreeMyXe, in retail format). An <b>Aurora</b> folder is optional and goes to <span class="mono">Apps/Aurora</span>. Files are uploaded to the server from your browser.</p></div>
    <div id="u-devs"></div>
    <div class="card"><h2>Backups</h2>${bk.tools_missing ? `<div class="notice warn">${esc(bk.tools_missing)}</div>` : ""}
      <div class="muted">Saved in <span class="mono">${esc(bk.dir)}</span>. Only the used part of a stick is stored, compressed.</div>
      ${bk.backups.length ? `<table style="margin-top:8px"><tbody>${bk.backups.map((b, i) => `<tr><td><b>${esc(b.name)}</b>${b.meta ? `<div class="muted" style="font-size:12.5px">${esc(b.meta.label || "")} · from ${esc(b.meta.source)} · ${fmtBytes(b.meta.size)} stick</div>` : ""}</td><td class="num">${fmtBytes(b.bytes)}</td><td class="num" style="white-space:nowrap"><button class="btn small" data-restore="${i}">Restore…</button> <button class="btn small danger" data-delbk="${i}">Delete</button></td></tr>`).join("")}</tbody></table>` : `<div class="muted" style="margin-top:8px">No backups yet. Use <b>Back up…</b> on a stick above.</div>`}</div>`;
  $("#u-refresh").onclick = () => render(root, ctx);

  // Upload files to the server's package folder, one at a time, keeping their relative paths.
  async function upload(files, mapPath) {
    const list = [...files].map((f) => ({ f, rel: mapPath(f) })).filter((x) => x.rel && !/(^|\/)(\.DS_Store|Thumbs\.db)$/.test(x.rel));
    const st = $("#pk-status");
    try {
      for (let i = 0; i < list.length; i++) {
        st.textContent = `Uploading ${i + 1} of ${list.length}: ${list[i].rel}`;
        const r = await fetch(`/api/usb/badavatar/file?path=${encodeURIComponent(list[i].rel)}`, { method: "PUT", headers: { "content-type": "application/octet-stream" }, body: list[i].f });
        if (!r.ok) throw new Error((await r.json().catch(() => ({}))).message || r.statusText);
      }
      toast(`${list.length} file${list.length === 1 ? "" : "s"} uploaded`, "ok");
      render(root, ctx);
    } catch (e) { st.textContent = ""; toast(e.message, "err"); }
  }
  const afterFirst = (f) => (f.webkitRelativePath || f.name).split("/").slice(1).join("/");
  $("#pk-folder").onclick = () => $("#pk-in-folder").click();
  $("#pk-in-folder").onchange = (e) => upload(e.target.files, afterFirst);
  $("#pk-fetch").onclick = async () => {
    if (!confirm("Download XeUnshackle BETA v1.03 (by Byrom90) from its GitHub release and add it to the package? It provides BadUpdatePayload/default.xex, a launch.ini and a few helper programs.")) return;
    try { const { job } = await api("/api/usb/badavatar/payload", { method: "POST" }); ctx.openJob(job); } catch (e) { toast(e.message, "err"); }
  };
  $("#pk-payload").onclick = () => $("#pk-in-payload").click();
  $("#pk-in-payload").onchange = (e) => upload(e.target.files, () => "BadUpdatePayload/default.xex");
  $("#pk-aurora").onclick = () => $("#pk-in-aurora").click();
  $("#pk-in-aurora").onchange = (e) => upload(e.target.files, (f) => { const r = afterFirst(f); return r ? "Apps/Aurora/" + r : ""; });
  $("#pk-remove")?.addEventListener("click", async () => {
    if (!confirm("Remove the uploaded exploit package from the server? Sticks already built are not affected.")) return;
    try { await api("/api/usb/badavatar/package?confirm=true", { method: "DELETE" }); render(root, ctx); } catch (e) { toast(e.message, "err"); }
  });

  const out = $("#u-devs");
  out.innerHTML = d.devices.length ? d.devices.map((v, i) => `<div class="card" style="${v.eligible ? "" : "opacity:.65"}"><div class="row"><div class="grow"><b>${esc((v.vendor + " " + v.model).trim() || v.path)}</b> <span class="mono muted">${esc(v.path)}</span>
      <div class="muted">${fmtBytes(v.size)}${v.fstype ? " · " + esc(v.fstype) : ""}${v.label ? " · " + esc(v.label) : ""}${v.mountpoints.length ? " · mounted at <span class='mono'>" + v.mountpoints.map(esc).join(", ") + "</span>" : " · not mounted"}</div>
      ${v.eligible ? "" : `<div style="color:var(--warn)">Off limits: ${esc(v.why_not)}</div>`}</div>
      ${v.eligible ? `<button class="btn primary" data-build="${i}" ${src.ok ? "" : "disabled"} title="${src.ok ? "" : "Add the exploit package first"}">Build a Bad Avatar stick…</button><button class="btn" data-backup="${i}" ${v.fs_path && !bk.tools_missing ? "" : "disabled"}>Back up…</button><button class="btn danger" data-format="${i}">Format as FAT32…</button>` : ""}</div></div>`).join("")
    : `<div class="card muted">No USB sticks found. Plug one in and press Refresh.</div>`;
  out.querySelectorAll("[data-build]").forEach((b) => (b.onclick = () => buildDialog(d.devices[+b.dataset.build])));
  out.querySelectorAll("[data-format]").forEach((b) => (b.onclick = () => formatDialog(d.devices[+b.dataset.format])));

  out.querySelectorAll("[data-backup]").forEach((b) => (b.onclick = async () => {
    const dev = d.devices[+b.dataset.backup];
    const name = prompt("Name for this backup", (dev.label || "stick") + "-" + new Date().toISOString().slice(0, 10));
    if (!name) return;
    try { const { job } = await api("/api/usb/backup", { body: { device: dev.path, name } }); ctx.openJob(job); } catch (e) { toast(e.message, "err"); }
  }));
  root.querySelectorAll("[data-delbk]").forEach((b) => (b.onclick = async () => {
    const bak = bk.backups[+b.dataset.delbk];
    if (!confirm(`Delete the backup ${bak.name}? This can't be undone.`)) return;
    try { await api(`/api/usb/backups?name=${encodeURIComponent(bak.name)}`, { method: "DELETE" }); render(root, ctx); } catch (e) { toast(e.message, "err"); }
  }));
  root.querySelectorAll("[data-restore]").forEach((b) => (b.onclick = () => {
    const bak = bk.backups[+b.dataset.restore];
    const choices = d.devices.filter((v) => v.eligible);
    if (!choices.length) return toast("Plug in a stick first", "err");
    const modal = el(`<div class="modal-bg"><div class="modal"><header>Restore ${esc(bak.name)}</header><div class="body" style="padding-top:14px;display:flex;flex-direction:column;gap:10px">
      <div class="notice err">This <b>erases everything</b> on the stick you choose and puts the backup on it. The stick must be at least as big as the original.</div>
      <label class="field">Restore onto<select id="r-dev">${choices.map((v) => `<option value="${esc(v.path)}">${esc((v.vendor + " " + v.model).trim())} · ${esc(v.path)} · ${fmtBytes(v.size)}</option>`).join("")}</select></label>
      <label class="field">Type the device path to confirm<input type="text" id="r-confirm" autocomplete="off" spellcheck="false"></label></div>
      <footer><button class="btn" id="r-no">Cancel</button><button class="btn danger" id="r-go" disabled>Restore</button></footer></div></div>`);
    document.body.append(modal);
    const m = (q) => modal.querySelector(q);
    const sync = () => (m("#r-go").disabled = m("#r-confirm").value.trim() !== m("#r-dev").value);
    m("#r-confirm").oninput = sync; m("#r-dev").onchange = sync;
    m("#r-no").onclick = () => modal.remove();
    m("#r-go").onclick = async () => { try { const { job } = await api("/api/usb/restore", { body: { backup: bak.name, device: m("#r-dev").value, confirm: m("#r-confirm").value.trim() } }); modal.remove(); ctx.openJob(job); } catch (e) { toast(e.message, "err"); } };
  }));

  function formatDialog(dev) {
    const modal = el(`<div class="modal-bg"><div class="modal"><header>Format ${esc(dev.path)}</header><div class="body" style="padding-top:14px;display:flex;flex-direction:column;gap:10px">
      <div class="notice err">This <b>erases everything</b> on ${esc((dev.vendor + " " + dev.model).trim())} (${fmtBytes(dev.size)}) and makes it one FAT32 volume with no partition table.</div>
      <label class="field">Name<input type="text" id="f-label" value="XBOX360" maxlength="11"></label>
      <label class="field">Type <span class="mono">${esc(dev.path)}</span> to confirm<input type="text" id="f-confirm" autocomplete="off" spellcheck="false"></label></div>
      <footer><button class="btn" id="f-no">Cancel</button><button class="btn danger" id="f-go" disabled>Format</button></footer></div></div>`);
    document.body.append(modal);
    const m = (s) => modal.querySelector(s);
    m("#f-confirm").oninput = () => (m("#f-go").disabled = m("#f-confirm").value.trim() !== dev.path);
    m("#f-no").onclick = () => modal.remove();
    m("#f-go").onclick = async () => { try { const { job } = await api("/api/usb/format", { body: { device: dev.path, label: m("#f-label").value, confirm: m("#f-confirm").value.trim() } }); modal.remove(); ctx.openJob(job); } catch (e) { toast(e.message, "err"); } };
  }

  function buildDialog(dev) {
    const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:92vh"><header>Build a Bad Avatar stick on ${esc(dev.path)}</header><div class="body" style="padding-top:14px">
      <div class="card"><label class="check"><input type="checkbox" id="b-format" ${dev.fstype === "vfat" ? "" : "checked"}><span>Format the stick first (erases everything)<small>Gives a clean FAT32 stick named BADUPDATE. Needed if the stick isn't FAT32 or has other things on it.</small></span></label>
        <label class="check" style="margin-top:8px"><input type="checkbox" id="b-default" checked><span>Make Aurora start automatically<small>Sets <span class="mono">Default =</span> in launch.ini. Leave it off to choose the program in DashLaunch yourself.</small></span></label></div>
      <div class="card"><h2>Plan</h2><div id="b-plan" class="muted">Checking…</div></div>
      <div id="b-confirm" class="hidden"><label class="field">Formatting erases everything. Type <span class="mono">${esc(dev.path)}</span> to confirm<input type="text" id="b-typed" autocomplete="off" spellcheck="false"></label></div></div>
      <footer><button class="btn" id="b-no">Close</button><button class="btn primary" id="b-go" disabled>Build the stick</button></footer></div></div>`);
    document.body.append(modal);
    const m = (s) => modal.querySelector(s);
    const body = () => ({ device: dev.path, format: m("#b-format").checked, set_default: m("#b-default").checked, label: "BADUPDATE", confirm: m("#b-typed").value.trim() });
    let ok = false;
    const sync = () => { m("#b-confirm").classList.toggle("hidden", !m("#b-format").checked); m("#b-go").disabled = !ok || (m("#b-format").checked && m("#b-typed").value.trim() !== dev.path); };
    async function plan() {
      try {
        const r = await api("/api/usb/badavatar/plan", { body: body() });
        const p = r.plan; ok = p.ok;
        m("#b-plan").innerHTML = `<ol style="padding-left:20px;line-height:1.7">${p.steps.map((s) => `<li>${esc(s)}</li>`).join("")}</ol>${p.problems.map((x) => `<div class="notice err" style="margin-top:6px">${esc(x)}</div>`).join("")}${p.warnings.map((x) => `<div style="color:var(--warn);margin-top:6px">⚠ ${esc(x)}</div>`).join("")}<div class="muted" style="margin-top:6px">${p.files} files · ${fmtBytes(p.bytes)}</div>`;
      } catch (e) { ok = false; m("#b-plan").innerHTML = `<div class="notice err">${esc(e.message)}</div>`; }
      sync();
    }
    ["#b-format", "#b-default"].forEach((s) => (m(s).onchange = plan));
    m("#b-typed").oninput = sync;
    m("#b-no").onclick = () => modal.remove();
    m("#b-go").onclick = async () => { try { const { job } = await api("/api/usb/badavatar/start", { body: body() }); modal.remove(); ctx.openJob(job); } catch (e) { toast(e.message, "err"); } };
    plan();
  }
}
