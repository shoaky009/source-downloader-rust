# Processor Control TODO

## Scope

Deliver two independently committed changes:

1. Full processor preparation and atomic activation.
2. Unified management for every processor execution, including scheduled trigger runs, manual runs, submitted-item runs, rename runs, reprocessing, collected dry-runs, and streamed dry-runs.

Component additions and LLM configuration generation are out of scope.

## Commit 1: Processor preparation and atomic activation

- [ ] Introduce a prepared processor value that owns a fully assembled, inactive processor.
- [ ] Validate component compatibility before preparation.
- [ ] Resolve and type-check every referenced component during preparation.
- [ ] Compile expressions, path patterns, rules, durations, and processor options during preparation.
- [ ] Keep preparation side-effect free: do not replace the active processor, register trigger tasks, or start rename tasks.
- [ ] Activate a prepared processor only after preparation succeeds.
- [ ] Replace the active processor under the manager lock.
- [ ] Register new trigger tasks and start its rename task during activation.
- [ ] Close and detach the previous processor only after the replacement is ready.
- [ ] Make create, update, reload, and validation use the same preparation contract.
- [ ] Persist the complete processor configuration rather than only the enabled flag.
- [ ] Ensure failed update/reload leaves the old processor and saved configuration unchanged.
- [ ] Add regression tests for successful activation and failed replacement.
- [ ] Commit this change separately.

## Commit 2: Unified run management

- [ ] Introduce stable run IDs and run kinds.
- [ ] Track queued/running/succeeded/failed/cancelled lifecycle states.
- [ ] Track creation, start, and completion timestamps plus failure details.
- [ ] Keep a bounded in-memory run history per application.
- [ ] Route scheduled trigger executions through the run registry.
- [ ] Route manual full runs through the run registry.
- [ ] Route submitted-item runs through the run registry.
- [ ] Route rename runs through the run registry.
- [ ] Route single-content reprocessing through the run registry.
- [ ] Route collected dry-runs through the run registry.
- [ ] Route streamed dry-runs through the run registry and retain terminal status when the client disconnects or cancels.
- [ ] Support cancellation by run ID for every running kind.
- [ ] Expose list, detail, cancellation, and event-stream endpoints.
- [ ] Return a run resource from asynchronous execution endpoints.
- [ ] Preserve dry-run result/event payloads while adding run identity and management.
- [ ] Add lifecycle, automatic-run, dry-run, cancellation, and bounded-history tests.
- [ ] Commit this change separately.

## Verification

- [ ] Run focused `source-downloader-core` tests.
- [ ] Run focused `web` tests.
- [ ] Run `cargo fmt --all --check`.
- [ ] Run `cargo check --workspace`.
- [ ] Run `cargo test --workspace`.
