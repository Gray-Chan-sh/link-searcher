// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().len() > 1 {
        if let Err(e) = link_searcher_lib::cli::run_cli() {
            eprintln!("link-searcher: {e}");
            std::process::exit(1);
        }
    } else {
        link_searcher_lib::run()
    }
}
