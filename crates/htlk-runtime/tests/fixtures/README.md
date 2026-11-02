# Runtime identity fixtures

- `native-empty.hex` is a complete canonical Draft 0.1 executable with a finite
  host-selected policy and native implementation profile. Its first two lines pin
  the envelope fingerprint and root-scope digest; remaining lines are exact
  lowercase hexadecimal envelope bytes.
- `runtime-identities.json` contains independent known-answer vectors for runtime
  identity domains, including typed content and producer provenance.

The Rust `verified` and `identity` suites consume these fixed assets. The independent
Node checker verifies CBOR ordering/shortest forms, policy JCS, hash framing,
envelope/record identities, and explicit native source contributions. It is a
fixture checker, not a second general executable verifier.

See [native identity and fixture maintenance](../../../../docs/testing.md#native-identity-and-fixtures)
for the explicit generation and review procedure. Tests never regenerate goldens.
