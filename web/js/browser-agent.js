// Share a folder of this computer (the Xbox's hard drive) with RustyBox from the browser.
//
// The page picks the folder (File System Access API, Chrome/Edge/Brave on a secure page), then does
// the file work for RustyBox: it keeps asking the server "is there anything to do?", does it, and
// sends the answer back. It speaks the same calls as the drive agent program. The page has to stay
// open (any RustyBox page will do, the loop runs from every one of them).

const DB = "rustybox-browser-drives";
const FREE_UNKNOWN = 2 ** 50; // a browser can't see how much room a drive has
const FAT32_MAX = 4 * 1024 ** 3 - 1;
const MAX_FILE = { fat32: FAT32_MAX };
const WORKERS = 3;

// Resolves once the drives remembered by this browser have been looked at (so a page doesn't
// conclude "no drive here" before that has happened).
let readyDone;
const readyP = new Promise((r) => { readyDone = r; });
export const whenReady = () => Promise.race([readyP, new Promise((r) => setTimeout(r, 3000))]);

export const supported = () => !!(window.isSecureContext && window.showDirectoryPicker);

/** Where to open RustyBox for it to work: the same server over HTTPS. */
export const secureAddress = () => `https://${location.hostname}:8443/`;

// ── Remembering drives (the folder handle survives a reload; permission may need a click) ──

const open = () => new Promise((res, rej) => {
  const r = indexedDB.open(DB, 1);
  r.onupgradeneeded = () => r.result.createObjectStore("drives", { keyPath: "id" });
  r.onsuccess = () => res(r.result);
  r.onerror = () => rej(r.error);
});
const tx = async (mode, f) => {
  const db = await open();
  return new Promise((res, rej) => {
    const t = db.transaction("drives", mode);
    const out = f(t.objectStore("drives"));
    t.oncomplete = () => res(out.result);
    t.onerror = () => rej(t.error);
  });
};
const saveDrive = (d) => tx("readwrite", (s) => s.put(d));
const loadDrives = () => tx("readonly", (s) => s.getAll()).catch(() => []);
export const forget = async (id) => { stop(id); await tx("readwrite", (s) => s.delete(id)); changed(); };

// ── State shown on the Drive page ──

const live = new Map(); // id → { drive, state, message, run }
const listeners = new Set();
export const onChange = (f) => { listeners.add(f); return () => listeners.delete(f); };
const changed = () => listeners.forEach((f) => { try { f(); } catch { /* a listener failing must not stop the others */ } });
export const drives = () => [...live.values()].map((l) => ({ id: l.drive.id, name: l.drive.name, folder: l.drive.handle.name, writable: l.drive.writable, state: l.state, message: l.message || "" }));

// ── Doing the work ──

class Fail extends Error {
  constructor(status, code, message) { super(message); this.status = status; this.code = code; }
}

const parts = (rel) => String(rel || "").split("/").filter((p) => p && p !== ".");
const hidden = (n) => n.startsWith(".") || n.endsWith(".part");

function validWrite(rel) {
  const p = parts(rel);
  if (!p.length || rel.length > 1024 || p.some((c) => c === ".." || c.startsWith(".") || c.length > 255 || /[\u0000-\u001f\u007f\\]/.test(c))) throw new Fail(400, "VALIDATION_ERROR", "That file name isn't allowed");
  if (p[p.length - 1].endsWith(".part")) throw new Fail(400, "VALIDATION_ERROR", "File names can't end in .part");
  return p;
}

function translate(e) {
  if (e instanceof Fail) return e;
  const n = e?.name;
  if (n === "NotFoundError") return new Fail(404, "NOT_FOUND", "That isn't on the drive");
  if (n === "NotAllowedError" || n === "SecurityError") return new Fail(403, "BROWSER_PERMISSION", "The browser no longer lets RustyBox into the folder. Open the Xbox drive page and click Reconnect.");
  if (n === "TypeMismatchError") return new Fail(400, "VALIDATION_ERROR", "That is a folder where a file was expected (or the other way round)");
  if (n === "QuotaExceededError") return new Fail(507, "NO_SPACE", "The drive is full");
  if (n === "InvalidModificationError" || n === "NoModificationAllowedError") return new Fail(409, "CONFLICT", "The drive wouldn't allow that change (is it read-only, or in use?)");
  return new Fail(500, "IO_ERROR", `${n || "Error"}: ${e?.message || e}`);
}

