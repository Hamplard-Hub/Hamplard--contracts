# Enrollment counter storage

`TotalActiveEnrollments` is now a persistent `uint32` entry, with the existing
persistent TTL policy. Every enrollment path, completion, refund and archive
refund uses the same read/write helpers. The platform cap still reads the
canonical counter before accepting an enrollment.

`EnrollmentRefCounter` also moves to its own persistent entry. Measurement
showed that leaving it in instance storage would still rewrite the instance
on every enrollment, even after moving the active-enrollment count.

## Platform enrollment cap

`set_platform_enrollment_cap()` rejects a new cap that is below the
current `TotalActiveEnrollments`. Allowing such a value would silently
halt all new enrollments while existing enrollments remain active, so the
call panics instead of accepting an impossible cap. A cap equal to the
current total is allowed and effectively blocks further enrollments until
capacity is freed by completion or refund. To lower the cap below the
current total, admins must first reduce the number of active enrollments.
`InstructorCourseCount` and `InstructorPendingCourseCount` likewise move from
instance storage to per-instructor persistent entries. Previously each new
instructor added instance entries, so instance size grew with the number of
instructors and every call became more expensive. These counters now use the
same persistent read/write helpers and TTL policy as the other counters, so
instance size no longer grows with the number of instructors.

## Existing deployments

Each counter reads persistent storage first. If absent, it reads the existing
instance value (or its original default). The first mutation writes that value
forward into persistent storage. The legacy instance value is retained but
Becomes noncanonical, avoiding a migration write to the instance during
enrollment. An upgrade must restore archived persistent counter entries before
using the contract; it must not discard them and fall back to stale legacy data.

This applies to the per-instructor counters as well: on first access after an
upgrade, `InstructorCourseCount` and `InstructorPendingCourseCount` fall back to
their legacy instance values (or their original default of zero) and write the
result forward into persistent storage. The legacy instance entries are left in
place but are noncanonical. Operators must ensure archived persistent
per-instructor counter entries are restored before use rather than discarded in
favor of stale legacy instance data.

There is no reset of active counts or reuse of existing enrollment references.
Reference increments now also reject `uint32` overflow explicitly. No public
function signature changes. Adding `rlib` alongside `cdylib` allows focused
integration tests without compiling unrelated, outdated unit-test call sites.

## Before and after budget

Measured with Soroban SGK 22.0.11 native test execution and the same fixture:
two approved courses, one funded student, a platform cap of two, and the default
budget reset immediately before each enrollment. Setup and the storage snapshot
assertion are excluded from the recorded costs.

| Enrollment | CPU before | CPU after | Memory bytes before | Memory bytes after | Instance data changed before / after |
| --- | ---: | ---: | ---: | ---: | --- |
| First course | 1,248,069 | 1,267,570 | 219,351 | 231,270 | yes / no |
| Second course | 1,277,794 | 1,277,634 | 213,176 | 215,924 | yes / no |

These are native host budget measurements, not network fee quotes or complete
WASM instruction costs. The extra persistent accesses have some overhead; the
benefit measured here is removing routine enrollment writes to the contract
instance, rather than claiming a CPU or fee saving.

The global counters remain shared writable keys, so strict platform-wide caps
and globally sequential references still serialize against their own entries.
This change does not claim fully independent course execution. Applying a
separately scheduled treasury update can still mutate instance configuration.

Run the focused regressions with:

```sh
cargo test -p hamplard --test enrollment_counter -- --nocapture
```

They assert unchanged instance data for normal enrollment in different courses,
legacy-count migration, cap enforcement, completion decrement and subsequent
capacity reuse. The existing unit suite on the base branch fails to compile at
eight outdated call sites, including missing `process_refund` arguments; those
unrelated changes are outside this fix.

## Blocklist storage

`TotalActiveEnrollments` and `EnrollmentRefCounter` are not the only keys that
grew instance storage. `TotalActiveEnrollments` and `EnrollmentRefCounter` were
addressed in the change above. The per-address blocklists `InstructorBlocked(addr)e
and `StudentBlocked(addr)remain in instance storage and grow with every blocked
address. That is tracked separately and is not covered by this document.
