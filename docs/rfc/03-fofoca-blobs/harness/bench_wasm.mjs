// S0.3, wasm half: portable vs +simd128, hash-only and outboard-only.
// The browser has no native BLAKE3 (crypto.subtle does not offer it), so wasm
// is the only implementation and SIMD is the only lever.
import { readFileSync } from "node:fs";

const sizeMiB = Number(process.argv[2] ?? 64);
const iters = Number(process.argv[3] ?? 5);
const len = sizeMiB * 1024 * 1024;

async function load(path) {
  const { instance } = await WebAssembly.instantiate(readFileSync(path), {});
  const ex = instance.exports;
  const ptr = ex.spike_alloc(len);
  new Uint8Array(ex.memory.buffer, ptr, len).set(
    Uint8Array.from({ length: len }, (_, i) => i % 251),
  );
  return { ex, ptr };
}

function best(fn) {
  fn(); // warm
  let b = Infinity;
  for (let i = 0; i < iters; i++) {
    const t0 = process.hrtime.bigint();
    fn();
    const ms = Number(process.hrtime.bigint() - t0) / 1e6;
    if (ms < b) b = ms;
  }
  return b;
}

const rows = [];
for (const name of ["portable", "simd"]) {
  const { ex, ptr } = await load(`${name}.wasm`);
  const hash = best(() => ex.spike_hash_only(ptr, len));
  const ob16 = best(() => ex.spike_outboard_only(ptr, len, 0));
  const ob64 = best(() => ex.spike_outboard_only(ptr, len, 1));
  rows.push({ name, hash, ob16, ob64 });
}

const rate = (ms) => (sizeMiB / (ms / 1000)).toFixed(1);
console.log(`\nS0.3 wasm (node) — ${sizeMiB} MiB, best of ${iters}\n`);
console.log("  variant    blake3 hash     outboard@16K    outboard@64K");
for (const r of rows) {
  console.log(
    `  ${r.name.padEnd(10)} ${(rate(r.hash) + " MiB/s").padEnd(15)} ` +
      `${(rate(r.ob16) + " MiB/s").padEnd(15)} ${rate(r.ob64)} MiB/s`,
  );
}
const [p, s] = rows;
console.log(
  `\n  simd128 speedup:  hash ${(p.hash / s.hash).toFixed(2)}x   ` +
    `outboard@16K ${(p.ob16 / s.ob16).toFixed(2)}x   ` +
    `outboard@64K ${(p.ob64 / s.ob64).toFixed(2)}x\n`,
);
