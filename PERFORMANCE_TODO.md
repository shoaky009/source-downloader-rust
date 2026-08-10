# SourceProcessor and Renamer Performance TODO

- [ ] Borrow JSON values while resolving variable-process-chain inputs instead of cloning the complete variable tree for every chain.
- [ ] Reuse each file's computed download path and original-layout string throughout one rename operation.
- [ ] Avoid rebuilding merged rename-variable JSON trees for repeated expression evaluation.
- [ ] Split save-path patterns once before checking overlong directory components.
- [ ] Keep asynchronous storage and filesystem work outside the SourceProcessor coordination mutex.
- [ ] Batch replacement file-content reads to remove serial storage round trips.
- [ ] Store only replacement-relevant data in the in-flight snapshot instead of cloning complete processing models.
- [ ] Compute each SourceItem hash once and pass it through the processing pipeline.
- [ ] Avoid repeated owned path, processor-name, and item-hash strings when persisting target-path metadata.
- [ ] Skip downloader submission when an item has only inline-data files.
