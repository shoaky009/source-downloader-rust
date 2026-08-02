# SourceProcessor Kotlin parity execution plan

## How to resume in a new session

Read this file and continue from the first unchecked item in **Execution order**.

Rules:

1. Use `/Users/shoaky/GitRepo/source-downloader/core/src/main/kotlin/io/github/shoaky/sourcedownloader/core/processor/SourceProcessor.kt` and `ProcessorOptions.kt` as behavioral references.
2. Keep each numbered item in its own commit. Update its checkbox in that commit.
3. For a bug, first add and run a focused regression test that fails for the described symptom, then apply the fix and rerun the same test.
4. Run `cargo fmt --all --check` and the owning crate tests for every commit. Run `cargo test --workspace` after the batch.
5. Do not add compatibility shims or silently change unrelated behavior.
6. Preserve untracked `.codegraph/`, `config.yaml`, and `source-downloader`.

## Follow-up parity backlog

The items below were found by a second end-to-end comparison with Kotlin
`SourceProcessor`. Continue from the first unchecked item, preserving the
one-item-per-commit and regression-test rules above.

### P3: maintainability after behavioral coverage

- [ ] **Extract ordered ItemAction settlement from Process::execute**
  - Defer this refactor until tests cover every `ItemAction`, failure stage,
    listener/persistence side effect, pointer-advance boundary, cancellation,
    and stop-scheduling/drain behavior.
  - Keep the existing `FuturesOrdered` scheduler, bounded parallelism,
    fetch-order settlement, and allocation/I/O profile unchanged.
  - Move only result settlement into private `CompletedItem`,
    `ScheduleDecision`, and action-specific settlement functions; keep
    lifecycle and scheduling control in `execute()`.
  - Do not replace the scheduler with a worker pool/channel or unify failure
    stages whose continuation and persistence semantics differ.
  - Acceptance: all pre-refactor behavioral tests remain unchanged and pass;
    targeted benchmarks show no throughput or allocation regression.

- [ ] **Add process-scoped Sleepable lifecycle when required**
  - Keep this deferred while no Source, ItemFileResolver, Downloader, or
    FileMover needs process-scoped acquisition and release.
  - Before implementation, define lifecycle tests covering normal completion,
    item/fetch failure, active cancellation, panic isolation, and overlapping
    process rejection.
  - Match Kotlin's acquire-before-processing and release-in-finalization
    contract without adding work to components that do not implement
    `Sleepable`.

### Intentional Rust divergences to preserve

- [x] Commit pointer progress in fetch order with `FuturesOrdered`; Kotlin
  updates in completion order.
- [x] Stop scheduling new items after a stopping error and drain already
  started work; Kotlin cancels its processing scope.
- [x] Run the first async rename check immediately rather than waiting.
- [x] Retry typed `ProcessingError::Retryable` errors rather than matching
  Kotlin `IOException`.
- [x] Keep retry attempts configurable.
- [x] Do not add `Sleepable` lifecycle hooks without a component that needs
  them.
- [x] Do not port the unused Kotlin channel buffer option.
- [x] Deduplicate an item hash for the entire Rust run rather than only while
  that item is in flight.

## Known non-goals

- Do not port Kotlin `channelBufferSize`: the Kotlin Channel implementation is commented out, while Rust already bounds active Item work with `parallelism` and `FuturesOrdered`.
- Do not change the existing clean-cutover configuration format merely to preserve unused placeholders.

## Verification baseline

Before this plan was created:

- `cargo fmt --all --check` passed.
- `cargo test --workspace` passed: 90 tests, 1 ignored, 15 suites.
- Strict Clippy did not pass because of pre-existing warnings in `storage-sqlite`, `component_manager`, component tests, and existing `source_processor` tests.