async function dirAt(root, rel, create = false) {
  let d = root;
  for (const p of parts(rel)) {
    if (p === "..") throw new Fail(400, "VALIDATION_ERROR", `Invalid path: ${rel}`);
    d = await d.getDirectoryHandle(p, { create });
  }
  return d;
}

async function handleAt(root, rel) {
  const p = parts(rel);
  if (p.some((c) => c === "..")) throw new Fail(400, "VALIDATION_ERROR", `Invalid path: ${rel}`);
  if (!p.length) return { kind: "directory", handle: root };
  const dir = await dirAt(root, p.slice(0, -1).join("/"));
  const name = p[p.length - 1];
  try { return { kind: "file", handle: await dir.getFileHandle(name), dir, name }; } catch (e) { if (e.name !== "TypeMismatchError" && e.name !== "NotFoundError") throw e; }
  try { return { kind: "directory", handle: await dir.getDirectoryHandle(name), dir, name }; } catch (e) { if (e.name === "NotFoundError") return null; throw e; }
}

const json = (status, body) => ({ status, type: "application/json", body: JSON.stringify(body) });
const ok = (body) => json(200, body);

async function listDir(root, rel) {
  const dir = await dirAt(root, rel);
  const out = [];
  for await (const h of dir.values()) {
    if (hidden(h.name)) continue;
    try {
      if (h.kind === "directory") out.push({ name: h.name, is_dir: true, size: 0, mtime: 0 });
      else { const f = await h.getFile(); out.push({ name: h.name, is_dir: false, size: f.size, mtime: Math.floor(f.lastModified / 1000) }); }
    } catch { /* something we can't stat is skipped, as the agent does */ }
  }
  return out;
}

const locks = new Set();

async function writeChunk(d, q, data) {
  if (!d.writable) throw new Fail(403, "READ_ONLY", "This drive was shared read-only");
  const total = Number(q.get("total"));
  const offset = Number(q.get("offset") || 0);
  const overwrite = q.get("overwrite") === "true";
  if (!Number.isFinite(total)) throw new Fail(400, "VALIDATION_ERROR", "Missing total (the file's full size)");
  if (offset > total) throw new Fail(400, "VALIDATION_ERROR", "offset is past the end of the file");
  const p = validWrite(q.get("path"));
  const name = p[p.length - 1];
  const key = p.join("/");
  if (locks.has(key)) throw new Fail(409, "UPLOAD_IN_PROGRESS", "That file is already being uploaded");
  locks.add(key);
  try {
    const dir = await dirAt(d.handle, p.slice(0, -1).join("/"), offset === 0);
    const partName = `${name}.part`;
    let have = 0;
    try { have = (await (await dir.getFileHandle(partName)).getFile()).size; } catch { /* no part yet */ }
    if (offset !== have) throw new Fail(409, "OFFSET_MISMATCH", `The drive has ${have} bytes of this file, not ${offset}. Ask for the upload status and continue from there.`);
    if (offset === 0) {
      const exists = await dir.getFileHandle(name).then(() => true, () => false);
      if (exists && !overwrite) throw new Fail(409, "EXISTS", "A file with that name is already there");
      const max = MAX_FILE[d.fsType];
      if (max && total > max) throw new Fail(413, "FILE_TOO_BIG", `This drive is ${d.fsType.toUpperCase()} and can't hold a file of ${total} bytes (the limit is 4 GiB).`);
    }
    const part = await dir.getFileHandle(partName, { create: true });
    const w = await part.createWritable({ keepExistingData: offset > 0 });
    if (offset + data.byteLength > total) { await w.truncate(offset); await w.close(); throw new Fail(400, "VALIDATION_ERROR", "More data arrived than the file's declared size"); }
    await w.write({ type: "write", position: offset, data });
    await w.close();
    const now = offset + data.byteLength;
    if (now < total) return ok({ done: false, offset: now });
    // Complete: under the real name. An existing file is replaced only when asked.
    if (overwrite) await dir.removeEntry(name).catch(() => {});
    await finish(dir, part, partName, name);
    return ok({ done: true, offset: now });
  } finally { locks.delete(key); }
}

