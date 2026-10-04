import { api, esc, fmtBytes, fmtTime } from "../util.js";

export const title = "Dashboard";

const QUICK = [
  ["games", "🎮", "Games", "Every game, and where each copy lives"],
  ["import", "📥", "Import", "Copy, move or convert games into a library"],
  ["drive", "💽", "Xbox drive", "Compare, sync and tidy the console's hard drive"],
  ["console", "🕹️", "Console", "Send games to your Xbox 360 over FTP"],
  ["discover", "🧭", "Discover", "Browse Xbox 360 games and download them"],
  ["wanted", "🔎", "Wanted", "Find games on Usenet and torrents, and import them"],
  ["torrents", "🧲", "Torrents", "Pick files from your .torrent files and download them"],
  ["updates", "🩹", "Title updates", "Find and install updates"],
  ["usb", "🔌", "USB", "Bad Avatar sticks, format and backups"],
];

const DAY = 24 * 3600;

function attention({ libs, jobs, games, igdb, consoles, st }) {
  const out = [];
  for (const l of libs) {
    for (const p of l.paths.filter((p) => !p.health.online)) {
      out.push({ level: "err", href: `library/${l.id}`, text: `${l.name}: “${p.label || p.path}” is offline`, sub: p.health.problem || "Its items are kept and shown as unavailable." });
    }
  }
  const now = Date.now() / 1000;
  const failed = jobs.filter((j) => j.status === "failed" && now - (j.finished || j.started) < DAY).slice(0, 4);
  for (const j of failed) out.push({ level: "err", href: `jobs/${j.id}`, text: `Failed: ${j.title}`, sub: `${j.error?.message?.split("\n")[0] || "See the job for why"} · ${fmtTime(j.finished || j.started)}` });
  const dups = games.filter((g) => g.duplicate).length;
  if (dups) out.push({ level: "warn", href: "games?only=duplicates", text: `${dups} game${dups === 1 ? "" : "s"} ${dups === 1 ? "has" : "have"} the same disc twice in one library`, sub: "Review them on the Games page." });
  if (igdb) {
    const bare = games.filter((g) => !g.info).length;
    if (bare) out.push({ level: "info", href: "games", text: `${bare} game${bare === 1 ? "" : "s"} without cover art or details`, sub: "Use “Fetch game info” on the Games page." });
  } else if (games.length) {
    out.push({ level: "info", href: "settings", text: "Cover art and details are off", sub: "Add your Twitch credentials in Settings to turn on IGDB." });
  }
  if (!consoles.length) out.push({ level: "info", href: "console", text: "No console set up yet", sub: "Add your Xbox 360's address to send games over FTP." });
  const missing = st.tools.filter((t) => !t.found);
  if (missing.length) out.push({ level: "info", href: "settings", text: `${missing.map((t) => t.name).join(", ")} not found`, sub: missing.map((t) => t.purpose).join(" · ") });
  return out;
}

// One bar per disk: folders on the same disk are the same size and fill, so they are shown once.
function disks(libs) {
  const by = new Map();
  for (const l of libs) {
    for (const p of l.paths.filter((p) => p.health.online && p.health.total)) {
      const key = `${p.health.total}|${p.remote_url ? new URL(p.remote_url).host : "here"}|${p.health.fs_type}`;
      const d = by.get(key) || { total: p.health.total, free: p.health.free, fs: p.health.fs_type, remote: !!p.remote_url, names: new Set() };
      d.names.add(p.health.remote_name || p.label || p.path);
      d.free = Math.min(d.free, p.health.free);
      by.set(key, d);
    }
  }
  return [...by.values()];
}

