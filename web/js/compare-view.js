// The comparison of two libraries, shared by the Compare page and the Xbox drive page: what is in
// both, what is only in one, damaged copies and duplicates, with copy / move / replace / delete.
import { api, el, esc, fmtBytes, toast } from "./util.js";
import { openSend } from "./import.js";

const plural = (n, w, many = w + "s") => `${n} ${n === 1 ? w : many}`;
const name = (m) => m.name || m.title_id;
const disc = (m) => (m.discs > 1 ? ` <span class="badge">disc ${m.disc ?? "?"}/${m.discs}</span>` : "");
const bytes = (cs) => cs.reduce((n, x) => n + (x.size || 0), 0);
const bad = (cs) => cs.find((x) => x.health);
const firstOk = (m, side) => (m[side] || []).find((x) => x.available);
const target = (lib, pathId) => lib.paths.find((p) => (pathId ? p.id === pathId : p.writable && p.health.online) && p.writable && p.health.online);

/** Ask before deleting games from a library (here or on a drive). Resolves true if a job was started. */
export async function removeDialog(lib, itemIds, ctx, why = "") {
  let plan;
  try { plan = await api(`/api/libraries/${lib.id}/items/remove`, { body: { item_ids: itemIds } }); } catch (e) { toast(e.message, "err"); return false; }
  return new Promise((resolve) => {
    const modal = el(`<div class="modal-bg"><div class="modal"><header>Delete from ${esc(lib.name)}?</header><div class="body" style="padding-top:14px">
      <div class="notice err">This <b>deletes</b> ${plural(plan.items.length, "game")} from ${esc(lib.name)} (${fmtBytes(plan.bytes)}). It can't be undone.${why ? " " + esc(why) : ""}</div>
      <table style="margin-top:10px"><tbody>${plan.items.map((i) => `<tr><td>${esc(i.name)}<div class="mono muted">${esc(i.relpath)}</div></td><td class="num">${fmtBytes(i.size)}</td></tr>`).join("")}</tbody></table></div>
      <footer><button class="btn" id="rm-no">Cancel</button><button class="btn danger" id="rm-yes">Delete ${plural(plan.items.length, "game")}</button></footer></div></div>`);
    document.body.append(modal);
    modal.querySelector("#rm-no").onclick = () => { modal.remove(); resolve(false); };
    modal.querySelector("#rm-yes").onclick = async () => {
      try { const { job } = await api(`/api/libraries/${lib.id}/items/remove`, { body: { item_ids: itemIds, confirm: true } }); modal.remove(); ctx.openJob(job); resolve(true); }
      catch (e) { toast(e.message, "err"); }
    };
  });
}

/** Duplicates in one library: show what would be kept and removed, and why. */
async function duplicatesDialog(lib, ctx) {
  let d;
  try { d = await api(`/api/libraries/${lib.id}/duplicates`); } catch (e) { return toast(e.message, "err"); }
  if (!d.groups.length) return toast(`No duplicates in ${lib.name}`, "ok");
  const modal = el(`<div class="modal-bg"><div class="modal wide" style="height:auto;max-height:92vh"><header>Duplicates in ${esc(lib.name)}</header><div class="body" style="padding-top:14px">
    <div class="notice">${plural(d.groups.length, "game")} ${d.groups.length === 1 ? "is" : "are"} here more than once. The best copy of each is kept: a reachable one, then one that isn't damaged, then the larger, then the one already in the tidy place. Untick any copy you want to keep as well.</div>
    ${d.groups.map((g) => `<div class="card" style="margin-top:10px"><b>${esc(g.name)}</b>${g.disc ? ` <span class="badge">disc ${g.disc}</span>` : ""} <span class="mono muted">${esc(g.title_id)}</span>
      <table style="margin-top:6px"><tbody>${g.items.map((i) => `<tr><td style="width:28px">${i.keep ? "✅" : `<input type="checkbox" class="dd" data-id="${i.item_id}" ${i.available && i.writable ? "checked" : "disabled"}>`}</td>
        <td><span class="mono">${esc(i.path_label)} / ${esc(i.relpath)}</span><div class="muted" style="font-size:12.5px">${esc(i.reason)}${i.keep || (i.available && i.writable) ? "" : " (can't be removed: the folder is offline or read-only)"}</div></td><td class="num">${fmtBytes(i.size)}</td></tr>`).join("")}</tbody></table></div>`).join("")}
    </div><footer><span class="muted grow" id="dd-sum"></span><button class="btn" id="dd-no">Cancel</button><button class="btn danger" id="dd-go">Review and delete…</button></footer></div></div>`);
  document.body.append(modal);
  const q = (s) => modal.querySelector(s);
  const chosen = () => [...modal.querySelectorAll(".dd:checked")].map((c) => +c.dataset.id);
  const sum = () => { const ids = new Set(chosen()); const b = d.groups.flatMap((g) => g.items).filter((i) => ids.has(i.item_id)).reduce((n, i) => n + i.size, 0); q("#dd-sum").textContent = `${plural(ids.size, "copy", "copies")} to delete, ${fmtBytes(b)} freed`; q("#dd-go").disabled = !ids.size; };
  modal.querySelectorAll(".dd").forEach((c) => (c.onchange = sum)); sum();
  q("#dd-no").onclick = () => modal.remove();
  q("#dd-go").onclick = async () => { if (await removeDialog(lib, chosen(), ctx, "Each game keeps its best copy.")) modal.remove(); };
}

