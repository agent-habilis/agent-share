//! S0.6 — does `IdbStore` actually work in a browser, on the main thread?
//!
//! The conformance suite runs `MemStore` and `FsStore` natively; neither can
//! reach `IndexedDB`, so the browser backend would otherwise ship on a compile
//! check. This runs the load-bearing properties against a real database in a
//! real tab.
//!
//! What is being falsified, in order of how much it would cost to be wrong:
//!
//! 1. **The blocks claim.** Accepting a file in n pieces must store n blocks
//!    and touch no more. If this fails the whole reason for choosing
//!    `IndexedDB` over main-thread OPFS is gone.
//! 2. **Partial serving.** A store holding some blocks must serve exactly those
//!    and refuse the rest, provably against the same root as the whole file.
//! 3. **Persistence.** The point of not using `MemStore`. Needs a reload, so it
//!    is reported rather than asserted on the first run.
//! 4. **The version gate.** A file whose mtime moved must go unbound rather
//!    than being served from a stale outboard.

use fofoca_blobs::{BlobStore, ChunkNum, ChunkRanges, FileId, IdbStore, build_outboard,
                   decode_into, encode_ranges, extent_of};
use wasm_bindgen::prelude::*;

const DB: &str = "s06-idb-harness";

fn data(len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| u8::try_from(index % 251).expect("bounded"))
        .collect()
}

fn file(key: &str, size: u64, mtime: i64) -> FileId {
    FileId {
        key: key.to_owned(),
        size,
        mtime,
    }
}

/// Run the checks, appending PASS/FAIL lines to a report.
#[wasm_bindgen]
pub async fn run() -> String {
    let mut out = String::new();
    match checks(&mut out).await {
        Ok(()) => out.push_str("\ndone\n"),
        Err(error) => out.push_str(&format!("\nFAIL harness aborted: {error:#}\n")),
    }
    out
}

macro_rules! check {
    ($out:expr, $name:expr, $cond:expr) => {{
        let passed = $cond;
        $out.push_str(if passed { "PASS " } else { "FAIL " });
        $out.push_str($name);
        $out.push('\n');
    }};
}

#[expect(clippy::too_many_lines, reason = "a linear spike script, read top to bottom")]
async fn checks(out: &mut String) -> anyhow::Result<()> {
    let store = IdbStore::open(DB).await?;
    out.push_str("opened database\n");

    // ---- 3. persistence, checked first because it reads what a prior run left
    let survivor = file("persist.bin", 4096, 7);
    let previous = store.bind(&survivor).await?;
    if let Some(root) = previous {
        let held = store.present(root).await?;
        let bytes = store.read_ranges(&survivor, &extent_of(4096)).await?;
        let mut target = Vec::new();
        let ok = decode_into(root, 4096, &bytes, &extent_of(4096), &mut target).is_ok()
            && target == data(4096)
            && held == extent_of(4096);
        check!(out, "persistence across reload", ok);
    } else {
        out.push_str("note persistence: first run, reload and run again\n");
        store.insert_complete(&survivor, &data(4096)).await?;
    }

    // ---- 1. the blocks claim
    //
    // Salted per run so every run gets a *fresh root*. Blocks and range sets
    // are both keyed by root, so reusing content would let a previous run's
    // blocks satisfy this one and the check would prove nothing.
    let size: u64 = 1 << 20;
    let salt = u64::try_from(js_sys::Date::now() as i64).unwrap_or(0);
    let mut bytes = data(1 << 20);
    bytes[..8].copy_from_slice(&salt.to_le_bytes());
    let (root, _) = build_outboard(&bytes);
    let target = file("pieces.bin", size, 1);

    let mut expected = ChunkRanges::empty();
    // Out of order on purpose: pieces come back from several peers at once.
    for step in [0u64, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15] {
        let window = ChunkRanges::from(ChunkNum(step * 64)..ChunkNum(step * 64 + 64));
        let encoded = encode_ranges(&bytes, &window)?;
        let held = store
            .write_verified(&target, root, &encoded, &window)
            .await?;
        expected |= window;
        if held != expected {
            out.push_str(&format!("FAIL held diverged at step {step}\n"));
            break;
        }
    }
    check!(
        out,
        "16 pieces accepted out of order, held tracks exactly",
        store.present(root).await? == expected
    );
    check!(
        out,
        "the reassembled file matches byte for byte",
        {
            let served = store.read_ranges(&target, &extent_of(size)).await?;
            let mut rebuilt = Vec::new();
            decode_into(root, size, &served, &extent_of(size), &mut rebuilt).is_ok()
                && rebuilt == bytes
        }
    );

    // ---- 2. partial serving
    // A different root again: this file must be *partial*, so it must not
    // inherit the complete block set the section above just stored.
    let mut other = data(1 << 20);
    other[..8].copy_from_slice(&salt.wrapping_add(1).to_le_bytes());
    let (other_root, _) = build_outboard(&other);
    let partial = file("partial.bin", size, 2);
    let first = ChunkRanges::from(ChunkNum(0)..ChunkNum(64));
    let absent = ChunkRanges::from(ChunkNum(64)..ChunkNum(128));
    let encoded = encode_ranges(&other, &first)?;
    store
        .write_verified(&partial, other_root, &encoded, &first)
        .await?;

    check!(out, "a held range serves", {
        let served = store.read_ranges(&partial, &first).await?;
        let mut got = Vec::new();
        decode_into(other_root, size, &served, &first, &mut got).is_ok()
            && got[..65536] == other[..65536]
    });
    check!(
        out,
        "an absent range is refused, not answered short",
        store.read_ranges(&partial, &absent).await.is_err()
    );

    // ---- 4. the version gate
    let moved = FileId {
        mtime: partial.mtime + 1,
        ..partial.clone()
    };
    check!(
        out,
        "a file whose mtime moved is unbound",
        store.bind(&moved).await?.is_none()
    );
    check!(
        out,
        "and refuses to serve rather than serving stale bytes",
        store.read_ranges(&moved, &first).await.is_err()
    );

    // ---- a tampered range must never be accepted
    let mut third = data(1 << 20);
    third[..8].copy_from_slice(&salt.wrapping_add(2).to_le_bytes());
    let (third_root, _) = build_outboard(&third);
    let victim = file("tampered.bin", size, 3);
    let mut rotten = encode_ranges(&third, &first)?;
    let last = rotten.len() - 1;
    rotten[last] ^= 0x01;
    check!(
        out,
        "a tampered range is rejected",
        store
            .write_verified(&victim, third_root, &rotten, &first)
            .await
            .is_err()
    );
    check!(
        out,
        "and leaves the store holding nothing for it",
        store.bind(&victim).await?.is_none()
    );

    Ok(())
}
