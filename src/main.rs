use std::env;
use std::path::PathBuf;

use anyhow::Result;
use clap::{crate_description, crate_name, crate_version, AppSettings, Parser, ValueHint};

use crate::site::Site;

mod latex;
mod site;

fn main() -> Result<()> {
    let opts: Opts = Opts::parse();
    let mut site = Site::load(&opts.dir, opts.drafts).expect("error loading site");
    print!("Building {}...", &opts.dir.to_string_lossy());
    site.build()?;
    println!("OK!");

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
}
