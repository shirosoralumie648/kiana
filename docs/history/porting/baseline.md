# Workflow baseline

- 2026-11-07: `CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo check -p kiana-permissions -p kiana-session` passed.
- Backup before workflow: `/tmp/kiana-before-workflow.tar.gz` (excludes `reference_project/` and `target/`).
- All workstreams must use `CARGO_TARGET_DIR=/data/cargo-target-sandbox`.
