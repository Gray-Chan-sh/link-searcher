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
        // Forward --data-dir to CLI subcommands via env var
        if args.len() > 2 && args[2] != "--data-dir" {
                unsafe { std::env::set_var("LINK_SEARCHER_DATA_DIR", cli_data_dir.to_string_lossy().to_string()); }
            link_searcher_lib::cli::run_cli().unwrap_or_else(|e| {
                eprintln!("link-searcher: {e}");
                std::process::exit(1);
            });
            return;
        }
        link_searcher_lib::run_with_data_dir(cli_data_dir);
    } else if args.is_empty() {
        link_searcher_lib::run();
    } else if let Err(e) = link_searcher_lib::cli::run_cli() {
        eprintln!("link-searcher: {e}");
        std::process::exit(1);
    }
}
