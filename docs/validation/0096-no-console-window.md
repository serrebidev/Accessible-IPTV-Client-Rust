# 0096 — Windows console window opened beside the app

Date: 2026-11-06. Branch `agent/no-console-window` from main
4b229d4 (v0.4.19). Ships as v0.4.20.

## Report

Brandon: opening the app on Windows also opened a cmd
(console) window. He asked for it gone, committed and
released.

## Cause and fix

The binary was linked with the default console subsystem —
Rust's default for a binary crate — so Windows creates a
console for it at launch. The Python client is a
GUI-subsystem program and never shows one.

Fix, one line at the top of `crates/iptv-app/src/main.rs`,
the same line BlindRadio and Blind Designer already carry:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
```

Release builds get the Windows GUI subsystem (no console);
debug builds keep a console for development output. The app
has no command-line mode and prints nothing to stdout in
normal operation (diagnostics go to its log), so nothing
loses its output channel.

Checked for the same gap in the sibling apps: BlindRadio
and Blind Designer already carry the line. BlindRSS-Rust
and BlindFB do not; BlindRSS has a command-line self-test
mode (`--blindrss-runtime-self-test`) whose console output
may be the reason there, so neither was changed on this
order — flagged to Brandon in the ship report.

## Verification

Local gates (fmt, clippy `-D warnings`, workspace tests,
playlist parity). The attribute is `cfg(windows)`-gated in
effect (the subsystem only exists on Windows), so the real
proof is the Release run's Windows leg building green plus
Brandon's launch on v0.4.20: app opens, no cmd window.
