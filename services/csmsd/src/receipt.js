// emob receipt: "Selbst prüfen". The WebAssembly build of `ocmf` checks every
// signed value on this device; nothing on the server has to be trusted. The
// module is compiled as the page loads, so the check works offline afterwards.
const box = document.getElementById("self-check");
const run = document.getElementById("self-check-run");
if (box && run && "WebAssembly" in self) {
  const compiled = WebAssembly.compileStreaming(fetch(box.dataset.wasm)).catch(() => null);
  box.querySelector(".nojs")?.remove();
  run.hidden = false;
  run.addEventListener("click", async () => {
    run.disabled = true;
    const summary = box.querySelector(".summary");
    const module = await compiled;
    if (!module) { summary.textContent = box.dataset.fail; run.disabled = false; return; }
    const w = (await WebAssembly.instantiate(module)).exports;
    const enc = new TextEncoder(), dec = new TextDecoder();
    const key = enc.encode(box.dataset.key);
    let all = true;
    for (const li of document.querySelectorAll("[data-ocmf]")) {
      const record = enc.encode(li.dataset.ocmf);
      const at = w.buffer(record.length + key.length);
      const input = new Uint8Array(w.memory.buffer, at, record.length + key.length);
      input.set(record);
      input.set(key, record.length);
      const code = w.verify(record.length, key.length);
      const said = dec.decode(new Uint8Array(w.memory.buffer, w.output(), w.output_len()));
      // What the device read out of the bytes it checked, not the server's text.
      li.querySelector(".said").textContent = said.split("\n")
        .map((line) => line.split("\t").slice(0, 3).join(" ")).join(" · ");
      const out = li.querySelector(".check");
      const tone = code === 0 ? "good" : code === 2 ? "warn" : "bad";
      out.className = `check badge tone-${tone}`;
      out.textContent = `${code === 0 ? "✓" : code === 2 ? "?" : "✗"} ${box.dataset["w" + code]}`;
      all &&= code === 0;
    }
    summary.textContent = all ? box.dataset.all : box.dataset.some;
    run.disabled = false;
  });
}
