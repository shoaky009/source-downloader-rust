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

## Already completed

### First batch

- `325314f` — honor `item_error_continue` for pointer failures
- `fdb2811` — enforce processor `task_group`
- `76f4698` — route process listeners by `ListenerMode`
- `e6a8844` — apply File Group path patterns
- `67e0a39` — carry Item Group providers into file variables
- `1896022` — merge provider variables into item rename context

### Second batch

- `f049cf7` — implement `DryRunProcess`
- `8184ff1` — implement content reprocessing
- `53d49fb` — process supplied source items
- `80e1620` — retry retryable item processing errors
- `1e483dd` — replace and cancel in-flight item downloads

## Execution order

### Existing behavior bugs

- [x] **Defer async downloader listeners until rename**
  - Problem: normal processing calls `ListenerMode::Each::on_item_success` even for an `AsyncDownloader`, then `run_rename()` calls it again.
  - Problem: `run_rename()` calls Batch listeners when the query returned waiting records even if none finished.
  - Kotlin reference: `SourceProcessor.kt:1028-1049`, `1069-1074`, `327-377`.
  - Acceptance:
    - async submission emits no success listener;
    - successful rename emits exactly one Each success;
    - all downloads unfinished emits no Batch completion;
    - at least one finished download emits one Batch completion.
  - Suggested commit: `fix: defer async downloader listeners until rename`

- [x] **Do not persist ItemContentFilter-filtered records**
  - Problem: Rust saves `ProcessingStatus::Filtered` content and files; the identity filter can then suppress the Item forever.
  - Kotlin reference: `SourceProcessor.kt:1080-1087`.
  - Acceptance:
    - ItemContentFilter-filtered Item advances its pointer;
    - it is available to the expected listener context;
    - neither ProcessingContent nor FileContent is persisted;
    - non-filtered Item persistence is unchanged.
  - Suggested commit: `fix: skip persistence for content-filtered items`

- [x] **Expose provider variables through `vars` namespaces**
  - Problem: Kotlin exposes `vars.<name>` and `file.vars.<name>`; Rust only exposes flat variables.
  - Kotlin reference: `Renamer.kt:231-265`.
  - Acceptance:
    - item/provider pattern variables are accessible through `vars`;
    - per-file variables are accessible through `file.vars`;
    - existing flat variables remain available;
    - file variables retain precedence over Item/provider variables.
  - Suggested commit: `fix: expose provider variable namespaces`

### Renamer options and component wiring

- [x] **Configure `VariableErrorStrategy`**
  - Add config parsing for Kotlin `ORIGINAL`, `PATTERN`, `STAY`, and `TO_UNRESOLVED`, preserving the distinct `ORIGINAL` and `STAY` behavior.
  - Stop constructing every processor with an opaque `Renamer::default()`.
  - Suggested commit: `feat: configure variable error strategy`

- [x] **Wire `VariableReplacer` components into Renamer**
  - SDK already has `ComponentRootType::VariableReplacer`, `as_variable_replacer()`, and `VariableReplacer`.
  - Add config, ProcessorManager resolution, and Renamer construction.
  - Apply replacers consistently to Item fields, attrs, provider variables, file attrs, and path layout values as Kotlin does.
  - Rust had no `VariableReplacer` implementation, so add Kotlin-equivalent `full-width`, `regex`, and `windows-path` built-ins.
  - Suggested commit: `feat: wire variable replacers into renamer`

- [x] **Configure Trimmer components and path-name length**
  - SDK and Renamer already contain Trimmer support, but `trimming` is always empty and path length is fixed at 255.
  - Add `trimming` and `path-name-length-limit` config and resolve Trimmer components through ProcessorManager.
  - Rust had no `Trimmer` implementation, so add Kotlin-equivalent `force` and `regex` built-ins.
  - Suggested commit: `feat: configure variable trimming and path limits`

- [x] **Implement Variable Process Chain**
  - Replace the unused `Renamer::variable_process_chain: Vec<String>` placeholder with a real model.
  - Add input, ordered provider chain, optional condition, key mapping, include keys, and exclude keys.
  - Exercise `VariableProvider::extract_from()` and `primary_variable_name()`; both are currently unused.
  - Each chain step must see values produced by earlier steps.
  - Suggested commit: `feat: implement variable process chains`

### Runtime options and lifecycle

- [x] **Configure retry policy**
  - Rust currently hard-codes three exponential retries with a ten-second maximum delay.
  - Add explicit retry attempts and backoff configuration without duplicating retry logic at call sites.
  - Suggested commit: `feat: configure processor retry policy`

- [x] **Run the initial async rename check promptly**
  - Rust waits a full `rename_task_interval` before the first check; Kotlin starts checking after 30 seconds.
  - Prefer an immediate first check unless an explicit initial-delay option is justified.
  - Suggested commit: `fix: run initial async rename check promptly`

- [x] **Add process-scoped component resource lifecycle if needed**
  - Kotlin acquires and releases Source, ItemFileResolver, Downloader, and FileMover around a process.
  - Rust `Stateful` only exposes state details and has no acquire/release contract.
  - Do not introduce this abstraction until a real component needs resource sleeping or reference counting.
  - Reviewed current components: no Source, ItemFileResolver, Downloader, or FileMover requires process-scoped acquire/release, so no new lifecycle interface is warranted.

