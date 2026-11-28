// Run a wasm32-wasip1 binary under Node's WASI (V8, the same engine as workerd).
// Used by `make coldstart` to estimate CPU cost in a fresh isolate.
// usage: PLUGINS=<plugins dir> node scripts/wasi-run.mjs <file.wasm> [args…]
import { WASI } from 'node:wasi';
import { readFileSync } from 'node:fs';

const [file, ...args] = process.argv.slice(2);
const wasi = new WASI({ version: 'preview1', args: [file, ...args], preopens: { '/plugins': process.env.PLUGINS } });
const mod = await WebAssembly.compile(readFileSync(file));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
wasi.start(inst);
