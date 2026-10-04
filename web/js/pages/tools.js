import { esc } from "../util.js";
import { pickFolder } from "../picker.js";
import { openConvert } from "../convert.js";

export const title = "Tools";

export async function render(root, ctx) {
  root.innerHTML = `
    <div class="page-head"><h1>Tools</h1><div class="muted">One-off jobs on files and folders. Everything shows a plan first.</div></div>
    <div class="card" style="max-width:720px">
      <h2>Create an ISO from a game folder</h2>
      <p class="muted">Pack a folder containing a game (with its <span class="mono">default.xex</span>) into a disc image, saved into one of your ISO libraries.</p>
      <div class="row"><button class="btn primary" id="mk">Choose a folder…</button><span class="mono muted" id="mk-path"></span></div>
    </div>`;
  root.querySelector("#mk").onclick = async () => {
    const folder = await pickFolder();
    if (!folder) return;
    root.querySelector("#mk-path").textContent = folder;
    openConvert({ op: "create", sourceFolder: folder, title: "Create ISO", onStarted: (job) => ctx.openJob(job) });
  };
}
