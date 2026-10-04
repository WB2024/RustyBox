// Chunked, resumable uploads. Each file goes up in pieces at byte offsets; if a piece fails
// the upload asks the server how much it has and carries on from there.
import { api } from "./util.js";

const CHUNK = 16 * 1024 * 1024;

async function putChunk(url, blob) {
  const res = await fetch(url, { method: "PUT", body: blob });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) { const e = new Error(data.message || res.statusText); e.code = data.error; e.status = res.status; throw e; }
  return data;
}

/**
 * Upload one file into a library folder.
 * opts: { libraryId, pathId, rel, overwrite, onProgress(done, total), signal }
 */
export async function uploadFile(file, opts) {
  const base = `/api/libraries/${opts.libraryId}/upload?path_id=${opts.pathId}&rel=${encodeURIComponent(opts.rel)}`;
  const total = file.size;
  const status = await api(base);
  if (status.exists && !opts.overwrite) { const e = new Error("A file with that name is already there"); e.code = "EXISTS"; throw e; }
  let offset = Math.min(status.offset, total);
  let retries = 0;
  for (;;) {
    if (opts.signal?.aborted) throw new Error("Cancelled");
    const end = Math.min(offset + CHUNK, total);
    const url = `${base}&total=${total}&offset=${offset}${opts.overwrite ? "&overwrite=true" : ""}`;
    try {
      const r = await putChunk(url, file.slice(offset, end));
      offset = r.offset;
      opts.onProgress?.(offset, total);
      if (r.done) return;
      retries = 0;
    } catch (e) {
      if (e.status && e.status < 500 && e.code !== "OFFSET_MISMATCH") throw e; // a real refusal
      if (++retries > 5) throw e;
      await new Promise((r) => setTimeout(r, 1000 * retries));
      offset = (await api(base)).offset; // carry on from what the server has
    }
  }
}
