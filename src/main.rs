use std::env;
use std::path::Path;

use crate::site::{Error, Site};

mod latex;
mod sass;
mod site;
mod util;

fn main() -> Result<(), Error> {
    let args: Vec<String> = env::args().skip(1).collect();

    let mut site = Site::load(Path::new(&args[0])).expect("error loading site");

    site.render_content()?;
    site.render_html()?;
    site.render_feed()?;

    Ok(())
}
