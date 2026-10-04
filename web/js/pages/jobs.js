import { api, esc, fmtTime } from "../util.js";
import { followJob, showResult } from "../jobs.js";

export const title = "Jobs";
let follower = null;

const FILTERS = [["all", "All", () => true], ["running", "Running", (j) => j.status === "running" || j.status === "queued"], ["failed", "Failed", (j) => j.status === "failed"]];

export async function render(root, ctx, arg) {
  let filter = "all";
  root.innerHTML = `
    <div class="page-head"><h1>Jobs</h1><div class="muted">Everything RustyBox is doing, or has done lately. The record is kept across restarts.</div></div>
    <div id="job-open"></div>
    <div class="card"><div class="fchips" id="job-chips" style="margin-bottom:10px"></div><div id="job-list"></div></div>`;
  let jobs = [];
  const draw = async () => {
    jobs = await api("/api/jobs");
    const fn = FILTERS.find((f) => f[0] === filter)[2];
    root.querySelector("#job-chips").innerHTML = FILTERS.map(([v, l, f]) => `<button class="fchip ${v === filter ? "on" : ""}" data-f="${v}">${l}<small>${jobs.filter(f).length}</small></button>`).join("");
    root.querySelectorAll("[data-f]").forEach((b) => (b.onclick = () => { filter = b.dataset.f; draw(); }));
    const shown = jobs.filter(fn);
    root.querySelector("#job-list").innerHTML = shown.length
      ? `<table><thead><tr><th>#</th><th>Job</th><th>Status</th><th>Progress</th><th>Started</th><th></th></tr></thead><tbody>
        ${shown.map((j) => `<tr style="cursor:pointer" data-id="${j.id}"><td>${j.id}</td><td>${esc(j.title)}<div class="muted">${esc(j.error ? j.error.message.split("\n")[0] : j.step)}</div></td>
          <td><span class="badge ${j.status}">${j.status}</span></td><td>${Math.round(j.pct)}%</td><td class="muted">${fmtTime(j.started)}</td>
          <td>${j.status === "running" || j.status === "queued" ? `<button class="btn small danger" data-stop="${j.id}">Stop</button>` : ""}</td></tr>`).join("")}
        </tbody></table>`
      : `<div class="empty"><span class="ico">⚙️</span>${filter === "all" ? "No jobs yet." : "Nothing here."}</div>`;
    root.querySelectorAll("tr[data-id]").forEach((tr) => (tr.onclick = () => ctx.openJob(tr.dataset.id)));
    root.querySelectorAll("[data-stop]").forEach((b) => (b.onclick = async (e) => {
      e.stopPropagation();
      if (!confirm("Stop this job? Work already done stays done; a copy in progress is left as a .part file that is never used.")) return;
      await api(`/api/jobs/${b.dataset.stop}/cancel`, { method: "POST" }).catch(() => {});
      draw();
    }));
  };
  await draw();
  ctx.onJobsChanged = draw;
  follower?.close();
  ctx.leave.push(() => follower?.close());
  if (arg) follower = followJob(arg, root.querySelector("#job-open"), { onEnd: draw, onResult: showResult });
}