export async function render(root, ctx) {
  const st = ctx.status;
  const [libs, consoles, content, jobs, g, igdbSt] = await Promise.all([
    api("/api/libraries"), api("/api/consoles"), api("/api/content/status"), api("/api/jobs"),
    api("/api/games").catch(() => ({ games: [], total: 0 })), api("/api/igdb/status").catch(() => ({ configured: false })),
  ]);
  const games = g.games;
  const running = jobs.filter((j) => j.status === "running" || j.status === "queued");
  const todo = attention({ libs, jobs, games, igdb: igdbSt.configured, consoles, st });
  const gameBytes = games.reduce((a, x) => a + x.bytes, 0);
  const dk = disks(libs);
  root.innerHTML = `
    <div class="page-head"><h1>Dashboard</h1><div class="muted">RustyBox v${esc(st.version)}${st.mock ? " · mock mode (pretend console and drives)" : ""}</div></div>
    <div class="stats">
      <a class="stat" href="#games"><b>${games.length.toLocaleString()}</b><span>games · ${fmtBytes(gameBytes)}</span></a>
      <a class="stat" href="#libraries"><b>${libs.length}</b><span>libraries</span></a>
      <a class="stat" href="#console"><b>${consoles.length}</b><span>console${consoles.length === 1 ? "" : "s"}</span></a>
      <a class="stat" href="#jobs"><b>${running.length}</b><span>job${running.length === 1 ? "" : "s"} running</span></a>
    </div>
    <div class="grid2" style="align-items:start">
      <div class="card"><h2>Needs attention</h2>
        ${todo.length ? todo.map((a) => `<a class="attn" href="#${a.href}"><span class="dot ${a.level === "warn" ? "" : a.level}"></span><div><div class="t">${esc(a.text)}</div><div class="muted" style="font-size:12.5px">${esc(a.sub || "")}</div></div></a>`).join("")
          : `<div class="empty"><span class="ico">✅</span>Everything looks fine.</div>`}
      </div>
      <div class="card"><h2>Storage</h2>
        ${dk.length ? dk.map((d) => { const used = d.total - d.free, pct = Math.round(used / d.total * 100);
          return `<div style="margin-bottom:12px"><div class="row" style="gap:8px"><b class="grow" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${d.remote ? "🖥 " : "💾 "}${esc([...d.names].slice(0, 3).join(", "))}${d.names.size > 3 ? ` +${d.names.size - 3}` : ""}</b><span class="muted">${fmtBytes(d.free)} free of ${fmtBytes(d.total)}</span></div>
            <div class="meter" style="margin-top:4px" title="${pct}% used${d.fs ? " · " + esc(d.fs) : ""}"><i class="${pct > 92 ? "high" : pct > 80 ? "mid" : ""}" style="width:${pct}%"></i></div></div>`; }).join("")
          : `<div class="empty"><span class="ico">💾</span>No folders are online yet.</div>`}
      </div>
    </div>
    <h2 style="margin:6px 0 10px">Jump to</h2>
    <div class="qa" style="margin-bottom:16px">${QUICK.map(([id, icon, name, hint]) => `<a href="#${id}"><div class="ico">${icon}</div><b>${esc(name)}</b><div class="muted" style="font-size:12.5px">${esc(hint)}</div></a>`).join("")}</div>
    <div class="card">
      <div class="row"><h2 class="grow" style="margin:0">Libraries</h2><a class="btn small" href="#libraries">Manage</a></div>
      ${libs.length ? `<table style="margin-top:8px"><thead><tr><th>Library</th><th>Type</th><th class="num">Items</th><th class="num">Size</th><th>Folders online</th></tr></thead><tbody>
        ${libs.map((l) => { const on = l.paths.filter((p) => p.health.online).length; return `<tr style="cursor:pointer" data-id="${l.id}"><td>${esc(l.icon || "")} ${esc(l.name)}</td><td class="muted">${esc(l.kind_label || l.kind)}</td>
          <td class="num">${l.items.toLocaleString()}</td><td class="num">${fmtBytes(l.bytes)}</td>
          <td><span class="badge ${on === l.paths.length ? "ok" : "err"}">${on} / ${l.paths.length}</span></td></tr>`; }).join("")}</tbody></table>`
        : `<div class="empty"><span class="ico">📚</span>No libraries yet. <a href="#libraries">Create one</a> to get started.</div>`}
      ${st.mock ? `<p><button class="btn small" id="demo">Run a demo job</button></p>` : ""}
    </div>
    <details class="card"><summary style="cursor:pointer"><b>Helper programs</b> <span class="muted">${st.tools.filter((t) => t.found).length} of ${st.tools.length} found</span></summary>
      <table style="margin-top:10px"><thead><tr><th>Program</th><th>Used for</th><th>Status</th></tr></thead><tbody>
      ${st.tools.map((t) => `<tr><td class="mono">${esc(t.name)}</td><td>${esc(t.purpose)}</td><td><span class="badge ${t.found ? "ok" : "err"}">${t.found ? "found" : "missing"}</span></td></tr>`).join("")}
      </tbody></table>
      <p class="muted" style="margin:10px 0 0">The Docker image includes all of them. Format and backup need the host's USB devices passed through to the container.</p>
    </details>`;
  root.querySelectorAll("tr[data-id]").forEach((tr) => (tr.onclick = () => (location.hash = `library/${tr.dataset.id}`)));
  root.querySelector("#demo")?.addEventListener("click", async () => {
    const { job } = await api("/api/jobs/demo", { method: "POST" });
    ctx.openJob(job);
  });
}
