use crate::util;
use grass::Options;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tera::Function;

pub struct SassContext {
    pub sass_dir: PathBuf,
    pub output_dir: PathBuf,
}

impl Function for SassContext {
    fn call(&self, args: &HashMap<String, Value>) -> tera::Result<Value> {
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
                    .map_err(|e| tera::Error::msg(e.to_string()))?;
                    util::write_p(&output_path, compiled).map_err(tera::Error::io_error)?;
                }

                Ok(Value::String(format!("/css/{}", output)))
            }
            _ => Err(tera::Error::msg("invalid args")),
        }
    }

    fn is_safe(&self) -> bool {
        true
    }
}
