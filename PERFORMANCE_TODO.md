# SourceProcessor and Renamer Performance TODO

These are optional optimization candidates, not mandatory changes. Each item must be
verified independently. If an implementation introduces substantial correctness,
complexity, compatibility, or maintenance risk, leave the code unchanged and record the
reason under **Decision log**.

- [x] Borrow JSON values while resolving variable-process-chain inputs instead of cloning the complete variable tree for every chain.
- [x] Reuse each file's computed download path throughout one rename operation.
- [ ] Avoid rebuilding merged rename-variable JSON trees for repeated expression evaluation. **Skipped:** `RenameVariables::all_variables` already uses `OnceLock`, so each variable set is merged at most once. Avoiding that single materialization would require changing the compiled-expression interface and adds disproportionate complexity without benchmark evidence.
- [x] Split save-path patterns once before checking overlong directory components.
- [ ] Keep asynchronous storage and filesystem work outside the SourceProcessor coordination mutex.
- [ ] Batch replacement file-content reads to remove serial storage round trips.
- [ ] Store only replacement-relevant data in the in-flight snapshot instead of cloning complete processing models.
- [ ] Compute each SourceItem hash once and pass it through the processing pipeline.
- [ ] Avoid repeated owned path, processor-name, and item-hash strings when persisting target-path metadata.
- [ ] Skip downloader submission when an item has only inline-data files.

## Decision log

- Completed items were independently verified before commit.
- Skipped items retain the existing implementation; the reason and evidence are recorded here.
- Skipped merged-variable-tree changes: the existing `OnceLock` already removes repeated builds; a deeper expression-interface change is not justified without profiling evidence.
