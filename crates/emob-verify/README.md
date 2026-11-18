# emob-verify

The check a driver runs on a signed meter value: one OCMF record, one key, one
verdict. It is `ocmf::verify` — the function `emob-eichrecht` runs before a
session may bill — plus the words a person reads.

- **Natively**, `emob_verify::check(record, key)` is what `emob verify <file>`
  runs over a S.A.F.E. transparency file.
- **As WebAssembly** (`just wasm`), the same function behind a C ABI that the
  receipt page `csmsd` serves at `/r/{token}` calls on the driver's own phone —
  no `wasm-bindgen`, no npm, no imports.

| Export | Meaning |
|---|---|
| `buffer(len) -> ptr` | an input buffer: the record, then the key |
| `verify(record_len, key_len) -> code` | `0` verified · `1` not verified · `2` curve not checkable here · `3` record unreadable · `4` key unreadable · `5` key/algorithm curve mismatch |
| `output() -> ptr`, `output_len()` | the readings the checked bytes state, `TX\tRV\tRU\tTM` per line |

Brainpool P256/P384 and secp192k1 have no audited pure-Rust arithmetic; a record
signed with one is reported as not checkable here, never as passed or failed.

The tests run all 256 records of the S.A.F.E. reference corpus (Apache-2.0, see
`tests/corpus/NOTICE`) and require the verdict to equal OpenSSL's and
`emob-eichrecht`'s.

`publish = false`: the module is an asset of `csmsd`, not a library.

## License

MIT OR Apache-2.0. Part of [emob](../../README.md).
