use std::env;

use anyhow::Result;

use crate::site::Site;

mod latex;
mod site;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let enable_drafts = args.contains(&"--drafts".to_string());

    let dir = &args[0];
    let mut site = Site::load(dir, enable_drafts).expect("error loading site");
    print!("Building {}...", dir);
    site.build()?;
    println!("OK!");

    Ok(())
}
