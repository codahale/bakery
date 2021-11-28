use katex::Opts;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::map;
use nom::multi::many0;
use nom::sequence::delimited;
use nom::{Err, IResult, Parser};

#[derive(Clone, Debug, PartialEq)]
pub enum AST {
    Literal(String),
    InlineEq(String),
    BlockEq(String),
}

pub fn parse_latex(i: &str) -> Result<Vec<AST>, nom::error::Error<String>> {
    many0(alt((
        parse_block_equation,
        parse_inline_equation,
        take_until("$$").map(|s: &str| AST::Literal(s.to_string())),
    )))(i)
    .map(|(rest, mut ast)| {
        ast.push(AST::Literal(rest.to_string()));
        ast
    })
    .map_err(|e| match e {
        Err::Incomplete(_) => unreachable!(),
        Err::Error(e) => nom::error::Error::new(e.input.to_string(), e.code),
        Err::Failure(_) => unreachable!(),
    })
}

pub fn render_latex(ast: Vec<AST>) -> katex::Result<String> {
    let block_opts = Opts::builder()
        .display_mode(true)
        .trust(true)
        .build()
        .unwrap();
    let inline_opts = Opts::builder()
        .display_mode(false)
        .trust(true)
        .build()
        .unwrap();
    let mut out = String::with_capacity(ast.len() * 100);

    for item in ast {
        out += &match item {
            AST::Literal(s) => s,
            AST::InlineEq(s) => katex::render_with_opts(&s, &inline_opts)?,
            AST::BlockEq(s) => katex::render_with_opts(&s, &block_opts)?,
        }
    }

    Ok(out)
}

fn parse_inline_equation(i: &str) -> IResult<&str, AST> {
    const DELIM: &str = "$$";
    delimited(
        tag(DELIM),
        map(take_until(DELIM), |s: &str| AST::InlineEq(s.to_string())),
        tag(DELIM),
    )(i)
}

fn parse_block_equation(i: &str) -> IResult<&str, AST> {
    delimited(
        tag("$$\n"),
        map(take_until("\n$$"), |s: &str| AST::BlockEq(s.to_string())),
        tag("\n$$"),
    )(i)
}

#[cfg(test)]
mod test {
    use super::*;
    use nom::error::ErrorKind;

    #[test]
    fn inline_equation() {
        assert_eq!(
            Ok(("", AST::InlineEq("one two three".to_string()))),
            parse_inline_equation("$$one two three$$")
        );

        assert!(parse_inline_equation("goof troop").is_err());
    }

    #[test]
    fn block_equation() {
        assert_eq!(
            Ok(("", AST::BlockEq("one two three".to_string()))),
            parse_block_equation("$$\none two three\n$$")
        );

        assert!(parse_block_equation("$$goof troop$$").is_err());
    }

    #[test]
    fn mixed_content() {
        assert_eq!(
            Err(nom::error::Error {
                input: "$$ three".to_string(),
                code: ErrorKind::Many0
            }),
            parse_latex("one two $$ three")
        );

        assert_eq!(
            Ok(vec![AST::Literal("one two three".to_string())]),
            parse_latex("one two three")
        );

        assert_eq!(
            Ok(vec![
                AST::Literal("one two ".to_string()),
                AST::InlineEq("N=1".to_string()),
                AST::Literal(" three".to_string()),
            ]),
            parse_latex("one two $$N=1$$ three")
        );

        assert_eq!(
            Ok(vec![
                AST::Literal("one two ".to_string()),
                AST::InlineEq("N=1".to_string()),
                AST::Literal(" and ".to_string()),
                AST::InlineEq("N=2".to_string()),
                AST::Literal(" three".to_string()),
            ]),
            parse_latex("one two $$N=1$$ and $$N=2$$ three")
        );

        assert_eq!(
            Ok(vec![
                AST::Literal("one two \n".to_string()),
                AST::BlockEq("N=1".to_string()),
                AST::Literal("\n and ".to_string()),
                AST::InlineEq("N=2".to_string()),
                AST::Literal(" three".to_string()),
            ]),
            parse_latex("one two \n$$\nN=1\n$$\n and $$N=2$$ three")
        );
    }

    #[test]
    fn html() {
        let html = render_latex(vec![
            AST::Literal("one ".to_string()),
            AST::InlineEq("N".to_string()),
            AST::Literal(" ".to_string()),
            AST::BlockEq("\\sigma".to_string()),
            AST::Literal(" two".to_string()),
        ])
        .expect("error rendering LaTeX");

        assert_eq!(
        "one <span class=\"katex\"><span class=\"katex-mathml\"><math xmlns=\"http://www.w3.org/1998/Math/MathML\"><semantics><mrow><mi>N</mi></mrow><annotation encoding=\"application/x-tex\">N</annotation></semantics></math></span><span class=\"katex-html\" aria-hidden=\"true\"><span class=\"base\"><span class=\"strut\" style=\"height:0.6833em;\"></span><span class=\"mord mathnormal\" style=\"margin-right:0.10903em;\">N</span></span></span></span> <span class=\"katex-display\"><span class=\"katex\"><span class=\"katex-mathml\"><math xmlns=\"http://www.w3.org/1998/Math/MathML\" display=\"block\"><semantics><mrow><mi>σ</mi></mrow><annotation encoding=\"application/x-tex\">\\sigma</annotation></semantics></math></span><span class=\"katex-html\" aria-hidden=\"true\"><span class=\"base\"><span class=\"strut\" style=\"height:0.4306em;\"></span><span class=\"mord mathnormal\" style=\"margin-right:0.03588em;\">σ</span></span></span></span></span> two",
         html)
    }
}
