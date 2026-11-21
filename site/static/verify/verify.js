// emob's OCMF verifier on the public site (spec 017): the receipt's own
// WebAssembly module, run on this device. One fetch — the module — and
// nothing after it; the page's Content-Security-Policy says so too.
const form = document.getElementById("verifier");
if (form && "WebAssembly" in self) {
  const $ = (id) => document.getElementById(id);
  const words = {
    0: ["good", "✓ Verified — this key signed exactly these bytes."],
    1: ["bad", "✗ Does not verify — the record was changed, or the key is another meter's."],
    2: ["warn", "? Cannot be checked here — this curve needs the S.A.F.E. Transparenzsoftware."],
    3: ["bad", "✗ Unreadable record — paste the whole line, starting with OCMF|."],
    4: ["bad", "✗ Unreadable key — hex, Base64 or Base32 of the meter's public key."],
    5: ["bad", "✗ The key is for another curve than the record names."],
  };
  const markers = { B: "begin", C: "charging", E: "end", L: "ended locally", R: "ended remotely", A: "aborted", T: "tariff change", P: "power failure", S: "suspended", X: "exception" };
  form.querySelector(".nojs")?.remove();
  const bytes = fetch(form.dataset.wasm).then((r) => r.arrayBuffer());
  const ready = bytes.then((b) => WebAssembly.instantiate(b)).then((m) => m.instance.exports);
  bytes.then(async (b) => {
    const hash = [...new Uint8Array(await crypto.subtle.digest("SHA-256", b))]
      .map((x) => x.toString(16).padStart(2, "0")).join("");
    $("module").textContent = `Module SHA-256 ${hash} — computed here, from the bytes this page loaded.`;
  }).catch(() => {});
  const check = async () => {
    const w = await ready;
    const enc = new TextEncoder(), dec = new TextDecoder();
    const record = enc.encode($("ocmf").value.trim()), key = enc.encode($("key").value.trim());
    const at = w.buffer(record.length + key.length);
    const input = new Uint8Array(w.memory.buffer, at, record.length + key.length);
    input.set(record);
    input.set(key, record.length);
    const code = w.verify(record.length, key.length);
    const [tone, text] = words[code] ?? words[3];
    const verdict = $("verdict");
    verdict.className = `verdict tone-${tone}`;
    verdict.textContent = text;
    const said = dec.decode(new Uint8Array(w.memory.buffer, w.output(), w.output_len()));
    const list = $("readings");
    list.replaceChildren(...said.split("\n").filter(Boolean).map((line) => {
      const [marker, value, unit, time] = line.split("\t");
      const li = document.createElement("li");
      li.textContent = `${markers[marker] ?? marker}: ${value} ${unit} · ${time}`;
      return li;
    }));
  };
  form.addEventListener("submit", (event) => { event.preventDefault(); check(); });
  $("example").addEventListener("click", () => {
    $("ocmf").value = form.dataset.exampleRecord;
    $("key").value = form.dataset.exampleKey;
    check();
  });
  $("tamper").addEventListener("click", () => {
    // One digit of the reading changed: 7753 Wh becomes 7754 Wh.
    $("ocmf").value = form.dataset.exampleRecord.replace('"RV":7753', '"RV":7754');
    $("key").value = form.dataset.exampleKey;
    check();
  });
}
