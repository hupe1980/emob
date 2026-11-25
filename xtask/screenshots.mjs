// The README's and the site's screenshots, taken from the running demo.
//
// Drives the Chrome installed on this machine over its DevTools protocol — no
// npm package, no browser download — against `emob demo` at a fixed clock and
// seed, so every image is the real page as an operator or a driver sees it.
// `just screenshots` starts the demo, runs this, and stops it.
//
//   node xtask/screenshots.mjs <links-file> [out-dir]     # default site/static/img
//   node xtask/screenshots.mjs <links-file> --a11y        # `just a11y`: checks, no images
//
// The links file is the one `emob demo --links` writes (heute, token, receipt,
// refused, inbox, and invoice once billd serves one). Fails if any page scrolls
// sideways at 320 or 390 px, if an image is 200 kB or more, or if a page shows
// the operator token or a receipt token (SR-079).
//
// `--a11y` (spec 011) checks every page a person reads — Heute, the receipt
// billed, refused and after "Selbst prüfen", the invoice page when there is
// one — in light and dark, at 320 and 1280 px, through Chrome's own
// accessibility tree and computed styles, since there is no npm and so no axe:
// `lang`; one h1; heading levels that never skip; a name on every link, button
// and input; alt text on every image; the banner and main landmarks; a visible
// focus indicator on every Tab stop; text contrast ≥ 4.5:1 (≥ 3:1 at ≥ 24 px,
// or ≥ 18.66 px bold) composited over the backgrounds behind it; targets of at
// least 24×24 CSS px (WCAG 2.2 2.5.8 — a link inside a sentence is exempt);
// and nothing past the right edge at 320 px. Each violation names the page,
// the scheme, the width and the element (SR-135).
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync, readFileSync, rmSync, mkdtempSync, existsSync } from "node:fs";
import { join, extname, normalize } from "node:path";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { setTimeout as sleep } from "node:timers/promises";

const argv = process.argv.slice(2);
const A11Y = argv.includes("--a11y");
const [linksFile, dir = "site/static/img"] = argv.filter((a) => a !== "--a11y");
if (!linksFile) { console.error("usage: node xtask/screenshots.mjs <links-file> [out-dir | --a11y]"); process.exit(64); }
const links = Object.fromEntries(readFileSync(linksFile, "utf8").trim().split("\n").map((l) => l.split(/=(.*)/s).slice(0, 2)));
for (const need of ["heute", "token", "receipt", "refused"]) {
  if (!links[need]) { console.error(`the links file names no ${need}`); process.exit(1); }
}
const secrets = [links.token, ...[links.receipt, links.refused].map((u) => u.split("/r/")[1]),
  ...(links.invoice ? [links.invoice.split("/").pop()] : [])];

const chrome = process.env.CHROME ?? [
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/chromium", "/usr/bin/chromium-browser",
].find(existsSync);
if (!chrome) { console.error("no Chrome found — set CHROME, or see `just doctor`"); process.exit(69); }

const BUDGET = 200 * 1024;
const DESKTOP = { width: 1280, height: 800, deviceScaleFactor: 2, mobile: false };
const PHONE = { width: 390, height: 844, deviceScaleFactor: 2, mobile: true };

