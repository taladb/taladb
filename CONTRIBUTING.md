# Contributing to TalaDB

First off, thank you for considering contributing to TalaDB! It's people like you that make the open-source community such an amazing place.

I welcome help in all areas: from core Rust optimizations to documentation and platform testing.

## Getting Started

### Prerequisites

To build and test TalaDB locally, you will need:
- [Rust](https://rustup.rs/) (stable 1.75+)
- [Node.js](https://nodejs.org/) (18+)
- [pnpm](https://pnpm.io/) (9+)
- [wasm-pack](https://rustwasm.github.io/wasm-pack/) (for browser builds)
- `@napi-rs/cli` (global or via pnpm)

### Setup

1. Fork the repository and clone it locally.
2. Install dependencies:
   ```bash
   pnpm install
   ```
3. Run the initial build to ensure everything is linked correctly:
   ```bash
   pnpm build
   ```

## Development Workflow

### Coding Standards
- **Rust**: We use consistent formatting. Run `cargo fmt` before committing.
- **TypeScript**: We use Prettier for formatting.
- **Lints**: Ensure `cargo clippy` passes for the core packages.

### Running Tests
Always run the tests before submitting a PR:

```bash
# Core Rust tests
cargo test --workspace

# TypeScript tests
pnpm --filter taladb test

# Browser WASM tests
wasm-pack test packages/@taladb/web --headless --chrome
```

## How to Contribute

1. **Find an issue**: Look for issues labeled `good first issue` or `help wanted`.
2. **Open an issue**: If you want to build something new, please open an issue first to discuss the design.
3. **Branch naming**: We suggest using prefixes like `feat/`, `fix/`, or `docs/` (e.g., `feat/add-geospatial-index`).
4. **Submit a Pull Request**: Provide a clear description of your changes and link to any relevant issues.

## Community

If you have questions or want to discuss the roadmap, feel free to open a [GitHub Discussion](https://github.com/taladb/taladb/discussions).

---

*By contributing to TalaDB, you agree that your contributions will be dual-licensed under the MIT license and the Apache License 2.0, at the recipient's option, without any additional terms or conditions.*
