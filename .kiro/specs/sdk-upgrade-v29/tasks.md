# Implementation Plan: Upgrade soroban-sdk to v29

## Overview

Bump `soroban-sdk = "22"` to `soroban-sdk = "29.0.1"` in the workspace `Cargo.toml`. Fix
every compile error the bump introduces (there may be none, or several depending on API
removals between v22 and v29). Regenerate snapshots only if the SDK changes XDR
serialisation. Document all observed changes.

The actual set of compile errors is not known until the bump is applied — tasks 3–7 are
**conditional**: execute only if `cargo build` produces an error in that category.

---

## Notes

Tasks 3–7 are conditional on compile errors discovered in task 2. If `cargo build` succeeds
after task 1 with zero errors, tasks 3–7 can be skipped and execution jumps directly to
task 8. Always run task 2 (the error enumeration step) before deciding which fix tasks apply.

---

## Task Dependency Graph

```json
{
  "waves": [
    {"wave": 1, "tasks": ["1"]},
    {"wave": 2, "tasks": ["2"]},
    {"wave": 3, "tasks": ["3", "4", "5", "6", "7"]},
    {"wave": 4, "tasks": ["8"]},
    {"wave": 5, "tasks": ["9"]},
    {"wave": 6, "tasks": ["10", "11"]}
  ]
}
```

---

## Tasks

- [ ] 1. Bump the dependency and state the Rust MSRV
  - Open `Cargo.toml` (workspace root).
  - Change `soroban-sdk = "22"` to `soroban-sdk = "29.0.1"` under `[workspace.dependencies]`.
  - Add `rust-version = "1.91.0"` to the `[workspace]` table (below `resolver = "2"`).
  - Do not change `contracts/hamplard/Cargo.toml` — it inherits via `{ workspace = true }`.
  - Run `cargo update` to regenerate `Cargo.lock` against the new pin.
  - **Files:** `Cargo.toml`, `Cargo.lock`
  - **Verification:** `cargo metadata --format-version 1 | grep soroban-sdk` shows `29.0.1`.

- [ ] 2. Run `cargo build` and catalogue every compile error
  - From the workspace root, run `cargo build 2>&1`.
  - For each error line, record: file, line number, error message, API name.
  - Errors will fall into one of the categories in `design.md` (A–F) or reveal a new category.
  - Do not fix anything yet — enumerate first so fixes can be applied systematically.
  - **Files:** none (read-only step)
  - **Verification:** complete list of errors written to a scratch note or terminal buffer.

- [ ] 3. Fix Category A compile errors — `token::Client` API changes (conditional)
  - **Execute only if** `cargo build` produced errors referencing `token::Client`, `token::transfer`, `token::try_transfer`, `token::balance`, or `token::decimals`.
  - For each affected call site in `lib.rs`, update the method name / signature to match the v29 `soroban-sdk` token module docs.
  - Calls to fix (if broken): `token_client.transfer(...)`, `token_client.try_transfer(...)`, `token_client.balance(...)`, `token_client.decimals()`.
  - After each edit, run `cargo build` to confirm the error is resolved before moving to the next.
  - Document each change in the `docs/sdk-v29-migration.md` API table (created in task 8).
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Verification:** `cargo build` produces no `token` errors.

- [ ] 4. Fix Category B compile errors — `env.deployer()` / Wasm upgrade API (conditional)
  - **Execute only if** `cargo build` produced errors referencing `deployer`, `update_current_contract_wasm`, or `upgrade_contract`.
  - Locate the call `env.deployer().update_current_contract_wasm(pending.new_wasm_hash)` in `upgrade_contract`.
  - Update to the v29 equivalent (check `soroban_sdk::Deployer` docs; likely renamed or moved to `env.current_contract().update_wasm(...)`).
  - Document the change in `docs/sdk-v29-migration.md`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Verification:** `cargo build` produces no `deployer` / `update_wasm` errors.

- [ ] 5. Fix Category C/D compile errors — removed re-exports or renamed imports (conditional)
  - **Execute only if** `cargo build` produced `error[E0432]: unresolved import` or `error[E0425]: cannot find` errors.
  - For each unresolved import in the `use soroban_sdk::...` block at the top of `lib.rs` or `test.rs`, find the new path in the v29 API docs and update the import.
  - Common candidates: `soroban_sdk::token`, `soroban_sdk::contracttype`, `soroban_sdk::Map`, `soroban_sdk::BytesN`.
  - Document each moved item in `docs/sdk-v29-migration.md`.
  - **Files:** `contracts/hamplard/src/lib.rs`, `contracts/hamplard/src/test.rs`
  - **Verification:** `cargo build` produces no import errors.

