// Aether desktop shell (ADR-012). Deliberately tiny: the daemon owns all
// state (ADR-001) and the console UI owns none — this window is just the
// webview around apps/aether-ui/dist. The UI connects to the daemon control
// plane over WebSocket exactly like the browser build; the daemon's origin
// check accepts the tauri://localhost origin for this purpose.
//
// No shell-side state, no shell-side privileges beyond the window itself —
// keeping the kill-the-UI test trivially true on desktop too.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running the Aether desktop shell");
}
