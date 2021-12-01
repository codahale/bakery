use anyhow::{anyhow, Context, Result};
use katex::Opts;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::anychar;
use nom::combinator::{eof, map, peek};
use nom::multi::many_till;
use nom::sequence::delimited;
use nom::IResult;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum AST {
    Literal(String),
    InlineEq(String),
    BlockEq(String),
}

pub fn render_latex(ast: Vec<AST>, macros: &HashMap<String, String>) -> Result<String> {
    let block_opts = Opts::builder()
        .display_mode(true)
        .trust(true)
        .macros(macros.clone())
        .build()
        .unwrap();
    let inline_opts = Opts::builder()
        .display_mode(false)
        .trust(true)
        .macros(macros.clone())
        .build()
        .unwrap();
    let mut out = String::with_capacity(ast.len() * 100);

    for item in ast {
        out += &match item {
            AST::Literal(s) => s,
            AST::InlineEq(s) => katex::render_with_opts(&s, &inline_opts)
                .with_context(|| format!("Invalid LaTeX equation: {:?}", s))?,
            AST::BlockEq(s) => katex::render_with_opts(&s, &block_opts)
                .with_context(|| format!("Invalid LaTeX equation: {:?}", s))?,
        }
    }

    Ok(out)
}

pub fn parse_latex(i: &str) -> Result<Vec<AST>> {
    map(
        many_till(
            alt((parse_block_equation, parse_inline_equation, parse_text)),
            eof,
        ),
        |(ast, _)| ast,
    )(i)
    .map(|(_, ast)| ast)
    .map_err(|_| anyhow!("Invalid LaTeX delimiters"))
}

const INLINE_START_DELIM: &str = r#"\\("#;
const INLINE_END_DELIM: &str = r#"\\)"#;

fn parse_inline_equation(i: &str) -> IResult<&str, AST> {
    delimited(
        tag(INLINE_START_DELIM),
        map(take_until(INLINE_END_DELIM), |s: &str| {
            AST::InlineEq(s.to_string())
        }),
        tag(INLINE_END_DELIM),
    )(i)
}

const BLOCK_START_DELIM: &str = r#"$$"#;
const BLOCK_END_DELIM: &str = r#"$$"#;

fn parse_block_equation(i: &str) -> IResult<&str, AST> {
    delimited(
        tag(BLOCK_START_DELIM),
        map(take_until(BLOCK_END_DELIM), |s: &str| {
            AST::BlockEq(s.to_string())
        }),
        tag(BLOCK_END_DELIM),
    )(i)
}

fn parse_text(i: &str) -> IResult<&str, AST> {
    map(
        many_till(
            anychar,
            peek(alt((eof, tag(BLOCK_START_DELIM), tag(INLINE_START_DELIM)))),
        ),
        |(a, _)| AST::Literal(a.into_iter().collect()),
    )(i)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn regular_text() {
        assert_eq!(
            Ok(("$$", AST::Literal("one two three ".to_string()))),
            parse_text("one two three $$")
        );

        assert_eq!(
            Ok((r#"\\("#, AST::Literal("one two three ".to_string()))),
            parse_text(r#"one two three \\("#)
        );

        assert_eq!(
            Ok(("", AST::Literal("one two three".to_string()))),
            parse_text("one two three")
        );

        assert_eq!(Ok(("", AST::Literal("".to_string()))), parse_text(""));
    }

    #[test]
    fn inline_equation() {
        assert_eq!(
            Ok(("", AST::InlineEq("one two three".to_string()))),
            parse_inline_equation(r#"\\(one two three\\)"#)
        );

        assert!(parse_inline_equation("goof troop").is_err());
    }

    #[test]
    fn block_equation() {
        assert_eq!(
            Ok(("", AST::BlockEq("one two three".to_string()))),
            parse_block_equation("$$one two three$$")
        );
    }

    #[test]
    fn mixed_content() {
        assert!(parse_latex(r#"one two $$ three"#).is_err());

        assert_eq!(
            vec![AST::Literal("one two three".to_string())],
            parse_latex(r#"one two three"#).unwrap()
        );

        assert_eq!(
            vec![
                AST::Literal("one two ".to_string()),
                AST::BlockEq("N=1".to_string()),
                AST::Literal(" three".to_string()),
            ],
            parse_latex(r#"one two $$N=1$$ three"#).unwrap()
        );

        assert_eq!(
            vec![
                AST::Literal("one two ".to_string()),
                AST::InlineEq("N=1".to_string()),
                AST::Literal(" three".to_string()),
            ],
            parse_latex(r#"one two \\(N=1\\) three"#).unwrap()
        );
    }

    #[test]
    fn html() {
        let html = render_latex(
            vec![
                AST::Literal("one ".to_string()),
                AST::InlineEq("N".to_string()),
                AST::Literal(" ".to_string()),
                AST::BlockEq("\\sigma".to_string()),
                AST::Literal(" two".to_string()),
            ],
            &HashMap::default(),
        )
        .expect("error rendering LaTeX");

        assert_eq!(
            r#"one <span class="katex"><span class="katex-mathml"><math xmlns="http://www.w3.org/1998/Math/MathML"><semantics><mrow><mi>N</mi></mrow><annotation encoding="application/x-tex">N</annotation></semantics></math></span><span class="katex-html" aria-hidden="true"><span class="base"><span class="strut" style="height:0.6833em;"></span><span class="mord mathnormal" style="margin-right:0.10903em;">N</span></span></span></span> <span class="katex-display"><span class="katex"><span class="katex-mathml"><math xmlns="http://www.w3.org/1998/Math/MathML" display="block"><semantics><mrow><mi>σ</mi></mrow><annotation encoding="application/x-tex">\sigma</annotation></semantics></math></span><span class="katex-html" aria-hidden="true"><span class="base"><span class="strut" style="height:0.4306em;"></span><span class="mord mathnormal" style="margin-right:0.03588em;">σ</span></span></span></span></span> two"#,
            html
        )
    }
}
