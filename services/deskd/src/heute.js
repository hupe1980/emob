/* Heute refreshes itself. Optional: the page is whole without this file.
   Every 15 s while the tab is visible and motion is welcome, the page is
   fetched again from its own URL and <main> swapped; open "Warum?" stay open
   and focus stays on the row it was on. */
const calm = matchMedia("(prefers-reduced-motion: reduce)");
async function refresh() {
  if (document.visibilityState !== "visible" || calm.matches) return;
  const open = [...document.querySelectorAll("details[open]")].map((d) => d.closest("li")?.id);
  const focus = document.activeElement?.closest("li")?.id;
  let text;
  try {
    const response = await fetch(location.href, { credentials: "same-origin", cache: "no-store" });
    if (!response.ok) return;
    text = await response.text();
  } catch {
    return;
  }
  const next = new DOMParser().parseFromString(text, "text/html").querySelector("main");
  const main = document.querySelector("main");
  if (!next || !main) return;
  main.replaceChildren(...next.childNodes);
  for (const id of open) document.getElementById(id)?.querySelector("details")?.setAttribute("open", "");
  if (focus) document.getElementById(focus)?.querySelector("a, summary, [tabindex]")?.focus({ preventScroll: true });
}
setInterval(refresh, 15000);
document.addEventListener("visibilitychange", refresh);
