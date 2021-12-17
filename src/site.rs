use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::fs;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use atom_syndication::{ContentBuilder, Entry, EntryBuilder, FeedBuilder, Text};
use chrono::{DateTime, Utc};
use ctor::ctor;
use grass::OutputStyle;
use gray_matter::{engine, Matter};
use itertools::Itertools;
use katex::Opts;
use lazy_static::lazy_static;
use notify::{DebouncedEvent, RecursiveMode, Watcher};
use pulldown_cmark::{html, CodeBlockKind, Event, Options, Parser, Tag};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use syntect::highlighting::ThemeSet;
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;
use tera::{Context as TeraContext, Tera};
use url::Url;
use walkdir::{DirEntry, WalkDir};

/// A bakery site.
#[derive(Debug, Serialize)]
pub struct Site {
    pages: Vec<Page>,
    config: SiteConfig,

    #[serde(skip_serializing)]
    dir: PathBuf,

    #[serde(skip_serializing)]
    target_dir: PathBuf,

    #[serde(skip_serializing)]
    drafts: bool,
}

impl Site {
    /// Load a bakery site from the given directory. If `drafts` is true, includes all draft pages.
    pub fn new<P: AsRef<Path> + Debug>(dir: P, drafts: bool) -> Result<Site> {
        // Convert the path to canonical, if possible.
        let dir = dir
            .as_ref()
            .canonicalize()
            .with_context(|| format!("Failed to find site directory: {:?}", dir))?;

        // Scan the content subdirectory for .md files.
        let content_dir = dir.join(CONTENT_SUBDIR);
        let paths = WalkDir::new(&content_dir)
            .into_iter()
            .filter_ok(|e| e.file_type().is_file())
            .map_ok(DirEntry::into_path)
            .filter_ok(|path| {
                path.extension()
                    .and_then(OsStr::to_str)
                    .map(|ext| ext == MARKDOWN_EXT)
                    .unwrap_or(false)
            })
            .collect::<walkdir::Result<Vec<PathBuf>>>()?;

        // In parallel, parse the TOML front matter from each page file.
        let matter = Matter::<engine::TOML>::new();
        let pages = paths
            .par_iter()
            .map(|path| {
                // Read the file contents.
                let s = fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read file {:?}", path))?;

                // Parse the front matter and contents.
                let parsed = matter
                    .parse_with_struct::<Page>(&s)
                    .ok_or_else(|| anyhow!("Invalid front matter in {:?}", path))?;

                // Extract the page metadata and add the content and excerpt.
                let mut page = parsed.data;
                page.content = parsed.content;
                page.excerpt = parsed.excerpt;

                // Infer the page name from the page filename.
                let mut page_name = path.strip_prefix(&content_dir).unwrap().to_path_buf();
                page_name.set_extension("");
                page.name = page_name.to_string_lossy().to_string();

                Ok(page)
            })
            // Filter out drafts, if necessary.
            .filter(|r| r.as_ref().map(|p| drafts || !p.draft).unwrap_or(true))
            .collect::<Result<Vec<Page>>>()?;

        // Load the site config.
        let config: SiteConfig = toml::from_str(&fs::read_to_string(dir.join(CONFIG_FILENAME))?)?;

        // Return the unbaked site.
        let target_dir = dir.join(TARGET_SUBDIR);
        Ok(Site {
            pages,
            config,
            dir,
            target_dir,
            drafts,
        })
    }

    /// Build the site.
    pub fn build(mut self) -> Result<()> {
        self.render_content()?;

        let mut clean: Result<()> = Ok(());
        let mut sass: Result<()> = Ok(());
        let mut assets: Result<()> = Ok(());
        let mut html: Result<()> = Ok(());
        let mut feed: Result<()> = Ok(());

        rayon::scope(|s| {
            s.spawn(|s| {
                clean = self.clean_target_dir();
                s.spawn(|_| sass = self.render_sass());
                s.spawn(|_| assets = self.copy_assets());
                s.spawn(|_| html = self.render_html());
                s.spawn(|_| feed = self.render_feed());
            });
        });

        clean.or(sass).or(assets).or(html).or(feed)
    }

