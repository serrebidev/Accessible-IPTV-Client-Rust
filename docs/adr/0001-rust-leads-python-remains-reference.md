# Rust leads while Python remains the reference product

The Rust port leads new development, but the wxPython application remains the shipping product and behavioral reference until the replacement gate is explicitly met. This allows the rewrite to move quickly without silently breaking the product or declaring parity from an unpinned moving target.

**Status**: accepted

**Consequences**: Every vertical slice records the Python revision it matches and uses shared fixtures or regression evidence; replacing Python remains a deliberate release decision.
