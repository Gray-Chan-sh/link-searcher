// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().len() > 1 {
        link_searcher_lib::cli::run_cli();
    } else {
        link_searcher_lib::run()
    }
}