// ── Chrome over DevTools ────────────────────────────────────────────────────
const port = 9333 + (process.pid % 500);
const profile = mkdtempSync(join(tmpdir(), "emob-shots-"));
const proc = spawn(chrome, ["--headless=new", "--disable-gpu", "--hide-scrollbars", "--no-first-run",
  "--no-default-browser-check", "--force-color-profile=srgb", "--font-render-hinting=none",
  // GitHub's Ubuntu runners restrict the unprivileged user namespaces Chrome's
  // sandbox needs; the pages are our own, served on loopback, so CI runs
  // without it. A developer's machine keeps the sandbox.
  // …and their /dev/shm is small, which crashes a renderer under load.
  ...(process.env.CI ? ["--no-sandbox", "--disable-dev-shm-usage"] : []),
  `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, "about:blank"], { stdio: ["ignore", "ignore", "pipe"] });
// Chrome's own complaint is the only clue when it does not come up; keep the tail.
let chromeErr = "";
proc.stderr.on("data", (chunk) => { chromeErr = (chromeErr + chunk).slice(-4000); });
let target;
// A cold start on a shared CI runner can take well over ten seconds.
for (let i = 0; i < 150 && !target; i++) {
  try { target = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === "page"); } catch { await sleep(200); }
}
if (!target) {
  proc.kill();
  console.error(`Chrome did not start within 30 s (${chrome})`);
  if (chromeErr) console.error(chromeErr);
  process.exit(69);
}
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((resolve) => (ws.onopen = resolve));
let next = 0;
const pending = new Map(), waiting = new Map();
ws.onmessage = (m) => {
  const msg = JSON.parse(m.data);
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
  else if (msg.method && waiting.has(msg.method)) { waiting.get(msg.method)(msg.params); waiting.delete(msg.method); }
};
const send = (method, params = {}) => new Promise((resolve, reject) => {
  const id = ++next;
  pending.set(id, (msg) => (msg.error ? reject(new Error(`${method}: ${msg.error.message}`)) : resolve(msg.result)));
  ws.send(JSON.stringify({ id, method, params }));
});
const event = (method) => new Promise((resolve) => waiting.set(method, resolve));
const js = async (expression) => (await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true })).result.value;
const done = () => { ws.close(); proc.kill(); try { rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 }); } catch {} };

await send("Page.enable");
await send("Page.bringToFront");
await send("Network.enable");
await send("DOM.enable");
await send("Accessibility.enable");
await send("Emulation.setFocusEmulationEnabled", { enabled: true });
await send("Emulation.setTimezoneOverride", { timezoneId: "Europe/Berlin" });
await send("Emulation.setLocaleOverride", { locale: "de-DE" });

const problems = [];
const basic = { Authorization: `Basic ${Buffer.from(`demo:${links.token}`).toString("base64")}` };

async function within(promise, label, milliseconds = 20000) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`Timed out waiting for ${label}`)), milliseconds);
    })]);
  } finally { clearTimeout(timer); }
}

async function open(url, metrics, scheme = "light", headers = {}) {
  await send("Network.setExtraHTTPHeaders", { headers });
  await send("Emulation.setDeviceMetricsOverride", metrics);
  await send("Emulation.setEmulatedMedia", { features: [
    { name: "prefers-color-scheme", value: scheme }, { name: "prefers-reduced-motion", value: "reduce" }] });
  const loaded = event("Page.loadEventFired");
  const navigation = await within(send("Page.navigate", { url }), "browser navigation");
  if (navigation.errorText) throw new Error(`Navigation failed: ${navigation.errorText}`);
  if (navigation.loaderId) await within(loaded, "document load");
  await within(js("document.fonts.ready.then(() => true)"), "document fonts");
  await sleep(300);
}

// What scrollWidth cannot see: content a stylesheet clips (`overflow-x: clip`
// or `hidden` on html/body hides it from the scroll check while it is still cut
// off on the phone). So every element is measured — any box whose right edge
// passes the viewport is a finding, unless an ancestor is a deliberate
// sideways scroller (`.scroll`, overflow-x auto/scroll) that holds it — and a
// root that clips is a finding of its own.
const OVERFLOWING = `(() => {
  const width = document.documentElement.clientWidth;
  const found = [];
  for (const root of [document.documentElement, document.body]) {
    const clip = getComputedStyle(root).overflowX;
    if (clip === "clip" || clip === "hidden") found.push(root.tagName.toLowerCase() + " clips sideways overflow (overflow-x: " + clip + ")");
  }
  const scroller = (el) => {
    for (let up = el.parentElement; up && up !== document.body; up = up.parentElement) {
      const x = getComputedStyle(up).overflowX;
      if (x === "auto" || x === "scroll") return true;
    }
    return false;
  };
  const name = (el) => el.tagName.toLowerCase() + (el.id ? "#" + el.id : "") + (el.className && typeof el.className === "string" ? "." + el.className.trim().split(/\\s+/).join(".") : "");
  for (const el of document.body.querySelectorAll("*")) {
    const box = el.getBoundingClientRect();
    if (box.width === 0 || box.height === 0) continue;
    if (box.right > width + 0.5 && !scroller(el)) {
      found.push(name(el) + " ends at " + Math.round(box.right) + " px, past the " + width + " px viewport");
      if (found.length >= 5) break;
    }
  }
  return found;
})()`;

// Sideways scroll at phone widths, and no secret on any page (SR-079).
async function inspect(name, url, headers = {}) {
  for (const width of [320, 390]) {
    await open(url, { ...PHONE, width }, "light", headers);
    const overflow = await js("document.documentElement.scrollWidth - document.documentElement.clientWidth");
    if (overflow > 0) problems.push(`${name}: scrolls sideways by ${overflow} px at ${width} px`);
    for (const found of await js(OVERFLOWING)) problems.push(`${name}: ${found} at ${width} px`);
  }
  const text = await js("document.documentElement.innerText");
  for (const secret of secrets) {
    if (secret && text.includes(secret)) problems.push(`${name}: the page shows a token (SR-079)`);
  }
}

async function shot(file, { url, metrics, scheme = "light", headers = {}, then }) {
  await open(url, metrics, scheme, headers);
  if (then) {
    if (typeof then === "function") {
      for (const problem of await then()) problems.push(`${file}: ${problem}`);
    } else {
      await js(then);
    }
    await sleep(600);
  }
  // The largest quality that fits the budget: deterministic for one page.
  for (const quality of [90, 82, 74, 66, 58, 50]) {
    const { data } = await send("Page.captureScreenshot", { format: "webp", quality });
    const bytes = Buffer.from(data, "base64");
    if (bytes.length < BUDGET || quality === 50) {
      if (bytes.length >= BUDGET) problems.push(`${file}: ${bytes.length} bytes, over the 200 kB budget`);
      writeFileSync(join(dir, file), bytes);
      console.log(`  ${join(dir, file)}  ${(bytes.length / 1024).toFixed(0)} kB (q${quality})`);
      return;
    }
  }
}

const heute = `${links.heute}?lang=de`;
const receipt = `${links.receipt}?lang=de`;
const refused = `${links.refused}?lang=de`;
// Spec 012: one station's page, the asset card — CP-3 runs 2.1 and bills.
const station = `${links.heute.replace(/\/$/, "")}/station/CP-3?lang=de`;
// Spec 021: the registry searched, and the onboarding form.
const stationList = `${links.heute.replace(/\/$/, "")}/stations?lang=de`;
const stationNew = `${links.heute.replace(/\/$/, "")}/stations/new?lang=de`;
// Spec 022: changing a station, from its card.
const stationEdit = `${links.heute.replace(/\/$/, "")}/station/CP-3/edit?lang=de`;
// Spec 015: a charge point's public price page — a driver's page, on a phone.
const price = `${new URL(links.receipt).origin}/p/DE*MUS*E00003?lang=de`;
// SC-001: at 320 px without script, the price per kWh is the first heading,
// the largest text on the page and above the fold.
const PRICE_FIRST = `(() => {
  const h1 = document.querySelector("h1");
  if (!h1 || !h1.classList.contains("price")) return ["price: the first heading is not the price"];
  const size = (el) => parseFloat(getComputedStyle(el).fontSize);
  const larger = [...document.querySelectorAll("body *")].filter((el) => el !== h1 && el.childElementCount === 0 && el.textContent.trim() && size(el) > size(h1));
  const found = larger.map((el) => "price: " + el.tagName.toLowerCase() + " is larger than the price");
  if (h1.getBoundingClientRect().bottom > innerHeight) found.push("price: below the fold");
  return found;
})()`;
const SELF_CHECK = `(async () => {
    document.getElementById('self-check-run').click();
    for (let i = 0; i < 50 && !document.querySelector('.check.tone-good, .check.tone-bad'); i++) await new Promise((r) => setTimeout(r, 100));
    return true;
  })()`;


// ── accessibility (spec 011) ────────────────────────────────────────────────
// What runs inside the page: everything a computed style answers. Contrast is
// the text's colour, alpha-composited over every background behind it up to
// the canvas; a background image anywhere on the way is a finding of its own,
// because its contrast cannot be computed.
const PAGE_CHECKS = `(() => {
  const found = [];
  const name = (el) => {
    const text = (el.innerText || el.getAttribute("aria-label") || "").trim().replace(/\\s+/g, " ").slice(0, 40);
    return el.tagName.toLowerCase() + (el.id ? "#" + el.id : "") +
      (typeof el.className === "string" && el.className.trim() ? "." + el.className.trim().split(/\\s+/).join(".") : "") +
      (text ? " \u201c" + text + "\u201d" : "");
  };
  const rgba = (text) => {
    const n = (text.match(/[\\d.]+/g) || []).map(Number);
    return n.length >= 3 ? [n[0], n[1], n[2], n.length > 3 ? n[3] : 1] : [0, 0, 0, 0];
  };
  const over = (top, under) => {
    const a = top[3] + under[3] * (1 - top[3]);
    if (a === 0) return [0, 0, 0, 0];
    return [0, 1, 2].map((i) => (top[i] * top[3] + under[i] * under[3] * (1 - top[3])) / a).concat(a);
  };
  const lum = ([r, g, b]) => [r, g, b].map((v) => { v /= 255; return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; })
    .reduce((sum, v, i) => sum + v * [0.2126, 0.7152, 0.0722][i], 0);
  const canvas = matchMedia("(prefers-color-scheme: dark)").matches ? [0, 0, 0, 1] : [255, 255, 255, 1];
  const backdrop = (el) => {
    const layers = [];
    for (let up = el; up; up = up.parentElement) {
      const style = getComputedStyle(up);
      if (style.backgroundImage !== "none") return null;
      layers.push(rgba(style.backgroundColor));
    }
    return layers.reduceRight((under, top) => over(top, under), canvas);
  };
  const visible = (el) => {
    const style = getComputedStyle(el);
    const box = el.getBoundingClientRect();
    return style.visibility !== "hidden" && style.display !== "none" && box.width > 0 && box.height > 0 && Number(style.opacity) > 0;
  };

  if (!document.documentElement.lang) found.push("lang: the html element names no language");
  const status = performance.getEntriesByType("navigation")[0]?.responseStatus;
  if (status && status !== 200) found.push("load: the page answered " + status);

  // Contrast, once per element that holds text of its own.
  const seen = new Set();
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const el = node.parentElement;
    if (!el || seen.has(el) || !node.textContent.trim() || !visible(el)) continue;
    seen.add(el);
    const style = getComputedStyle(el);
    const back = backdrop(el);
    if (!back) { found.push("contrast: " + name(el) + " sits on a background image; its contrast cannot be computed"); continue; }
    const fore = over(rgba(style.color), back);
    const [hi, lo] = [lum(fore), lum(back)].sort((a, b) => b - a);
    const ratio = (hi + 0.05) / (lo + 0.05);
    const size = parseFloat(style.fontSize), bold = Number(style.fontWeight) >= 700;
    const large = size >= 24 || (size >= 18.66 && bold);
    const need = large ? 3 : 4.5;
    if (ratio < need) found.push("contrast: " + name(el) + " is " + ratio.toFixed(2) + ":1, needs " + need + ":1 (" + style.color + " on rgb(" + back.slice(0, 3).map(Math.round).join(", ") + "))");
  }

  // Targets: 24×24 CSS px (WCAG 2.2 2.5.8); a link inside a sentence is exempt.
  const interactive = "a[href], button, input:not([type=hidden]), select, textarea, summary, [tabindex]:not([tabindex='-1'])";
  for (const el of document.querySelectorAll(interactive)) {
    if (!visible(el) || el.classList.contains("skip")) continue;
    // A dot on a map is exempt as an "equivalent" (2.5.8): the table under
    // the map links every station it draws, at full size (spec 022).
    if (el.closest("figure.map")) continue;
    const box = el.getBoundingClientRect();
    if (box.width >= 24 && box.height >= 24) continue;
    const inline = getComputedStyle(el).display === "inline" && el.parentElement &&
      (el.parentElement.innerText ?? "").trim().length > (el.innerText ?? "").trim().length;
    if (!inline) found.push("target size: " + name(el) + " is " + Math.round(box.width) + "×" + Math.round(box.height) + " px, needs 24×24");
  }

  for (const img of document.querySelectorAll("img:not([alt])")) found.push("image: " + name(img) + " has no alt attribute");

  // What each Tab stop looks like unfocused, for the focus check that follows.
  window.__unfocused = new Map();
  for (const el of document.querySelectorAll(interactive)) {
    const style = getComputedStyle(el);
    window.__unfocused.set(el, [style.outlineStyle, style.outlineWidth, style.outlineColor, style.boxShadow].join("|"));
  }
  return found;
})()`;

const FOCUSED = `(() => {
  const el = document.activeElement;
  if (!el || el === document.body) return null;
  const style = getComputedStyle(el);
  const now = [style.outlineStyle, style.outlineWidth, style.outlineColor, style.boxShadow].join("|");
  const shows = (style.outlineStyle !== "none" && parseFloat(style.outlineWidth) > 0) || style.boxShadow !== "none";
  const before = window.__unfocused.get(el);
  const text = (el.innerText || el.getAttribute("aria-label") || "").trim().replace(/\\s+/g, " ").slice(0, 40);
  const label = el.tagName.toLowerCase() + (el.id ? "#" + el.id : "") + (text ? " \u201c" + text + "\u201d" : "");
  return { label, visible: shows && now !== before };
})()`;

// The accessibility tree: names, headings, landmarks, as assistive technology
// reads them — Chrome's own computation, not ours.
async function treeFindings() {
  const found = [];
  const { nodes } = await send("Accessibility.getFullAXTree");
  const live = nodes.filter((node) => !node.ignored);
  const describe = async (node) => {
    try {
      const { object } = await send("DOM.resolveNode", { backendNodeId: node.backendDOMNodeId });
      const { result } = await send("Runtime.callFunctionOn", { objectId: object.objectId, returnByValue: true,
        functionDeclaration: "function () { return this.outerHTML.slice(0, 80); }" });
      return result.value;
    } catch { return `${node.role?.value} node`; }
  };
  const named = ["link", "button", "textbox", "searchbox", "combobox", "checkbox", "radio", "listbox", "slider", "spinbutton", "switch", "tab", "menuitem", "image", "img"];
  for (const node of live) {
    const role = node.role?.value;
    if (named.includes(role) && !(node.name?.value ?? "").trim()) found.push(`name: a ${role} has no accessible name — ${await describe(node)}`);
  }
  // Heading levels in reading order: the tree's own levels, ordered as the
  // document is (the tree's node list is not in document order).
  const levels = [];
  for (const node of live.filter((n) => n.role?.value === "heading")) {
    const { object } = await send("DOM.resolveNode", { backendNodeId: node.backendDOMNodeId });
    const { result } = await send("Runtime.callFunctionOn", { objectId: object.objectId, returnByValue: true,
      functionDeclaration: "function () { return [...document.querySelectorAll('*')].indexOf(this); }" });
    levels.push([result.value, Number(node.properties?.find((p) => p.name === "level")?.value?.value ?? 0)]);
  }
  levels.sort((a, b) => a[0] - b[0]);
  const ones = levels.filter(([, level]) => level === 1).length;
  if (ones !== 1) found.push(`headings: ${ones} h1, needs exactly one`);
  for (let i = 0, last = 0; i < levels.length; last = levels[i][1], i++) {
    if (levels[i][1] > last + 1) { found.push(`headings: h${levels[i][1]} follows h${last || "(none)"} — a level is skipped`); break; }
  }
  for (const landmark of ["banner", "main"]) {
    const count = live.filter((node) => node.role?.value === landmark).length;
    if (count !== 1) found.push(`landmarks: ${count} ${landmark}, needs exactly one`);
  }
  return found;
}

async function accessible(url, metrics, scheme, headers, then) {
  await open(url, { ...metrics, deviceScaleFactor: 1 }, scheme, headers);
  // A step to take first; when it answers with findings, they count too.
  const said = typeof then === "function" ? await then() : then ? await js(then) : null;
  if (then) await sleep(300);
  if (typeof then === "function") await send("Emulation.setEmulatedMedia", { media: "screen", features: [
    { name: "prefers-color-scheme", value: scheme }, { name: "prefers-reduced-motion", value: "reduce" }] });
  const found = [...(Array.isArray(said) ? said : []), ...await js(PAGE_CHECKS), ...await treeFindings()];
  if (metrics.width <= 320) {
    const overflow = await js("document.documentElement.scrollWidth - document.documentElement.clientWidth");
    if (overflow > 0) found.push(`reflow: scrolls sideways by ${overflow} px`);
    for (const item of await js(OVERFLOWING)) found.push(`reflow: ${item}`);
  }
  const text = await js("document.documentElement.innerText");
  if (secrets.some((secret) => secret && text.includes(secret))) found.push("the page shows a token (SR-079)");
  // Focus: Tab through every stop; each must look different from unfocused.
  const stops = await js(`document.querySelectorAll("a[href], button:not([disabled]), input:not([type=hidden]), select, textarea, summary, [tabindex]:not([tabindex='-1'])").length`);
  const first = new Set();
  for (let i = 0; i < Math.min(stops + 1, 80); i++) {
    for (const type of ["keyDown", "keyUp"]) {
      await send("Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
    }
    const focused = await js(FOCUSED);
    if (!focused) continue;
    if (first.has(focused.label)) break;
    first.add(focused.label);
    if (!focused.visible) found.push(`focus: ${focused.label} shows no focus indicator on Tab`);
  }
  if (stops > 0 && first.size === 0) found.push("focus: Tab reached no element");
  return found;
}

// Spec 017: the public site's verifier, when `zola build` has written it —
// served from site/public on loopback, the module included. Its step loads
// the worked example and the tampered one and says what each answered.
let verifyPage = null;
if (A11Y && existsSync("site/public/verify/index.html") && existsSync("site/public/verify/emob_verify.wasm")) {
  const types = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".wasm": "application/wasm", ".svg": "image/svg+xml", ".png": "image/png", ".webp": "image/webp", ".json": "application/json" };
  const site = createServer((req, res) => {
    let path = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname)).replace(/^(\.\.[/\\])+/, "");
    if (path.endsWith("/")) path += "index.html";
    const file = join("site/public", path);
    if (!existsSync(file)) { res.writeHead(404).end(); return; }
    res.writeHead(200, { "content-type": types[extname(file)] ?? "application/octet-stream" }).end(readFileSync(file));
  });
  await new Promise((done) => site.listen(0, "127.0.0.1", done));
  site.unref();
  verifyPage = `http://127.0.0.1:${site.address().port}/verify/`;
}
const VERIFY_EXAMPLE = `(async () => {
  const wait = async (want) => { for (let i = 0; i < 50 && !document.querySelector("#verdict").className.includes(want); i++) await new Promise((r) => setTimeout(r, 100)); };
  const found = [];
  document.getElementById("example").click(); await wait("tone-good");
  if (!document.querySelector("#verdict").className.includes("tone-good")) found.push("verify: the worked example did not verify: " + document.querySelector("#verdict").textContent);
  document.getElementById("tamper").click(); await wait("tone-bad");
  if (!document.querySelector("#verdict").className.includes("tone-bad")) found.push("verify: the tampered example did not fail");
  if (!/[0-9a-f]{64}/.test(document.querySelector("#module").textContent)) found.push("verify: no module hash shown");
  return found;
})()`;

