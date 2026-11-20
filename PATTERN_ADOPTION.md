# Pattern adoption

Declared per the pattern canon adoption format; every claim is verified against code in this repo.

| Pattern | Status | Evidence |
|---|---|---|
| T17 | partial | src/engine.rs:45 (dirty set) + src/engine.rs:49 (cached topological order); dirty-set recalc, not a memoized query graph |
| T13 | adopted | src/lib.rs:7 (engine composes the L0 language and L1 storage and codec crates) |
