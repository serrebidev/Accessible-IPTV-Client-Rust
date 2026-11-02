# Accessible-IPTV-Client-Rust
Rust port of Accessible-IPTV-Client (in development; Python app remains the shipping product)

The foundation uses Rust **1.97.1** (stable release, also the explicit MSRV),
pinned in `rust-toolchain.toml`. Run Cargo from this checkout so rustup selects
that toolchain. Dependency versions are resolved by the committed `Cargo.lock`.

- `crates/iptv-core`: widget-free domain types, parsing and search; no dependencies.
- `crates/iptv-app`: wxDragon desktop adapter and virtual channel list.

On Windows, install the MSVC C++ build tools and Windows SDK, CMake (on `PATH`),
and LLVM/libclang. wxDragon's native build requires these even for `cargo check`.
For the standard LLVM installation, use this PowerShell session setup:

```powershell
$env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
$env:PATH = 'C:\Program Files\CMake\bin;C:\Program Files\LLVM\bin;' + $env:PATH
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
Use Alt+O or Tab to **Open playlist**, choose a local `.m3u`/`.m3u8` file,
then Tab to **Categories**. Use arrows to choose All Channels or a playlist
group, then Tab to **Search channels** to filter within that category. Search ignores
case and ranks exact names, word starts, then other matches; ties retain
playlist order. Clear the field to restore the active category. All Channels comes first;
groups follow in case-sensitive lexical order, with source-order rows. Missing,
empty and whitespace-only groups share **Uncategorized** with explicitly named
Uncategorized entries. Tab or Enter
moves to **Channels**; Shift+Tab returns from Channels to search. Browse with
the arrow keys, Home and End. Results preserve the selected channel while it
remains present; otherwise the first result is selected. Search completion
never moves keyboard focus. No matches produce zero rows and recovery text.
Reading and parsing run on a cancellable worker. Open another file to replace a
pending load, or use **Cancel loading** (Alt+C). Typing a new search cancels a
pending load and searches the previous playlist. Cancelling the file chooser
keeps the current operation intact. Failed loads keep the previous channels;
use **Open playlist** to retry or choose another file. Empty playlists have zero
rows and recovery guidance. Tab from Channels to the read-only **Playlist status**
field to review the current state. Loads, results, empty and recovery states use
live-region announcements without moving keyboard focus. Owner-heard NVDA
validation remains pending.
After `cargo build -p iptv-app`, run `powershell -NoProfile -File tools/smoke-foundation.ps1`
on an interactive Windows desktop to inspect the native accessibility tree.
This briefly opens the prototype and closes it normally; it does not send keys
or change screen-reader/system settings.

See [the toolkit decision](docs/adr/0003-wxdragon-first-toolkit.md) and
[the prototype acceptance checklist](docs/validation/0002-toolkit-foundation.md).
Build and unit-test success does not close the owner-heard NVDA gate.
See [local browse evidence and remaining checks](docs/validation/0003-local-playlist-browse.md).

See [incremental search evidence and remaining checks](docs/validation/0004-incremental-channel-search.md).

See [playlist recovery and pinned Python parity evidence](docs/validation/0005-playlist-recovery.md).

See [the pending owner-heard NVDA acceptance record and session steps](docs/validation/0006-owner-heard-accessibility.md).

See [grouped category evidence and remaining checks](docs/validation/0010-grouped-category-browsing.md).
