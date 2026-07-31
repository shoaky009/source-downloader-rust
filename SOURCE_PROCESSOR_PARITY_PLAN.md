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

- [ ] **Add process-scoped component resource lifecycle if needed**
  - Kotlin acquires and releases Source, ItemFileResolver, Downloader, and FileMover around a process.
  - Rust `Stateful` only exposes state details and has no acquire/release contract.
  - Do not introduce this abstraction until a real component needs resource sleeping or reference counting.

### Adjacent blocker outside SourceProcessor

- [ ] **Implement the built-in HTTP downloader**
  - `source-downloader-core/src/components/http_downloader.rs` still has `todo!()` in metadata, submit, and cancel.
  - Keep this separate from SourceProcessor commits.

## Known non-goals

- Do not port Kotlin `channelBufferSize`: the Kotlin Channel implementation is commented out, while Rust already bounds active Item work with `parallelism` and `FuturesOrdered`.
- Do not change the existing clean-cutover configuration format merely to preserve unused placeholders.

## Verification baseline

Before this plan was created:

- `cargo fmt --all --check` passed.
- `cargo test --workspace` passed: 90 tests, 1 ignored, 15 suites.
- Strict Clippy did not pass because of pre-existing warnings in `storage-sqlite`, `component_manager`, component tests, and existing `source_processor` tests.
