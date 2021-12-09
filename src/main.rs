use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueHint};

use crate::site::Site;

mod site;

fn main() -> Result<()> {
    let opts: Opts = Opts::parse();

    println!("Building site...");
    Site::build(&opts.dir, opts.drafts)?;

    if opts.watch {
        println!("Watching for changes...");
        Site::watch(&opts.dir, opts.drafts)?;
    }

    Ok(())
}

/// Build a dang website, I guess.
#[deny(missing_docs)]
#[derive(Debug, Parser)]
#[clap(about, version, author)]
struct Opts {
    /// The site directory.
    #[clap(value_hint = ValueHint::DirPath)]
    dir: PathBuf,

    /// Include draft pages.
    #[clap(long)]
    drafts: bool,

    /// Watch for changed files and rebuild.
    #[clap(long)]
    watch: bool,
}
