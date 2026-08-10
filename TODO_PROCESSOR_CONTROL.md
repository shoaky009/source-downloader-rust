# Processor Control TODO

## Scope

Deliver two independently committed changes:

1. Full processor preparation and atomic activation.
2. Unified management for every processor execution, including scheduled trigger runs, manual runs, submitted-item runs, rename runs, reprocessing, collected dry-runs, and streamed dry-runs.

Component additions and LLM configuration generation are out of scope.

## Commit 1: Processor preparation and atomic activation

- [x] Introduce a prepared processor value that owns a fully assembled, inactive processor.
- [x] Validate component compatibility before preparation.
- [x] Resolve and type-check every referenced component during preparation.
- [x] Compile expressions, path patterns, rules, durations, and processor options during preparation.
- [x] Keep preparation side-effect free: do not replace the active processor, register trigger tasks, or start rename tasks.
- [x] Activate a prepared processor only after preparation succeeds.
- [x] Replace the active processor under the manager lock.
- [x] Register new trigger tasks and start its rename task during activation.
- [x] Close and detach the previous processor only after the replacement is ready.
- [x] Make create, update, reload, and validation use the same preparation contract.
- [x] Persist the complete processor configuration rather than only the enabled flag.
- [x] Ensure failed update/reload leaves the old processor and saved configuration unchanged.
- [x] Add regression tests for successful activation and failed replacement.
- [x] Commit this change separately.

## Commit 2: Unified run management

- [x] Introduce stable run IDs and run kinds.
- [x] Track queued/running/succeeded/failed/cancelled lifecycle states.
- [x] Track creation, start, and completion timestamps plus failure details.
- [x] Keep only active runs in memory and remove each run immediately on completion.
- [x] Route scheduled trigger executions through the run registry.
- [x] Route manual full runs through the run registry.
- [x] Route submitted-item runs through the run registry.
- [x] Route rename runs through the run registry.
- [x] Route single-content reprocessing through the run registry.
- [x] Route collected dry-runs through the run registry.
- [x] Route streamed dry-runs through the run registry and emit terminal status before removal when the client disconnects or cancels.
- [x] Support cancellation by run ID for every running kind.
- [x] Expose list, detail, cancellation, and event-stream endpoints.
- [x] Return a run resource from asynchronous execution endpoints.
- [x] Preserve dry-run result/event payloads while adding run identity and management.
- [x] Add lifecycle, automatic-run, dry-run, cancellation, and completed-run removal tests.
- [x] Commit this change separately.

## Verification

- [x] Run focused `source-downloader-core` tests.
- [x] Run focused `web` tests.
- [x] Run `cargo fmt --all --check`.
- [x] Run `cargo check --workspace`.
- [x] Run `cargo test --workspace`.
