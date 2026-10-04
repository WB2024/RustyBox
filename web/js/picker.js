// A server-side folder picker: browse the folders RustyBox is allowed to use and choose one.
import { api, el, esc } from "./util.js";

export function pickFolder(start = "") {
  return new Promise((resolve) => {
    const modal = el(`
      <div class="modal-bg"><div class="modal">
        <header>Choose a folder</header>
        <div class="body">
          <div class="crumbs mono" id="pk-path"></div>
          <div id="pk-list"></div>
          <div class="notice err hidden" id="pk-err"></div>
        </div>
        <footer><span class="muted grow mono" id="pk-sel"></span>
          <button class="btn" id="pk-cancel">Cancel</button><button class="btn primary" id="pk-ok" disabled>Use this folder</button></footer>
      </div></div>`);
    document.body.append(modal);
    const q = (s) => modal.querySelector(s);
    let current = "";
    const close = (v) => { modal.remove(); resolve(v); };
    q("#pk-cancel").onclick = () => close(null);
    q("#pk-ok").onclick = () => close(current);
    modal.onclick = (e) => { if (e.target === modal) close(null); };

    async function go(path) {
      try {
        const d = await api("/api/fs/browse?path=" + encodeURIComponent(path));
        q("#pk-err").classList.add("hidden");
        current = d.path;
        q("#pk-path").textContent = d.path || "Allowed folders";
        q("#pk-sel").textContent = d.path;
        q("#pk-ok").disabled = !d.path;
        q("#pk-list").innerHTML =
          (d.path ? `<div class="brow" data-p="${esc(d.parent ?? "")}">⬆️ <span>${d.parent ? "Up" : "All allowed folders"}</span></div>` : "") +
          (d.dirs.length ? d.dirs.map((x) => `<div class="brow" data-p="${esc(x.path)}">📁 <span>${esc(x.name)}</span></div>`).join("")
            : `<div class="muted" style="padding:10px">No sub-folders here.</div>`);
        q("#pk-list").querySelectorAll(".brow").forEach((r) => (r.onclick = () => go(r.dataset.p)));
      } catch (e) {
        const box = q("#pk-err");
        box.textContent = e.message;
        box.classList.remove("hidden");
      }
    }
    go(start);
  });
}

