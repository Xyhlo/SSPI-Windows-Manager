#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(code) = game_search_lib::child_process() { std::process::exit(code); }
    game_search_lib::run()
}
