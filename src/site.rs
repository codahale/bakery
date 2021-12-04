use std::collections::HashMap;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use atom_syndication::{ContentBuilder, Entry, EntryBuilder, FeedBuilder, Text};
use chrono::{DateTime, Utc};
use fs_extra::dir::CopyOptions;
use grass::OutputStyle;
use gray_matter::{engine, Matter};
use pulldown_cmark::{html, Options, Parser};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tera::{Context as TeraContext, Tera};
use url::Url;

use crate::latex::latex_to_html;

#[derive(Debug, Serialize)]
pub struct Site {
    pages: Vec<Page>,
    config: SiteConfig,

    #[serde(skip_serializing)]
    templates: Tera,

    #[serde(skip_serializing)]
    dir: PathBuf,
}

impl Site {
    pub fn load<P: AsRef<Path> + Debug>(dir: P, enable_drafts: bool) -> Result<Site> {
        let matter = Matter::<engine::TOML>::new();
        let canonical_dir = dir
            .as_ref()
            .canonicalize()
            .with_context(|| format!("Failed to find site directory: {:?}", dir))?;

        let content_dir = canonical_dir.join(CONTENT_SUBDIR);
        let paths = glob::glob(
            content_dir
                .join("**")
                .join("*.md")
                .to_string_lossy()
                .as_ref(),
        )
        .with_context(|| format!("Failed to find content directory: {:?}", &content_dir))?
        .filter_map(Result::ok)
        .collect::<Vec<PathBuf>>();

        let pages = paths
            .par_iter()
            .map(|path| {
                let file = matter.parse(
                    &fs::read_to_string(path)
                        .with_context(|| format!("Failed to read file {:?}", path))?,
                );
                let pod = file
                    .data
                    .ok_or_else(|| anyhow!("Missing front matter in {:?}", path))?;
                let mut page: Page = pod
                    .deserialize()
                    .with_context(|| format!("Invalid front matter in {:?}", path))?;
                page.content = file.content;
                page.excerpt = file.excerpt;
                if enable_drafts {
                    page.draft = false;
                }
                let mut page_name = path.strip_prefix(&content_dir).unwrap().to_path_buf();
                page_name.set_extension("");
                page.name = page_name.to_string_lossy().to_string();
                Ok(page)
            })
            .collect::<Result<Vec<Page>>>()?;

        let templates = Tera::parse(
            canonical_dir
                .join(TEMPLATES_DIR)
                .join("**")
                .join("*")
                .to_string_lossy()
                .as_ref(),
        )?;

        let config: SiteConfig =
            toml::from_str(&fs::read_to_string(dir.as_ref().join(CONFIG_FILENAME))?)?;
        let site = Site {
            pages,
            templates,
            config,
            dir: canonical_dir,
        };

        Ok(site)
    }

    pub fn build(&mut self) -> Result<()> {
        self.clean_output_dir()?;
        self.render_sass()?;
        self.copy_assets()?;
        self.render_content()?;
        self.render_html()?;
        self.render_feed()
    }

    fn copy_assets(&self) -> Result<()> {
        let site_dir = self.dir.join(TARGET_SUBDIR);
        let _ = fs::create_dir(&site_dir);

        let paths = glob::glob(
            self.dir
                .join(STATIC_SUBDIR)
                .join("[!_]*")
                .to_string_lossy()
                .as_ref(),
        )
        .with_context(|| format!("Error traversing {:?}", &self.dir))?
        .collect::<std::result::Result<Vec<PathBuf>, glob::GlobError>>()
        .with_context(|| format!("Error traversing {:?}", &self.dir))?;

        let options = &CopyOptions::new();
        fs_extra::copy_items(&paths, &site_dir, options)
            .with_context(|| format!("Error copying assets: {:?}", paths))?;

        Ok(())
    }

    fn clean_output_dir(&self) -> Result<()> {
        let _ = fs::remove_dir_all(self.dir.join(TARGET_SUBDIR));

        Ok(())
    }