- [ ] 6. Fix Category E compile errors — `testutils` API changes in `test.rs` (conditional)
  - **Execute only if** `cargo build` (with the `testutils` feature, i.e. `cargo test --no-run`) produced errors in `test.rs`.
  - Locate the `last_event_val` helper and any `env.events()` / `mock_all_auths` / `mock_auths` call sites.
  - Update to the v29 testutils equivalents. The v29 SDK added `env.events().all()` — if the helper's internals changed, rewrite it using the new API while keeping the same external interface (`last_event_val(env, contract_id, event_name) -> Val`).
  - Document any testutils API changes in `docs/sdk-v29-migration.md`.
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Verification:** `cargo test --no-run` (compile only) exits 0.

- [ ] 7. Fix any remaining compile errors not covered by Categories A–F
  - **Execute only if** `cargo build` still fails after tasks 3–6.
  - For each remaining error, identify the removed or changed API from the soroban-sdk v29 changelog or docs.
  - Apply the minimal fix (update call site, adjust type, or add/remove a `.clone()`).
  - Document every fix in `docs/sdk-v29-migration.md` under "API changes requiring source edits".
  - **Files:** `contracts/hamplard/src/lib.rs`, `contracts/hamplard/src/test.rs`
  - **Verification:** `cargo build` exits 0 with no errors.

- [ ] 8. Write `docs/sdk-v29-migration.md`
  - Create the file using the template from `design.md` §"Migration Notes Document".
  - Fill in every section:
    - **Version bumped** — from `22.x` to `29.0.1`, MSRV to `1.91.0`.
    - **Security advisories resolved** — `GHSA-96xm-fv9w-pf3f`, `GHSA-4chv-4c6w-w254`, `CVE-2026-32322` with one-line descriptions.
    - **API changes requiring source edits** — one row per fix applied in tasks 3–7 (write "None" if no source changes were needed).
    - **API changes with no source edit** — note any deprecated-but-still-compiling APIs observed.
    - **Snapshot changes** — "None" if snapshots were unaffected, otherwise list changed files.
    - **New features available** — list `#[contractevent]` and any improved testutils APIs available in v29 (not yet used, flagged for the typed-events issue).
  - **Files:** `docs/sdk-v29-migration.md`
  - **Verification:** file is present, all sections filled, no placeholder text remains.

- [ ] 9. Run the full test suite and handle snapshot changes
  - From `contracts/hamplard/`, run `cargo test 2>&1`.
  - **If all tests pass and snapshots are unchanged:** task complete.
  - **If tests fail due to snapshot mismatch:**
    - Inspect the diff of changed JSON files (`git diff contracts/hamplard/test_snapshots/`) to confirm only serialisation-format differences (no logic changes).
    - Run `UPDATE_SNAPSHOTS=1 cargo test` to regenerate.
    - Run `cargo test` again (without the flag) to confirm snapshots are now stable.
    - Stage the updated snapshot files.
  - **If tests fail for any other reason:** diagnose and fix before proceeding.
  - Update the "Snapshot changes" section of `docs/sdk-v29-migration.md` with the outcome.
  - **Files:** `contracts/hamplard/test_snapshots/test/*.json` (if changed)
  - **Verification:** `cargo test` exits 0 with no `FAILED` lines.

- [ ] 10. Verify the Wasm build with `stellar contract build`
  - From `contracts/hamplard/`, run `stellar contract build` (or `make build` if the Makefile wraps it).
  - The command must exit 0 and produce a `.wasm` file with no deprecation warnings from the SDK.
  - If `stellar-cli` is not installed in the environment, run `cargo build --release --target wasm32-unknown-unknown` as an equivalent check.
  - **Files:** none (verification step)
  - **Verification:** Wasm artifact is produced, no SDK deprecation warnings.

- [ ] 11. Final check — regression test for the 13 already-correct events and the 3 security advisories
  - Run `cargo test 2>&1` one final time from the workspace root.
  - Confirm zero failures.
  - Confirm `cargo metadata --format-version 1 | grep soroban-sdk` still shows `29.0.1`.
  - Confirm `grep 'soroban-sdk' Cargo.toml` shows `"29.0.1"` (not the old `"22"`).
  - Confirm `grep 'rust-version' Cargo.toml` shows `"1.91.0"`.
  - **Files:** none (verification step)
  - **Verification:** all of the above greps return expected values; `cargo test` exits 0.
