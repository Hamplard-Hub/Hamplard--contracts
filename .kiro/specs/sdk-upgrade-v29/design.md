# Design: Upgrade soroban-sdk to v29

## Overview

This is a dependency-bump upgrade. The only files that change are `Cargo.toml` files,
`Cargo.lock`, any source lines in `lib.rs` / `test.rs` that reference removed or renamed
SDK APIs, and (if content changes) the 323 snapshot JSON files. No contract logic, storage
schema, or event layout changes.

---

## Version Delta

| Crate | Current pin | Target pin |
|---|---|---|
| `soroban-sdk` | `"22"` (workspace) | `"29.0.1"` |
| Rust MSRV | unstated | `1.91.0` (add to workspace `Cargo.toml`) |

The workspace `Cargo.toml` carries the single pin. The contract `Cargo.toml` inherits it via
`{ workspace = true }` for both `[dependencies]` and `[dev-dependencies]`. No per-crate
overrides are needed.

---

## Known Breaking Changes: v22 → v29

The table below summarises every SDK interface change that could affect this contract,
based on the public release history and security advisories. Items marked **AFFECTS CONTRACT**
require a source edit; items marked **no impact** require only awareness.

### Security Fixes (no source change required)

| Advisory | Fixed in | Impact on this contract |
|---|---|---|
| `GHSA-96xm-fv9w-pf3f` — `Bytes::slice` / `Vec::slice` / `Prng::gen_range` arithmetic overflow | `> 25.0.1` | This contract does not call `Bytes::slice`, `Vec::slice`, or `Prng::gen_range` directly. The fix is silent — **no source change**. |
| `GHSA-4chv-4c6w-w254` — `#[contractimpl]` trait vs inherent name collision | Patch release | The contract's `HamplardContract` impl block has no trait implementations with name-colliding inherent methods. **No source change**. |
| `CVE-2026-32322` — auth bypass via crafted scalar field comparisons | Later release | The contract does not perform scalar field equality comparisons. **No source change**. |

### API Changes (compile-error candidates)

The SDK follows Stellar protocol versioning. Major bumps (v23, v24, … v29) each track a
protocol version and may deprecate or remove items. The categories below cover the APIs this
contract actually uses. Compile errors must be resolved before any test can run.

**Compile error resolution process:**
1. Edit `Cargo.toml` to pin `soroban-sdk = "29.0.1"`.
2. Run `cargo build 2>&1 | grep "^error"` to enumerate all errors.
3. For each error, apply the fix described in the relevant category below.
4. Repeat until `cargo build` exits 0.
5. Then run `cargo test`.

#### Category A — `token::Client` / `token::Interface` changes

The `token` module is re-exported from `soroban-sdk`. Between v22 and v29 the Stellar Asset
Contract interface gained new methods (`transfer_from`, `approve`) and some function signatures
may have changed. The contract currently calls:

```rust
token::Client::new(&env, &token)
    .transfer(from, to, &amount)
    .try_transfer(from, to, &amount)
    .balance(&address)
    .decimals()
```

All four of these are stable SAC interface methods and are expected to survive. If a compile
error appears here, the fix is to check `soroban-sdk` docs for the new signature and update
the call site.

#### Category B — `env.deployer().update_current_contract_wasm(hash)`

Used in `upgrade_contract`. The `Deployer` API surface changed in SDK v23+ to consolidate
methods. If `update_current_contract_wasm` was renamed or moved, the fix is:

```rust
// Possible new form (check actual compile error):
env.deployer().update_current_contract_wasm(pending.new_wasm_hash);
// OR
env.current_contract().update_wasm(pending.new_wasm_hash);
```

#### Category C — `soroban_sdk::Symbol::new` stability

`Symbol::new` has been stable since before v22. No change expected.

#### Category D — Removed re-exports or renamed macros