// Follow the refusal that this demo actually recorded. Never send the desk's
// Authorization header to an origin supplied by the page.
await open(heute, DESKTOP, "light", basic);
const sessionPath = await js(`document.querySelector('a[href^="/sessions/"]')?.getAttribute("href")`);
if (!/^\/sessions\/\d+(?:\?lang=(?:de|en))?$/.test(sessionPath ?? "")) {
  done();
  throw new Error("Heute has no link to the recorded refusal");
}
const session = new URL(sessionPath, heute);
session.searchParams.set("lang", "en");

// Exercise the same native, body-only forms as an operator. Only the disposable
// a11y estate runs confirmations. Each result alternates the card's state.
async function submitDeskForm(path, fields) {
  const submitted = await js(`(() => {
    const form = [...document.forms].find(el => el.method === "post" &&
      new URL(el.action).origin === location.origin && new URL(el.action).pathname === ${JSON.stringify(path)} &&
      (${JSON.stringify(path)} !== "/lookup" || el.elements.kind?.value === "reference"));
    if (!form) return false;
    if (form.elements.reason) {
      form.elements.reason.value = form.elements.blocked.value === "true" ? "lost" : "recovered";
    }
    for (const [name, value] of Object.entries(${JSON.stringify(fields)})) {
      if (!form.elements[name]) return false;
      form.elements[name].value = value;
    }
    if (!form.checkValidity()) return false;
    setTimeout(() => form.requestSubmit(), 0);
    return true;
  })()`);
  if (!submitted) throw new Error(`Missing or invalid desk form for ${path}`);
}
async function deskStep(path, fields = {}) {
  const loaded = event("Page.loadEventFired");
  await submitDeskForm(path, fields);
  await within(loaded, `desk form ${path}`);
}
async function cardJourney(stage) {
  await deskStep("/lookup");
  if (!await js('!!document.querySelector("#card-history")')) return ["missing card history"];
  if (stage === "detail") {
    await send("Emulation.setEmulatedMedia", { media: "print" });
    const printed = await js(`document.querySelector("#card-history").getClientRects().length > 0 &&
      ![...document.querySelectorAll("main form")].some(el => el.getClientRects().length)`);
    await send("Emulation.setEmulatedMedia", { media: "" });
    return printed ? [] : ["printed card must show history and hide forms"];
  }
  await deskStep("/cards/preview");
  if (!await js(`!!document.querySelector('form[action^="/cards/confirm"]')`)) return ["missing reviewed confirmation"];
  if (stage === "review") return [];
  await deskStep("/cards/confirm");
  return await js('!!document.querySelector("#card-history") && document.body.innerText.includes("Change confirmed")')
    ? [] : ["missing applied decision or history"];
}

