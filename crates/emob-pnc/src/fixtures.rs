//! A V2G test PKI, minted by OpenSSL, embedded as the bytes it produced.
//!
//! # Why these and not a published pool's
//!
//! `emob-pnc` validates certificate paths, and a path validation checked only
//! against DER this repository produced is checked against its own opinions —
//! the same argument `emob-eichrecht` makes about running S.A.F.E.'s records
//! rather than its own. So every certificate here is encoded by **OpenSSL**,
//! to the same ASN.1, with the Annex F fields where Annex F says they go.
//!
//! What that is *not* is interoperability with a live ecosystem. Hubject's own
//! open test PKI is in `specs/roaming/openplugncharge-master.zip` — a real V2G
//! Root, real OEM sub-CAs and real provisioning certificates — and
//! `tests/a_contract_is_installed.rs` runs the provisioning chain against it
//! where the working copy has that archive, saying by name when it does not.
//! The archive is third-party and unlicensed for redistribution, which is why
//! it is indexed rather than embedded.
//!
//! # Re-minting
//!
//! `just pnc-fixtures`, which runs `scripts/make-test-pki.sh` and prints this
//! file. The validity windows are absolute — 2020 to 2050, and 2060 for the
//! roots — so nothing here rots and every test names the instant it judges at,
//! which is the property the whole workspace is built on.

/// Decode one of this module's certificates to DER.
///
/// # Panics
///
/// Never: every constant here is base64 this module emitted.
#[must_use]
pub fn der(pem_body: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(pem_body)
        .expect("a constant this module emitted")
}

/// The V2G root of the ecosystem this platform is connected to.
pub const ROOT: &str = concat!(
    "MIIBxjCCAW2gAwIBAgIUMFK1jwrVp8FJqWehWWmUqYuQrDEwCgYIKoZIzj0EAwIwPzES",
    "MBAGA1UECgwJZW1vYiB0ZXN0MRMwEQYKCZImiZPyLGQBGRYDVjJHMRQwEgYDVQQDDAtW",
    "MkcgUm9vdCBDQTAgFw0yMDAxMDEwMDAwMDBaGA8yMDYwMDEwMTAwMDAwMFowPzESMBAG",
    "A1UECgwJZW1vYiB0ZXN0MRMwEQYKCZImiZPyLGQBGRYDVjJHMRQwEgYDVQQDDAtWMkcg",
    "Um9vdCBDQTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABPZO5m1i099z1uh9akN3g/Zh",
    "kde5M3AwhgdU57CeWmGwOyMHah5tP8RKbwcirAj4kSugOZouQfpAsDG1u6c1vTKjRTBD",
    "MBIGA1UdEwEB/wQIMAYBAf8CAQIwDgYDVR0PAQH/BAQDAgEGMB0GA1UdDgQWBBTdGPNe",
    "w/WRg8p79oAblOD7twKzWTAKBggqhkjOPQQDAgNHADBEAiApSj1evlSlD7NXA8kEOUf/",
    "dsxvXBHdmsr439Me38Rp6QIgGBZAiO18tA7lIRigvFn+KAYkx/LGur2b/kESyHNEFcY=",
);

/// The Mobility Operator's first sub-CA, under [`ROOT`].
pub const MO_SUB_CA_1: &str = concat!(
    "MIIB1DCCAXmgAwIBAgIUJfVbzh/cnWrVBbxJt3MTP2lGJuwwCgYIKoZIzj0EAwIwPzES",
    "MBAGA1UECgwJZW1vYiB0ZXN0MRMwEQYKCZImiZPyLGQBGRYDVjJHMRQwEgYDVQQDDAtW",
    "MkcgUm9vdCBDQTAgFw0yMDAxMDEwMDAwMDBaGA8yMDUwMDEwMTAwMDAwMFowKjESMBAG",
    "A1UECgwJZW1vYiB0ZXN0MRQwEgYDVQQDDAtNTyBTdWIgQ0EgMTBZMBMGByqGSM49AgEG",
    "CCqGSM49AwEHA0IABGkDONDNZqH1Bz7/YiXK0P/Zxqn68YdOMu74yl4V+qt5epK0oMJz",
    "qWPk7D9y93AD5UP8jler0zwTQy0Q7jcVhWSjZjBkMBIGA1UdEwEB/wQIMAYBAf8CAQEw",
    "DgYDVR0PAQH/BAQDAgEGMB0GA1UdDgQWBBTWX4d8JaeXV3zH5FfayhElSA6ijzAfBgNV",
    "HSMEGDAWgBTdGPNew/WRg8p79oAblOD7twKzWTAKBggqhkjOPQQDAgNJADBGAiEAybVO",
    "PrfS0XS4v7t/9RRejx/F/Ksb0kjPVwonlEPwT5ICIQDd3HCTesY7pfSX6fl66hoJi6KU",
    "/IwHoyc9dIZuBTdUyg==",
);

