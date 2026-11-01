# Use wxDragon as the first Rust desktop toolkit

The first Rust desktop prototype uses wxDragon because it preserves the wxWidgets/native-control accessibility model already exercised by the wxPython reference, exposes explicit accessibility labels and focus APIs, and supports Windows-native controls. Alternatives remain a future escape hatch only if the prototype fails the agreed accessibility or packaging gates.

**Status**: accepted

**Evidence**: The prototype targets wxDragon 0.9.24. Its documented API provides native list controls, keyboard focus, accessible labels/descriptions, and Windows support. The Python reference remains the acceptance model for the hands-on accessibility pass.
