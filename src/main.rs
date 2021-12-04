use std::env;

use anyhow::Result;

use crate::site::Site;

mod latex;
mod site;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let enable_drafts = args.contains(&"--drafts".to_string());
    let watch = args.contains(&"--watch".to_string());

    let mut site = Site::load(&args[0], enable_drafts).expect("error loading site");
    site.build()?;

    if watch {
        site.watch()?;
    }

    Ok(())
}
