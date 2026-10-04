// Following a job: replays its event log, then streams new events over SSE, and recovers
// from dropped connections by catching up from the plain JSON endpoint.
import { api, esc } from "./util.js";

const TERMINAL = ["done", "failed", "cancelled"];
export const isActive = (j) => !TERMINAL.includes(j.status);

export function followJob(jobId, container, { onEnd, onResult } = {}) {
  container.innerHTML = `
    <div class="jobpanel card">
      <div class="head"><b class="jp-title">Job</b><span class="badge running jp-status">running</span>
        <span class="grow"></span><button class="btn small danger jp-cancel">Cancel</button></div>
      <div class="bar"><i class="jp-bar"></i></div>
      <div class="stepline muted jp-step"></div>
      <div class="notice err jp-error hidden"></div>
      <div class="jp-results"></div>
      <pre class="log jp-log"></pre>
    </div>`;
  const q = (s) => container.querySelector(s);
  let count = 0, ended = false, es = null, fails = 0;

  const addLog = (msg, cls = "") => {
    const log = q(".jp-log");
    // Follow the end of the log, unless the reader has scrolled up to read something.
    const atEnd = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
    const line = document.createElement("div");
    if (cls) line.className = cls;
    line.textContent = msg;
    log.append(line);
    if (atEnd) log.scrollTop = log.scrollHeight;
  };
  const setStatus = (s) => {
    const b = q(".jp-status");
    b.textContent = s.replace("_", " ");
    b.className = "badge jp-status " + s;
    q(".jp-bar").parentElement.className = "bar " + (s === "done" || s === "failed" ? s : "");
    q(".jp-cancel").classList.toggle("hidden", TERMINAL.includes(s));
  };
  const handle = (ev) => {
    switch (ev.type) {
      case "progress": q(".jp-bar").style.width = ev.pct + "%"; break;
      case "step": q(".jp-step").textContent = ev.msg; addLog("▶ " + ev.msg, "step"); break;
      case "log": addLog(ev.msg); break;
      case "status": setStatus(ev.status); break;
      case "result": onResult?.(ev.name, ev.data, q(".jp-results")); break;
      case "error": {
        const e = q(".jp-error");
        e.classList.remove("hidden");
        e.innerHTML = `<b>${esc(ev.error)}</b> — ${esc(ev.message).replace(/\n/g, "<br>")}`;
        break;
      }
    }
  };
  const finish = () => { ended = true; es?.close(); onEnd?.(); };
  const open = () => {
    es = new EventSource(`/api/jobs/${jobId}/events?from=${count}`);
    es.onmessage = (m) => { count++; fails = 0; handle(JSON.parse(m.data)); };
    es.addEventListener("end", finish);
    es.onerror = async () => {
      es.close();
      if (ended) return;
      try {
        const d = await api(`/api/jobs/${jobId}`);
        d.events.slice(count).forEach((ev) => { count++; handle(ev); });
        if (TERMINAL.includes(d.job.status)) return finish();
        fails = 0;
      } catch (e) {
        if (e.status === 404 || ++fails > 30) { ended = true; addLog("⚠ Lost connection to the server.", "step"); return; }
      }
      setTimeout(open, 1500);
    };
  };
  api(`/api/jobs/${jobId}`).then((d) => { q(".jp-title").textContent = d.job.title; }).catch(() => {});
  q(".jp-cancel").onclick = () => api(`/api/jobs/${jobId}/cancel`, { method: "POST" }).catch(() => {});
  open();
  return { close() { ended = true; es?.close(); } };
}

// Summaries for the structured results jobs report.
export function showResult(name, d, box) {
  if (name === "convert") {
    box.insertAdjacentHTML("beforeend", `<div class="notice" style="margin:8px 0"><b>${esc(d.op)}</b>: ${d.done} done${d.failed ? `, <span style="color:var(--err)">${d.failed} failed</span>` : ""}.
      ${d.outputs.map((o) => `<div class="mono">${esc(o)}</div>`).join("")}</div>`);
  } else if (name === "import") {
    box.insertAdjacentHTML("beforeend", `<div class="notice" style="margin:8px 0">Import: <b>${d.done}</b> of ${d.total} done${d.failed ? `, <span style="color:var(--err)">${d.failed} failed</span>` : ""}.</div>`);
  } else if (name === "remove") {
    box.insertAdjacentHTML("beforeend", `<div class="notice" style="margin:8px 0">Removed <b>${d.done}</b> game${d.done === 1 ? "" : "s"}${d.failed ? `, <span style="color:var(--err)">${d.failed} failed</span>` : ""}.</div>`);
  } else if (name === "igdb") {
    box.insertAdjacentHTML("beforeend", `<div class="notice" style="margin:8px 0">Game information: <b>${d.matched}</b> matched${d.unmatched ? `, ${d.unmatched} with no match (you can choose one by hand on the Games page)` : ""}${d.failed ? `, <span style="color:var(--err)">${d.failed} failed</span>` : ""}.</div>`);
  } else if (name === "tidy") {
    box.insertAdjacentHTML("beforeend", `<div class="notice" style="margin:8px 0">Moved or renamed <b>${d.moved}</b> item${d.moved === 1 ? "" : "s"}${d.failed ? `, <span style="color:var(--err)">${d.failed} failed</span>` : ""}.</div>`);
  } else if (name === "abgx") {
    box.insertAdjacentHTML("beforeend", `<div class="notice ${d.errors?.length || d.exit_code ? "err" : ""}" style="margin:8px 0">abgx360 ${d.fix ? "check and fix" : "check"} finished${d.errors?.length ? ` with ${d.errors.length} error${d.errors.length === 1 ? "" : "s"}` : d.exit_code ? ` with exit code ${d.exit_code}` : ""}. The log below is its full output.</div>`);
  } else if (name === "scan") {
    box.insertAdjacentHTML("beforeend", `<div class="notice" style="margin:8px 0">Library updated: ${d.added} added, ${d.updated} changed, ${d.removed} removed.</div>`);
  }
}
