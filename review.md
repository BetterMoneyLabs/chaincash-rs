# Code Review Guidelines

This document describes what reviewers look for when evaluating pull requests in `chaincash-rs`.

## Review Checklist

1. **Correctness & Safety**
   - Does the change solve the linked issue accurately without introducing regressions?
   - Are error cases handled idiomatically using `Result` and crate error types rather than unguarded `unwrap()` / `expect()` calls in production codepaths?

2. **Public API Documentation**
   - Are new or modified public types, traits, and functions documented with clear `///` doc comments?
   - Are configuration changes reflected in `config/default.toml` and relevant README files?

3. **Tests**
   - Are unit or integration tests included for new functionality and edge cases?
   - Do all workspace tests pass (`cargo test`)?

4. **Code Style & CI Checks**
   - Does `cargo fmt --all -- --check` pass?
   - Does `cargo clippy --all-features -- -D warnings` pass with zero warnings?
   - If dependencies were added or updated, do `cargo deny` checks pass?
