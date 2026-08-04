//! S0.5, headless half — does `web-sys` actually expose OPFS synchronous
//! access handles to Rust, with the random-access read/write-at-offset methods
//! `fofoca-blobs`' `OpfsStore` would need?
//!
//! This compiles but is not run here: the browser half (does it *work*, in
//! Safari and Chrome, and survive a reload) needs a real browser and is driven
//! by `opfs.html` + `opfs-worker.js`.

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    DedicatedWorkerGlobalScope, FileSystemDirectoryHandle, FileSystemFileHandle,
    FileSystemGetFileOptions, FileSystemReadWriteOptions, FileSystemSyncAccessHandle,
};

/// Open (creating if absent) `name` in the origin-private filesystem root and
/// take a **synchronous** access handle to it.
///
/// The sync handle is the only OPFS API with random-access `read`/`write` at an
/// arbitrary offset, and it exists **only inside a Worker** — hence the
/// `DedicatedWorkerGlobalScope` rather than `Window`.
pub async fn open_sync_handle(name: &str) -> Result<FileSystemSyncAccessHandle, JsValue> {
    let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let root: FileSystemDirectoryHandle =
        JsFuture::from(scope.navigator().storage().get_directory())
            .await?
            .unchecked_into();

    let opts = FileSystemGetFileOptions::new();
    opts.set_create(true);
    let file: FileSystemFileHandle =
        JsFuture::from(root.get_file_handle_with_options(name, &opts))
            .await?
            .unchecked_into();

    let handle: FileSystemSyncAccessHandle =
        JsFuture::from(file.create_sync_access_handle())
            .await?
            .unchecked_into();
    Ok(handle)
}

/// Write `data` at `offset` — the operation a range-addressed store lives on.
pub fn write_at(
    handle: &FileSystemSyncAccessHandle,
    offset: f64,
    data: &mut [u8],
) -> Result<f64, JsValue> {
    let opts = FileSystemReadWriteOptions::new();
    opts.set_at(offset);
    handle.write_with_u8_array_and_options(data, &opts)
}

/// Read into `out` at `offset`.
pub fn read_at(
    handle: &FileSystemSyncAccessHandle,
    offset: f64,
    out: &mut [u8],
) -> Result<f64, JsValue> {
    let opts = FileSystemReadWriteOptions::new();
    opts.set_at(offset);
    handle.read_with_u8_array_and_options(out, &opts)
}

/// Durability barrier — without this a crash loses recent writes, which for a
/// persisted range bitfield means claiming to hold bytes we do not.
pub fn flush(handle: &FileSystemSyncAccessHandle) -> Result<(), JsValue> {
    handle.flush()
}

pub fn size(handle: &FileSystemSyncAccessHandle) -> Result<f64, JsValue> {
    handle.get_size()
}

/// Proves every binding above is reachable, so the linker cannot strip the
/// check into vacuity.
#[wasm_bindgen]
pub async fn opfs_smoke(name: String) -> Result<JsValue, JsValue> {
    let handle = open_sync_handle(&name).await?;
    let mut payload = *b"fofoca-blobs range write";
    write_at(&handle, 65536.0, &mut payload)?;
    flush(&handle)?;
    let mut back = [0u8; 24];
    read_at(&handle, 65536.0, &mut back)?;
    let total = size(&handle)?;
    handle.close();
    Ok(JsValue::from_str(&format!(
        "wrote+read {} bytes at offset 65536; file size {total}; match={}",
        back.len(),
        back == payload
    )))
}
