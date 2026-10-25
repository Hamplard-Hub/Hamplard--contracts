# Contract Benchmarks

Baseline CPU and memory measurements for the most write-heavy Hamplard
contract paths. These are regression baselines, not Stellar network fee
quotes: they use the Soroban host cost model from `soroban-sdk` 22 and the
test environment's default budget.

## Reproduce

```bash
cargo test -p hamplard benchmark_ -- --ignored --nocapture
```

Each test prints one line per measured invocation:

```text
BENCH <scenario> cpu_instructions=<n> memory_bytes=<n>
```

The values below were recorded on 2026-10-20 with the workspace debug profile
on macOS. Budget counters are read immediately after the measured invocation,
so each row covers only that function call.

## Baseline

| Scenario | CPU instructions | Memory bytes |
|---|---:|---:|
| `enroll` (single course) | 1,219,412 | 207,220 |
| `batch_enroll` (10 courses) | 12,219,529 | 2,309,981 |
| `archive_course` (10 refunds) | 7,314,947 | 1,753,304 |
| `register_course` (50-course catalog) | 987,918 | 281,609 |

## Notes

- `batch_enroll` grows roughly linearly with the number of courses.
- `archive_course` includes one token refund and one history write per student.
- `register_course` includes the cost of appending to and persisting the
  on-chain course catalog, so a larger catalog increases the write size.
- Re-run the command after changing storage layout or hot-path logic and update
  this table when the change is intentional.
