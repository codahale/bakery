use std::path::{Path, PathBuf};
use std::{fs, io};

use atom_syndication::{ContentBuilder, Entry, EntryBuilder, FeedBuilder, Text};
use chrono::{DateTime, Utc};
use gray_matter::{engine, Matter};
use pulldown_cmark::{html, Options, Parser};
use serde::{Deserialize, Serialize};
use tera::{Context, Tera};
use thiserror::Error;
use walkdir::WalkDir;

use crate::latex::{parse_latex, render_latex};
use crate::sass::SassContext;
use crate::util;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    BadGlob(#[from] glob::PatternError),

    #[error("missing front matter in page `{0}`")]
    MissingFrontMatter(PathBuf),

    #[error("invalid front matter in page `{0}`: {1}")]
    InvalidFrontMatter(PathBuf, #[source] serde_json::error::Error),

    #[error(transparent)]
    BlockParsing(#[from] nom::error::Error<String>),

    #[error(transparent)]
    LaTeXParsing(#[from] katex::Error),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::error::Error),

    #[error(transparent)]
    Template(#[from] tera::Error),
}

#[derive(Debug, Serialize)]
pub struct Site {
    pages: Vec<Page>,

    #[serde(skip_serializing)]
    templates: Tera,

    #[serde(skip_serializing)]
    dir: PathBuf,
}

impl Site {
    pub fn load(dir: &Path) -> Result<Site, Error> {
        let matter = Matter::<engine::YAML>::new();
        let mut pages = vec![];
        let canonical_dir = dir.canonicalize()?;

        let content_dir = canonical_dir.join(CONTENT_SUBDIR);
        for path in walk_dir(&content_dir) {
            let file = matter.parse(&fs::read_to_string(&path)?);
            let pod = file
                .data
                .ok_or_else(|| Error::MissingFrontMatter(path.to_path_buf()))?;
            let mut page: Page = pod
                .deserialize()
                .map_err(|e| Error::InvalidFrontMatter(path.to_path_buf(), e))?;
            page.content = file.content;
            page.excerpt = file.excerpt;
            let mut page_name = path.strip_prefix(&content_dir).unwrap().to_path_buf();
            page_name.set_extension("");
            page.name = page_name.to_string_lossy().to_string();
            pages.push(page);
        }

        let templates = Tera::parse(
            &canonical_dir
                .join("templates")
                .join("**")
                .join("*")
                .to_string_lossy(),
        )?;

        let output_dir = canonical_dir.join("target");
        let sass_dir = canonical_dir.join("sass");
        let mut site = Site {
            pages,
            templates,
            dir: canonical_dir,
        };
        site.templates.register_function(
            "sass",
            SassContext {
                output_dir,
                sass_dir,
            },
        );

        Ok(site)
    }

    pub fn clean_target_dir(&self) -> Result<(), Error> {
        fs::remove_dir_all(self.dir.join("target"))?;

        Ok(())
    }

    pub fn render_content(&mut self) -> Result<(), Error> {
        for page in self.pages.iter_mut() {
            page.content = render_markdown(&render_latex(parse_latex(&page.content)?)?);
        }

        Ok(())
    }

    pub fn render_html(&mut self) -> Result<(), Error> {
        for page in self.pages.iter() {
            let mut context = Context::from_serialize(page)?;
            context.insert("site", &self);

            let html = self.templates.render(&page.template, &context)?;

            let path = if page.name == "index" {
                self.dir.join("target").join("index.html")
            } else {
                self.dir.join("target").join(&page.name).join("index.html")
            };

            util::write_p(path, html)?;
        }

        Ok(())
    }

    pub fn render_feed(&self) -> Result<(), Error> {
        let mut feed_entries: Vec<Entry> = vec![];
        for page in self.pages.iter().filter(|&p| p.date.is_some()) {
            feed_entries.push(
                EntryBuilder::default()
                    .id(&page.name)
                    .content(
                        ContentBuilder::default()
                            .value(page.content.clone())
                            .build(),
                    )
                    .title(Text::plain(&page.title))
                    .updated(page.date.unwrap())
                    .build(),
            )
        }

        let feed = FeedBuilder::default().entries(feed_entries).build();
        let atom = feed.to_string();

        util::write_p(self.dir.join("target").join("atom.xml"), &atom)?;

        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Page {
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

#[derive(Debug, Serialize)]
struct RenderContext {
    page: Page,
}

const CONTENT_SUBDIR: &str = "content";
const MARKDOWN_EXT: &str = "md";

fn walk_dir<P: AsRef<Path>>(dir: P) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|e| {
            e.path()
                .extension()
                .unwrap_or_default()
                .to_str()
                .unwrap_or("")
                == MARKDOWN_EXT
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn render_markdown(content: &str) -> String {
    let mut out = String::with_capacity(content.len() * 2);
    let opts = Options::all();
    let p = Parser::new_ext(content, opts);
    html::push_html(&mut out, p);

    out
}
