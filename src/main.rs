
mod app;
mod args;
mod child;
mod control;
mod output;
mod servers;

use clap::Parser;

use crate::args::Args;

fn main() -> Result<(), anyhow::Error> {
    let args = Args::parse();
    app::run(args)
}
