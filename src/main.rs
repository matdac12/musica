#![windows_subsystem = "windows"]

mod download;
mod job;
mod launch;
mod library;
mod log;
mod player;
mod search;
mod settings;
mod tools;
mod ui;

fn main() {
    let command = launch::parse_args(std::env::args().skip(1));
    std::process::exit(launch::run(command));
}