const contracts = new URL("/contracts?lang=en", heute);
let issuedAgreement;
async function enrolmentJourney(stage) {
  if (stage === "receipt" && issuedAgreement) {
    await deskStep("/contracts/lookup", { agreement: issuedAgreement });
  } else {
    const selection = await js(`[...document.querySelectorAll("select[name=selection] option")]
      .find(option => option.value.endsWith("|standard"))?.value`);
    if (!selection) return ["missing eligible provider offer"];
    await deskStep("/contracts/preview", { selection, stock: "STOCK-1004" });
    const operation = await js('document.querySelector("input[name=operation]")?.value');
    if (!operation || !await js('document.body.innerText.includes("Gross contract prices")')) {
      return ["missing reviewed price or confirmation"];
    }
    if (stage === "review") return [];
    await deskStep("/contracts/confirm");
    issuedAgreement = operation;
  }
  return await js('document.body.innerText.includes("Agreement issued")')
    ? [] : ["missing issued agreement"];
}

if (A11Y) {
  await open(session.href, DESKTOP, "light", basic);
  await send("Emulation.setEmulatedMedia", { media: "print" });
  const printable = await js(`(() => {
    const timeline = document.querySelector(".timeline");
    return timeline?.getClientRects().length && timeline.querySelector("time") &&
      ![...document.querySelectorAll("main nav, main form")].some(el => el.getClientRects().length);
  })()`);
  if (!printable) problems.push("printed session: missing timeline or visible navigation/actions");
  await send("Emulation.setEmulatedMedia", { media: "" });
  const lookup = new URL("/lookup?lang=de", heute);
  const pages = [
    ["Heute", heute, basic],
    ["session evidence", session.href, basic,
      `(() => document.querySelector("h1")?.textContent === "Session evidence" &&
        document.querySelector("main time") && document.querySelector('form[method="post"]')
        ? [] : ["missing recorded evidence or card action"])()`],
    ...["detail", "review", "result"].map(stage => [
      `card ${stage}`, session.href, basic, () => cardJourney(stage),
    ]),
    ["driver contracts", contracts.href, basic],
    ...["review", "receipt"].map(stage => [
      `enrolment ${stage}`, contracts.href, basic, () => enrolmentJourney(stage),
    ]),
    ["operator lookup", lookup.href, basic,
      `(() => document.querySelector('form[method="post"] input[name="value"]')
        ? [] : ["missing private lookup form"])()`],
    ["station", station, basic],
    ["stations", stationList, basic],
    ["new station", stationNew, basic],
    ["change station", stationEdit, basic],
    ["price", price, {}, PRICE_FIRST],
    ["provider price", new URL("/p/DE*MUS*E00001/providers/DE*MSP/standard?lang=en", price).href, {},
      `(() => document.querySelector("h1")?.textContent === "Contract price" &&
        document.body.textContent.includes("6.30 EUR") && document.querySelector('a[href^="/p/offers/"]')
        ? [] : ["provider price: the gross example or historical reference is missing"])()`],
    ...(verifyPage ? [["verifier (site)", verifyPage, {}, VERIFY_EXAMPLE]] : []),
    ["receipt (billed)", receipt, {},
      `(() => {
        const value = document.querySelector(".receipt-stats .value");
        if (!value?.textContent.includes("kWh")) return ["missing measured energy"];
        const range = document.createRange();
        range.selectNodeContents(value);
        return range.getClientRects().length === 1 ? [] : ["energy and unit occupy different lines"];
      })()`],
    ["receipt (refused)", refused],
    ["receipt (checked)", receipt, {}, SELF_CHECK],
    ...(links.invoice ? [["invoice", `${links.invoice}?lang=de`]] : []),
  ];
  let checked = 0;
  for (const [name, url, headers = {}, then] of pages) {
    for (const scheme of ["light", "dark"]) {
      for (const metrics of [{ ...PHONE, width: 320 }, DESKTOP]) {
        const where = `${name} (${scheme}, ${metrics.width} px)`;
        try {
          for (const found of await within(accessible(url, metrics, scheme, headers, then), where, 45000)) {
            problems.push(`${where}: ${found}`);
          }
        } catch (error) {
          done();
          throw new Error(`${where}: ${error.message}`);
        }
        checked++;
      }
    }
  }
  done();
  if (problems.length) { console.error(problems.join("\n")); console.error(`✕ ${problems.length} accessibility violation(s)`); process.exit(1); }
  console.log(`✓ accessibility: ${pages.length} pages × 2 schemes × 2 widths (${checked} views), 0 violations${links.invoice ? "" : " — no invoice page in the links file"}`);
  process.exit(0);
}

