# AGENTS.md

Guidance for coding agents working in `/Users/shoaky/GitRepo/source-downloader-rust`.

## Scope

- This is a Rust workspace for a SourceDownloader rewrite.
- Workspace members: `source-downloader-core`, `source-downloader-sdk`, `applications/web`, `plugins/common`, `component-macro`, `storage-memory`, `storage-sqlite`.
- `source-downloader-sdk` defines traits, models, and shared helpers.
- `plugins/common` implements built-in plugin functionality.
- `source-downloader-core` composes components, config, processor lifecycle, and runtime behavior.
- `applications/web` is the Axum application and main executable.
- `storage-sqlite` is the main persistence adapter.
- `storage-memory` exists but is currently incomplete and contains `todo!()` placeholders.

## Repository Rules

- No repo-local Cursor rules were found in `.cursor/rules/`.
- No `.cursorrules` file was found.
- No `.github/copilot-instructions.md` file was found.
- Treat this file as the primary agent instruction set for this repository.

## Tooling Baseline

- Rust edition: `2024`.
- Build system: Cargo workspace at the repository root.
- No custom `rustfmt.toml`, `.rustfmt.toml`, `clippy.toml`, `.clippy.toml`, `Makefile`, or `justfile` were found.
- Release profile is size-optimized in the workspace root: `opt-level = "z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.

## Recommended Commands

Run commands from the repository root unless a package-specific working directory is clearly better.

### Build

- Build the whole workspace: `cargo build --workspace`
- Build one crate: `cargo build -p source-downloader-core`
- Build release artifacts: `cargo build --workspace --release`
- Fast typecheck without producing binaries: `cargo check --workspace`

### Format

- Format everything: `cargo fmt --all`
- Check formatting in CI style: `cargo fmt --all --check`

### Lint

- Lint all targets: `cargo clippy --workspace --all-targets`
- Treat warnings as errors when validating a change: `cargo clippy --workspace --all-targets -- -D warnings`

### Test

- Run the full test suite: `cargo test --workspace`
- Run tests for one crate: `cargo test -p source-downloader-core`
- Run one exact test by name: `cargo test -p source-downloader-core test_save_processing_content_without_id -- --exact --nocapture`
- Run one async/unit test by module path: `cargo test -p source-downloader-core components::fixed_schedule_trigger::tests::test_add_remove_task -- --exact --nocapture`
- Run tests matching a substring: `cargo test -p storage-sqlite save_processing_content`
- Prefer `--exact` when the test name is specific and stable.

### Benchmarks

- The workspace has a Criterion benchmark in `source-downloader-core`.
- Run it with: `cargo bench -p source-downloader-core --bench filter`

### Run

- Start the web application: `cargo run -p web --bin web`
- Show CLI flags: `cargo run -p web --bin web -- --help`

## Single-Test Workflow

When asked to run or fix a single test, prefer this sequence:

1. Identify the owning crate from the file path.
2. Run the exact test first with `cargo test -p <crate> <test_name> -- --exact --nocapture`.
3. If the exact test name is unclear, run a substring match once to discover it.
4. After the fix, rerun the same single test.
5. If the change touches shared abstractions, run the full crate tests.
6. Run workspace tests only when the change is broad or the user asks for it.

Examples:

- `cargo test -p storage-sqlite test_save_processing_content_with_id -- --exact --nocapture`
- `cargo test -p source-downloader-sdk test_hashing -- --exact --nocapture`

## Architecture Notes

- Prefer changes in the lowest appropriate layer.
- Put trait contracts and shared models in `source-downloader-sdk`.
- Put orchestration, managers, config, and processing flow in `source-downloader-core`.
- Put HTTP routing, request parsing, and response shaping in `applications/web`.
- Put storage-specific data access in `storage-sqlite`.
- Put reusable plugin implementations in `plugins/common`.
- Avoid leaking web concerns into core or SDK crates.

## Code Style

### Formatting and Layout

- Follow standard `rustfmt` output.
- Use 4-space indentation.
- Keep trailing commas in multiline structs, enums, match arms, and function calls.
- Keep files module-focused; avoid adding unrelated edits while touching a file.

### Imports

- No strict custom import order is enforced by the repo.
- Keep imports grouped logically and minimize churn in files you touch.
- Prefer this general grouping when editing imports: local `crate::...`, workspace crates like `source_downloader_*`, third-party crates, then `std` if that matches the surrounding file.
- Remove newly introduced unused imports.

### Naming

- Use `snake_case` for functions, modules, variables, and fields.
- Use `PascalCase` for structs, enums, and traits.
- Use `SCREAMING_SNAKE_CASE` for constants and static values.
- Match the repository's preference for descriptive domain names such as `ProcessorOptionConfig`, `ComponentRootType`, and `ProcessingTargetPath`.

### Types and APIs

- Prefer explicit domain types over loosely typed maps at API boundaries, except where the repository intentionally uses `serde_json::Map<String, Value>` for component/config props.
- Reuse existing SDK traits and models before creating new abstractions.
- This codebase frequently uses `Arc<dyn Trait>` for shared runtime components; follow that pattern in core/plugin integration points.
- Use `async-trait` for async trait methods because the workspace already depends on it.
- Avoid unnecessary cloning in loops or hot paths.

### Error Handling

- Prefer returning `Result<T, E>` over panicking in production code.
- Existing code uses lightweight string-based/domain-specific errors; preserve that style unless a larger refactor is requested.
- Convert external errors with `map_err` and attach actionable context.
- In web handlers, convert failures into `AppError` or other existing response-layer error types.
- Do not add new `unwrap()` or `expect()` calls in normal runtime paths unless failure is truly impossible and documented.
- `unwrap()`/`expect()` is acceptable in tests, narrow startup code, or obviously invariant internal code, but prefer safer propagation in libraries.

### Logging and Observability

- Use `tracing` macros for runtime events.
- Match the existing style of operational log messages such as `Processor[created]`, `[run-start]`, or `name=value` fields inside messages.
- Log meaningful state transitions, retries, skips, and failure reasons.

### Serde and Config Models

- The repository relies heavily on `serde` for config and API payloads.
- Use derives (`Serialize`, `Deserialize`) consistently.
- Mirror existing wire-format conventions: `rename_all = "kebab-case"` for config-heavy structs and `rename_all = "camelCase"` for API-facing SDK models when appropriate.
- Use `#[serde(default)]` and `skip_serializing_if` to preserve backward-compatible config behavior.

