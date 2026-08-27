# SourceProcessor and Renamer Performance TODO

These are optional optimization candidates, not mandatory changes. Each item must be
verified independently. If an implementation introduces substantial correctness,
complexity, compatibility, or maintenance risk, leave the code unchanged and record the
reason under **Decision log**.

- [x] Borrow JSON values while resolving variable-process-chain inputs instead of cloning the complete variable tree for every chain.
- [x] Reuse each file's computed download path throughout one rename operation.
- [x] Avoid rebuilding merged rename-variable JSON trees for repeated expression evaluation. `RenameVariables::all_variables` caches the merged map in `OnceLock`; repeated expressions reuse the same materialization until trim variables change.
- [x] Split save-path patterns once before checking overlong directory components.
- [ ] Keep asynchronous storage and filesystem work outside the SourceProcessor coordination mutex. **Skipped:** the mutex protects the atomic sequence of existence checks, replacement decisions, path reservation, and in-flight registration. Moving I/O out requires an optimistic revalidation protocol; changing it without contention benchmarks and dedicated race tests risks duplicate downloads and incorrect replacement ownership.
- [x] Batch replacement file-content reads to remove serial storage round trips.
- [ ] Store only replacement-relevant data in the in-flight snapshot instead of cloning complete processing models. **Skipped:** replacement deciders receive the full `InProcessingItem` interface, including source item, item variables, files, status, and failure metadata. Narrowing the snapshot would either break that interface or add a second near-duplicate model; use allocation profiling before accepting that maintenance cost.
- [x] Compute each SourceItem hash once and pass it through the processing pipeline.
- [x] Avoid repeated owned processor-name and item-hash strings when persisting target-path metadata. `ProcessingStorage::save_paths` now receives the shared processor name and item hash once per batch plus owned path strings.
- [x] Skip downloader submission when an item has only inline-data files. After inline data is written directly, an empty `download_files` slice now means there is no remote work to submit.

## Decision log

- Completed items were independently verified before commit.
- Skipped items retain the existing implementation; the reason and evidence are recorded here.
- Completed merged-variable-tree caching: `RenameVariables::all_variables` materializes the merged map once and invalidates the cache only when trim variables change.
- Skipped coordination-mutex changes: reducing the critical section safely requires a new revalidation protocol and concurrency regression tests; the current serialization preserves path ownership correctness.
- Skipped in-flight snapshot narrowing: the replacement-decider interface exposes nearly the full stored model, so a smaller snapshot would not remain behaviorally equivalent without broader interface redesign.
- Completed target-path metadata batching: the storage seam accepts `processor_name`, `item_hash`, and `Vec<String>` separately, so callers no longer allocate duplicate metadata per path; adapters expand rows only when required by persistence.
- Completed empty downloader submission skipping: `SourceProcessor` calls `Downloader::submit` only when at least one non-inline file remains; inline-only items are covered by a regression test.
