//! One way to say something to the console.
//!
//! The client used to reach for `web_sys::console::*` directly, thirty-odd
//! times, each call inventing its own prefix and its own idea of what deserved
//! a warning. That is fine until something goes wrong across two machines and
//! the only record of what the tab believed is whatever those calls happened to
//! print — which is how a share that had silently stopped following updates
//! looked exactly like one that was merely slow.
//!
//! So: one prefix, one level vocabulary, and events named for the question they
//! answer rather than the line they sit on.

/// `log!(debug, "…")` — the levels are the console's own, so a reader filters
/// in devtools rather than through a bespoke switch.
macro_rules! log {
    (debug, $($arg:tt)*) => {
        web_sys::console::debug_1(&wasm_bindgen::JsValue::from_str(
            &format!("[share] {}", format_args!($($arg)*)),
        ))
    };
    (info, $($arg:tt)*) => {
        web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(
            &format!("[share] {}", format_args!($($arg)*)),
        ))
    };
    (warn, $($arg:tt)*) => {
        web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(
            &format!("[share] {}", format_args!($($arg)*)),
        ))
    };
}
