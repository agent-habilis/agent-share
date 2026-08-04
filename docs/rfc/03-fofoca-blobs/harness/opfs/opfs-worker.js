// S0.5 browser half — OPFS random-access from a Worker.
//
// FileSystemSyncAccessHandle is the only OPFS API with read/write at an
// arbitrary offset, and it exists ONLY inside a Worker. That is the constraint
// `fofoca-blobs`' OpfsStore is built around, and the same Worker is where
// hashing has to live anyway.

const FILE = "fofoca-blobs-s05.bin";
// The marker lives in its own file. Sharing one with the random-access test
// meant the shuffled writes clobbered offset 0 and the marker never survived —
// a bug in the test that looked exactly like a persistence failure.
const MARKER_FILE = "fofoca-blobs-s05-marker.bin";
const CHUNK = 64 * 1024;

function log(msg, ok) {
  self.postMessage({ type: "log", msg, ok });
}

async function handle(name = FILE) {
  const root = await navigator.storage.getDirectory();
  const file = await root.getFileHandle(name, { create: true });
  return file.createSyncAccessHandle();
}

// Test 1: random-access range writes, out of order, then verify by reading back.
async function randomAccess(totalMiB) {
  const h = await handle();
  const chunks = (totalMiB * 1024 * 1024) / CHUNK;

  // A deterministic but non-sequential write order — a real seeder receives
  // ranges from several peers in whatever order they arrive.
  const order = [...Array(chunks).keys()];
  for (let i = order.length - 1; i > 0; i--) {
    const j = (i * 2654435761) % (i + 1);
    [order[i], order[j]] = [order[j], order[i]];
  }

  // Start from a known size so the size assertion below means something even
  // when a previous run left a larger file behind.
  h.truncate(0);
  const buf = new Uint8Array(CHUNK);
  const t0 = performance.now();
  for (const idx of order) {
    buf.fill(idx & 0xff);
    h.write(buf, { at: idx * CHUNK });
  }
  h.flush();
  const writeMs = performance.now() - t0;

  const t1 = performance.now();
  let bad = 0;
  const back = new Uint8Array(CHUNK);
  for (const idx of order) {
    h.read(back, { at: idx * CHUNK });
    if (back[0] !== (idx & 0xff) || back[CHUNK - 1] !== (idx & 0xff)) bad++;
  }
  const readMs = performance.now() - t1;
  const size = h.getSize();
  h.close();

  const rate = (ms) => ((totalMiB / (ms / 1000)).toFixed(1) + " MiB/s");
  log(
    `random-access ${totalMiB} MiB in ${CHUNK / 1024} KiB chunks, shuffled: ` +
      `write ${rate(writeMs)}, read ${rate(readMs)}, size=${size}, mismatches=${bad}`,
    bad === 0 && size === totalMiB * 1024 * 1024,
  );
  return bad === 0;
}

// Test 2: does it survive a reload? Writes a marker; a second run reads it.
async function persistence() {
  const h = await handle(MARKER_FILE);
  const probe = new Uint8Array(16);
  h.read(probe, { at: 0 });
  const seen = new TextDecoder().decode(probe).startsWith("PERSISTED");
  const marker = new TextEncoder().encode("PERSISTED-------");
  h.write(marker, { at: 0 });
  h.flush();
  h.close();
  log(
    seen
      ? "persistence: marker from a PREVIOUS page load was found — OPFS survives reload"
      : "persistence: no prior marker (first run). Reload the page to confirm.",
    seen,
  );
  return seen;
}

async function quota() {
  const est = await navigator.storage.estimate();
  const gb = (n) => (n / 1024 ** 3).toFixed(2);
  log(`quota: usage ${gb(est.usage)} GiB of quota ${gb(est.quota)} GiB`, true);
}

self.onmessage = async (e) => {
  try {
    if (typeof FileSystemFileHandle === "undefined") {
      log("FileSystemFileHandle is undefined — OPFS unavailable", false);
      return;
    }
    const proto = FileSystemFileHandle.prototype;
    if (!("createSyncAccessHandle" in proto)) {
      log("createSyncAccessHandle missing — no sync handles in this browser", false);
      return;
    }
    log(`UA: ${navigator.userAgent}`, true);
    await quota();
    await persistence();
    await randomAccess(e.data?.mib ?? 64);
    self.postMessage({ type: "done" });
  } catch (err) {
    log(`EXCEPTION: ${err && err.message ? err.message : String(err)}`, false);
    self.postMessage({ type: "done" });
  }
};
