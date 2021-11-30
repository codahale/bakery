use std::collections::HashMap;
use std::path::PathBuf;

use grass::Options;
use serde_json::Value;
use tera::{Error, Function, Result};
use url::Url;

use crate::util;

pub struct SassContext {
    pub sass_dir: PathBuf,
    pub output_dir: PathBuf,
    pub base_url: Url,
}

impl Function for SassContext {
    fn call(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let input = args.get("input");
        let output = args.get("output");

        match (input, output) {
            (Some(Value::String(input)), Some(Value::String(output))) => {
                let output_path = self.output_dir.join("css").join(output);
                if !output_path.exists() {
                    let compiled = grass::from_path(
                        &self.sass_dir.join(input).to_string_lossy(),
                        &Options::default(),
                    )
                    .map_err(|e| Error::msg(e.to_string()))?;
                    util::write_p(&output_path, compiled)?;
                }

                let mut css_url = self.base_url.clone();
                let mut path = css_url
                    .path_segments_mut()
                    .map_err(|_| tera::Error::msg("Invalid site URL"))?;
                path.push("css");
                path.push(output);
                drop(path);
                Ok(Value::String(css_url.path().to_string()))
            }
            _ => Err(Error::msg("invalid args")),
        }
    }

    fn is_safe(&self) -> bool {
        true
    }
}