Between v22 and v29 several items moved modules. Common cases:
- `soroban_sdk::contracttype` → still `soroban_sdk::contracttype` (stable).
- `soroban_sdk::contract` / `contractimpl` → still stable.
- `soroban_sdk::token` — still re-exported.

If any `use soroban_sdk::...` import fails to resolve, find the new path in the v29 docs and
update the `use` statement in `lib.rs`.

#### Category E — `testutils` feature changes

The `[dev-dependencies]` section enables `features = ["testutils"]`. Between v22 and v29 the
testutils module gained:
- Improved `mock_all_auths` / `mock_auths` helpers.
- `env.events().all()` for inspecting all emitted events in a test.

These are **additive** and never remove existing test-utilities APIs that v22 tests rely on.
The existing `last_event_val` helper in `test.rs` (a hand-rolled wrapper) should continue to
compile; if its internals call a removed API, update it to use the equivalent v29 API.

#### Category F — `ArbitrationCaseKey` as a storage key

The contract uses a `#[contracttype]` struct (`ArbitrationCaseKey`) directly as a storage key
via `env.storage().persistent().get(&case_key)`. This pattern relies on `IntoVal<Env, Val>`
being derived by `#[contracttype]`. This derivation was stable at v22 and remains stable at
v29. **No change expected.**

---

## Compile Error Resolution Workflow

```
1. Edit workspace Cargo.toml: soroban-sdk = "29.0.1"
2. cargo build 2>&1 | grep "^error"   → list all errors
3. For each error:
     a. Identify the API (token, deployer, Symbol, use-path, testutils)
     b. Apply the fix from Category A–F above (or discover a new category)
     c. Document the fix in docs/sdk-v29-migration.md
4. cargo build  → must exit 0
5. cargo test   → must exit 0, all 323 snapshots stable
```

---

## Test Snapshot Strategy

The 323 JSON files in `contracts/hamplard/test_snapshots/test/` record ledger state,
authorization traces, and diagnostic data captured by the Soroban test framework. They do
**not** record event payloads (events are stored in a separate in-memory buffer, not in
snapshots). However, if the SDK changes how the host serialises XDR or auth traces, snapshot
content may change.

**Expected outcome:** snapshots unchanged (pure dependency bump, no storage layout change).

**If snapshots do change:**
1. Inspect the diff to confirm only whitespace / serialisation format changes, not logic.
2. Run `UPDATE_SNAPSHOTS=1 cargo test` to regenerate.
3. Run `cargo test` again (without the flag) to confirm stability.
4. Commit the updated snapshots as part of this PR.

---

## Files Changed

| File | Change |
|---|---|
| `Cargo.toml` | `soroban-sdk = "22"` → `soroban-sdk = "29.0.1"` and add `rust-version = "1.91.0"` |
| `Cargo.lock` | Regenerated by Cargo automatically |
| `contracts/hamplard/src/lib.rs` | Fix any compile errors from Categories A–F |
| `contracts/hamplard/src/test.rs` | Fix any compile errors from testutils API changes |
| `contracts/hamplard/test_snapshots/test/*.json` | Regenerate if SDK changes serialisation |
| `docs/sdk-v29-migration.md` | New file — records every observed API change |

---

## Migration Notes Document

`docs/sdk-v29-migration.md` is written **during** the upgrade (after `cargo build` succeeds)
not before. Its template:

```markdown
# soroban-sdk v22 → v29 Migration Notes

## Version bumped
- `soroban-sdk`: `22.x` → `29.0.1`
- Rust MSRV: (previous) → `1.91.0`

## Security advisories resolved
- GHSA-96xm-fv9w-pf3f ...
- GHSA-4chv-4c6w-w254 ...
- CVE-2026-32322 ...

## API changes requiring source edits
<!-- Fill in each error encountered and the fix applied -->
| Location | Old API | New API | Reason |
|---|---|---|---|

## API changes with no source edit (informational)
<!-- Note deprecated items that still compile but should be revisited -->

## Snapshot changes
<!-- "None" or list of changed snapshots with reason -->

## New features available (not yet used)
- `#[contractevent]` typed event macro (see separate typed-events issue)
- ...
```

---

## Non-Goals

- Migrating to `#[contractevent]` typed events — separate issue.
- Upgrading any other dependency (e.g. dev tooling, CI actions).
- Changing any contract logic, storage schema, or event payload.
- Updating the `stellar-cli` / `stellar contract build` version.

