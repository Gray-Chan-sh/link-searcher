// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("--data-dir") && args.len() >= 2 {
        let cli_data_dir = std::path::PathBuf::from(&args[1]);
        if let Err(msg) =
            link_searcher_lib::commands::helpers::check_cli_data_dir_overlap(&cli_data_dir)
        {
            eprintln!("link-searcher: {msg}");
            std::process::exit(1);
        }
        link_searcher_lib::run_with_data_dir(cli_data_dir);
    } else if args.is_empty() {
        link_searcher_lib::run();
    } else if let Err(e) = link_searcher_lib::cli::run_cli() {
        eprintln!("link-searcher: {e}");
        std::process::exit(1);
    }
}
