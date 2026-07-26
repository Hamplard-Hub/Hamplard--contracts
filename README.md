# Hamplard Contracts

Hamplard Contracts is the Soroban smart contract layer for the Hamplard platform. It manages course lifecycle state, enrollment payments, instructor revenue splits, certificate issuance, and revocation in a trustless and auditable way.

This repository contains the Rust contract implementation and its test suite for the on-chain portion of the Hamplard ecosystem.

## What this project does

The contract provides the core rules for:

- registering and approving courses
- pausing and unpausing courses
- archiving courses permanently
- enrolling students and splitting payments automatically
- marking enrollments complete
- issuing and revoking certificates
- emitting events for off-chain indexing and backend synchronization

## Main contract concepts

### Courses
Courses move through a lifecycle:

- Pending → Active after admin approval
- Active → Paused by admin or instructor
- Paused → Active again after unpause
- Active/Paused → Archived permanently by admin

### Enrollments and payments
When a student enrolls, the contract transfers the payment split atomically:

- platform fee to the treasury
- instructor share to the instructor earnings balance

### Certificates
After backend verification confirms course completion, an admin can issue an on-chain certificate. Certificates remain verifiable and can later be revoked with metadata captured on-chain.

## Key features

- multi-admin authorization for sensitive operations such as archiving
- course status enforcement and lifecycle rules
- instructor earnings accounting
- certificate issuance and revocation tracking
- Soroban event emission for off-chain systems and indexers

## Repository structure

- [contracts/hamplard/src/lib.rs](contracts/hamplard/src/lib.rs) — contract logic and storage definitions
- [contracts/hamplard/src/test.rs](contracts/hamplard/src/test.rs) — contract tests and regression coverage
- [contracts/hamplard/Cargo.toml](contracts/hamplard/Cargo.toml) — contract package manifest
- [contracts/hamplard/Makefile](contracts/hamplard/Makefile) — build and deployment helpers

## Development

### Prerequisites

- Rust
- Soroban toolchain support
- Stellar CLI (optional for deployment and interaction)

### Run tests

From the repository root:

```bash
cargo test
```

Or within the contract crate:

```bash
cd contracts/hamplard
cargo test
```

## Notes

The contract intentionally keeps only the core trust and verification data on-chain. Content, student progress, and other backend-facing details are expected to live off-chain and be synchronized through contract events.
