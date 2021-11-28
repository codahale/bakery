use std::env;
use std::path::Path;

use crate::site::Site;

mod latex;
mod sass;
mod site;
mod util;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    let mut site = Site::load(Path::new(&args[0])).expect("error loading site");

    site.render_content().unwrap();
    site.render_html().unwrap();
}

// fn main() {
//     let args: Vec<String> = env::args().skip(1).collect();
//
//     // Compile SASS.
//     grass::from_path("testdata/sass/style.scss", &grass::Options::default())
//         .expect("error compiling SASS");
//
//     // Expand globs.
//     let paths: Vec<PathBuf> = glob::glob(&args[0])
//         .expect("invalid glob pattern")
//         .filter_map(Result::ok)
//         .collect();
//
//     // Parse templates.
//     let templates = Tera::new(&args[1]).expect("error parsing templates");
//
//     // Parse frontmatter.
//     let matter = Matter::<engine::YAML>::new();
//     let mut entities: Vec<ParsedEntity> = paths
//         .iter()
//         .map(|p| fs::read_to_string(p).expect(&format!("error reading file: {:?}", p)))
//         .map(|s| matter.parse(&s))
//         .collect();
//
//     // Render LaTeX.
//     let block_eq = Regex::new(r"\n\$\$([\s\S]+?)\$\$").unwrap();
//     let block_opts = Opts::builder()
//         .display_mode(true)
//         .trust(true)
//         .build()
//         .unwrap();
//     let inline_eq = Regex::new(r"\$\$([\s\S]+?)\$\$").unwrap();
//     let inline_opts = Opts::builder()
//         .display_mode(false)
//         .trust(true)
//         .build()
//         .unwrap();
//     for mut entity in entities.iter_mut() {
//         // Render blocks first, then inline.
//         entity.content = inline_eq
//             .replace_all(
//                 block_eq
//                     .replace_all(&entity.content, |caps: &Captures| {
//                         katex::render_with_opts(&caps[1], &block_opts)
//                             .expect("invalid LaTeX equation")
//                     })
//                     .as_ref(),
//                 |caps: &Captures| {
//                     katex::render_with_opts(&caps[1], &inline_opts).expect("invalid LaTeX equation")
//                 },
//             )
//             .to_string();
//     }
//
//     // Render Markdown.
//     for mut entity in entities.iter_mut() {
//         entity.content = render_markdown(&entity.content);
//     }
//
//     let mut feed_entries: Vec<Entry> = vec![];
//     for entity in entities {
//         let mut context = Context::new();
//         context.insert("content", &entity.content);
//
//         let html = templates
//             .render("output.html", &context)
//             .expect("error rendering HTML");
//
//         println!("{}", &html);
//
//         feed_entries.push(
//             EntryBuilder::default()
//                 .content(ContentBuilder::default().value(html).build())
//                 .build(),
//         )
//     }
//
//     let feed = FeedBuilder::default().entries(feed_entries).build();
//     eprintln!("{}", feed.to_string());
// }
//
// fn render_markdown(content: &str) -> String {
//     let mut out = String::with_capacity(content.len() * 2);
//     let opts = Options::all();
//     let p = Parser::new_ext(content, opts);
//     html::push_html(&mut out, p);
//
//     out
// }
