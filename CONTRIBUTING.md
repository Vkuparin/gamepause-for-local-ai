# Contributing

Bug reports, launcher compatibility fixes, documentation, and measured performance improvements are welcome. Check existing issues first. Include GamePause / Windows / LM Studio versions, the launcher, expected behavior, and sanitized diagnostics. Do not upload complete recovery journals, tokens, prompts, or private paths.

Develop on Windows x64 with the pinned Rust toolchain. Keep idle work bounded, save recovery before changing model state, and avoid telemetry or automatic downloads. Include failure-focused tests when changing restoration or discovery parsing.

Before opening a pull request:

```powershell
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Use a separate `--data-dir` for experiments and observation mode by default. Active tests need models you can restore. Document live scenarios exercised. Fixtures must use fictional paths and model names. See [Development](docs/DEVELOPMENT.md). Contributions use the project's MIT license.