/// Its second, which signs contracts.
pub const MO_SUB_CA_2: &str = concat!(
    "MIIBvTCCAWSgAwIBAgIUaMbQ4Fkj4d/52Km0yxj1QdxOsp4wCgYIKoZIzj0EAwIwKjES",
    "MBAGA1UECgwJZW1vYiB0ZXN0MRQwEgYDVQQDDAtNTyBTdWIgQ0EgMTAgFw0yMDAxMDEw",
    "MDAwMDBaGA8yMDUwMDEwMTAwMDAwMFowKjESMBAGA1UECgwJZW1vYiB0ZXN0MRQwEgYD",
    "VQQDDAtNTyBTdWIgQ0EgMjBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABEvDnDLnqaVa",
    "EoQx6XDiR4zjP9Gl3T//N4VQWtVxefHtL0sxCCbBXMsSplnFDs37YHbD+DVrXxSDkomK",
    "PWHO6U6jZjBkMBIGA1UdEwEB/wQIMAYBAf8CAQAwDgYDVR0PAQH/BAQDAgEGMB0GA1Ud",
    "DgQWBBQ7Wcu+WC3oxyH9lt0Ui1sT8F/UqzAfBgNVHSMEGDAWgBTWX4d8JaeXV3zH5Ffa",
    "yhElSA6ijzAKBggqhkjOPQQDAgNHADBEAiA4f4SeA2pFw8ciiwRYLhvIRNbQMjzSxSYD",
    "h3qF3MMtAQIgGoB7dL/6XuNeLsjHNefmPk/uc5CSqJ8DZMtGUt8X7ww=",
);

/// A contract certificate — `CN=DE8AACA2B3C4D5L`, which `[V2G2-108]` makes the EMAID.
pub const CONTRACT: &str = concat!(
    "MIIBvDCCAWKgAwIBAgIUKmB5nH/qsluzCFWGnNASdnh3qO4wCgYIKoZIzj0EAwIwKjES",
    "MBAGA1UECgwJZW1vYiB0ZXN0MRQwEgYDVQQDDAtNTyBTdWIgQ0EgMjAgFw0yMDAxMDEw",
    "MDAwMDBaGA8yMDUwMDEwMTAwMDAwMFowLjESMBAGA1UECgwJZW1vYiB0ZXN0MRgwFgYD",
    "VQQDDA9ERThBQUNBMkIzQzRENUwwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAR3P3WM",
    "aXaWpGSFN3LWYSyI6fN+i3oG+4mbVwMLJ+54IOwo8pck651snTC4R7xx17pMAvTYoY7f",
    "BsTMevlp6Vc4o2AwXjAMBgNVHRMBAf8EAjAAMA4GA1UdDwEB/wQEAwIGwDAdBgNVHQ4E",
    "FgQUh9/TO3/mX8YSPRjOl/zoyNsGQHIwHwYDVR0jBBgwFoAUO1nLvlgt6Mch/ZbdFItb",
    "E/Bf1KswCgYIKoZIzj0EAwIDSAAwRQIgJsil6jazGtGlAQH5ZxeLv0t6xcU6WmGOJQrk",
    "UJwUAToCIQCPdjeY+TPcWm4vk+w0Fh6iVUo97oV0G475uLPB+m2fJg==",
);

/// The manufacturer's first sub-CA, under [`ROOT`].
pub const OEM_SUB_CA_1: &str = concat!(
    "MIIB1TCCAXqgAwIBAgIUJfVbzh/cnWrVBbxJt3MTP2lGJu0wCgYIKoZIzj0EAwIwPzES",
    "MBAGA1UECgwJZW1vYiB0ZXN0MRMwEQYKCZImiZPyLGQBGRYDVjJHMRQwEgYDVQQDDAtW",
    "MkcgUm9vdCBDQTAgFw0yMDAxMDEwMDAwMDBaGA8yMDUwMDEwMTAwMDAwMFowKzESMBAG",
    "A1UECgwJZW1vYiB0ZXN0MRUwEwYDVQQDDAxPRU0gU3ViIENBIDEwWTATBgcqhkjOPQIB",
    "BggqhkjOPQMBBwNCAASJxvKLyGLNm+FX2+SuYYCz++O73g7ESQzRS0MOVc4Y7j5n0tKQ",
    "yHgZfsngjVK0d9ybeR8BStgF4QuKVdvh1ZQco2YwZDASBgNVHRMBAf8ECDAGAQH/AgEB",
    "MA4GA1UdDwEB/wQEAwIBBjAdBgNVHQ4EFgQUOVx/n2GXncRSXE45NeEopQTy9v0wHwYD",
    "VR0jBBgwFoAU3RjzXsP1kYPKe/aAG5Tg+7cCs1kwCgYIKoZIzj0EAwIDSQAwRgIhAK2d",
    "0Vx7+JT5pIB2xSCcZKeupvfYukRzYkjY6fOaSI9HAiEAiwOhLZKWEiAK/HgvmZa0GkcJ",
    "LwVt+yy1NUNL3WGHMsg=",
);

