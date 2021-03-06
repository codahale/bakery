use std::collections::HashMap;
use std::fmt::Debug;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use std::{fs, thread};

use aho_corasick::AhoCorasick;
use anyhow::{anyhow, bail, Context, Result};
use atom_syndication::{ContentBuilder, Entry, EntryBuilder, FeedBuilder, Text};
use chrono::{DateTime, Utc};
use globset::{Glob, GlobSetBuilder};
use globwalk::{FileType, GlobWalkerBuilder};
use grass::OutputStyle;
use katex::Opts;
use notify::{DebouncedEvent, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use pulldown_cmark::{html, CodeBlockKind, Event, Options, Parser, Tag};
use serde::{Deserialize, Serialize};
use syntect::highlighting::{Theme, ThemeSet};
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;
use tera::{Context as TeraContext, Tera};
use tracing::instrument;
use url::Url;
use walkdir::WalkDir;

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

    // Ignore temp files and the target directory.
    let ignored = GlobSetBuilder::new()
        .add(Glob::new("*~")?)
        .add(Glob::new(target_dir.to_string_lossy().as_ref())?)
        .add(Glob::new(target_dir.join("**").join("*").to_string_lossy().as_ref())?)
        .build()?;

    loop {
        match rx.recv() {
            Ok(event) => match event {
                DebouncedEvent::Create(path)
                | DebouncedEvent::Chmod(path)
                | DebouncedEvent::Write(path)
                | DebouncedEvent::Remove(path)
                | DebouncedEvent::Rename(path, _) => {
                    // Ignore files in the target dir and temporary files.
                    if !ignored.is_match(&path) {
                        tracing::info!(changed=?path, "rebuild");
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
    let dir = Arc::new(dir);

    // Load the site config.
    let config = load_config(&dir)?;

    // Scan the content subdirectory for .md files and load them, parsing the TOML front matter.
    // Filter out drafts, if necessary.
    let content_dir = dir.join(CONTENT_SUBDIR);
    let pages =
        load_pages(&content_dir)?.into_iter().filter(|p| drafts || !p.draft).collect::<Vec<Page>>();

    // Clean the target directory in another thread.
    let target_dir = Arc::new(dir.join(TARGET_SUBDIR));
    let clean = {
        let target_dir = target_dir.clone();
        thread::spawn(move || clean_target_dir(&target_dir))
    };

    // Render Markdown and LaTeX in another thread.
    let theme = THEME_SET
        .themes
        .get(&config.theme)
        .ok_or_else(|| anyhow!("Invalid syntax theme: {:?}", &config.theme))?;
    let markdown = thread::spawn(move || render_markdown(pages, theme));

    // Wait for the target directoy to be cleaned before using it.
    clean.join().unwrap()?;

    // Copy all asset files in another thread.
    let assets = {
        let dir = dir.clone();
        let target_dir = target_dir.clone();
        thread::spawn(move || copy_assets(&dir, &target_dir))
    };

    // Render SASS files in another thread.
    let sass = {
        let dir = dir.clone();
        let target_dir = target_dir.clone();
        let sass = config.sass;
        thread::spawn(move || render_sass(&dir, &target_dir, &sass))
    };

    // Wait for fully rendered pages.
    let pages = markdown.join().unwrap()?;

    // Render HTML files.
    render_html(&dir, &target_dir, &pages)?;

    // Render Atom feed.
    render_feed(&config.title, &target_dir, config.base_url.as_str(), &pages)?;

    // Wait for assets and SASS to complete.
    assets.join().unwrap()?;
    sass.join().unwrap()?;

    tracing::info!("site built");

    Ok(())
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
    let matcher = AhoCorasick::new(&["\n---\n"]);
    find_pages(content_dir)?
        .iter()
        .map(|path| {
            // Read the file contents.
            let s = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read file {:?}", path))?;

            if let Some(m) = matcher.earliest_find(&s) {
                // Extract the page metadata and add the content.
                let header = s[0..m.start()].trim_matches(&['-', '-', '-'] as &[_]);
                let content = &s[m.end()..];
                let mut page: Page = toml::from_str(header)
                    .with_context(|| format!("Invalid front matter in {:?}", path))?;
                page.content = content.to_string();

                tracing::debug!(?path, "loaded page");

                // Infer the page name from the page filename.
                let mut page_name = path.strip_prefix(&content_dir).unwrap().to_path_buf();
                page_name.set_extension("");
                page.name = page_name.to_string_lossy().to_string();

                Ok(page)
            } else {
                bail!("Invalid front matter in {:?}", path);
            }
        })
        .collect::<Result<Vec<Page>>>()
}

#[instrument]
fn find_pages(content_dir: &Path) -> Result<Vec<PathBuf>> {
    GlobWalkerBuilder::new(&content_dir, "*.md")
        .file_type(FileType::FILE)
        .build()?
        .map(|r| r.map(walkdir::DirEntry::into_path).map_err(anyhow::Error::new))
        .collect::<Result<Vec<PathBuf>>>()
}

#[instrument]
fn copy_assets(dir: &Path, target_dir: &Path) -> Result<()> {
    let static_dir = dir.join(STATIC_SUBDIR);

    // Traverse the static dir, directories-first.
    WalkDir::new(&static_dir).contents_first(false).into_iter().try_for_each(|entry| {
        let entry = entry?;
        let is_dir = entry.file_type().is_dir();
        let is_file = entry.file_type().is_file();
        let src = entry.into_path();
        let dst = target_dir.join(src.strip_prefix(&static_dir).unwrap());

        if is_dir {
            // Create directories as needed.
            tracing::debug!(?src, ?dst, "creating dir");
            fs::create_dir_all(&dst)
                .with_context(|| format!("Error creating directory: {:?}", &dst))?;
        } else if is_file {
            // Copy files.
            tracing::debug!(?src, ?dst, "copying asset");
            fs::copy(&src, &dst)
                .with_context(|| format!("Error copying asset {:?} to {:?}", &src, &dst))?;
        }

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
        tracing::debug!(?input, ?output, "rendering sass file");
        let css_path = css_dir.join(output);
        let sass_path = sass_dir.join(input);

        let css = grass::from_path(sass_path.to_string_lossy().as_ref(), &options)
            .map_err(|e| anyhow::Error::msg(e.to_string()))?;

        fs::write(&css_path, css).with_context(|| format!("Error writing {:?}", &css_path))
    })
}

#[instrument(skip(pages, theme))]
fn render_markdown(mut pages: Vec<Page>, theme: &Theme) -> Result<Vec<Page>> {
    let md_opts = Options::all();
    let inline_opts = Opts::builder().display_mode(false).build()?;
    let block_opts = Opts::builder().display_mode(true).build()?;

    for page in pages.iter_mut() {
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
                        events.push(Event::Html(katex::render_with_opts(s, &inline_opts)?.into()));
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
                            tracing::debug!(?kind, block=?s, "rendering code block");
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
    }

    Ok(pages)
}

#[instrument(skip(pages))]
fn render_html(dir: &Path, target_dir: &Path, pages: &[Page]) -> Result<()> {
    let templates =
        Tera::new(dir.join(TEMPLATES_DIR).join("**").join("*").to_string_lossy().as_ref())?;
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
                .content(ContentBuilder::default().value(page.content.clone()).build())
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

static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);