/** Rename `<name>.part` to `<name>`. Where the browser can't rename, copy it and remove the part. */
async function finish(dir, part, partName, name) {
  if (typeof part.move === "function") {
    try { await part.move(name); return; } catch { /* fall back to copying */ }
  }
  const src = await part.getFile();
  const dest = await dir.getFileHandle(name, { create: true });
  const w = await dest.createWritable();
  try { await src.stream().pipeTo(w); } catch (e) { await dir.removeEntry(name).catch(() => {}); throw e; }
  await dir.removeEntry(partName);
}

async function handle(d, job, body) {
  const q = new URLSearchParams(job.query || "");
  const root = d.handle;
  const must = (f) => { if (!d.writable) throw new Fail(403, "READ_ONLY", "This drive was shared read-only"); return f(); };
  switch (`${job.method} ${job.path}`) {
    case "GET info":
      return ok({ name: d.name, version: "browser", read_only: !d.writable, root: `${root.name} (shared from a browser)`, fs: { fs_type: d.fsType, total: FREE_UNKNOWN, free: FREE_UNKNOWN, max_file: MAX_FILE[d.fsType] ?? null } });
    case "GET list": return ok(await listDir(root, q.get("path")));
    case "GET stat": {
      const h = await handleAt(root, q.get("path"));
      if (!h) return ok({ exists: false, is_dir: false, size: 0 });
      return ok({ exists: true, is_dir: h.kind === "directory", size: h.kind === "file" ? (await h.handle.getFile()).size : 0 });
    }
    case "GET file": {
      const h = await handleAt(root, q.get("path"));
      if (!h) throw new Fail(404, "NOT_FOUND", "That isn't on the drive");
      if (h.kind !== "file") throw new Fail(400, "VALIDATION_ERROR", "That isn't a file");
      const f = await h.handle.getFile();
      const m = /^bytes=(\d*)-(\d*)$/.exec(job.range || "");
      if (!m) return { status: 200, type: "application/octet-stream", body: await f.arrayBuffer() };
      const start = m[1] === "" ? Math.max(0, f.size - Number(m[2])) : Number(m[1]);
      const end = m[1] === "" || m[2] === "" ? f.size - 1 : Math.min(Number(m[2]), f.size - 1);
      if (start >= f.size || end < start) return { status: 416, type: "application/json", body: JSON.stringify({ error: "RANGE", message: "That range is past the end of the file", recoverable: true }), range: `bytes */${f.size}` };
      return { status: 206, type: "application/octet-stream", body: await f.slice(start, end + 1).arrayBuffer(), range: `bytes ${start}-${end}/${f.size}` };
    }
    case "GET write": {
      const p = validWrite(q.get("path"));
      const dir = await dirAt(root, p.slice(0, -1).join("/")).catch(() => null);
      const name = p[p.length - 1];
      const size = async (n) => { try { return (await (await dir.getFileHandle(n)).getFile()).size; } catch { return null; } };
      return ok({ offset: dir ? (await size(`${name}.part`)) ?? 0 : 0, exists: dir ? (await size(name)) !== null : false, free: null });
    }
    case "PUT write": return writeChunk(d, q, body);
    case "POST mkdir": return must(async () => { const p = validWrite(JSON.parse(new TextDecoder().decode(body)).path); await dirAt(root, p.join("/"), true); return ok({ ok: true }); });
    case "POST delete": return must(async () => {
      const p = parts(JSON.parse(new TextDecoder().decode(body)).path);
      if (!p.length) throw new Fail(400, "VALIDATION_ERROR", "The top folder can't be removed");
      const dir = await dirAt(root, p.slice(0, -1).join("/"));
      await dir.removeEntry(p[p.length - 1], { recursive: true });
      return ok({ ok: true });
    });
    case "POST rename": return must(async () => {
      const r = JSON.parse(new TextDecoder().decode(body));
      const from = parts(r.from), to = validWrite(r.to);
      const src = await handleAt(root, from.join("/"));
      if (!src) throw new Fail(404, "NOT_FOUND", `${r.from} doesn't exist`);
      const sameCase = from.join("/").toLowerCase() === to.join("/").toLowerCase();
      const there = await handleAt(root, to.join("/"));
      if (there && !sameCase) throw new Fail(409, "EXISTS", "Something with that name is already there");
      const destDir = await dirAt(root, to.slice(0, -1).join("/"), true);
      if (typeof src.handle.move !== "function") throw new Fail(501, "NOT_SUPPORTED", "This browser can't rename files on the drive. Use a recent Chrome or Edge.");
      await src.handle.move(destDir, to[to.length - 1]);
      return ok({ ok: true });
    });
    case "POST scan": throw new Fail(501, "NOT_SUPPORTED", "Scanning is done by RustyBox itself for a drive shared from a browser");
    default: throw new Fail(404, "NOT_FOUND", `Unknown call ${job.method} ${job.path}`);
  }
}

