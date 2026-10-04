import { api, esc } from "../util.js";
import { compareView } from "../compare-view.js";

export const title = "Compare";

export async function render(root, ctx) {
  const libs = (await api("/api/libraries")).filter((l) => ["iso", "god"].includes(l.kind));
  const opt = (l) => `<option value="${l.id}">${l.paths.some((p) => p.remote_url) ? "🖥 " : ""}${esc(l.name)} (${l.items})</option>`;
  const remoteFirst = libs.find((l) => l.paths.some((p) => p.remote_url));
  root.innerHTML = `
    <div class="page-head"><h1>Compare</h1><div class="muted">See which games are in one library and not another, for example your GOD library against the Xbox's drive. Games are matched by title ID and disc, whatever their folders are called. Tick games to copy, move, replace or delete them, mirror one library onto the other, or clean up duplicates.</div></div>
    <div class="card"><div class="row">
      <label class="field grow">Library<select id="ca">${libs.map(opt).join("")}</select></label><span style="padding-top:18px">against</span>
      <label class="field grow">Library<select id="cb">${libs.map(opt).join("")}</select></label>
      <button class="btn primary" id="go" style="margin-top:18px">Compare</button></div></div>
    <div id="out"></div>`;
  if (libs.length > 1) { root.querySelector("#cb").value = (remoteFirst || libs[1]).id; if (remoteFirst && remoteFirst.id === libs[0].id) root.querySelector("#ca").value = libs[1].id; }
  const $ = (s) => root.querySelector(s);
  $("#go").onclick = () => {
    const A = libs.find((l) => l.id === +$("#ca").value), B = libs.find((l) => l.id === +$("#cb").value);
    if (A.id === B.id) return $("#out").innerHTML = `<div class="notice err">Choose two different libraries.</div>`;
    compareView($("#out"), { A, B, ctx });
  };
}
