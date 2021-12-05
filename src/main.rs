use std::env;
use std::path::PathBuf;

use anyhow::Result;
use clap::{crate_description, crate_name, crate_version, AppSettings, Parser, ValueHint};

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

#[derive(Debug, Parser)]
#[clap(bin_name = crate_name!(), about = crate_description!(), version = crate_version!())]
#[clap(setting = AppSettings::HelpRequired)]
struct Opts {
    #[clap(about = "The site directory", value_hint = ValueHint::DirPath)]
    dir: PathBuf,

    #[clap(long, about = "Include draft pages")]
    drafts: bool,

    #[clap(long, about = "Watch for changed files and rebuild")]
    watch: bool,
}
