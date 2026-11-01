# Use wxDragon as the first Rust desktop toolkit

The first Rust desktop prototype uses wxDragon because it preserves the wxWidgets/native-control accessibility model already exercised by the wxPython reference, exposes explicit accessibility labels and focus APIs, and supports Windows-native controls. Alternatives remain a future escape hatch only if the prototype fails the agreed accessibility or packaging gates.

**Status**: selected for the foundation prototype; owner-heard acceptance pending.

**Evidence**: The prototype targets wxDragon 0.9.24. Its documented API provides native list controls, keyboard focus, accessible labels/descriptions, and Windows support. The Python reference remains the acceptance model for the hands-on accessibility pass.

## Assessment for issue #2

This is a written capability comparison, not a claim that every toolkit was
prototyped or heard with NVDA. Only wxDragon is implemented. Repository policy
requires wxDragon for the GUI; alternatives below are recorded to explain the
choice, not introduced as additional toolkits.

| Boundary | wxDragon 0.9.24 / wxWidgets | egui / eframe | GTK 4 / gtk-rs |
| --- | --- | --- | --- |
| Windows fit | Native Windows controls, matching the wxPython reference's list family. | Custom rendering with an AccessKit accessibility tree. | Windows backend available; GTK 4.18 introduced AccessKit support on Windows. |
| Keyboard | Native virtual report list and panel traversal; initial focus set once. Must verify with keyboard. | Focus/navigation must be checked for the chosen widgets and application composition. | Toolkit focus/navigation support; must verify the Windows integration. |
| Accessible names | Explicit label/description APIs plus the adjacent static-label pattern from Python. | Built-in widgets contribute roles/names; separate labels can use `labelled_by`. | Accessible roles/properties; Windows output needs its own validation. |
| Focus | Native focus API; no background focus changes in this prototype. | Focus follows immediate-mode state; application must preserve identity across updates. | Widget focus model; list changes still need identity and restoration rules. |
| Announcements | Evaluate trypsynth's live-region for later dynamic status. Startup empty state is static text and list description; it is not a tested live announcement. | AccessKit tree exposure alone does not prove status announcements; requires a dedicated prototype. | AccessKit support alone does not prove live status output; requires a dedicated prototype. |
| Packaging | Native C++/SDK, CMake and libclang build prerequisites; native dependencies must be audited for release packaging. trypsynth uses shipfitter. | Rust/native renderer and accessibility backend must be packaged and validated on supported Windows versions. | Requires distributing the GTK runtime and its native dependencies with the Rust adapter. |
| Maintainability | Reuses the reference's wx control behavior and the required ecosystem; pin bindings and test native regressions. | Viable Rust alternative, but would require a different list/accessibility implementation and owner approval. | Viable cross-platform toolkit, but adds a separate runtime and departs from the reference/control ecosystem. |

Select wxDragon for the smallest faithful prototype. Reject egui and GTK for
this foundation because neither provides demonstrated benefits sufficient to
justify changing the mandated toolkit and reference control model. This is a
project-fit decision, not a finding that either alternative is inaccessible.

## Reference and consequences

Python reference inspected at `9aa89e053225ef2c83c4f3659775a00723012ff7`:
`channel_widgets.py::_VirtualChannelList`, `client_ui.py::UiMixin._build_ui`,
and `tools/smoke_accessible_names_nvda.py`. The Rust shell uses the same virtual,
single-selection report list with hidden header, an immediately preceding
visible Channels label, and an explicit accessible name. An empty model has
zero native rows; status is never a selectable fake channel.

The core has no dependency on wxDragon or native types. Widget construction,
focus, layout, and accessible strings stay in `iptv-app`. No unused provider,
formats, or build-job crates are introduced by this ticket.

Production UI acceptance remains conditional on the
[prototype checklist](../validation/0002-toolkit-foundation.md). If Windows
inspection or owner-heard NVDA fails, fix the native boundary and retest before
extending it. Toolkit reconsideration requires an explicit policy decision.

Sources consulted 2026-11-01:

- [wxDragon 0.9.24 API and source](https://docs.rs/wxdragon/0.9.24/wxdragon/), including virtual list callbacks and accessibility APIs.
- [paperback workspace](https://github.com/trypsynth/paperback/blob/master/Cargo.toml) and [fedra dependencies](https://github.com/trypsynth/fedra/blob/main/Cargo.toml): workspace separation and wxDragon/packaging ecosystem examples.
- [egui accessibility documentation](https://github.com/emilk/egui/blob/main/docs/accessibility.md): AccessKit, labels, and inspection support.
- [GTK 4.18 release notes](https://gitlab.gnome.org/GNOME/gtk/-/blob/4.18.0/NEWS) and [GTK Windows development](https://www.gtk.org/docs/installations/windows/): Windows accessibility and runtime/build distribution.
