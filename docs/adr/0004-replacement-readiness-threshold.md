# Replacement readiness is candidate approval, not a product switch

**Status**: accepted

**Decision**: Rust replacement readiness requires four critical gates on the
current revision: owner-heard NVDA/keyboard acceptance, installed-libVLC
playback, live ffmpeg recording, and packaging/updater validation in the
Windows VM. Physical-renderer evidence is reported separately as a risk rather
than a mandatory gate.

A maintainer may waive an unavailable critical environment gate only with an
explicit dated rationale, named risk, expiration or retest date, and owner
notification. A waiver is never recorded as a pass.

Readiness approves Rust as a candidate for the owner’s replacement decision. It
does not switch the shipping product, retire Python, authorize a release, or
publish an installer. Those actions require separate decisions.

**Consequences**: Automated tests and source evidence cannot by themselves
close the replacement gate. The readiness ledger must keep automated,
owner-heard, live-environment, physical-renderer, waived, blocked, and unrun
evidence distinct. The owner can defer the swap without invalidating the Rust
candidate evidence.
