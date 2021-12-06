use std::collections::HashMap;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use atom_syndication::{ContentBuilder, Entry, EntryBuilder, FeedBuilder, Text};
use chrono::{DateTime, Utc};
use fs_extra::dir::CopyOptions;
use grass::OutputStyle;
use gray_matter::{engine, Matter};
use katex::Opts;
use notify::{DebouncedEvent, RecursiveMode, Watcher};
use pulldown_cmark::{escape, html, CodeBlockKind, Event, Options, Parser, Tag};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use syntect::highlighting::ThemeSet;
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;
use tera::{Context as TeraContext, Tera};
use url::Url;

#[derive(Debug, Serialize)]
pub struct Site {
    pages: Vec<Page>,
    config: SiteConfig,

    #[serde(skip_serializing)]
    dir: PathBuf,

    #[serde(skip_serializing)]
    target_dir: PathBuf,
}

impl Site {
    pub fn build<P: AsRef<Path> + Debug>(dir: P, enable_drafts: bool) -> Result<()> {
        let matter = Matter::<engine::TOML>::new();
        let dir = dir
            .as_ref()
            .canonicalize()
            .with_context(|| format!("Failed to find site directory: {:?}", dir))?;

        let content_dir = dir.join(CONTENT_SUBDIR);
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

        let config: SiteConfig = toml::from_str(&fs::read_to_string(dir.join(CONFIG_FILENAME))?)?;
        let target_dir = dir.join(TARGET_SUBDIR);
        let mut site = Site {
            pages,
            config,
            dir,
            target_dir,
        };

        site.clean_output_dir()?;
        site.render_sass()?;
        site.copy_assets()?;
        site.render_content()?;
        site.render_html()?;
        site.render_feed()
    }