// ── The loop: ask for work, do it, answer ──

async function pollLoop(l) {
  const d = l.drive;
  const base = `/api/browser-agent/${d.id}`;
  const auth = { Authorization: `Bearer ${d.token}` };
  while (l.run) {
    let job;
    try {
      const res = await fetch(`${base}/poll`, { headers: auth });
      if (res.status === 401) { l.state = "error"; l.message = "RustyBox no longer knows this drive. Connect it again."; changed(); return; }
      if (res.status === 204) continue;
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      job = await res.json();
      if (l.state !== "connected") { l.state = "connected"; l.message = ""; changed(); }
    } catch {
      if (l.state === "connected") { l.state = "reconnecting"; l.message = "Can't reach RustyBox, trying again…"; changed(); }
      await new Promise((r) => setTimeout(r, 2000));
      continue;
    }
    let out;
    try {
      const body = job.body_len > 0 ? new Uint8Array(await (await fetch(`${base}/job/${job.job}/body`, { headers: auth })).arrayBuffer()) : new Uint8Array();
      out = await handle(d, job, body.buffer);
    } catch (e) {
      const f = translate(e);
      out = json(f.status, { error: f.code, message: f.message, recoverable: true });
    }
    try {
      await fetch(`${base}/job/${job.job}/answer`, { method: "POST", headers: { ...auth, "x-agent-status": String(out.status), "x-agent-content-type": out.type, ...(out.range ? { "x-agent-content-range": out.range } : {}) }, body: out.body });
    } catch { /* the server gave up waiting, nothing to do */ }
  }
}

function start(drive) {
  stop(drive.id);
  const l = { drive, state: "connecting", message: "", run: true };
  live.set(drive.id, l);
  for (let i = 0; i < WORKERS; i++) pollLoop(l);
  changed();
}

function stop(id) { const l = live.get(id); if (l) { l.run = false; live.delete(id); } }

// ── Connecting and resuming ──

/** Ask the user for the drive's folder. Must be called from a click. */
export async function choose(writable) {
  return window.showDirectoryPicker({ id: "rustybox-xbox-drive", mode: writable ? "readwrite" : "read" });
}

/** Share a chosen folder: tell the server, remember it, and start answering. Returns what the library needs. */
export async function share({ handle, name, writable, fsType }) {
  const prev = (await loadDrives()).find((d) => d.name === name);
  const res = await fetch("/api/browser-agent/register", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ name, id: prev?.id, token: prev?.token }) });
  const reg = await res.json();
  if (!res.ok) throw new Error(reg.message || "RustyBox refused");
  const drive = { id: reg.id, token: reg.token, name, handle, writable: !!writable, fsType };
  await saveDrive(drive);
  start(drive);
  // Wait until the server sees us asking for work, so the first call doesn't find nobody there.
  for (let i = 0; i < 40 && live.get(drive.id)?.state !== "connected"; i++) await new Promise((r) => setTimeout(r, 150));
  return { url: reg.url, token: reg.token, id: reg.id };
}

/** On every page load: take up the drives shared before, as far as the browser allows without a click. */
export async function resume() {
  try { await resumeAll(); } finally { readyDone(); }
}

async function resumeAll() {
  if (!supported()) return;
  for (const d of await loadDrives()) {
    try {
      const mode = d.writable ? "readwrite" : "read";
      if ((await d.handle.queryPermission({ mode })) === "granted") start(d);
      else { live.set(d.id, { drive: d, state: "needs-permission", message: "Click Reconnect to let RustyBox use the folder again", run: false }); changed(); }
    } catch { /* a folder that has gone away is shown by the Drive page */ }
  }
}

/** Give the permission back after a reload (needs a click). */
export async function reconnect(id) {
  const l = live.get(id);
  if (!l) return;
  const mode = l.drive.writable ? "readwrite" : "read";
  if ((await l.drive.handle.requestPermission({ mode })) !== "granted") throw new Error("The browser didn't give permission");
  start(l.drive);
}
