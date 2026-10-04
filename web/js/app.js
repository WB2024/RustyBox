// Shell: navigation, status, the global active-job banner, and page loading.
import { $, api, esc, toast } from "./util.js";
import { isActive } from "./jobs.js";
import "./modal.js";

// The sidebar is grouped by what you are doing, not by how the program is built.
export const GROUPS = [
  { pages: [{ id: "dashboard", icon: "🏠", label: "Dashboard" }] },
  { label: "Library", pages: [
    { id: "games", icon: "🎮", label: "Games" },
    { id: "libraries", icon: "📚", label: "Libraries" },
    { id: "discover", icon: "🧭", label: "Discover" },
    { id: "wanted", icon: "🔎", label: "Wanted" },
    { id: "torrents", icon: "🧲", label: "Torrents" },
  ] },
  { label: "Xbox 360", pages: [
    { id: "console", icon: "🕹️", label: "Console" },
    { id: "drive", icon: "💽", label: "Xbox drive" },
    { id: "updates", icon: "🩹", label: "Updates" },
    { id: "content", icon: "🧩", label: "Content" },
    { id: "usb", icon: "🔌", label: "USB" },
  ] },
  { label: "Move and fix", pages: [
    { id: "import", icon: "📥", label: "Import" },
    { id: "compare", icon: "⚖️", label: "Compare" },
    { id: "tools", icon: "🧰", label: "Tools" },
  ] },
  { label: "System", pages: [
    { id: "jobs", icon: "⚙️", label: "Jobs", count: true },
    { id: "settings", icon: "🎛️", label: "Settings" },
  ] },
];
export const PAGES = [...GROUPS.flatMap((g) => g.pages), { id: "library", hidden: true, label: "Library" }];

const ctx = {
  status: null,
  params: new URLSearchParams(),
  // Things a page wants undone when the user leaves it (open event streams, timers).
  leave: [],
  onJobsChanged: null,
  openJob(id) { location.hash = `jobs/${id}`; },
};

function nav() {
  $("#nav").innerHTML = GROUPS.map((g) =>
    (g.label ? `<div class="grp">${esc(g.label)}</div>` : "") +
    g.pages.map((p) => `<button data-page="${p.id}">${p.icon} ${esc(p.label)}${p.count ? ` <span class="count hidden" id="jobcount"></span>` : ""}</button>`).join("")).join("");
  $("#nav").onclick = (e) => { const b = e.target.closest("button"); if (b) { location.hash = b.dataset.page; closeMenu(); } };
}

function closeMenu() { document.body.classList.remove("nav-open"); $("#menu").setAttribute("aria-expanded", "false"); }
function toggleMenu() { const o = document.body.classList.toggle("nav-open"); $("#menu").setAttribute("aria-expanded", String(o)); }

// A page that is slow to load must not paint over the one the user has moved on to.
let showing = 0;
const skeleton = `<div class="skeleton" aria-busy="true"><i class="big"></i><i></i><i></i><i></i></div>`;

async function show() {
  const me = ++showing;
  const [path, query = ""] = (location.hash.slice(1) || "dashboard").split("?");
  const [id, arg] = path.split("/");
  ctx.params = new URLSearchParams(query);
  const page = PAGES.find((p) => p.id === id) || PAGES[0];
  const navId = page.id === "library" ? "libraries" : page.id;
  document.querySelectorAll("#nav button").forEach((b) => {
    b.classList.toggle("active", b.dataset.page === navId);
    if (b.dataset.page === navId) b.setAttribute("aria-current", "page"); else b.removeAttribute("aria-current");
  });
  ctx.onJobsChanged = null;
  ctx.leave.splice(0).forEach((f) => { try { f(); } catch { /* nothing to undo */ } });
  // Each page view gets its own container, so a slow page that finishes late can only write
  // into its own, already-replaced one.
  const box = document.createElement("div");
  box.className = "pageview";
  box.innerHTML = skeleton;
  $("#page").replaceChildren(box);
  window.scrollTo(0, 0);
  try {
    const mod = await import(`./pages/${page.id}.js`);
    if (me !== showing) return;
    document.title = `${mod.title} · RustyBox`;
    await mod.render(box, ctx, arg);
    if (me === showing) $("#main").focus({ preventScroll: true });
  } catch (e) {
    if (me !== showing) return;
    box.innerHTML = `<div class="notice err"><b>This page didn't load.</b> ${esc(e.message)}<div style="margin-top:8px"><button class="btn small" id="retry">Try again</button></div></div>`;
    box.querySelector("#retry").onclick = show;
  }
}