// Add a folder to a library: one on this server (picked from the folders RustyBox may use), or a
// drive on another machine, reached through a RustyBox drive agent. Resolves to a folder
// description for the API, or null if cancelled.
export function chooseFolder() {
  return new Promise((resolve) => {
    const modal = el(`
      <div class="modal-bg"><div class="modal">
        <header>Add a folder</header>
        <div class="body" style="display:flex;flex-direction:column;gap:12px;padding-top:14px">
          <label class="check"><input type="radio" name="fw" value="local" checked><span><b>On this server</b><small>Any folder from the disks and shares mounted into RustyBox.</small></span></label>
          <label class="check"><input type="radio" name="fw" value="remote"><span><b>A drive on another computer</b><small>For example the Xbox's hard drive plugged into your PC. Run <span class="mono">rustybox agent --root /media/you/DRIVE</span> on that computer first.</small></span></label>
          <div id="fw-remote" class="hidden" style="display:none;flex-direction:column;gap:10px">
            <label class="field">Address<input type="text" id="fw-url" placeholder="http://192.168.1.48:8099" spellcheck="false" autocomplete="off"></label>
            <label class="field">Token (shown when the agent starts)<input type="password" id="fw-token" autocomplete="new-password"></label>
            <label class="field">Folder on the drive (optional)<input type="text" id="fw-sub" placeholder="Games" spellcheck="false"><small class="muted">Start inside the drive, for example in the <b>Games</b> folder, so saves and profile folders aren't listed.</small></label>
            <div class="row"><button class="btn small" id="fw-test">Test connection</button><span id="fw-info" class="muted"></span></div>
            <label class="check"><input type="checkbox" id="fw-w"><span>Writable<small>Allow RustyBox to put games on this drive.</small></span></label>
          </div>
          <div class="notice err hidden" id="fw-err"></div>
        </div>
        <footer><button class="btn" id="fw-cancel">Cancel</button><button class="btn primary" id="fw-go">Continue</button></footer>
      </div></div>`);
    document.body.append(modal);
    const q = (s) => modal.querySelector(s);
    const mode = () => modal.querySelector("input[name=fw]:checked").value;
    const sync = () => { q("#fw-remote").style.display = mode() === "remote" ? "flex" : "none"; q("#fw-go").textContent = mode() === "remote" ? "Add drive" : "Choose a folder…"; };
    modal.querySelectorAll("input[name=fw]").forEach((r) => (r.onchange = sync));
    sync();
    const fail = (m) => { q("#fw-err").textContent = m; q("#fw-err").classList.toggle("hidden", !m); };
    const done = (v) => { modal.remove(); resolve(v); };
    q("#fw-cancel").onclick = () => done(null);
    const test = async () => {
      fail("");
      q("#fw-info").textContent = "Testing…";
      try {
        const i = await api("/api/remote/test", { body: { url: q("#fw-url").value, token: q("#fw-token").value } });
        const gb = (n) => (n / 1e9).toFixed(0) + " GB";
        q("#fw-info").innerHTML = `<span style="color:var(--ok)">Connected to <b>${esc(i.name)}</b></span> · ${esc(i.fs_type)} · ${gb(i.free)} free${i.max_file ? " · files must be under 4 GB" : ""}${i.read_only ? " · shared read-only" : ""}`;
        if (i.read_only) { q("#fw-w").checked = false; q("#fw-w").disabled = true; }
        return i;
      } catch (e) { q("#fw-info").textContent = ""; fail(e.message); return null; }
    };
    q("#fw-test").onclick = test;
    q("#fw-go").onclick = async () => {
      fail("");
      if (mode() === "local") {
        modal.style.display = "none";
        const p = await pickFolder();
        if (!p) return done(null);
        return done({ path: p, label: p.split("/").filter(Boolean).pop(), writable: false });
      }
      const i = await test();
      if (!i) return;
      done({ remote: { url: q("#fw-url").value, token: q("#fw-token").value, subdir: q("#fw-sub").value.trim() || null }, label: i.name + (q("#fw-sub").value.trim() ? " / " + q("#fw-sub").value.trim() : ""), writable: q("#fw-w").checked && !i.read_only, info: i });
    };
  });
}

// Choose a folder inside a library folder, which can be on a drive attached to another computer.
// Resolves to the folder's path relative to the library folder ("" for its top), or null if cancelled.
export function pickLibraryFolder(libraryId, pathId, start = "") {
  return new Promise((resolve) => {
    const modal = el(`
      <div class="modal-bg"><div class="modal">
        <header>Choose a folder</header>
        <div class="body"><div class="crumbs mono" id="lf-path"></div><div id="lf-list"></div><div class="notice err hidden" id="lf-err"></div></div>
        <footer><span class="muted grow mono" id="lf-sel"></span><button class="btn" id="lf-cancel">Cancel</button><button class="btn primary" id="lf-ok">Use this folder</button></footer>
      </div></div>`);
    document.body.append(modal);
    const q = (s) => modal.querySelector(s);
    let current = "";
    const close = (v) => { modal.remove(); resolve(v); };
    q("#lf-cancel").onclick = () => close(null);
    q("#lf-ok").onclick = () => close(current);
    async function go(rel) {
      try {
        const d = await api(`/api/libraries/${libraryId}/paths/${pathId}/dirs?path=${encodeURIComponent(rel)}`);
        q("#lf-err").classList.add("hidden");
        current = d.path;
        q("#lf-path").textContent = "/" + d.path;
        q("#lf-sel").textContent = d.path ? d.path : "(the top)";
        q("#lf-list").innerHTML = (d.parent !== null && d.parent !== undefined ? `<div class="brow" data-p="${esc(d.parent)}">⬆️ <span>Up</span></div>` : "") +
          (d.dirs.length ? d.dirs.map((x) => `<div class="brow" data-p="${esc(d.path ? d.path + "/" + x : x)}">📁 <span>${esc(x)}</span></div>`).join("") : `<div class="muted" style="padding:10px">No sub-folders here.</div>`);
        q("#lf-list").querySelectorAll(".brow").forEach((r) => (r.onclick = () => go(r.dataset.p)));
      } catch (e) { q("#lf-err").textContent = e.message; q("#lf-err").classList.remove("hidden"); }
    }
    go(start);
  });
}
