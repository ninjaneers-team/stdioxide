
use clap::Parser;
use stdioxide::{app, args::Args};

fn main() -> Result<(), anyhow::Error> {
    let args = Args::parse();
    app::run(args)
}