    pub fn watch<P: AsRef<Path> + Debug>(dir: P, enable_drafts: bool) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::watcher(tx, Duration::from_secs(1))?;
        watcher.watch(&dir, RecursiveMode::Recursive)?;
        let target_dir = dir.as_ref().canonicalize()?.join(TARGET_SUBDIR);
        loop {
            match rx.recv() {
                Ok(event) => match event {
                    DebouncedEvent::NoticeWrite(path)
                    | DebouncedEvent::NoticeRemove(path)
                    | DebouncedEvent::Create(path)
                    | DebouncedEvent::Write(path)
                    | DebouncedEvent::Remove(path)
                    | DebouncedEvent::Rename(path, _) => {
                        if !path
                            .extension()
                            .map(|s| s.to_string_lossy().ends_with('~'))
                            .unwrap_or(false)
                            && !path.starts_with(&target_dir)
                        {
                            println!("Rebuilding site...");
                            Site::build(&dir, enable_drafts)?;
                        }
                    }
                    _ => {}
                },
                Err(e) => bail!(e),
            }
        }
    }

    fn copy_assets(&self) -> Result<()> {
        let _ = fs::create_dir(&self.target_dir);

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
        fs_extra::copy_items(&paths, &self.target_dir, options)
            .with_context(|| format!("Error copying assets: {:?}", paths))?;

        Ok(())
    }

    fn clean_output_dir(&self) -> Result<()> {
        let _ = fs::remove_dir_all(&self.target_dir);

        Ok(())
    }

    fn render_sass(&self) -> Result<()> {
        let sass_dir = self.dir.join(SASS_SUBDIR);
        let css_dir = self.target_dir.join(CSS_SUBDIR);

        let mut options = grass::Options::default().style(if self.config.sass.compressed {
            OutputStyle::Compressed
        } else {
            OutputStyle::Expanded
        });

        for path in self.config.sass.load_paths.iter() {
            options = options.load_path(path);
        }

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
        let ss = SyntaxSet::load_defaults_newlines();
        let themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .get(&self.config.theme)
            .ok_or_else(|| anyhow!("Invalid syntax theme: {:?}", &self.config.theme))?;

        let inline_opts = Opts::builder().display_mode(false).build()?;
        let block_opts = Opts::builder().display_mode(true).build()?;

        self.pages
            .par_iter_mut()
            .map(|page| {
                let mut out = String::with_capacity(page.content.len() * 2);
                let mut fence_kind: Option<String> = None;
                let events = Parser::new_ext(&page.content, md_opts)
                    .map(|e| match e {
                        Event::Code(s) => {
                            if s.starts_with('$') && s.ends_with('$') {
                                // Convert inline LaTeX blocks (e.g. `$N+1`) to HTML.
                                Ok(Event::Html(
                                    katex::render_with_opts(&s[1..s.len() - 1], &inline_opts)?
                                        .into(),
                                ))
                            } else {
                                // Pass regular inline code blocks on to the formatter.
                                Ok(Event::Code(s))
                            }
                        }
                        Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(kind))) => {
                            // If the fenced code block doesn't have a kind, pass it on directly.
                            if kind.is_empty() {
                                Ok(Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(kind))))
                            } else {
                                // Otherwise, record the fenced block kind, but don't pass the start
                                // on to the formatter. We'll handle our own <pre><code> blocking.
                                fence_kind = Some(kind.to_string());
                                Ok(Event::Text("".into()))
                            }
                        }
                        Event::End(Tag::CodeBlock(CodeBlockKind::Fenced(kind))) => {
                            // If the fenced code block doesn't have a kind, pass it on directly.
                            if fence_kind.is_none() {
                                Ok(Event::End(Tag::CodeBlock(CodeBlockKind::Fenced(kind))))
                            } else {
                                // Reset the fenced block kind. Again, don't pass the end on to the
                                // formatter.
                                fence_kind = None;
                                Ok(Event::Text("".into()))
                            }
                        }
                        Event::Text(s) => {
                            // If we've previously recorded a code block fence kind, then this text
                            // is the contents of a fenced code block with a specified kind.
                            if let Some(kind) = &fence_kind {
                                if kind.as_str() == "latex" {
                                    // Render LaTeX as HTML using KaTeX.
                                    let html = katex::render_with_opts(&s, &block_opts)?;
                                    Ok(Event::Html(html.into()))
                                } else if let Some(syntax) = ss.find_syntax_by_token(kind) {
                                    // If we can find a Syntect syntax for the given kind, format it
                                    // as syntax highlighted HTML.
                                    let html = highlighted_html_for_string(&s, &ss, syntax, theme);
                                    Ok(Event::Html(html.into()))
                                } else {
                                    // If we don't know what kind this code is, just escape it and
                                    // slap it in a <pre><code> block.
                                    let mut html = String::with_capacity(s.len());
                                    html.push_str("<pre><code>");
                                    escape::escape_html(&mut html, &s)?;
                                    html.push_str("</code></pre>");
                                    Ok(Event::Html(html.into()))
                                }
                            } else {
                                // If we're not in a fenced code block, just pass the text on.
                                Ok(Event::Text(s))
                            }
                        }
                        // Pass all other events on untouched.
                        e => Ok(e),
                    })
                    // Convert to a vec of events and return the first error, if any.
                    .collect::<Result<Vec<Event>>>()?;

                // Render as HTML.
                html::push_html(&mut out, events.into_iter());
                page.content = out;

                Ok(())
            })
            .collect()
    }

    fn render_html(&mut self) -> Result<()> {
        let templates = Tera::parse(
            self.dir
                .join(TEMPLATES_DIR)
                .join("**")
                .join("*")
                .to_string_lossy()
                .as_ref(),
        )?;
        self.pages
            .par_iter()
            .map(|page| {
                let mut context = TeraContext::from_serialize(page)
                    .with_context(|| format!("Error rendering page {}", page.name))?;
                context.insert("site", &self);

                let html = templates
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

    #[serde(default = "default_theme")]
    theme: String,

    #[serde(default, skip_serializing)]
    sass: SassConfig,
}

fn default_theme() -> String {
    "InspiredGitHub".into()
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct SassConfig {
    compressed: bool,
    targets: HashMap<PathBuf, PathBuf>,
    load_paths: Vec<PathBuf>,
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
