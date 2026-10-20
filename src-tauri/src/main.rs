// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|a| a == "--mcp") {
        remail_app::run_mcp_bridge();
        return;
    }
    remail_app::run()
}