---

## Architecture

This is a dependency-only upgrade. The contract architecture is unchanged: a single Soroban
smart contract (`HamplardContract`) in one `cdylib` crate, with no external crate dependencies
beyond `soroban-sdk` itself. The upgrade does not introduce new crates, modules, or layers.

```
workspace
└── contracts/hamplard      (cdylib)
    ├── src/lib.rs           ← may need API call-site fixes
    └── src/test.rs          ← may need testutils API fixes
```

---

## Components and Interfaces

The only component boundary affected is the **contract ↔ soroban-sdk boundary**. The public
function signatures of `HamplardContract` are unchanged (requirement 3.2). The SDK's internal
host-contract interface (WASM host functions) may have changed between v22 and v29 at the ABI
level, but this is transparent — Cargo rebuilds the WASM against the new host ABI automatically.

| Component | Interface | Change |
|---|---|---|
| `HamplardContract` public fns | `contractimpl` exports | Unchanged |
| `token::Client` | SAC methods called by contract | May need call-site updates (task 3) |
| `env.deployer()` | Wasm upgrade path | May need rename (task 4) |
| `testutils` helpers in `test.rs` | `last_event_val`, `mock_all_auths` | May need updates (task 6) |
| Snapshot JSON files | XDR serialisation format | Regenerate if changed (task 9) |

---

## Data Models

All `#[contracttype]` structs, enums, and `DataKey` / `DataKey2` variants are unchanged.
Soroban's XDR encoding of contract types is determined by the type definitions, not by the SDK
version. As long as no type definition is modified, deployed state is compatible across the
upgrade.

---

## Correctness Properties

### Property 1: Build correctness
`cargo build` exits 0 with zero errors after the bump.

**Validates: Requirements 2.1**

### Property 2: Test correctness
`cargo test` exits 0 with all 323 snapshots stable (or regenerated and then stable on a second run).

**Validates: Requirements 2.4, 3.3**

### Property 3: Security patch coverage
The three patched CVEs (`GHSA-96xm-fv9w-pf3f`, `GHSA-4chv-4c6w-w254`, `CVE-2026-32322`) are no longer present in the resolved dependency tree at `soroban-sdk = "29.0.1"`.

**Validates: Requirements 2.2**

### Property 4: Interface compatibility
Every public function callable before the upgrade (`init`, `register_course`, `enroll`, `approve_course`, etc.) remains callable with the same argument and return types after the upgrade.

**Validates: Requirements 3.1, 3.2**

---

## Error Handling

Compile errors introduced by the bump are resolved iteratively by category (A–F in the "Known
Breaking Changes" section). If an error does not match any known category, it is treated as a
new category, fixed minimally, and documented in `docs/sdk-v29-migration.md`.

Test failures (non-snapshot) after `cargo build` succeeds indicate a behavioural change in the
SDK — investigate the specific test, understand the SDK change, and apply the minimal fix.

---

## Testing Strategy

1. `cargo build` — compile check, no tests run.
2. `cargo test --no-run` — compile the test binary (catches testutils errors).
3. `cargo test` — runs all 323 snapshot tests plus unit tests.
4. `UPDATE_SNAPSHOTS=1 cargo test` then `cargo test` — regenerate and stabilise snapshots if needed.
5. `stellar contract build` (or `cargo build --release --target wasm32-unknown-unknown`) — confirms a valid Wasm artifact.
