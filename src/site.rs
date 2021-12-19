use std::collections::HashMap;
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
use globwalk::{FileType, GlobWalkerBuilder};
use grass::OutputStyle;
use gray_matter::{engine, Matter};
use katex::Opts;
use lazy_static::lazy_static;
use notify::{DebouncedEvent, RecursiveMode, Watcher};
use pulldown_cmark::{html, CodeBlockKind, Event, Options, Parser, Tag};
use serde::{Deserialize, Serialize};
use syntect::highlighting::ThemeSet;
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;
use tera::{Context as TeraContext, Tera};
use tracing::instrument;
use url::Url;
use walkdir::{DirEntry, WalkDir};

#[instrument]
pub fn watch<P: AsRef<Path> + Debug>(dir: P, drafts: bool) -> Result<()> {
    // Convert the path to canonical, if possible.
    let dir = dir
        .as_ref()
        .canonicalize()
        .with_context(|| format!("Failed to find site directory: {:?}", dir))?;
    let target_dir = dir.join(TARGET_SUBDIR);

    // Create a channel pair for events and send a first event to trigger the initial build.
    let (tx, rx) = mpsc::channel();
    tx.send(DebouncedEvent::Write(dir.clone()))?;

    // Watch the site directory for changes.
    let mut watcher = notify::watcher(tx, Duration::from_secs(1))?;
    watcher.watch(&dir, RecursiveMode::Recursive)?;

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
                        && !path.starts_with(&target_dir)
                    {
                        tracing::info!(target:"rebuild", changed=?path);
                        build(&dir, drafts)?;
                    }
                }
                _ => {}
            },
            Err(e) => bail!(e),
        }
    }
}

#[instrument]
pub fn build<P: AsRef<Path> + Debug>(dir: P, drafts: bool) -> Result<()> {
    // Convert the path to canonical, if possible.
    let dir = dir
        .as_ref()
        .canonicalize()
        .with_context(|| format!("Failed to find site directory: {:?}", dir))?;

    // Load the site config.
    let config = load_config(&dir)?;

    // Scan the content subdirectory for .md files and load them, parsing the TOML front matter.
    // Filter out drafts, if necessary.
    let content_dir = dir.join(CONTENT_SUBDIR);
    let mut pages = load_pages(&content_dir)?
        .into_iter()
        .filter(|p| drafts || !p.draft)
        .collect::<Vec<Page>>();

    // Render Markdown and LaTeX.
    let target_dir = dir.join(TARGET_SUBDIR);
    let (clean, markdown) = rayon::join(
        || clean_target_dir(&target_dir),
        || render_markdown(&mut pages, &config.theme),
    );

    // Check results.
    clean.and(markdown)?;

    // Copy all asset files.
    // Render SASS files.
    // Render HTML files.
    // Render Atom feed.
    let ((assets, sass), (html, feed)) = rayon::join(
        || {
            rayon::join(
                || copy_assets(&dir, &target_dir),
                || render_sass(&dir, &target_dir, &config.sass),
            )
        },
        || {
            rayon::join(
                || render_html(&dir, &target_dir, &pages),
                || render_feed(&config.title, &target_dir, config.base_url.as_str(), &pages),
            )
        },
    );

    assets
        .and(sass)
        .and(html)
        .and(feed)
        .map(|_| tracing::info!("site built"))
}

#[instrument]
fn load_config(dir: &Path) -> Result<SiteConfig> {
    let config_path = dir.join(CONFIG_FILENAME);
    tracing::debug!(?config_path, "loading config");
    toml::from_str(
        &fs::read_to_string(&config_path)
            .with_context(|| format!("Unable to read config file: {:?}", &config_path))?,
    )
    .with_context(|| format!("Unable to parse config file: {:?}", &config_path))
}

#[instrument]
fn clean_target_dir(target_dir: &Path) -> Result<()> {
    let _ = fs::remove_dir_all(target_dir);
    tracing::debug!(?target_dir, "cleaned target dir");
    fs::create_dir(target_dir).with_context(|| format!("Error creating {:?}", target_dir))
}

