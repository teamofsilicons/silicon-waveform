# Dependency advisory review

Reviewed 2026-10-10: `RUSTSEC-2023-0071` affects RSA private-key operations whose timing an attacker can observe. Upstream has no patched stable release. The one-ID cargo-deny exception is confined to Waveform; other advisory, license and source checks remain enabled.

`cargo tree --locked -i rsa --edges normal` gives `waveform → silicon-accounts-client 0.4.0 → jsonwebtoken 11.1.0 → rsa 0.9.10`. The Accounts client's `src/jwt.rs:121` rejects every algorithm other than EdDSA before verification, constructs only an Ed25519 decoding key, and validates with `Algorithm::EdDSA` at line 166. Its `src/silicon_key.rs:112-128` signs Silicon assertions directly with ed25519-dalek. Waveform has no RSA key-loading, signing or decryption calls; its JWT test helper (`src/test_support.rs`) also uses Ed25519. Waveform's RSA certificate-generation commands run the external OpenSSL executable for PostgreSQL TLS and do not call the Rust `rsa` crate.

The vulnerable private-key operations are therefore unreachable through this application. Reassess this exception whenever the Accounts client, JWT algorithms or signing code changes. This review does not apply to the Accounts server or other repositories that perform RSA private-key operations.

Source: https://rustsec.org/advisories/RUSTSEC-2023-0071
