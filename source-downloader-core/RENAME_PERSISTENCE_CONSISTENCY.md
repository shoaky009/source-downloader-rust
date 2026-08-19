# Rename persistence consistency

## Problem

`SourceProcessor::process_rename_content` spans filesystem changes and multiple storage
writes without one recoverable commit boundary:

1. move or replace downloaded files;
2. save the terminal `ProcessingContent` status;
3. save the compressed `FileContent` snapshot;
4. delete target-path reservations.

The missing-download-state path similarly saves `DownloadFailed` before loading the file
snapshot and deleting reservations.

If a later storage operation fails after a terminal status is saved, the record is no
longer selected by the `WaitingToRename` query. Stale file snapshots or target-path
reservations can therefore remain indefinitely. Merely moving the terminal status write
to the end is insufficient: filesystem changes are not transactional, and retrying after
a final status write failure can repeat a move or replacement.

## Required invariants

- A terminal status is visible only when its file snapshot and target-path reservations
  agree with it.
- Retrying after any intermediate failure must not repeat a destructive filesystem
  operation.
- Recovery must work after process termination, not only for errors returned in the same
  run.
- Listener success events must occur only after the durable terminal state is complete.

## Recommended direction

Introduce an explicit durable intermediate state such as `FinalizingRename`, with enough
operation metadata to detect which filesystem actions already completed. Add a storage
operation that atomically updates the processing record, file snapshot, and target-path
relations. On startup or rename scans, recover `FinalizingRename` records idempotently:
inspect the source and target paths, complete or compensate remaining actions, then commit
the terminal status in the storage transaction.

This requires coordinated changes to the SDK storage contract, SQLite implementation,
migrations, processor recovery logic, and failure-injection tests. It should remain a
separate change from local scheduling and performance fixes.

## Failure tests required before implementation is complete

- file move succeeds, file snapshot save fails;
- file replacement succeeds, target-path deletion fails;
- terminal status save fails after filesystem completion;
- process terminates between each persistence step;
- missing-download-state cleanup fails after status update;
- recovery is repeated twice and remains idempotent.
