// Runs the S0.1 spike module under node: proves the wasm actually *executes*
// (not just links), that portable and SIMD agree, and times the hash path —
// which is the S0.3 measurement.
import { readFileSync } from "node:fs";

const path = process.argv[2];
const sizeMiB = Number(process.argv[3] ?? 8);
const iters = Number(process.argv[4] ?? 3);

const bytes = readFileSync(path);
const { instance } = await WebAssembly.instantiate(bytes, {});
const ex = instance.exports;

if (!ex.memory) {
  console.error("no exported memory; exports:", Object.keys(ex).join(", "));
  process.exit(1);
}
if (!ex.spike_exercise_all) {
  console.error("no spike_exercise_all; exports:", Object.keys(ex).join(", "));
  process.exit(1);
}

const len = sizeMiB * 1024 * 1024;
// Allocate through Rust. Writing to an arbitrary offset instead collides with
// dlmalloc's heap and faults as soon as the spike grows a Vec.
const ptr = ex.spike_alloc(len);
// Re-read the buffer after every possible grow: memory.buffer is detached and
// replaced on growth, so a view captured earlier goes stale.
new Uint8Array(ex.memory.buffer, ptr, len).set(
  Uint8Array.from({ length: len }, (_, i) => i % 251),
);

let checksum = null;
const times = [];
for (let i = 0; i < iters; i++) {
  const t0 = process.hrtime.bigint();
  const r = ex.spike_exercise_all(ptr, len);
  const t1 = process.hrtime.bigint();
  const ms = Number(t1 - t0) / 1e6;
  times.push(ms);
  if (checksum === null) checksum = r;
  else if (checksum !== r) {
    console.error(`nondeterministic result: ${checksum} vs ${r}`);
    process.exit(1);
  }
}

const best = Math.min(...times);
// spike_exercise_all does, per block size (2 of them): 1 outboard build,
// 1 full encode, 1 decode+validate, 1 tamper build+encode+decode. Report
// wall-clock per MiB of input rather than pretending it is a pure hash rate.
const mbps = (sizeMiB / (best / 1000)).toFixed(1);
console.log(
  `${path.split("/").pop().padEnd(14)} checksum=${checksum} ` +
    `best=${best.toFixed(1)}ms over ${sizeMiB}MiB  =>  ${mbps} MiB/s (full exercise)`,
);
