# Goal 17: Capability Component Catalog Pruning and Consolidation

**Traces to:** ADR-0128.

## What is wanted

1. **Dependency Audit with `comp-capgraph`**:
   - Run `comp-capgraph` across all application specs in `apps/` and gate tests to identify unreachable components.
2. **Move Polyglot Demos**:
   - Reorganize polyglot proof components (`*-go`, `*-py`, `*-cs`, `*-zig`) into a dedicated `components/polyglot/` folder or separate workspace.
3. **Archive Unused Prototypes**:
   - Move unreferenced test domains to an archive directory.
4. **Update `components/Cargo.toml` and CI Gates**:
   - Verify `cargo xtask build` builds production components faster.
   - Verify `components/gates.sh` passes without regressions.

## Acceptance Gate

- Component directory count in top-level `components/` reduced by at least 30%.
- `cargo xtask build` and `reconciler/tests/witsurface.rs` pass cleanly.