    /// Build the site and watch for updated files, rebuilding when necessary. Does not return.
    pub fn watch(self) -> Result<()> {
        let dir = self.dir.clone();
        let drafts = self.drafts;

        // Create a channel pair for events and send a first event to trigger the initial build.
        let (tx, rx) = mpsc::channel();
        tx.send(DebouncedEvent::Write(dir.clone()))?;

        // Watch the site directory for changes.
        let mut watcher = notify::watcher(tx, Duration::from_secs(1))?;
        watcher.watch(&self.dir, RecursiveMode::Recursive)?;

        loop {
            match rx.recv() {
                Ok(event) => match event {
                    DebouncedEvent::Create(path)
                    | DebouncedEvent::Chmod(path)
                    | DebouncedEvent::Write(path)
                    | DebouncedEvent::Remove(path)
                    | DebouncedEvent::Rename(path, _) => {
                        // Ignore files in the target dir and temporary files.
                        if !path
                            .extension()
                            .map(|s| s.to_string_lossy().ends_with('~'))
                            .unwrap_or(false)
                            && !path.starts_with(&self.target_dir)
                        {
                            println!("Rebuilding site...");
                            let site = Site::new(&dir, drafts)?;
                            site.build()?;
                        }
                    }
                    _ => {}
                },
                Err(e) => bail!(e),
            }
        }
    }

    /// Copy all of the files in the `static` subdirectory into the target directory.
    fn copy_assets(&self) -> Result<()> {
        let static_dir = self.dir.join(STATIC_SUBDIR);

        // Collect asset directories and files.
        let (dirs, files): (Vec<DirEntry>, Vec<DirEntry>) = WalkDir::new(&static_dir)
            .into_iter()
            .filter_map(|r| r.ok())
            .filter(|e| e.file_type().is_dir() || e.file_type().is_file())
            .partition(|e| e.file_type().is_dir());

        // Create the asset directory structure.
        for dir in dirs {
            let dst = self
                .target_dir
                .join(dir.path().strip_prefix(&static_dir).unwrap());
            fs::create_dir_all(&dst)
                .with_context(|| format!("Error creating directory: {:?}", &dst))?;
        }

        // Copy the files over in parallel.
        files
            .par_iter()
            .map(|e| {
                let dst = self
                    .target_dir
                    .join(e.path().strip_prefix(&static_dir).unwrap());

                fs::copy(e.path(), &dst)
                    .with_context(|| format!("Error copying asset {:?} to {:?}", e.path(), &dst))?;

                Ok(())
            })
            .collect::<Result<Vec<()>>>()?;

        Ok(())
    }

    /// Removes all files and subdirectories from the `target` subdirectory and re-creates it.
    fn clean_target_dir(&self) -> Result<()> {
        let _ = fs::remove_dir_all(&self.target_dir);
        fs::create_dir(&self.target_dir)
            .with_context(|| format!("Error creating {:?}", &self.target_dir))
    }