    fn render_sass(&self) -> Result<()> {
        let sass_dir = self.dir.join(SASS_SUBDIR);
        let css_dir = self.dir.join(TARGET_SUBDIR).join(CSS_SUBDIR);

        let mut options = grass::Options::default();
        for path in self.config.sass.load_paths.iter() {
            options = options.load_path(path);
        }

        options = options.style(if self.config.sass.compressed {
            OutputStyle::Compressed
        } else {
            OutputStyle::Expanded
        });

        self.config
            .sass
            .targets
            .par_iter()
            .map(|(output, input)| {
                let css_path = css_dir.join(output);
                let sass_path = sass_dir.join(input);

                let css = grass::from_path(sass_path.to_string_lossy().as_ref(), &options)
                    .map_err(|e| anyhow::Error::msg(e.to_string()))?;

                write_p(&css_path, css)?;

                Ok(())
            })
            .collect::<Result<()>>()
    }

    fn render_content(&mut self) -> Result<()> {
        let md_opts = Options::all();

        self.pages
            .par_iter_mut()
            .map(|page| {
                let mut out = String::with_capacity(page.content.len() * 2);
                let latex_html = latex_to_html(&page.content, &self.config.latex.macros)
                    .with_context(|| format!("Error rendering LaTeX in page {}", page.name))?;
                let parser = Parser::new_ext(&latex_html, md_opts);
                html::push_html(&mut out, parser);
                page.content = out;

                Ok(())
            })
            .collect()
    }

    fn render_html(&mut self) -> Result<()> {
        self.pages
            .par_iter()
            .map(|page| {
                let mut context = TeraContext::from_serialize(page)
                    .with_context(|| format!("Error rendering page {}", page.name))?;
                context.insert("site", &self);

                let html = self
                    .templates
                    .render(&page.template, &context)
                    .with_context(|| format!("Error rendering page {}", page.name))?;

                let path = if page.name == "index" {
                    self.dir.join(TARGET_SUBDIR).join(INDEX_HTML)
                } else {
                    self.dir
                        .join(TARGET_SUBDIR)
                        .join(&page.name)
                        .join(INDEX_HTML)
                };

                write_p(path, html)?;

                Ok(())
            })
            .collect()
    }

    fn render_feed(&self) -> Result<()> {
        let entries: Vec<Entry> = self
            .pages
            .iter()
            .filter(|&p| p.date.is_some())
            .map(|page| {
                EntryBuilder::default()
                    .id(&page.name)
                    .content(
                        ContentBuilder::default()
                            .value(page.content.clone())
                            .build(),
                    )
                    .title(Text::plain(&page.title))
                    .updated(page.date.unwrap())
                    .build()
            })
            .collect();

        let atom = FeedBuilder::default()
            .title(self.config.title.as_str())
            .id(self.config.base_url.as_str())
            .entries(entries)
            .build()
            .to_string();
        write_p(self.dir.join(TARGET_SUBDIR).join(FEED_FILENAME), &atom)?;

        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SiteConfig {
    base_url: Url,
    title: String,

    #[serde(default, skip_serializing)]
    sass: SassConfig,

    #[serde(default, skip_serializing)]
    latex: LatexConfig,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct SassConfig {
    compressed: bool,
    targets: HashMap<PathBuf, PathBuf>,
    load_paths: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct LatexConfig {
    macros: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Page {
    title: String,
    description: String,
    template: String,
    date: Option<DateTime<Utc>>,

    #[serde(default)]
    draft: bool,

    #[serde(skip_deserializing)]
    excerpt: Option<String>,

    #[serde(skip_deserializing)]
    name: String,

    #[serde(skip_deserializing)]
    content: String,
}

fn write_p<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) -> Result<()> {
    if let Some(parent) = path.as_ref().parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Error creating directory {:?}", &parent))?;
    }

    fs::write(&path, contents)
        .with_context(|| format!("Error creating file {:?}", path.as_ref()))?;

    Ok(())
}

const CONTENT_SUBDIR: &str = "content";
const CSS_SUBDIR: &str = "css";
const SASS_SUBDIR: &str = "sass";
const STATIC_SUBDIR: &str = "static";
const TARGET_SUBDIR: &str = "target";
const TEMPLATES_DIR: &str = "templates";

const CONFIG_FILENAME: &str = "bakery.toml";
const FEED_FILENAME: &str = "atom.xml";
const INDEX_HTML: &str = "index.html";