### Traits, Components, and Modules

- Keep component supplier registration centralized in module aggregators like `source-downloader-core/src/components/mod.rs`.
- New components should fit the existing supplier + component pattern already used in core and plugins.
- Put public module declarations in `lib.rs`/`mod.rs` and keep visibility as narrow as practical.

### Concurrency and Async

- Tokio is the runtime used across the workspace.
- Use `Arc`, `Mutex`, `RwLock`, and atomics consistently with existing code.
- Prefer `parking_lot` locks where the surrounding code already uses them.

### Comments and Documentation

- Add comments only when they explain why a choice exists, an invariant, or a non-obvious workaround.
- Avoid repeating what the code already makes obvious.
- If you add a TODO, make it concrete and actionable.

## Testing Conventions

- Most tests are inline `#[cfg(test)]` module tests near the implementation.
- Async tests use `#[tokio::test]`.
- Existing test names are mostly `test_*`; follow the local file convention instead of renaming old tests for style purity.
- Keep unit tests focused and easy to run individually.
- `storage-sqlite` tests commonly use `sqlite::memory:`.

## Change Discipline For Agents

- Check for unrelated workspace changes before editing; do not revert user changes.
- Keep patches minimal and local to the requested task.
- If you touch public contracts in `source-downloader-sdk`, review downstream crates for compile impact.
- If you cannot run a broader validation step, say so explicitly in your handoff.