    /// Creates the `css` directory in the `target` subdirectory and renders all registered
    /// SASS/SCSS files.
    fn render_sass(&self) -> Result<()> {
        let sass_dir = self.dir.join(SASS_SUBDIR);
        let css_dir = self.target_dir.join(CSS_SUBDIR);
        fs::create_dir(&css_dir).with_context(|| format!("Error creating {:?}", &css_dir))?;

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

                fs::write(&css_path, css).with_context(|| format!("Error writing {:?}", &css_path))
            })
            .collect::<Result<()>>()
    }

    /// Renders all Markdown, including embedded LaTeX equations.
    fn render_content(&mut self) -> Result<()> {
        let md_opts = Options::all();
        let theme = THEME_SET
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
                let mut events = Vec::with_capacity(1024);
                for event in Parser::new_ext(&page.content, md_opts) {
                    match &event {
                        Event::Code(s) => {
                            if s.starts_with('$') && s.ends_with('$') {
                                // Convert inline LaTeX blocks (e.g. `$N+1`) to HTML.
                                events.push(Event::Html(
                                    katex::render_with_opts(&s[1..s.len() - 1], &inline_opts)?
                                        .into(),
                                ));
                            } else {
                                // Pass regular inline code blocks on to the formatter.
                                events.push(event);
                            }
                        }
                        Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(kind))) => {
                            // If the fenced code block doesn't have a kind, pass it on directly.
                            if kind.is_empty() {
                                events.push(event);
                            } else {
                                // Otherwise, record the fenced block kind, but don't pass the start
                                // on to the formatter. We'll handle our own <pre><code> blocking.
                                fence_kind = Some(kind.to_string());
                            }
                        }
                        Event::End(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => {
                            // If the fenced code block doesn't have a kind, pass it on directly.
                            if fence_kind.is_none() {
                                events.push(event);
                            } else {
                                // Reset the fenced block kind. Again, don't pass the end on to the
                                // formatter.
                                fence_kind = None;
                            }
                        }
                        Event::Text(s) => {
                            // If we've previously recorded a code block fence kind, then this text
                            // is the contents of a fenced code block with a specified kind.
                            if let Some(kind) = &fence_kind {
                                if kind.as_str() == "latex" {
                                    // Render LaTeX as HTML using KaTeX.
                                    let html = katex::render_with_opts(s, &block_opts)?;
                                    events.push(Event::Html(html.into()))
                                } else if let Some(syntax) = SYNTAX_SET.find_syntax_by_token(kind) {
                                    // If we can find a Syntect syntax for the given kind, format it
                                    // as syntax highlighted HTML.
                                    let html =
                                        highlighted_html_for_string(s, &SYNTAX_SET, syntax, theme);
                                    events.push(Event::Html(html.into()))
                                } else {
                                    // If we don't know what kind this code is, just slap it in a
                                    // <pre><code> block.
                                    events.extend_from_slice(&[
                                        Event::Html("<pre><code>".into()),
                                        Event::Text(s.clone()),
                                        Event::Html("</code></pre>".into()),
                                    ]);
                                }
                            } else {
                                // If we're not in a fenced code block, just pass the text on.
                                events.push(event);
                            }
                        }
                        // Pass all other events on untouched.
                        _ => events.push(event),
                    }
                }

                // Render as HTML.
                html::push_html(&mut out, events.into_iter());
                page.content = out;

                Ok(())
            })
            .collect()
    }

    /// Render all pages using their declared HTML templates.
    fn render_html(&self) -> Result<()> {
        let templates = Tera::new(
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
                let path = if page.name == "index" {
                    self.dir.join(TARGET_SUBDIR).join(INDEX_HTML)
                } else {
                    self.dir
                        .join(TARGET_SUBDIR)
                        .join(&page.name)
                        .join(INDEX_HTML)
                };

                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir(parent);
                }

                let f = BufWriter::new(
                    File::create(&path).with_context(|| format!("Error creating {:?}", &path))?,
                );

                let mut context = TeraContext::from_serialize(page)
                    .with_context(|| format!("Error rendering page {}", page.name))?;
                context.insert("site", &self);

                templates
                    .render_to(&page.template, &context, f)
                    .with_context(|| format!("Error rendering page {}", page.name))
            })
            .collect()
    }

    /// Build and render an Atom feed of all pages with dates.
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

        let path = self.dir.join(TARGET_SUBDIR).join(FEED_FILENAME);
        let f = BufWriter::new(File::create(&path)?);

        FeedBuilder::default()
            .title(self.config.title.as_str())
            .id(self.config.base_url.as_str())
            .entries(entries)
            .build()
            .write_to(f)
            .with_context(|| format!("Error creating {:?}", &path))?;

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

const MARKDOWN_EXT: &str = "md";

const CONTENT_SUBDIR: &str = "content";
const CSS_SUBDIR: &str = "css";
const SASS_SUBDIR: &str = "sass";
const STATIC_SUBDIR: &str = "static";
const TARGET_SUBDIR: &str = "target";
const TEMPLATES_DIR: &str = "templates";

const CONFIG_FILENAME: &str = "bakery.toml";
const FEED_FILENAME: &str = "atom.xml";
const INDEX_HTML: &str = "index.html";

lazy_static! {
    static ref SYNTAX_SET: SyntaxSet = SyntaxSet::load_defaults_newlines();
    static ref THEME_SET: ThemeSet = ThemeSet::load_defaults();
}

#[ctor]
fn init() {
    rayon::spawn(|| {
        lazy_static::initialize(&SYNTAX_SET);
    });
    rayon::spawn(|| {
        lazy_static::initialize(&THEME_SET);
    })
}