#[instrument]
fn load_pages(content_dir: &Path) -> Result<Vec<Page>> {
    let page_paths = find_pages(content_dir)?;
    let matter = Matter::<engine::TOML>::new();
    page_paths
        .iter()
        .map(|path| {
            // Read the file contents.
            let s = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read file {:?}", path))?;

            // Parse the front matter and contents.
            let parsed = matter
                .parse_with_struct::<Page>(&s)
                .ok_or_else(|| anyhow!("Invalid front matter in {:?}", path))?;

            tracing::debug!(path=?path, "loaded page");

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
        .collect::<Result<Vec<Page>>>()
}

#[instrument]
fn find_pages(content_dir: &Path) -> Result<Vec<PathBuf>> {
    GlobWalkerBuilder::new(&content_dir, "*.md")
        .file_type(FileType::FILE)
        .build()?
        .map(|r| {
            r.map(walkdir::DirEntry::into_path)
                .map_err(anyhow::Error::new)
        })
        .collect::<Result<Vec<PathBuf>>>()
}

#[instrument]
fn copy_assets(dir: &Path, target_dir: &Path) -> Result<()> {
    let static_dir = dir.join(STATIC_SUBDIR);

    // Collect asset directories and files.
    let (dirs, files): (Vec<DirEntry>, Vec<DirEntry>) = WalkDir::new(&static_dir)
        .into_iter()
        .filter_map(|r| r.ok())
        .filter(|e| e.file_type().is_dir() || e.file_type().is_file())
        .partition(|e| e.file_type().is_dir());

    // Create the asset directory structure.
    for dir in dirs {
        tracing::debug!(dir=?dir, "creating dir");
        let dst = target_dir.join(dir.path().strip_prefix(&static_dir).unwrap());
        fs::create_dir_all(&dst)
            .with_context(|| format!("Error creating directory: {:?}", &dst))?;
    }

    // Copy the files over.
    files.iter().try_for_each(|e| {
        let dst = target_dir.join(e.path().strip_prefix(&static_dir).unwrap());
        tracing::debug!(src=?e.path(), dst=?dst, "copying asset");
        fs::copy(e.path(), &dst)
            .with_context(|| format!("Error copying asset {:?} to {:?}", e.path(), &dst))?;

        Ok(())
    })
}

#[instrument]
fn render_sass(dir: &Path, target_dir: &Path, sass: &SassConfig) -> Result<()> {
    let sass_dir = dir.join(SASS_SUBDIR);
    let css_dir = target_dir.join(CSS_SUBDIR);
    fs::create_dir(&css_dir).with_context(|| format!("Error creating {:?}", &css_dir))?;

    let mut options = grass::Options::default().style(if sass.compressed {
        OutputStyle::Compressed
    } else {
        OutputStyle::Expanded
    });

    for path in sass.load_paths.iter() {
        options = options.load_path(path);
    }

    sass.targets.iter().try_for_each(|(output, input)| {
        tracing::debug!(input=?input, output=?output, "rendering sass file");
        let css_path = css_dir.join(output);
        let sass_path = sass_dir.join(input);

        let css = grass::from_path(sass_path.to_string_lossy().as_ref(), &options)
            .map_err(|e| anyhow::Error::msg(e.to_string()))?;

        fs::write(&css_path, css).with_context(|| format!("Error writing {:?}", &css_path))
    })
}

#[instrument(skip(pages))]
fn render_markdown(pages: &mut [Page], theme: &str) -> Result<()> {
    let md_opts = Options::all();
    let theme = THEME_SET
        .themes
        .get(theme)
        .ok_or_else(|| anyhow!("Invalid syntax theme: {:?}", theme))?;

    let inline_opts = Opts::builder().display_mode(false).build()?;
    let block_opts = Opts::builder().display_mode(true).build()?;

    pages.iter_mut().try_for_each(|page| {
        tracing::debug!(page=?page.name, "parsing markdown");
        let mut out = String::with_capacity(page.content.len() * 2);
        let mut fence_kind: Option<String> = None;
        let mut events = Vec::with_capacity(1024);
        for event in Parser::new_ext(&page.content, md_opts) {
            match &event {
                Event::Code(s) => {
                    if s.starts_with('$') && s.ends_with('$') {
                        // Convert inline LaTeX blocks (e.g. `$N+1`) to HTML.
                        let s = &s[1..s.len() - 1];
                        tracing::debug!(block=?s, "rendering inline equation");
                        events.push(Event::Html(
                            katex::render_with_opts(s, &inline_opts)?.into(),
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
                            tracing::debug!(block=?s, "rendering display equation");
                            let html = katex::render_with_opts(s, &block_opts)?;
                            events.push(Event::Html(html.into()))
                        } else if let Some(syntax) = SYNTAX_SET.find_syntax_by_token(kind) {
                            // If we can find a Syntect syntax for the given kind, format it
                            // as syntax highlighted HTML.
                            tracing::debug!(kind=?kind, block=?s, "rendering code block");
                            let html = highlighted_html_for_string(s, &SYNTAX_SET, syntax, theme);
                            events.push(Event::Html(html.into()))
                        } else {
                            // If we don't know what kind this code is, just slap it in a
                            // <pre><code> block.
                            tracing::debug!(block=?s, "rendering pre block");
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
}

#[instrument(skip(pages))]
fn render_html(dir: &Path, target_dir: &Path, pages: &[Page]) -> Result<()> {
    let templates = Tera::new(
        dir.join(TEMPLATES_DIR)
            .join("**")
            .join("*")
            .to_string_lossy()
            .as_ref(),
    )?;
    pages.iter().try_for_each(|page| {
        let path = if page.name == "index" {
            target_dir.join(INDEX_FILENAME)
        } else {
            target_dir.join(&page.name).join(INDEX_FILENAME)
        };

        if let Some(parent) = path.parent() {
            let _ = fs::create_dir(parent);
        }

        let f = BufWriter::new(
            File::create(&path).with_context(|| format!("Error creating {:?}", &path))?,
        );

        let mut context = TeraContext::from_serialize(page)
            .with_context(|| format!("Error rendering page {}", page.name))?;
        context.insert("pages", pages);

        tracing::debug!(page=?page.name, dst=?path, "rendered html");
        templates
            .render_to(&page.template, &context, f)
            .with_context(|| format!("Error rendering page {}", page.name))
    })
}

#[instrument(skip(pages))]
fn render_feed(title: &str, target_dir: &Path, base_url: &str, pages: &[Page]) -> Result<()> {
    let entries: Vec<Entry> = pages
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

    let path = target_dir.join(FEED_FILENAME);
    let f = BufWriter::new(File::create(&path)?);

    tracing::debug!(dst=?path, "rendered feed");

    FeedBuilder::default()
        .title(title)
        .id(base_url)
        .entries(entries)
        .build()
        .write_to(f)
        .with_context(|| format!("Error creating {:?}", &path))?;

    Ok(())
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

const CONTENT_SUBDIR: &str = "content";
const CSS_SUBDIR: &str = "css";
const SASS_SUBDIR: &str = "sass";
const STATIC_SUBDIR: &str = "static";
const TARGET_SUBDIR: &str = "target";
const TEMPLATES_DIR: &str = "templates";

const CONFIG_FILENAME: &str = "bakery.toml";
const FEED_FILENAME: &str = "atom.xml";
const INDEX_FILENAME: &str = "index.html";

lazy_static! {
    static ref SYNTAX_SET: SyntaxSet = SyntaxSet::load_defaults_newlines();
    static ref THEME_SET: ThemeSet = ThemeSet::load_defaults();
}

#[ctor]
fn init() {
    rayon::spawn(|| lazy_static::initialize(&SYNTAX_SET));
    rayon::spawn(|| lazy_static::initialize(&THEME_SET));
}
