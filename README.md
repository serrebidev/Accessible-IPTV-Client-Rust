# Accessible-IPTV-Client-Rust
Rust port of Accessible-IPTV-Client (in development; Python app remains the shipping product)

The foundation uses Rust **1.97.1** (stable release, also the explicit MSRV),
pinned in `rust-toolchain.toml`. Run Cargo from this checkout so rustup selects
that toolchain. Dependency versions are resolved by the committed `Cargo.lock`.

- `crates/iptv-core`: widget-free domain types, parsing and search; no dependencies.
- `crates/iptv-app`: wxDragon desktop adapter and empty virtual channel list.

On Windows, install the MSVC C++ build tools and Windows SDK, CMake (on `PATH`),
and LLVM/libclang. wxDragon's native build requires these even for `cargo check`.
For the standard LLVM installation, use this PowerShell session setup:

```powershell
$env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
cmake --version
rustc --version
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

The first build downloads/builds wxWidgets through `wxdragon-sys` and can take
several minutes. The core can be checked separately with `cargo test -p iptv-core`
without a GUI toolchain. Run the prototype with `cargo run -p iptv-app`.
After `cargo build -p iptv-app`, run `powershell -NoProfile -File tools/smoke-foundation.ps1`
on an interactive Windows desktop to inspect the native accessibility tree.
This briefly opens the prototype and closes it normally; it does not send keys
or change screen-reader/system settings.

See [the toolkit decision](docs/adr/0003-wxdragon-first-toolkit.md) and
[the prototype acceptance checklist](docs/validation/0002-toolkit-foundation.md).
Build and unit-test success does not close the owner-heard NVDA gate.
