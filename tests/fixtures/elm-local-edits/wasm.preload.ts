import { plugin } from 'bun';
import { resolve } from 'node:path';

// Load the freshly compiled production WASM and matching glue without writing
// generated artifacts into packages/server. This does not mock any exports.
plugin({ name: 'conformance-wasm', setup(build) {
  build.onResolve({ filter: /(^|\/)wasm\/pyre_wasm\.js$/ }, () => ({
    path: resolve('target/conformance-wasm/pyre_wasm.js'),
  }));
} });