/// Its second, which signs vehicles.
pub const OEM_SUB_CA_2: &str = concat!(
    "MIIBvzCCAWagAwIBAgIUL1Fyzwee+HonlLbOyg9X6BqDxIowCgYIKoZIzj0EAwIwKzES",
    "MBAGA1UECgwJZW1vYiB0ZXN0MRUwEwYDVQQDDAxPRU0gU3ViIENBIDEwIBcNMjAwMTAx",
    "MDAwMDAwWhgPMjA1MDAxMDEwMDAwMDBaMCsxEjAQBgNVBAoMCWVtb2IgdGVzdDEVMBMG",
    "A1UEAwwMT0VNIFN1YiBDQSAyMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAETYSKhfio",
    "e96zt0yJMYXDEZL5o8YHHbLZQ+XyIt9r26BFTS2zFyGUE9CEOvTYsFzcEUIZaONTKY2R",
    "hhCbmwvyraNmMGQwEgYDVR0TAQH/BAgwBgEB/wIBADAOBgNVHQ8BAf8EBAMCAQYwHQYD",
    "VR0OBBYEFFgEISdkoHDGMe+nPH0S0/EZ8o/cMB8GA1UdIwQYMBaAFDlcf59hl53EUlxO",
    "OTXhKKUE8vb9MAoGCCqGSM49BAMCA0cAMEQCIBZsLafdpOvSfKSiptdb6T9aV9Y1QMU5",
    "+6jhXFNHfU9cAiA/qvcF5oNYXmIrgw5I6kariUDuv44hLEA2emZAdGXpEQ==",
);

/// An OEM provisioning certificate — `CN=WVW00000000000001`, which `[V2G2-933]` makes the `CertID`.
pub const PROVISIONING: &str = concat!(
    "MIIBvzCCAWWgAwIBAgIUGERT3mjM5Nk/OKkGImdOBlrNPxYwCgYIKoZIzj0EAwIwKzES",
    "MBAGA1UECgwJZW1vYiB0ZXN0MRUwEwYDVQQDDAxPRU0gU3ViIENBIDIwIBcNMjAwMTAx",
    "MDAwMDAwWhgPMjA1MDAxMDEwMDAwMDBaMDAxEjAQBgNVBAoMCWVtb2IgdGVzdDEaMBgG",
    "A1UEAwwRV1ZXMDAwMDAwMDAwMDAwMDEwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAARW",
    "SKzFrpQWmLkB06nx0DRSbq0rj5Y3gax3liZGEEt+pwiIbYKpnN96BKEnyr6z1rNOXkrL",
    "33pWG0xxm8JxJykZo2AwXjAMBgNVHRMBAf8EAjAAMA4GA1UdDwEB/wQEAwIHgDAdBgNV",
    "HQ4EFgQU5s6LgzqoqrzudUal2SiKlU/DG3cwHwYDVR0jBBgwFoAUWAQhJ2SgcMYx76c8",
    "fRLT8Rnyj9wwCgYIKoZIzj0EAwIDSAAwRQIhAL31g8KB3L2O+AVyp+HO7mMva38RzYQu",
    "sUbiKFyWiTC5AiBJffnNz71RL0f/o25iPrG0QmCRngDsrFenZ80RMOpIJQ==",
);

