# Contributing to ChainCash-RS

Thank you for your interest in contributing to `chaincash-rs`! This guide outlines our development workflow, code quality standards, and documentation expectations.

## Development Setup

1. Clone the repository with submodules:
   ```bash
   git clone --recurse-submodules https://github.com/BetterMoneyLabs/chaincash-rs.git
   ```
2. The project uses the stable Rust toolchain specified in `rust-toolchain.toml` with `rustfmt` and `clippy`.

## Formatting, Linting, and Testing

Before opening a Pull Request, please ensure all checks pass locally:

- **Formatting**:
  ```bash
  cargo fmt --all -- --check
  ```
- **Linting (warnings are denied in CI)**:
  ```bash
  cargo clippy --all-features -- -D warnings
  ```
- **Unit & Integration Tests**:
  ```bash
  cargo test
  ```
- **Dependency Audit**:
  ```bash
  cargo deny check bans licenses sources advisories
  ```

## Documentation Guidelines

While we do not enforce `#[deny(missing_docs)]` at the compiler level, contributors are strongly encouraged to document all public APIs:

- Add Rustdoc comments (`///`) to public structs, enums, traits, functions, and methods across workspace crates.
- Include module-level documentation (`//!`) when introducing new modules or crates.
- Document error conditions and panics where applicable.

## Pull Request Conventions

- Use semantic commit/PR titles (e.g., `feat(server): add endpoint`, `fix(offchain): handle locked wallet`, `ci: improve workflows`).
- Keep pull requests focused on a single issue or feature and include unit tests covering new behavior.
