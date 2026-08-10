# SourceProcessor and Renamer Performance TODO

These are optional optimization candidates, not mandatory changes. Each item must be
verified independently. If an implementation introduces substantial correctness,
complexity, compatibility, or maintenance risk, leave the code unchanged and record the
reason under **Decision log**.

- [x] Borrow JSON values while resolving variable-process-chain inputs instead of cloning the complete variable tree for every chain.
- [x] Reuse each file's computed download path throughout one rename operation.
- [ ] Avoid rebuilding merged rename-variable JSON trees for repeated expression evaluation. **Skipped:** `RenameVariables::all_variables` already uses `OnceLock`, so each variable set is merged at most once. Avoiding that single materialization would require changing the compiled-expression interface and adds disproportionate complexity without benchmark evidence.
- [x] Split save-path patterns once before checking overlong directory components.
- [ ] Keep asynchronous storage and filesystem work outside the SourceProcessor coordination mutex. **Skipped:** the mutex protects the atomic sequence of existence checks, replacement decisions, path reservation, and in-flight registration. Moving I/O out requires an optimistic revalidation protocol; changing it without contention benchmarks and dedicated race tests risks duplicate downloads and incorrect replacement ownership.
- [x] Batch replacement file-content reads to remove serial storage round trips.
- [ ] Store only replacement-relevant data in the in-flight snapshot instead of cloning complete processing models. **Skipped:** replacement deciders receive the full `InProcessingItem` interface, including source item, item variables, files, status, and failure metadata. Narrowing the snapshot would either break that interface or add a second near-duplicate model; use allocation profiling before accepting that maintenance cost.
- [x] Compute each SourceItem hash once and pass it through the processing pipeline.
- [ ] Avoid repeated owned path, processor-name, and item-hash strings when persisting target-path metadata. **Skipped:** `ProcessingTargetPath` intentionally owns all three fields and the storage call crosses an async seam. Removing the per-row processor/hash copies requires changing the shared storage model or normalizing persistence; `Arc<str>` would only move atomic-reference overhead into every adapter. No change without evidence that items commonly contain enough files for this metadata to matter.
- [ ] Skip downloader submission when an item has only inline-data files. **Skipped:** `Downloader::submit` is item-level, not merely a loop over `download_files`. Remote adapters such as qBittorrent and Transmission submit `source_item.download_uri` independently of that slice, so an empty slice does not mean the call is a no-op. Skipping it centrally would change behavior.

## Decision log

- Completed items were independently verified before commit.
- Skipped items retain the existing implementation; the reason and evidence are recorded here.
- Skipped merged-variable-tree changes: the existing `OnceLock` already removes repeated builds; a deeper expression-interface change is not justified without profiling evidence.
- Skipped coordination-mutex changes: reducing the critical section safely requires a new revalidation protocol and concurrency regression tests; the current serialization preserves path ownership correctness.
- Skipped in-flight snapshot narrowing: the replacement-decider interface exposes nearly the full stored model, so a smaller snapshot would not remain behaviorally equivalent without broader interface redesign.
- Skipped target-path metadata ownership changes: the shared async storage model requires owned rows; avoiding these strings needs a broader data-model change rather than a local optimization.
- Skipped empty downloader submissions: qBittorrent and Transmission use `submit` to enqueue the source URI even when no per-file entries are present; preserving adapter semantics is required.
