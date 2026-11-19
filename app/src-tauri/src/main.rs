// No console window behind the map in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    colony_app_lib::run()
}
