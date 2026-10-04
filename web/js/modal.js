// Behaviour every dialog shares, without each one having to ask for it: Escape closes the top
// one, the dialog is announced as a dialog, focus goes into it and returns afterwards, and Tab
// stays inside it.
const FOCUSABLE = 'a[href], button:not([disabled]), input:not([disabled]):not([type=hidden]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

const opened = new Map(); // modal-bg -> element that had focus before

function setup(bg) {
  if (opened.has(bg)) return;
  opened.set(bg, document.activeElement);
  const box = bg.querySelector(".modal") || bg;
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  const head = box.querySelector("header");
  if (head) { head.id ||= "mh" + Math.random().toString(36).slice(2, 7); box.setAttribute("aria-labelledby", head.id); }
  // Focus the first field, or failing that the first button, unless the dialog already did.
  queueMicrotask(() => {
    if (box.contains(document.activeElement) && document.activeElement !== document.body) return;
    (box.querySelector("input:not([type=hidden]), textarea, select") || box.querySelector(FOCUSABLE))?.focus();
  });
}

function teardown(bg) {
  const back = opened.get(bg);
  opened.delete(bg);
  if (back && document.contains(back)) back.focus?.({ preventScroll: true });
}

new MutationObserver((muts) => {
  for (const m of muts) {
    m.addedNodes.forEach((n) => { if (n.nodeType === 1 && n.classList.contains("modal-bg")) setup(n); });
    m.removedNodes.forEach((n) => { if (n.nodeType === 1 && opened.has(n)) teardown(n); });
  }
}).observe(document.body, { childList: true });

document.addEventListener("keydown", (e) => {
  const all = document.querySelectorAll(".modal-bg");
  const top = all[all.length - 1];
  if (!top) return;
  if (e.key === "Escape") {
    // A dialog can say it must not be dismissed by accident (while something is running).
    if (top.dataset.sticky) return;
    e.preventDefault();
    top.remove();
  } else if (e.key === "Tab") {
    const items = [...top.querySelectorAll(FOCUSABLE)].filter((x) => x.offsetParent !== null);
    if (!items.length) return;
    const first = items[0], last = items[items.length - 1];
    if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
    else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
  }
});