// ── Running jobs: the banner, the counts, and a poll that backs off when nothing is happening ──
let jobTimer = null;
async function refreshJobs() {
  clearTimeout(jobTimer);
  let active = [];
  try { active = (await api("/api/jobs?active=1")).filter(isActive); } catch { /* try again later */ }
  const cnt = $("#jobcount");
  if (cnt) { cnt.textContent = active.length; cnt.classList.toggle("hidden", !active.length); }
  const tb = $("#tb-jobs");
  tb.textContent = `${active.length} running`;
  tb.classList.toggle("hidden", !active.length);
  const top = active[0];
  const onJobs = location.hash.startsWith("#jobs");
  $("#banner").classList.toggle("hidden", !top || onJobs);
  if (top) {
    $("#bn-title").textContent = top.title + (active.length > 1 ? `  (+${active.length - 1} more)` : "");
    $("#bn-bar").style.width = top.pct + "%";
    $("#bn-step").textContent = top.step;
    $("#banner").onclick = () => ctx.openJob(top.id);
  }
  ctx.onJobsChanged?.();
  // Quick while something runs, slow when idle, and not at all in a hidden tab.
  if (!document.hidden) jobTimer = setTimeout(refreshJobs, active.length ? 1500 : 6000);
}

function theme() {
  const cur = document.documentElement.dataset.theme
    || (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");
  const next = cur === "light" ? "dark" : "light";
  document.documentElement.dataset.theme = next;
  try { localStorage.setItem("rb-theme", next); } catch { /* private mode */ }
}

async function quickFind() {
  const m = await import("./quickfind.js");
  m.open(PAGES.filter((p) => !p.hidden));
}

async function start() {
  nav();
  $("#menu").onclick = toggleMenu;
  $("#scrim").onclick = closeMenu;
  $("#theme").onclick = theme;
  $("#qf-open").onclick = quickFind;
  $("#qf-open-m").onclick = () => { closeMenu(); quickFind(); };
  document.addEventListener("keydown", (e) => {
    const typing = /^(input|textarea|select)$/i.test(e.target.tagName) || e.target.isContentEditable;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") { e.preventDefault(); quickFind(); }
    else if (e.key === "/" && !typing && !e.ctrlKey && !e.metaKey && !e.altKey) { e.preventDefault(); quickFind(); }
  });
  // The little "More" menus close when you pick something or click away.
  document.addEventListener("click", (e) => {
    document.querySelectorAll("details.menu[open]").forEach((d) => {
      if (!d.contains(e.target) || e.target.closest(".menu-pop button")) d.open = false;
    });
  });
  document.addEventListener("visibilitychange", () => { if (!document.hidden) refreshJobs(); });
  $("#main").tabIndex = -1;
  ctx.status = await api("/api/status");
  $("#ver").textContent = "v" + ctx.status.version;
  $("#mockflag").classList.toggle("hidden", !ctx.status.mock);
  const auth = await api("/api/auth/status");
  if (auth.enabled) {
    const b = $("#logout");
    b.classList.remove("hidden");
    b.onclick = async () => { await api("/api/logout", { method: "POST" }); location.reload(); };
  }
  window.addEventListener("hashchange", () => { show(); refreshJobs(); });
  await show();
  refreshJobs();
}

start().catch((e) => toast(e.message, "err"));
