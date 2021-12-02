use std::env;
use std::path::Path;

use anyhow::Result;

use crate::site::Site;

mod latex;
mod site;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();

    let mut site = Site::load(Path::new(&args[0])).expect("error loading site");

    site.clean_output_dir()?;
    site.render_sass()?;
    site.copy_assets()?;
    site.render_content()?;
    site.render_html()?;
    site.render_feed()?;

    Ok(())
}
