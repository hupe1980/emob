+++
title = "Verify an OCMF record in your browser"
description = "Paste a signed OCMF meter value and the meter's key: emob's verifier runs as WebAssembly in your browser. Nothing is uploaded."

[extra]
csp = "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; form-action 'none'; base-uri 'none'"
+++

Paste one signed value — the line that starts with `OCMF|` — and the public
key of the meter that signed it. The check runs **on this device**: the page
loads one WebAssembly module, the same one every emob receipt runs, and sends
nothing anywhere afterwards. It works with the network unplugged.

*Ein signierter Messwert und der Schlüssel des Zählers: das Urteil rechnet
dieser Browser aus, nichts wird hochgeladen.*

<form id="verifier" class="verifier" data-wasm="emob_verify.wasm"
  data-example-record="OCMF|{&quot;FV&quot;:&quot;1.0&quot;,&quot;GI&quot;:&quot;Heinz_CSA&quot;,&quot;GS&quot;:&quot;&quot;,&quot;GV&quot;:&quot;1.6.0&quot;,&quot;PG&quot;:&quot;T28&quot;,&quot;MV&quot;:&quot;DZG&quot;,&quot;MM&quot;:&quot;DVH4013&quot;,&quot;MS&quot;:&quot;000033022620&quot;,&quot;MF&quot;:&quot;&quot;,&quot;IS&quot;:true,&quot;IT&quot;:&quot;ISO14443&quot;,&quot;ID&quot;:&quot;7bcf78ad&quot;,&quot;RD&quot;:[{&quot;TM&quot;:&quot;2021-08-25T10:01:04,000+0200 I&quot;,&quot;TX&quot;:&quot;B&quot;,&quot;RV&quot;:7753,&quot;RI&quot;:&quot;1-b:1.8.0&quot;,&quot;RU&quot;:&quot;Wh&quot;,&quot;RT&quot;:&quot;AC&quot;,&quot;EF&quot;:&quot;&quot;,&quot;ST&quot;:&quot;G&quot;}]}|{&quot;SD&quot;:&quot;3045022100AA010A609E2A6D5E5DAC54CB469FA4A430BFBADC0CDF5F82F5291A6C9CEACE9B022052E7ECC79DEF081C303703ABC67C07C4CFA842AF6EE21B12C1263C21E80D92FF&quot;}"
  data-example-key="3059301306072A8648CE3D020106082A8648CE3D03010703420004E5CE2801D716BF55B71477CA666BE6A8B8994FEBBF05B04BC0AE7879A62AE77460A14335F3F30657DD7ADC4338398C837DD2DE52D8FCAAE78601D1476FE1C21F">
<p><label for="ocmf">OCMF record</label><br>
<textarea id="ocmf" rows="6" spellcheck="false" autocomplete="off" placeholder="OCMF|{&quot;FV&quot;:&quot;1.0&quot;, …}|{&quot;SD&quot;:&quot;…&quot;}"></textarea></p>
<p><label for="key">The meter's public key — hex, Base64 or Base32, as the meter or the Bundesnetzagentur's list prints it</label><br>
<textarea id="key" rows="3" spellcheck="false" autocomplete="off"></textarea></p>
<p><button type="submit" id="run">Verify</button>
<button type="button" id="example">Load a worked example</button>
<button type="button" id="tamper">…and tamper with it</button></p>
<p class="nojs">The check needs JavaScript and WebAssembly; without them, the
<code>emob verify</code> command does the same offline.</p>
<div id="verdict" class="verdict" role="status" aria-live="polite"></div>
<ol id="readings" class="readings"></ol>
<p id="module" class="module"></p>
</form>
<script src="verify.js" type="module"></script>

## What it checks, and what it does not

It answers one question — **did this key sign these bytes?** — with ECDSA
over the record exactly as written, on the curve the record names. Changing a
single digit of the reading makes it fail; try the tampered example. It
decodes the readings the device checked, so what you see under the verdict
was read from the signed bytes, not from any server.

Three answers are not "verified":

- **does not verify** — the signature does not match these bytes under this
  key: the record was changed, or the key is another meter's;
- **cannot be checked here** — a curve this build does not implement; the
  S.A.F.E. *Transparenzsoftware* can;
- **unreadable** — the record or the key is not in a form the module reads.

A verified signature is necessary for billing and not sufficient: whether a
value may be billed also depends on the meter's status flags and on the
session's records being complete — that is what emob checks before a session
bills. See [Eichrecht, verified end to end](@/docs/eichrecht.md).

## The module

`emob_verify.wasm` is built reproducibly from
[`crates/emob-verify`](https://github.com/hupe1980/emob/tree/main/crates/emob-verify)
(`just wasm`), and is held to the S.A.F.E. reference corpus — every record
with OpenSSL's verdict — in CI. The SHA-256 above is computed in your browser
from the bytes it loaded; compare it with the one in the release notes.
The worked example is `test_ocmf_ebee_01` from that corpus (Apache-2.0).
