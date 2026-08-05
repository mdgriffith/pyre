# Building Pyre

This guide covers how to build the Pyre project and its components.

## Prerequisites

You'll need:
- **Rust** and **Cargo** installed
- **Bun** installed for generated TypeScript compile and runtime tests
- **iconv** installed (for character encoding support)
- **wasm-pack** (for building the WASM component)

Alternatively, you can use [devbox](https://www.jetify.com/devbox) for the Rust and iconv dependencies without polluting your system. Bun and wasm-pack still need to be installed separately:

```bash
devbox shell
```

## Building the Main Project

To build the Pyre CLI and library:

```bash
cargo build
```

To build in release mode (optimized):

```bash
cargo build --release
```

The binary will be located at `target/release/pyre` (or `target/debug/pyre` for debug builds).

## Building the WASM Component

The WASM component is located in the `wasm/` directory and is used for browser/Node.js environments.

To build the WASM package:

```bash
cd wasm
wasm-pack build --target web
```

This will generate the WASM package in `wasm/pkg/` with:
- `pyre_wasm.js` - JavaScript bindings
- `pyre_wasm_bg.wasm` - The compiled WASM binary
- TypeScript definitions

### Build Options

- `--target web` - For browser environments
- `--target nodejs` - For Node.js environments
- `--target bundler` - For bundlers like webpack

## Building Everything

You can use the provided build script to build both the main project and WASM:

```bash
./scripts/build
```

This script:
1. Builds the main Rust project with `cargo build`
2. Builds the WASM component with `wasm-pack build --target web`

## Development

For development, you can run the CLI directly:

```bash
cargo run -- <command>
```

For example:
```bash
cargo run -- migrate db/playground.db
```

## Testing

Run the test suite:

```bash
bun install --frozen-lockfile
cargo test
```

Generated TypeScript tests require the pinned TypeScript and Zod 4 dependencies installed by Bun. They fail rather than skip when the JavaScript toolchain is unavailable.

## Benchmarks

Run benchmarks:

```bash
cargo bench
```