### Adjacent blocker outside SourceProcessor
- [x] **Implement the built-in HTTP downloader**
  - Implements bounded parallel streaming downloads, request headers, HTTP error classification, partial-file cleanup, and cancellation.
  - Keep this separate from SourceProcessor commits.


## Follow-up parity backlog

The items below were found by a second end-to-end comparison with Kotlin
`SourceProcessor`. Continue from the first unchecked item, preserving the
one-item-per-commit and regression-test rules above.

### P0: processing result correctness

- [x] **Let processor download headers override source headers**
  - Previously Rust inserted processor headers before source headers, so source
    values win on duplicate names. Kotlin applies processor
    `download-options.headers` last.
  - Acceptance: duplicate header names use the processor value; disjoint source
    and processor headers are both sent.
  - Suggested commit: `fix: prioritize processor download headers`

- [x] **Preserve SourceFile tags when applying FileTaggers**
  - Previously Rust replaced `SourceFile.tags` with FileTagger output and
    processor tags whenever a FileTagger was configured.
  - Kotlin merges FileTagger output with the original SourceFile tags.
  - Acceptance: original and generated tags are retained; processor metadata
    tags are not injected into SourceFile tags.
  - Suggested commit: `fix: preserve source file tags when tagging`

- [x] **Select the latest replacement history**
  - Kotlin groups prior renamed content by item hash and selects the greatest
    `createTime`. Previously Rust kept the first query result without an
    ordering contract.
  - Acceptance: `FileReplacementDecider` receives the newest renamed prior
    content for each item hash, independent of storage query order.
  - Suggested commit: `fix: select latest replacement history`

- [x] **Normalize processor and resolved source-file paths**
  - Kotlin makes configured save/download roots absolute and relativizes every
    absolute resolved SourceFile path against the download root.
  - Rust preserves relative roots and only relativizes paths for which
    `strip_prefix(download_path)` succeeds.
  - Acceptance: define and test the cross-platform path contract for relative
    roots, absolute paths under the download root, and absolute paths outside
    the download root; fix the known Windows-sensitive `sync_downloader_case`.
  - Suggested commit: `fix: normalize processor processing paths`

- [x] **Persist continued and skippable Item failures**
  - Kotlin persists a `FAILURE` ProcessingContent when
    `item-error-continue=true` or an error is explicitly skippable.
  - Rust currently invokes error listeners and continues without saving a
    failure record.
  - Acceptance: continued and skippable failures are queryable as failed
    ProcessingContent; a non-skippable stopping error retains stop semantics.
  - Suggested commit: `fix: persist continued item failures`

### P1: event and lifecycle parity

- [x] **Define async rename listener event semantics**
  - Successful rename emits Each success and Batch completion.
  - An already-existing target appears only in Batch completion.
  - Missing downloader state and unfinished downloads emit no events.
  - Rename failure emits Each error and remains visible in Batch context.
  - In-flight cancellation emits no event before async rename processing.

- [x] **Define ProcessListener failure isolation**
  - Listener callbacks return `Result<(), ProcessingError>`.
  - `SourceProcessor` logs each listener error and continues notifying later
    listeners for the same event.
  - Listener panics are programming errors and remain prohibited by contract.

- [x] **Expose processor runtime snapshots**
  - `SourceProcessor::runtime_snapshot()` exposes creation time, the latest run
    failure, latest start/end times, and a lock-consistent processing flag.
  - Starting a run clears its end time; successful runs clear the prior failure.

- [x] **Expose processor information**
  - `SourceProcessor::information()` returns a typed management view covering
    resolved components, filters, listeners, renaming pipeline, paths, tags,
    download settings, and scalar processing options.

- [x] **Add active process cancellation**
  - `SourceProcessor::close()` aborts the active processing future, rejects new
    runs, and stops the weak-reference rename loop.
  - `ProcessorManager::destroy_processor()` closes the removed processor, so
    processor reload/delete and application reload share the same cancellation
    contract.

### P2: Web adapter and streaming gaps

- [x] **Implement processor detail and list endpoints**
  - Detail returns persisted processor configuration or `404`; list applies
    name filtering before pagination and includes runtime or startup errors.
- [x] **Implement processor dry-run endpoint**
  - GET without a body and POST options both delegate to
    `SourceProcessor::dry_run()`; omitted `filterProcessed` defaults to `true`.
- [x] **Implement streaming dry-run in Core and Web**
  - Core emits each `DryRunResult` through a bounded stream and cancels the
    process when the receiver disconnects; Web exposes it as
    `application/x-ndjson`.
- [x] **Implement manual rename endpoint**
  - The Web endpoint resolves the running processor and delegates to
    `SourceProcessor::run_rename()`.
- [x] **Implement submitted SourceItem endpoint**
  - Submitted items delegate to `SourceProcessor::run_items()` and retain the
    fixed-item process semantics without advancing the source pointer.
- [x] **Implement processor state endpoint**
  - Core exposes the persisted state or the Source default pointer; Web maps it
    to the Kotlin-compatible `sourceId`, `pointer`, `lastActiveTime`, and
    `retryTimes` view.
- [x] **Implement pointer update endpoint**
  - Core requires an existing processor/source state, merges object pointer
    values, persists the result, and Web returns `404` when the state is absent.
- [x] **Implement processor content deletion endpoint**
  - Storage adapters bulk-delete processing records and target paths by
    processor name and return affected-row counts through Core and Web.
- [ ] **Connect the processing reprocess endpoint to Core**

The Web handlers above must delegate to Core behavior rather than duplicate
SourceProcessor orchestration.

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
