//! Live Code Walk frontend entry point (Leptos CSR, WASM).

mod app;
mod transport;
mod viewer;

use app::App;
use leptos::mount::mount_to_body;
use leptos::prelude::*;

fn main() {
    // Route panics to the browser console for debuggable stack traces.
    console_error_panic_hook::set_once();
    mount_to_body(|| view! { <App /> });
}
