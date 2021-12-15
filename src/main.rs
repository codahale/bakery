use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueHint};

use crate::site::Site;

mod site;

fn main() -> Result<()> {
    let opts: Opts = Opts::parse();
    let site = Site::new(&opts.dir, opts.drafts)?;

    if opts.watch {
        site.watch()?;
    } else {
        site.build()?;
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
