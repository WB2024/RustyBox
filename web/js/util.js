// Small helpers shared by every page.
export const $ = (sel, root = document) => root.querySelector(sel);
export const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

export const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

export function el(html) {
  const t = document.createElement("template");
  t.innerHTML = html.trim();
  return t.content.firstElementChild;
}

export function toast(msg, kind = "info", ms = 5000) {
  const t = document.createElement("div");
  t.className = "toast " + kind;
  t.textContent = msg;
  $("#toasts").append(t);
  setTimeout(() => t.remove(), ms);
}

export function fmtBytes(n) {
  if (n == null) return "";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return `${n >= 100 || i === 0 ? Math.round(n) : n.toFixed(1)} ${u[i]}`;
}

export function fmtTime(secs) {
  return secs ? new Date(secs * 1000).toLocaleString() : "";
}

// JSON API call. Errors carry the server's structured fields (code, message, status).
export async function api(path, opts = {}) {
  const init = { method: opts.method || "GET", headers: {} };
  if (opts.body !== undefined) {
    init.method = opts.method || "POST";
    init.headers["content-type"] = "application/json";
    init.body = JSON.stringify(opts.body);
  }
  let res;
  try { res = await fetch(path, init); } catch { throw new Error("Cannot reach the RustyBox server"); }
  const text = await res.text();
  let data = null;
  try { data = text ? JSON.parse(text) : null; } catch { /* not JSON */ }
  if (res.status === 401 && data?.error === "AUTH_REQUIRED") { location.reload(); throw new Error("Log in first"); }
  if (!res.ok) {
    const e = new Error(data?.message || text || res.statusText);
    e.code = data?.error;
    e.status = res.status;
    throw e;
  }
  return data;
}
