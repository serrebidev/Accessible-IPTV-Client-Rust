# Validation 0033: packaging and updater VM gate

Issue: #83. This is a repeatable Windows VM procedure for the documented
package and updater contract. It does not publish a release, sign an installer,
or edit `.github/workflows`.

## Preparation

Record the application revision, Windows edition/build, Rust toolchain, CMake
version when applicable, and the names and SHA-256 values of every test
artifact. Use a temporary scratch directory that contains no personal files.

```powershell
$ErrorActionPreference = 'Stop'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --quiet
cargo build --release -p iptv-app
Get-FileHash target\release\iptv-app.exe -Algorithm SHA256
```

## Package checks

Using a copy of the release artifact in the scratch directory, verify that the
package contains the binary, help resources, locale catalogues when present,
and no credential-bearing or machine-specific files. Verify that the package
layout matches `PACKAGING.md` and that the binary starts without beside-files.

An absent optional dependency or unavailable packaging tool is recorded as
`Blocked` with its version/error, not as a pass.

## Updater checks

Use sanitized local manifest and archive fixtures to verify:

- case-insensitive selection of `update-manifest.json`;
- version comparison and the GitHub-host token boundary;
- archive SHA-256 verification against the downloaded bytes;
- installer field spelling compatibility;
- missing manifest, missing asset, malformed JSON, digest mismatch, and
  unavailable release errors.

The in-app installer remains out of scope. The verified download helper is
tested without installing or replacing the running application.

## Result record

Record each check as `Pass`, `Fail`, `Blocked`, or `Not run`, with command,
tool/artifact identity, and evidence path. No release upload, tag, signing,
workflow edit, or external update installation is part of this gate.
