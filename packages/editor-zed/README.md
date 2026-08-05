# Pyre for Zed

Zed language support for `.pyre` files, kept as an independent extension inside
the Pyre repository.

## Features

- Tree-sitter syntax highlighting, bracket matching, indentation, and outline
- Live project diagnostics from `pyre check`, including unsaved buffers
- Full-document formatting through the Pyre formatter

## Install for development

1. Build the Pyre binary from the repository root:

   ```sh
   cargo build
   ```

2. In Zed, run `zed: extensions`, choose **Install Dev Extension**, and select
   `packages/editor-zed`.

The extension first uses a `pyre-lsp` binary configured in Zed settings, then a
`pyre` executable on `PATH`, and finally `target/debug/pyre` in the open
worktree. The language server is started with `pyre lsp`.

The grammar manifest follows the repository's `main` branch so the monorepo can
host the grammar without a second repository. Grammar edits must be committed
and pushed to the referenced revision before Zed can fetch them; reinstall the
dev extension after changing the grammar or extension manifest.

## Format on save

Enable language-server formatting for Pyre in Zed's settings:

```json
{
  "languages": {
    "Pyre": {
      "formatter": "language_server",
      "format_on_save": "on"
    }
  }
}
```

## Custom binary

To use a specific Pyre build:

```json
{
  "lsp": {
    "pyre-lsp": {
      "binary": {
        "path": "/absolute/path/to/pyre",
        "arguments": ["lsp"]
      }
    }
  }
}
```

If `arguments` is omitted, the extension supplies `lsp` automatically.

## Development

Regenerate and exercise the grammar with:

```sh
cd packages/editor-zed/grammar
npx tree-sitter-cli@0.25.8 generate
npx tree-sitter-cli@0.25.8 parse ../../../playground/simple/pyre/schema.pyre
```

Check the Zed adapter independently with:

```sh
cd packages/editor-zed
cargo check
```
