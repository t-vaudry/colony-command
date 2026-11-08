//! Prints the script for a workflow file: `cargo run -p colony-core --example dump_script -- <file.json> [task]`.
use colony_core::script::{compile, Options};

fn main() {
    let mut a = std::env::args().skip(1);
    let file = a.next().expect("workflow file");
    let task = a.next().unwrap_or_default();
    let text = std::fs::read_to_string(&file).expect("read");
    let id = std::path::Path::new(&file).file_stem().unwrap().to_string_lossy().to_string();
    let w = colony_core::workflow::parse(&id, &text).expect("parse");
    print!("{}", compile(&w, &Options { task, judge_model: Some("haiku".into()), advisor: true }).expect("compile"));
}