/** Mirror: make B match A. Shows exactly what will be copied and (if asked) removed. */
function mirrorDialog(c, A, B, bPathId, ctx) {
  const missing = c.only_a, extras = c.only_b;
  const damaged = c.both.filter((m) => bad(m.b) && firstOk(m, "a") && !bad(m.a));
  const dest = target(B, bPathId);
  const items = [...missing, ...damaged].map((m) => firstOk(m, "a")).filter(Boolean).map((x) => ({ library_id: x.library_id, item_id: x.item_id }));
  const extraIds = extras.flatMap((m) => m.b.map((x) => x.item_id));
  const modal = el(`<div class="modal-bg"><div class="modal"><header>Mirror ${esc(A.name)} onto ${esc(B.name)}</header><div class="body" style="padding-top:14px">
    <p>After this, ${esc(B.name)} has the same games as ${esc(A.name)}, in the same folder layout.</p>
    <ul style="line-height:1.8"><li><b>${plural(missing.length, "game")}</b> missing from ${esc(B.name)} (${fmtBytes(missing.reduce((n, m) => n + bytes(m.a), 0))}) will be copied across.</li>
      <li><b>${plural(damaged.length, "damaged copy", "damaged copies")}</b> on ${esc(B.name)} will be replaced with the good copy.</li>
      <li><b>${plural(extras.length, "game")}</b> are only on ${esc(B.name)} (${fmtBytes(extras.reduce((n, m) => n + bytes(m.b), 0))}).</li></ul>
    <label class="check"><input type="checkbox" id="mi-rm" ${extras.length ? "" : "disabled"}><span>Also delete those ${extras.length} from ${esc(B.name)}<small>Only the games ${esc(A.name)} doesn't have. You'll be asked to confirm the list before anything is deleted.</small></span></label>
    <div class="notice hidden err" id="mi-err" style="margin-top:10px"></div></div>
    <footer><button class="btn" id="mi-no">Cancel</button><button class="btn primary" id="mi-go" ${dest && (items.length || extras.length) ? "" : "disabled"}>${items.length ? "Plan the copy…" : "Continue"}</button></footer></div></div>`);
  document.body.append(modal);
  const q = (s) => modal.querySelector(s);
  q("#mi-no").onclick = () => modal.remove();
  q("#mi-go").onclick = async () => {
    const rm = q("#mi-rm").checked;
    modal.remove();
    if (rm && extraIds.length) { const started = await removeDialog(B, extraIds, ctx, `They aren't in ${A.name}.`); if (!started) return; }
    if (items.length) openSend({ items, title: `Mirror ${A.name} onto ${B.name}`, defaultDest: { libraryId: B.id, pathId: dest.id }, replace: damaged.length > 0, onStarted: ctx.openJob });
  };
}

/**
 * Draw the comparison of libraries `A` and `B` into `out`. `bPathId` picks the folder of B that
 * things are sent to (for a drive with several folders). `extras(c)` may add HTML above the sections.
 */
