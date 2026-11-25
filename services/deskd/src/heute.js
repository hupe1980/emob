/* Heute refreshes itself. Optional: the page is whole without this file.
   Every 15 s while the tab is visible and motion is welcome, the page is
   fetched again from its own URL. Leave the DOM alone while someone reads
   an open disclosure or has focused a control. */
const calm = matchMedia("(prefers-reduced-motion: reduce)");
function reading() {
  return document.querySelector("details[open]") || document.activeElement?.closest("main a, main summary, main button, main [tabindex]");
}
async function refresh() {
  if (document.visibilityState !== "visible" || calm.matches || reading()) return;
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
  if (!next || !main || reading()) return;
  main.replaceChildren(...next.childNodes);
}
setInterval(refresh, 15000);
document.addEventListener("visibilitychange", refresh);
