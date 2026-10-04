import { api, esc, toast } from "../util.js";

export const title = "Settings";

export async function render(root) {
  const auth = await api("/api/auth/status");
  const st = await api("/api/settings");
  root.innerHTML = `
    <div class="page-head"><h1>Settings</h1><div class="muted">Each section saves on its own.</div></div>
    <div class="masonry">
    <div class="card">
      <h2>Conversion</h2>
      <div class="row" style="flex-direction:column;align-items:stretch;gap:10px">
        <label class="field">Worker threads for ISO to GOD (more is faster on an SSD; use 1 for a spinning disk)<input type="number" id="cv-threads" min="1" max="16" value="${st.convert_threads}"></label>
        <label class="field">Default GOD layout<select id="cv-layout"><option value="name_titleid">Game Name / TitleID</option><option value="titleid">TitleID only</option></select></label>
        <div><button class="btn primary" id="cv-save">Save</button></div>
      </div>
    </div>
    <div class="card">
      <h2>Game information (IGDB)</h2>
      <p class="muted">Cover art and details come from <a href="https://www.igdb.com" target="_blank" rel="noopener">IGDB</a>, through a free Twitch application.
        Create one at <a href="https://dev.twitch.tv/console/apps/create" target="_blank" rel="noopener">dev.twitch.tv/console</a>: set the OAuth redirect URL to <span class="mono">https://localhost</span> (it isn't used) and the client type to <b>Confidential</b>.</p>
      ${st.igdb.mock ? `<p class="notice">Mock mode: sample details are used, and nothing is sent to IGDB.</p>` : st.igdb.source === "environment"
        ? `<p class="notice">The credentials are set by environment variables (Client ID ${esc(st.igdb.client_id_hint)}). Change them there.</p>
           <div><button class="btn" id="ig-test">Test connection</button> <span id="ig-result" class="muted"></span></div>`
        : `<div class="row" style="flex-direction:column;align-items:stretch;gap:10px">
            <label class="field">Client ID<input type="text" id="ig-id" value="${esc(st.igdb_client_id)}" autocomplete="off" spellcheck="false"></label>
            <label class="field">Client Secret<input type="password" id="ig-secret" autocomplete="new-password" placeholder="${st.igdb_client_secret_set ? esc(st.igdb_client_secret_hint) + " saved. Leave blank to keep it" : ""}"></label>
            <label class="check"><input type="checkbox" id="ig-auto" ${st.igdb_auto ? "checked" : ""}><span>Look up new games automatically<small>After a scan or conversion, fetch cover art and details for games that don't have them.</small></span></label>
            <div class="row"><button class="btn" id="ig-test">Test connection</button><button class="btn primary" id="ig-save">Save</button>
              ${st.igdb_client_secret_set ? `<button class="btn danger" id="ig-remove">Remove</button>` : ""}<span id="ig-result" class="muted"></span></div>
          </div>`}
    </div>
    <div class="card">
      <h2>Downloads (Usenet and torrents)</h2>
      <p class="muted">Your indexers (or Prowlarr), SABnzbd, qBittorrent, the quality profile and where finished games go are set up in one place, on the Wanted page.</p>
      <div><a class="btn primary" href="#wanted" id="go-usenet">Open the download setup</a></div>
    </div>
    <div class="card">
      <h2>Automation</h2>
      <div class="row" style="flex-direction:column;align-items:stretch;gap:10px">
        <label class="field">Rescan all libraries every (minutes, 5 to 10080; 0 = never)<input type="number" id="au-scan" min="0" max="10080" value="${st.scan_interval_minutes}"></label>
        <label class="field">Tell me when jobs finish<select id="au-on"><option value="failures">Only when something fails</option><option value="all">When any job finishes</option><option value="off">Never</option></select></label>
        <label class="field">Where to send the message (ntfy, Discord, Slack or any web address that takes JSON)<input type="password" id="au-url" autocomplete="off" placeholder="${st.notify_url_set ? esc(st.notify_url_host) + " saved. Leave blank to keep it" : "https://ntfy.sh/my-topic"}"></label>
        <div class="row"><button class="btn primary" id="au-save">Save</button><button class="btn" id="au-test">Send a test message</button>${st.notify_url_set ? `<button class="btn danger" id="au-remove">Remove address</button>` : ""}<span class="muted" id="au-result"></span></div>
      </div>
    </div>
    <div class="card">
      <h2>Security</h2>
      ${auth.from_environment
        ? `<p class="notice">The login is set by environment variables (user <b>${esc(auth.user)}</b>). Change it there.</p>`
        : `<p class="muted">By default anyone on your network can use RustyBox. Turn on a login to require a user name and password.</p>
          <div class="row" style="flex-direction:column;align-items:stretch;gap:10px">
            <label class="check"><input type="checkbox" id="auth-on" ${auth.enabled ? "checked" : ""}><span>Require a login</span></label>
            <label class="field">User name<input type="text" id="auth-user" value="${esc(auth.user || "")}" autocomplete="username"></label>
            <label class="field">Password (at least 8 characters)<input type="password" id="auth-pass" autocomplete="new-password" placeholder="${auth.enabled ? "leave blank to keep the current one" : ""}"></label>
            <div><button class="btn primary" id="auth-save">Save</button></div>
          </div>`}
    </div>
    </div>`;
  root.querySelector("#cv-layout").value = st.god_layout;
  root.querySelector("#cv-save").addEventListener("click", async () => {
    try {
      await api("/api/settings", { method: "PUT", body: { convert_threads: +root.querySelector("#cv-threads").value, god_layout: root.querySelector("#cv-layout").value } });
      toast("Saved", "ok");
    } catch (e) { toast(e.message, "err"); }
  });
  const igResult = (t, ok) => { const r = root.querySelector("#ig-result"); r.textContent = t; r.style.color = ok ? "var(--ok)" : "var(--err)"; };
  root.querySelector("#ig-test")?.addEventListener("click", async () => {
    igResult("Testing…", true);
    try {
      const body = { client_id: root.querySelector("#ig-id")?.value, client_secret: root.querySelector("#ig-secret")?.value };
      const r = await api("/api/igdb/test", { body });
      igResult(`Connected. The access token lasts about ${r.token_days} days and renews itself.`, true);
    } catch (e) { igResult(e.message, false); }
  });
  root.querySelector("#ig-save")?.addEventListener("click", async () => {
    try {
      await api("/api/settings", { method: "PUT", body: { igdb_client_id: root.querySelector("#ig-id").value, igdb_client_secret: root.querySelector("#ig-secret").value, igdb_auto: root.querySelector("#ig-auto").checked } });
      toast("Saved", "ok");
      render(root);
    } catch (e) { toast(e.message, "err"); }
  });
  root.querySelector("#ig-remove")?.addEventListener("click", async () => {
    if (!confirm("Remove the saved IGDB credentials? Game details already fetched are kept.")) return;
    await api("/api/settings", { method: "PUT", body: { igdb_remove: true } });
    render(root);
  });
  root.querySelector("#go-usenet").onclick = () => sessionStorage.setItem("wanted-open-setup", "1");
  root.querySelector("#au-on").value = st.notify_on;
  const note = (id, t, ok) => { const r = root.querySelector(id); r.textContent = t; r.style.color = ok ? "var(--ok)" : "var(--err)"; };
  root.querySelector("#au-save").onclick = async () => {
    try { await api("/api/settings", { method: "PUT", body: { scan_interval_minutes: +root.querySelector("#au-scan").value, notify_on: root.querySelector("#au-on").value, notify_url: root.querySelector("#au-url").value } }); toast("Saved", "ok"); render(root); }
    catch (e) { toast(e.message, "err"); }
  };
  root.querySelector("#au-test").onclick = async () => {
    note("#au-result", "Sending…", true);
    try { await api("/api/notify/test", { body: { notify_url: root.querySelector("#au-url").value } }); note("#au-result", "Sent. Check your phone or channel.", true); } catch (e) { note("#au-result", e.message, false); }
  };
  root.querySelector("#au-remove")?.addEventListener("click", async () => { await api("/api/settings", { method: "PUT", body: { notify_remove: true } }); render(root); });
  root.querySelector("#auth-save")?.addEventListener("click", async () => {
    try {
      const enabled = root.querySelector("#auth-on").checked;
      await api("/api/auth/config", { body: { enabled, username: root.querySelector("#auth-user").value, password: root.querySelector("#auth-pass").value || undefined } });
      toast(enabled ? "Login saved" : "Login turned off", "ok");
      root.querySelector("#auth-pass").value = "";
    } catch (e) { toast(e.message, "err"); }
  });
}