export async function compareView(out, { A, B, bPathId, ctx, onChange }) {
  out.innerHTML = `<span class="muted">Comparing…</span>`;
  let c, dupA = { groups: [] }, dupB = { groups: [] };
  try {
    [c, dupA, dupB] = await Promise.all([api(`/api/compare?a=${A.id}&b=${B.id}`), api(`/api/libraries/${A.id}/duplicates`).catch(() => ({ groups: [] })), api(`/api/libraries/${B.id}/duplicates`).catch(() => ({ groups: [] }))]);
  } catch (e) { out.innerHTML = `<div class="notice err">${esc(e.message)}</div>`; return; }
  const bDest = target(B, bPathId), aDest = target(A);
  const damaged = c.both.filter((m) => bad(m.b)), damagedOnly = c.only_b.filter((m) => bad(m.b));
  const fixable = damaged.filter((m) => firstOk(m, "a") && !bad(m.a));
  const missingBytes = c.only_a.reduce((n, m) => n + bytes(m.a), 0);
  const bFree = bDest?.health?.free || 0;
  const differ = c.both.filter((m) => m.a[0] && m.b[0] && m.a[0].relpath !== m.b[0].relpath);

  const table = (list, side, show) => list.length ? `<table style="margin-top:8px"><thead><tr><th style="width:28px"><input type="checkbox" data-all="${side}"></th><th>Game</th><th>Title ID</th><th>Where</th><th class="num">Size</th></tr></thead><tbody>
    ${list.map((m, i) => { const cs = show(m); return `<tr><td><input type="checkbox" class="sel" data-side="${side}" data-i="${i}"></td>
      <td>${esc(name(m))}${disc(m)}${bad(m.b) && side !== "only-a" ? ` <span class="badge err">damaged</span><div class="muted" style="font-size:12.5px">${esc(bad(m.b).health)}</div>` : ""}${bad(m.a) && side === "only-a" ? ` <span class="badge err">damaged</span><div class="muted" style="font-size:12.5px">${esc(bad(m.a).health)}</div>` : ""}${m.a?.length > 1 || m.b?.length > 1 ? ` <span class="badge warn">more than one copy</span>` : ""}</td>
      <td class="mono">${esc(m.title_id)}</td><td class="mono muted">${cs.map((x) => `${x.kind.toUpperCase()} ${esc(x.relpath)}`).join("<br>")}</td><td class="num">${fmtBytes(bytes(cs))}</td></tr>`; }).join("")}</tbody></table>` : `<div class="muted" style="margin-top:8px">Nothing.</div>`;
  const btn = (act, label, cls = "", ok = true) => `<button class="btn small ${cls}" data-act="${act}" ${ok ? "" : "disabled"}>${label}</button>`;
  const dupBtn = (d, lib, which) => d.groups.length ? `<div class="notice warn" style="margin-top:10px"><b>${plural(d.groups.length, "game")}</b> ${d.groups.length === 1 ? "is" : "are"} in ${esc(lib.name)} more than once (${fmtBytes(d.bytes)} could be freed). <button class="btn small" data-dup="${which}">Review duplicates…</button></div>` : "";

  out.innerHTML = `
    <div class="row" style="margin:6px 0 12px;flex-wrap:wrap"><span class="badge ok">${c.both.length} in both</span><span class="badge accent">${c.only_a.length} only in ${esc(A.name)}</span><span class="badge accent">${c.only_b.length} only in ${esc(B.name)}</span>${damaged.length ? `<span class="badge err">${damaged.length} damaged in ${esc(B.name)}</span>` : ""}<div class="grow"></div>
      ${btn("mirror", `Mirror ${esc(A.name)} onto ${esc(B.name)}…`, "primary", !!bDest && (c.only_a.length || c.only_b.length || fixable.length))}</div>
    ${dupBtn(dupA, A, "a")}${dupBtn(dupB, B, "b")}
    ${damaged.length ? `<div class="card"><div class="row"><h2 class="grow" style="margin:0">Damaged in ${esc(B.name)} <span class="muted">(${damaged.length})</span></h2>
      ${btn("replace", `Replace from ${esc(A.name)}…`, "primary", !!bDest)}${btn("del-b-damaged", "Delete…", "danger", !!bDest)}</div>
      ${fixable.length < damaged.length ? `<div class="notice warn" style="margin-top:8px">${damaged.length - fixable.length} of these have no good copy in ${esc(A.name)}, so they can't be replaced from it.</div>` : ""}${table(damaged, "damaged", (m) => m.b)}</div>` : ""}
    <div class="card"><div class="row"><h2 class="grow" style="margin:0">Only in ${esc(A.name)} <span class="muted">(${c.only_a.length}${missingBytes ? ", " + fmtBytes(missingBytes) : ""})</span></h2>
      ${btn("only-a-copy", `Copy to ${esc(B.name)}…`, "primary", !!bDest)}${btn("only-a-move", "Move…", "", !!bDest)}${btn("only-a-del", "Delete…", "danger")}</div>
      ${missingBytes > bFree && bFree ? `<div class="notice warn" style="margin-top:8px">Everything missing is ${fmtBytes(missingBytes)} but ${esc(B.name)} has ${fmtBytes(bFree)} free: pick which to copy.</div>` : ""}${table(c.only_a, "only-a", (m) => m.a)}</div>
    <div class="card"><div class="row"><h2 class="grow" style="margin:0">Only in ${esc(B.name)} <span class="muted">(${c.only_b.length})</span></h2>
      ${btn("only-b-copy", `Copy to ${esc(A.name)}…`, "primary", !!aDest)}${btn("only-b-move", "Move…", "", !!aDest)}${btn("only-b-del", "Delete…", "danger", !!bDest)}</div>
      ${damagedOnly.length ? `<div class="notice warn" style="margin-top:8px">${damagedOnly.length} of these are damaged and ${esc(A.name)} doesn't have a good copy: delete them and get a fresh one.</div>` : ""}${table(c.only_b, "only-b", (m) => m.b)}</div>
    <div class="card"><div class="row"><h2 class="grow" style="margin:0">In both <span class="muted">(${c.both.length})</span></h2>
      ${btn("both-replace-b", `Replace ${esc(B.name)}'s copy…`, "", !!bDest)}${btn("both-replace-a", `Replace ${esc(A.name)}'s copy…`, "", !!aDest)}${btn("both-del-b", `Delete from ${esc(B.name)}…`, "danger", !!bDest)}${btn("both-del-a", `Delete from ${esc(A.name)}…`, "danger")}</div>
      ${differ.length ? `<div class="notice" style="margin-top:8px">${differ.length} of these are in differently named folders on the two sides. Tidy both so they match.</div>` : ""}${table(c.both, "both", (m) => m.a.concat(m.b))}</div>`;

  const lists = { damaged, "only-a": c.only_a, "only-b": c.only_b, both: c.both };
  out.querySelectorAll("[data-all]").forEach((x) => (x.onchange = () => out.querySelectorAll(`.sel[data-side="${x.dataset.all}"]`).forEach((s) => (s.checked = x.checked))));
  const chosen = (side) => [...out.querySelectorAll(`.sel[data-side="${side}"]:checked`)].map((s) => lists[side][+s.dataset.i]);
  const send = (picked, from, to, toPath, mode, replace, label) => {
    const items = picked.map((m) => firstOk(m, from)).filter(Boolean).map((x) => ({ library_id: x.library_id, item_id: x.item_id }));
    if (!items.length) return toast("Those games aren't reachable right now", "err");
    openSend({ items, title: `${label} — ${plural(picked.length, "game")}`, defaultDest: { libraryId: to.id, pathId: toPath.id }, mode, replace, onStarted: ctx.openJob });
  };
  const del = (picked, lib, from) => removeDialog(lib, picked.flatMap((m) => (m[from] || []).map((x) => x.item_id)), ctx);
  const acts = {
    mirror: () => mirrorDialog(c, A, B, bPathId, ctx),
    replace: (p) => send(p.filter((m) => firstOk(m, "a")), "a", B, bDest, "copy", true, `Replace from ${A.name}`),
    "del-b-damaged": (p) => del(p, B, "b"),
    "only-a-copy": (p) => send(p, "a", B, bDest, "copy", false, `Copy to ${B.name}`),
    "only-a-move": (p) => send(p, "a", B, bDest, "move", false, `Move to ${B.name}`),
    "only-a-del": (p) => del(p, A, "a"),
    "only-b-copy": (p) => send(p, "b", A, aDest, "copy", false, `Copy to ${A.name}`),
    "only-b-move": (p) => send(p, "b", A, aDest, "move", false, `Move to ${A.name}`),
    "only-b-del": (p) => del(p, B, "b"),
    "both-replace-b": (p) => send(p, "a", B, bDest, "copy", true, `Replace ${B.name}'s copy`),
    "both-replace-a": (p) => send(p, "b", A, aDest, "copy", true, `Replace ${A.name}'s copy`),
    "both-del-b": (p) => del(p, B, "b"),
    "both-del-a": (p) => del(p, A, "a"),
  };
  const sideOf = { replace: "damaged", "del-b-damaged": "damaged" };
  out.querySelectorAll("[data-act]").forEach((b) => (b.onclick = () => {
    const act = b.dataset.act;
    if (act === "mirror") return acts.mirror();
    const side = sideOf[act] || (act.startsWith("only-a") ? "only-a" : act.startsWith("only-b") ? "only-b" : "both");
    const picked = chosen(side);
    if (!picked.length) return toast("Tick the games first", "err");
    acts[act](picked);
  }));
  out.querySelectorAll("[data-dup]").forEach((b) => (b.onclick = () => duplicatesDialog(b.dataset.dup === "a" ? A : B, ctx)));
  onChange?.(c);
}