/// **Another ecosystem's** V2G root. The whole of R10 is that this one is not [`ROOT`].
pub const OTHER_ROOT: &str = concat!(
    "MIIB3TCCAYOgAwIBAgIUAS/Of2sBPt9LUPbJfBTPu5x4kB4wCgYIKoZIzj0EAwIwSjEX",
    "MBUGA1UECgwOYW5vdGhlciBzY2hlbWUxEzARBgoJkiaJk/IsZAEZFgNWMkcxGjAYBgNV",
    "BAMMEU90aGVyIFYyRyBSb290IENBMCAXDTIwMDEwMTAwMDAwMFoYDzIwNjAwMTAxMDAw",
    "MDAwWjBKMRcwFQYDVQQKDA5hbm90aGVyIHNjaGVtZTETMBEGCgmSJomT8ixkARkWA1Yy",
    "RzEaMBgGA1UEAwwRT3RoZXIgVjJHIFJvb3QgQ0EwWTATBgcqhkjOPQIBBggqhkjOPQMB",
    "BwNCAARivbCLwsCEgcOM1jWGhiEKBpB5l/mD3nJpuqQXMmcpN27tgV40VJ0w+aN0GiYN",
    "cWmG/8fj8tp7ThiwrlJ+YM7eo0UwQzASBgNVHRMBAf8ECDAGAQH/AgECMA4GA1UdDwEB",
    "/wQEAwIBBjAdBgNVHQ4EFgQUhdK8SfXrOWUBpxWX/0/ztgMWm0EwCgYIKoZIzj0EAwID",
    "SAAwRQIhAL1lvoOenz9qKzlJXzTveLGRn/LKzVJpL8p/cKdb1HV/AiA2+RSElecGsqmr",
    "jcrYhaBDWlCE3y5xWll2cvZLj88DVg==",
);

/// That ecosystem's Mobility Operator sub-CA.
pub const OTHER_MO_SUB_CA_1: &str = concat!(
    "MIIB6TCCAY+gAwIBAgIUBJJJMIDU3dgoEo4rkIAjcLHNXYAwCgYIKoZIzj0EAwIwSjEX",
    "MBUGA1UECgwOYW5vdGhlciBzY2hlbWUxEzARBgoJkiaJk/IsZAEZFgNWMkcxGjAYBgNV",
    "BAMMEU90aGVyIFYyRyBSb290IENBMCAXDTIwMDEwMTAwMDAwMFoYDzIwNTAwMTAxMDAw",
    "MDAwWjA1MRcwFQYDVQQKDA5hbm90aGVyIHNjaGVtZTEaMBgGA1UEAwwRT3RoZXIgTU8g",
    "U3ViIENBIDEwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAT0HcSdfe+jNeVZ4tDxu/Sw",
    "Z2j37PjnFX2fZdpCvc5aKgqLu0qCLRyL3aNmsL1HY/pwbuvacH+sape+mZWO6sC4o2Yw",
    "ZDASBgNVHRMBAf8ECDAGAQH/AgEBMA4GA1UdDwEB/wQEAwIBBjAdBgNVHQ4EFgQUL3w1",
    "4KryYwIwM/NtM/NmCZnLXsQwHwYDVR0jBBgwFoAUhdK8SfXrOWUBpxWX/0/ztgMWm0Ew",
    "CgYIKoZIzj0EAwIDSAAwRQIhAK/ar2S0KYny7ASStnpvlDglUAMXoZx+LtO5XOPN3dmh",
    "AiBXmHm/sYf3am9ue4r5Twm2qwGRa8OGfGEtHvLwxzu3jg==",
);

/// A contract minted in the other ecosystem. Impeccable, and not this platform's.
pub const FOREIGN_CONTRACT: &str = concat!(
    "MIIBzTCCAXKgAwIBAgIUcQn3ZdP57vXGYoaIVbS8PZzYelEwCgYIKoZIzj0EAwIwNTEX",
    "MBUGA1UECgwOYW5vdGhlciBzY2hlbWUxGjAYBgNVBAMMEU90aGVyIE1PIFN1YiBDQSAx",
    "MCAXDTIwMDEwMTAwMDAwMFoYDzIwNTAwMTAxMDAwMDAwWjAzMRcwFQYDVQQKDA5hbm90",
    "aGVyIHNjaGVtZTEYMBYGA1UEAwwPREU4QkJDQTJCM0M0RDVNMFkwEwYHKoZIzj0CAQYI",
    "KoZIzj0DAQcDQgAEGETFPsO7cn2/2VlPiORFWkv6QBRxYzPy7Z4yIGyyHnpx12aKE4Bj",
    "odMt87utUKWVTIWoZmS3Rec2nICzzMC846NgMF4wDAYDVR0TAQH/BAIwADAOBgNVHQ8B",
    "Af8EBAMCBsAwHQYDVR0OBBYEFL9PkxKQ//tag0eY/JaN/FpSIAmPMB8GA1UdIwQYMBaA",
    "FC98NeCq8mMCMDPzbTPzZgmZy17EMAoGCCqGSM49BAMCA0kAMEYCIQChF4bu01hrGX1s",
    "/ezStM7zt61WASqBp8ypKJqlViRSDwIhAKlh5wuuBr/uUATP03AXbKmKWNYt35hSPW1L",
    "Vs/MGi7J",
);
