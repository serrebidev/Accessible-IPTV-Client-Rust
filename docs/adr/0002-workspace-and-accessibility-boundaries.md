# Keep Rust domain logic independent from the desktop UI

The Rust port will use workspace boundaries with a wx-free core/domain layer, format and provider layers behind narrow interfaces, and a desktop application layer. GUI toolkit selection is evaluated with a short written comparison and one accessible-list prototype before production UI work, with wxDragon as the default unless another option proves it meets the accessibility and Windows requirements better.

**Status**: accepted

**Consequences**: The first vertical slice is a local M3U browse flow, not a parser-only library or a playback-first rewrite; automated accessibility checks and owner-heard NVDA/keyboard validation remain separate completion gates.