mkdirSync(dir, { recursive: true });
await inspect("Heute", heute, basic);
await inspect("station", station, basic);
await inspect("receipt", receipt);
await inspect("refused receipt", refused);
if (links.inbox) await inspect("partner inbox", links.inbox);

await shot("heute-light.webp", { url: heute, metrics: DESKTOP, headers: basic });
await shot("heute-dark.webp", { url: heute, metrics: DESKTOP, scheme: "dark", headers: basic });
await shot("heute-mobile-light.webp", { url: heute, metrics: PHONE, headers: basic });
await shot("heute-mobile-dark.webp", { url: heute, metrics: PHONE, scheme: "dark", headers: basic });
await shot("session-evidence-light.webp", { url: session.href, metrics: DESKTOP, headers: basic });
await shot("session-evidence-dark.webp", { url: session.href, metrics: DESKTOP, scheme: "dark", headers: basic });
await shot("enrolment-review-light.webp", { url: contracts.href, metrics: DESKTOP, headers: basic, then: () => enrolmentJourney("review") });
await shot("enrolment-review-dark.webp", { url: contracts.href, metrics: DESKTOP, scheme: "dark", headers: basic, then: () => enrolmentJourney("review") });
await shot("card-review-light.webp", { url: session.href, metrics: DESKTOP, headers: basic, then: () => cardJourney("review") });
await shot("card-review-dark.webp", { url: session.href, metrics: DESKTOP, scheme: "dark", headers: basic, then: () => cardJourney("review") });
await shot("station-light.webp", { url: station, metrics: DESKTOP, headers: basic });
await shot("station-dark.webp", { url: station, metrics: DESKTOP, scheme: "dark", headers: basic });
await shot("stations-light.webp", { url: stationList, metrics: DESKTOP, headers: basic });
await shot("stations-dark.webp", { url: stationList, metrics: DESKTOP, scheme: "dark", headers: basic });
await shot("station-new-light.webp", { url: stationNew, metrics: DESKTOP, headers: basic });
await shot("price-light.webp", { url: price, metrics: PHONE });
await shot("price-dark.webp", { url: price, metrics: PHONE, scheme: "dark" });
await shot("receipt-light.webp", { url: receipt, metrics: PHONE });
await shot("receipt-dark.webp", { url: receipt, metrics: PHONE, scheme: "dark" });
await shot("receipt-refused.webp", { url: refused, metrics: PHONE });
await shot("receipt-checked.webp", {
  url: receipt, metrics: PHONE,
  then: `(async () => {
    document.getElementById('self-check-run').click();
    for (let i = 0; i < 50 && !document.querySelector('.check.tone-good, .check.tone-bad'); i++) await new Promise((r) => setTimeout(r, 100));
    document.querySelector('ol.values').scrollIntoView({ block: 'start' }); window.scrollBy(0, -72);
    return true;
  })()`,
});

done();
if (problems.length) { console.error(problems.join("\n")); process.exit(1); }
console.log(`✓ screenshots in ${dir}: no page scrolls sideways at 320 or 390 px, none shows a token`);
