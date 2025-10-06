use std::process;

use cargo_git_release::{Cli, ReleaseTool};
use clap::Parser as _;

fn main() {
    let args = Cli::parse();
    let mut tool = ReleaseTool::new(args);

    if let Err(error) = tool.run() {
        println!("error: {}", error);
        process::exit(1);
    }
}
