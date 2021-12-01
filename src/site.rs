use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use atom_syndication::{ContentBuilder, Entry, EntryBuilder, FeedBuilder, Text};
use chrono::{DateTime, Utc};
use fs_extra::dir::CopyOptions;
use gray_matter::{engine, Matter};
use pulldown_cmark::{html, Options, Parser};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tera::{Context as TeraContext, Tera};
use url::Url;

use crate::latex::{parse_latex, render_latex};
use crate::util;

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
    pub fn load(dir: &Path) -> Result<Site> {
        let matter = Matter::<engine::TOML>::new();
        let canonical_dir = dir
            .canonicalize()
            .with_context(|| format!("Failed to find site directory: {:?}", dir))?;

        let content_dir = canonical_dir.join(CONTENT_SUBDIR);
        let paths = glob::glob(&content_dir.join("**").join("*.md").to_string_lossy())
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
                let mut page_name = path.strip_prefix(&content_dir).unwrap().to_path_buf();
                page_name.set_extension("");
                page.name = page_name.to_string_lossy().to_string();
                Ok(page)
            })
            .collect::<Result<Vec<Page>>>()?;

        let templates = Tera::parse(
            &canonical_dir
                .join("_templates")
                .join("**")
                .join("*")
                .to_string_lossy(),
        )?;

        let config: SiteConfig = toml::from_str(&fs::read_to_string(dir.join("bakery.toml"))?)?;
        let site = Site {
            pages,
            templates,
            config,
            dir: canonical_dir,
        };

        Ok(site)
    }

    pub fn copy_assets(&self) -> Result<()> {
        let site_dir = self.dir.join(SITE_SUBDIR);
        let _ = fs::create_dir(&site_dir);

        let paths = glob::glob(&self.dir.join("[!_]*").to_string_lossy())
            .with_context(|| format!("Error traversing {:?}", &self.dir))?
            .collect::<std::result::Result<Vec<PathBuf>, glob::GlobError>>()
            .with_context(|| format!("Error traversing {:?}", &self.dir))?;

        let options = &CopyOptions::new();
        fs_extra::copy_items(&paths, &site_dir, options)
            .with_context(|| format!("Error copying assets: {:?}", paths))?;

        Ok(())
    }

    pub fn clean_output_dir(&self) -> Result<()> {
        let _ = fs::remove_dir_all(self.dir.join(SITE_SUBDIR));

        Ok(())
    }

    pub fn render_sass(&self) -> Result<()> {
        let sass_dir = self.dir.join(SASS_SUBDIR);
        let css_dir = self.dir.join(SITE_SUBDIR).join(CSS_SUBDIR);

        self.config
            .sass
            .par_iter()
            .map(|(output, input)| {
                let css_path = css_dir.join(output);
                let sass_path = sass_dir.join(input);

                let css = grass::from_path(
                    sass_path.to_string_lossy().as_ref(),
                    &grass::Options::default(),
                )
                .map_err(|e| anyhow::Error::msg(e.to_string()))?;

                util::write_p(&css_path, css)?;

                Ok(())
            })
            .collect::<Result<()>>()
    }

    pub fn render_content(&mut self) -> Result<()> {
        let opts = Options::all();

        self.pages
            .par_iter_mut()
            .map(|page| {
                let mut out = String::with_capacity(page.content.len() * 2);
                let latex_ast = parse_latex(&page.content)
                    .with_context(|| format!("Invalid LaTeX delimiters in page {}", page.name))?;
                let latex_html = render_latex(latex_ast)?;
                let parser = Parser::new_ext(&latex_html, opts);
                html::push_html(&mut out, parser);
                page.content = out;

                Ok(())
            })
            .collect()
    }

    pub fn render_html(&mut self) -> Result<()> {
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
                    self.dir.join(SITE_SUBDIR).join(INDEX_HTML)
                } else {
                    self.dir.join(SITE_SUBDIR).join(&page.name).join(INDEX_HTML)
                };

                util::write_p(path, html)?;

                Ok(())
            })
            .collect()
    }

    pub fn render_feed(&self) -> Result<()> {
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
        util::write_p(self.dir.join(SITE_SUBDIR).join(FEED_FILENAME), &atom)?;

        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct SiteConfig {
    base_url: Url,
    title: String,

    #[serde(default)]
    sass: HashMap<PathBuf, PathBuf>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Page {
    title: String,
    description: String,
    template: String,
    date: Option<DateTime<Utc>>,

    #[serde(skip_deserializing)]
    excerpt: Option<String>,

    #[serde(skip_deserializing)]
    name: String,

    #[serde(skip_deserializing)]
    content: String,
}

const CONTENT_SUBDIR: &str = "_content";
const SITE_SUBDIR: &str = "_site";
const SASS_SUBDIR: &str = "_sass";
const CSS_SUBDIR: &str = "css";
const FEED_FILENAME: &str = "atom.xml";
const INDEX_HTML: &str = "index.html";
