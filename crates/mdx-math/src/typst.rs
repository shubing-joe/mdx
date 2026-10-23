//! LaTeX → Typst math conversion (`tylax`-backed).
//!
//! [`latex_to_typst`] returns a **Typst math expression body**: exactly the text
//! that belongs between the `$…$` delimiters of an equation, nothing else. That
//! contract is what lets `mdx-typst` splice the result into `$ … $` without
//! inspecting it, and it is why the result never contains a `$` fence, a
//! `#set`/`#let`/`#show`/`#import` statement, a `#math.equation(…)` wrapper, or
//! a raw LaTeX command.
//!
//! Pipeline (each step is total — malformed input produces an `Err`, never a
//! panic):
//!
//! 1. **Unwrap** surrounding whitespace, `$$…$$` / `$…$` / `\[…\]` fences and a
//!    trivial outer `\begin{equation}`-style environment.
//! 2. **Reject** structurally broken LaTeX before it reaches the converter:
//!    unbalanced `{`/`}`, mismatched `\begin`/`\end`, unbalanced
//!    `\left`/`\right`. The third-party parser is a black box that may panic, so
//!    it is additionally called behind a panic guard.
//! 3. **Convert** with `tylax`.
//! 4. **Extract** the math body from anything document-shaped the converter
//!    emitted (`#math.equation(…)[…]`, directive lines, stray fences).
//!
//! Errors are one line: a short reason plus the offending snippet, at most
//! [`MAX_SNIPPET_CHARS`] characters of it.

use std::panic::{AssertUnwindSafe, catch_unwind};

/// Longest input snippet quoted back in an error message.
const MAX_SNIPPET_CHARS: usize = 80;

/// Opening/closing math fences, longest first so that `$$` is not read as two
/// `$` fences with an empty body between them.
const FENCES: [(&str, &str); 3] = [("$$", "$$"), ("\\[", "\\]"), ("$", "$")];

/// Environments that only mark an expression as display math: unwrapping them
/// changes nothing about the body itself.
const WRAPPER_ENVS: [&str; 4] = ["equation", "equation*", "displaymath", "math"];

/// Typst statements that are markup rather than math. `tylax` can emit them
/// when it treats its input as a document; none of them may survive into a math
/// expression body.
const MARKUP_DIRECTIVES: [&str; 4] = ["#set", "#let", "#show", "#import"];

/// `tylax` substitutes this literal for an empty mandatory argument (for
/// example `\frac{}{}`). It renders as the product `z·w·s`, so it must never be
/// passed on as if it were the formula.
const EMPTY_ARG_PLACEHOLDER: &str = "zws";

/// Convert a LaTeX math expression into a Typst math expression body.
///
/// The input may be bare LaTeX (`\frac{1}{2}`), fenced (`$$…$$`, `$…$`,
/// `\[…\]`) or wrapped in a trivial `\begin{equation}` environment; all three
/// produce the same body. The result is intended to be placed between `$`
/// delimiters, and is therefore guaranteed to contain no `$`, no Typst markup
/// statement and no document wrapper.
///
/// # Errors
///
/// Returns a short reason on:
///
/// * input that is empty once unwrapped,
/// * unbalanced braces / mismatched environments / unbalanced `\left`…`\right`,
/// * an unescaped `$` left inside the expression (i.e. the caller passed more
///   than one expression),
/// * a converter result that is not a math expression body, or that leaks the
///   empty-argument placeholder `zws`.
///
/// The reason quotes at most 80 characters of the offending input.
///
/// ```
/// # use mdx_math::latex_to_typst;
/// let body = latex_to_typst(r"\frac{1}{2}")?;
/// assert!(!body.contains('$') && !body.contains('\\'));
/// assert!(latex_to_typst(r"\frac{1}{2").is_err());
/// # Ok::<(), String>(())
/// ```
pub fn latex_to_typst(src: &str) -> Result<String, String> {
    let body = unwrap_math_source(src)?;
    check_structure(&body)?;

    let converted = convert_with_tylax(&body)?;

    finalize_math(&converted)
}

// ---------------------------------------------------------------------------
// 1. Unwrapping
// ---------------------------------------------------------------------------

/// Trim, strip math fences and a trivial outer wrapper environment.
fn unwrap_math_source(src: &str) -> Result<String, String> {
    let mut body = src.trim().to_string();

    // Repeat so `$$\begin{equation}…\end{equation}$$`-style nesting unwinds too.
    // Each pass removes at least one fence pair, so this terminates.
    loop {
        let mut stripped = false;

        for (open, close) in FENCES {
            if let Some(inner) = strip_fence(&body, open, close) {
                body = inner;
                stripped = true;
                break;
            }
        }

        if !stripped {
            break;
        }
    }

    if let Some(inner) = strip_wrapper_environment(&body) {
        body = inner;
    }

    if body.is_empty() {
        return Err(indict("empty math expression", &[]));
    }

    if let Some(pos) = find_unescaped(&body, '$') {
        return Err(indict_at(
            "unescaped '$' inside math expression (convert one expression at a time)",
            &body.chars().collect::<Vec<char>>(),
            pos,
        ));
    }

    Ok(body)
}

/// Strip `open`/`close` when the whole string is one fenced group.
fn strip_fence(body: &str, open: &str, close: &str) -> Option<String> {
    if body.len() <= open.len() + close.len() {
        return None;
    }

    let inner = body.strip_prefix(open)?.strip_suffix(close)?;

    Some(inner.trim().to_string())
}

/// Strip `\begin{equation}…\end{equation}` and friends when they wrap the whole
/// expression. Only the trivial, outermost, single-environment case is handled;
/// anything else is left for the converter.
fn strip_wrapper_environment(body: &str) -> Option<String> {
    let rest = body.trim_start();
    let after_begin = rest.strip_prefix("\\begin{")?;
    let name_end = after_begin.find('}')?;
    let name = &after_begin[..name_end];

    if !WRAPPER_ENVS.contains(&name) {
        return None;
    }

    let after_name = after_begin[name_end + 1..].trim_end();
    let closer = format!("\\end{{{name}}}");
    let inner = after_name.strip_suffix(&closer)?;

    Some(inner.trim().to_string())
}

// ---------------------------------------------------------------------------
// 2. Structural validation
// ---------------------------------------------------------------------------

/// Check the LaTeX structure the converter cannot be trusted with: braces,
/// environment nesting and `\left`/`\right` pairs.
///
/// Positions in the error messages are character (not byte) offsets, and the
/// quoted snippet is always a single line of at most [`MAX_SNIPPET_CHARS`]
/// characters.
fn check_structure(src: &str) -> Result<(), String> {
    let chars: Vec<char> = src.chars().collect();

    let mut opened_at: Vec<usize> = Vec::new();
    let mut environments: Vec<(String, usize)> = Vec::new();
    let mut lefts = 0usize;
    let mut rights = 0usize;
    let mut first_left_at = 0usize;

    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            // A LaTeX comment runs to the end of the line and may hide a brace.
            '%' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\\' => {
                let command_at = i;
                let (name, next) = read_command(&chars, i);
                i = next;

                match name.as_str() {
                    "begin" | "end" => {
                        let Some((environment, after)) = read_braced_name(&chars, i) else {
                            return Err(indict_at(
                                &format!("malformed \\{name}: expected \\{name}{{environment}}"),
                                &chars,
                                command_at,
                            ));
                        };
                        i = after;

                        if environment.is_empty() {
                            return Err(indict_at(
                                &format!("malformed \\{name}{{}}: empty environment name"),
                                &chars,
                                command_at,
                            ));
                        }

                        if name == "begin" {
                            environments.push((environment, command_at));
                        } else {
                            match environments.pop() {
                                None => {
                                    return Err(indict_at(
                                        &format!(
                                            "\\end{{{environment}}} without a matching \
                                             \\begin{{{environment}}}"
                                        ),
                                        &chars,
                                        command_at,
                                    ));
                                }
                                Some((open, _)) if open != environment => {
                                    return Err(indict_at(
                                        &format!(
                                            "environment mismatch: \\begin{{{open}}} is closed by \
                                             \\end{{{environment}}}"
                                        ),
                                        &chars,
                                        command_at,
                                    ));
                                }
                                Some(_) => {}
                            }
                        }
                    }
                    "left" => {
                        if lefts == rights {
                            first_left_at = command_at;
                        }
                        lefts += 1;
                    }
                    "right" => rights += 1,
                    _ => {}
                }
            }
            '{' => {
                opened_at.push(i);
                i += 1;
            }
            '}' => {
                let Some(_) = opened_at.pop() else {
                    return Err(indict_at("unmatched '}'", &chars, i));
                };
                i += 1;
            }
            _ => i += 1,
        }
    }

    if let Some((name, at)) = environments.last() {
        return Err(indict_at(
            &format!("unclosed \\begin{{{name}}}"),
            &chars,
            *at,
        ));
    }

    if let Some(at) = opened_at.last() {
        return Err(indict_at("unclosed '{'", &chars, *at));
    }

    if lefts != rights {
        let at = if lefts > rights { first_left_at } else { 0 };
        return Err(indict_at(
            &format!("unbalanced \\left ({lefts}) / \\right ({rights})"),
            &chars,
            at,
        ));
    }

    Ok(())
}

/// Read the command name starting at `chars[at]` (which must be `\`).
///
/// Returns the name and the index just past it. A control *symbol* such as
/// `\{`, `\%`, `\,` or `\\` yields that one character as the name, which is why
/// `\{` is not counted as a group delimiter by [`check_structure`]. Reading the
/// full alphabetic run is what keeps `\leftarrow` from being mistaken for
/// `\left`.
fn read_command(chars: &[char], at: usize) -> (String, usize) {
    let after_backslash = at + 1;

    if after_backslash >= chars.len() {
        return ("\\".to_string(), chars.len());
    }

    if chars[after_backslash].is_ascii_alphabetic() {
        let mut end = after_backslash;
        while end < chars.len() && chars[end].is_ascii_alphabetic() {
            end += 1;
        }

        return (chars[after_backslash..end].iter().collect(), end);
    }

    (chars[after_backslash].to_string(), after_backslash + 1)
}

/// Read a `{name}` group, skipping whitespace before the braces.
///
/// Returns the name and the index just past the closing brace, or `None` when
/// the braces are missing or unterminated.
fn read_braced_name(chars: &[char], at: usize) -> Option<(String, usize)> {
    let mut i = at;
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }

    if chars.get(i) != Some(&'{') {
        return None;
    }

    i += 1;
    let start = i;
    while i < chars.len() && chars[i] != '}' {
        i += 1;
    }

    if i >= chars.len() {
        return None;
    }

    Some((chars[start..i].iter().collect(), i + 1))
}

// ---------------------------------------------------------------------------
// 3. Conversion
// ---------------------------------------------------------------------------

/// Run `tylax` behind a panic guard.
///
/// `latex_to_typst` cannot fail, and its parser is third-party code, so a panic
/// there would otherwise take down the whole render. A caught panic is reported
/// like any other failure.
fn convert_with_tylax(body: &str) -> Result<String, String> {
    match catch_unwind(AssertUnwindSafe(|| tylax::latex_to_typst(body))) {
        Ok(converted) => Ok(converted),
        Err(_) => Err(indict(
            "LaTeX converter panicked on this input",
            &body.chars().collect::<Vec<char>>(),
        )),
    }
}

// ---------------------------------------------------------------------------
// 4. Post-processing
// ---------------------------------------------------------------------------

/// Reduce a converter result to a bare math expression body.
///
/// This is where a document-shaped result is defused: `#math.equation(…)[…]`
/// wrappers are unwrapped, directive lines are dropped, and any remaining `$`
/// fences are removed. Whitespace is collapsed to single spaces (and trailing
/// line breaks dropped) so the body can be embedded in an inline equation.
fn finalize_math(converted: &str) -> Result<String, String> {
    let mut text = unwrap_equation_blocks(converted)?;

    if has_markup_directive(&text) {
        text = match body_between_dollars(&text) {
            Some(inner) => inner.to_string(),
            None => drop_directive_lines(&text),
        };
    }

    let text = strip_dollar_fences(&text);
    let text = trim_trailing_line_breaks(&collapse_whitespace(&text));
    let chars: Vec<char> = text.chars().collect();

    if text.is_empty() {
        return Err(indict(
            "LaTeX conversion produced no math expression",
            &converted.chars().collect::<Vec<char>>(),
        ));
    }

    if let Some((marker, at)) = find_markup_outside_math(&chars) {
        return Err(indict_at(
            &format!("converter emitted Typst markup `{marker}` outside math mode"),
            &chars,
            at,
        ));
    }

    if let Some(at) = find_placeholder(&chars) {
        return Err(indict_at(
            &format!("converter emitted the empty-argument placeholder `{EMPTY_ARG_PLACEHOLDER}`"),
            &chars,
            at,
        ));
    }

    Ok(text)
}

/// Replace every `#math.equation(…)[ body ]` wrapper with its `body`.
///
/// The scan is string-literal aware: a `mat(delim: "[", …)` argument (what
/// `bmatrix`/`vmatrix` convert to) must not be mistaken for the content block,
/// and a stray `]` inside a string must not close it early. Unterminated
/// wrappers are an error rather than silently truncated output.
fn unwrap_equation_blocks(converted: &str) -> Result<String, String> {
    const CALL: &str = "#math.equation(";

    let mut out = String::with_capacity(converted.len());
    let mut rest = converted;

    while let Some(start) = rest.find(CALL) {
        out.push_str(&rest[..start]);

        let after_call = &rest[start + CALL.len()..];
        let chars: Vec<char> = after_call.chars().collect();

        let Some(open) = find_content_open(&chars) else {
            return Err(indict("unterminated `#math.equation` call", &chars));
        };
        let Some(close) = find_matching_bracket(&chars, open) else {
            return Err(indict("unterminated `#math.equation` body", &chars));
        };

        push_spaced(&mut out, &chars[open + 1..close].iter().collect::<String>());

        rest = &after_call[byte_len(&chars[..close + 1])..];
    }

    push_spaced(&mut out, rest);

    Ok(out)
}

/// Index of the `[` that opens a call's content block: the first `[` seen after
/// the call's own parentheses closed, ignoring string literals.
fn find_content_open(chars: &[char]) -> Option<usize> {
    let mut parens = 1i32;
    let mut in_string = false;
    let mut i = 0usize;

    while i < chars.len() {
        let c = chars[i];

        if in_string {
            match c {
                '\\' => i += 2,
                '"' => {
                    in_string = false;
                    i += 1;
                }
                _ => i += 1,
            }
            continue;
        }

        match c {
            '"' => {
                in_string = true;
                i += 1;
            }
            '(' => {
                parens += 1;
                i += 1;
            }
            ')' => {
                parens -= 1;
                i += 1;
            }
            '[' if parens <= 0 => return Some(i),
            _ => i += 1,
        }
    }

    None
}

/// Index of the `]` closing the content block opened at `open`, ignoring string
/// literals and escaped brackets.
fn find_matching_bracket(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut in_string = false;
    let mut i = open + 1;

    while i < chars.len() {
        let c = chars[i];

        if in_string {
            match c {
                '\\' => i += 2,
                '"' => {
                    in_string = false;
                    i += 1;
                }
                _ => i += 1,
            }
            continue;
        }

        match c {
            '"' => {
                in_string = true;
                i += 1;
            }
            '\\' => i += 2,
            '[' => {
                depth += 1;
                i += 1;
            }
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }

    None
}

/// Append `text`, keeping a space between it and what is already there so two
/// unwrapped bodies can never fuse into one token.
fn push_spaced(out: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }

    let needs_space = out
        .chars()
        .next_back()
        .zip(text.chars().next())
        .is_some_and(|(previous, next)| !previous.is_whitespace() && !next.is_whitespace());

    if needs_space {
        out.push(' ');
    }

    out.push_str(text);
}

/// `true` when any line starts with a Typst markup statement.
fn has_markup_directive(text: &str) -> bool {
    text.lines()
        .any(|line| directive_in(line.trim_start()).is_some())
}

/// Which markup statement `line` starts with, if any. The character after the
/// keyword must not continue an identifier, so `#settings` is not `#set`.
fn directive_in(line: &str) -> Option<&'static str> {
    MARKUP_DIRECTIVES.iter().copied().find(|directive| {
        line.strip_prefix(directive).is_some_and(|rest| {
            !rest
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_')
        })
    })
}

/// Remove the lines that are markup statements, keeping the rest in order.
fn drop_directive_lines(text: &str) -> String {
    text.lines()
        .filter(|line| directive_in(line.trim_start()).is_none())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text between the first and last unescaped `$` of a document-shaped
/// result — i.e. the body of its equation.
fn body_between_dollars(text: &str) -> Option<&str> {
    let chars: Vec<char> = text.chars().collect();

    let first = find_unescaped(text, '$')?;
    let last = (0..chars.len())
        .rev()
        .find(|i| chars[*i] == '$' && !is_escaped(&chars, *i))?;

    if first >= last {
        return None;
    }

    Some(&text[byte_len(&chars[..first + 1])..byte_len(&chars[..last])])
}

/// Drop `$` fences (keeping `\$`, which is a literal dollar in Typst math).
fn strip_dollar_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();

    while let Some(c) = chars.next() {
        match c {
            '$' => {}
            '\\' => {
                out.push(c);
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
            }
            _ => out.push(c),
        }
    }

    out
}

/// Collapse every whitespace run to one space and trim both ends. A math
/// expression body is embedded inline, so a newline in it would otherwise
/// decide between an inline and a block equation.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;

    for c in text.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }

        if pending_space {
            out.push(' ');
            pending_space = false;
        }

        out.push(c);
    }

    out
}

/// Drop trailing Typst math line breaks, which a final ` \\` row separator
/// would otherwise leave dangling at the end of the body.
fn trim_trailing_line_breaks(text: &str) -> String {
    let mut body = text.trim_end();

    while let Some(stripped) = body.strip_suffix('\\') {
        body = stripped.trim_end();
    }

    body.to_string()
}

/// First `#set`-style statement left in the body, with its character position.
fn find_markup_outside_math(chars: &[char]) -> Option<(String, usize)> {
    let mut i = 0usize;

    while i < chars.len() {
        if chars[i] != '#' {
            i += 1;
            continue;
        }

        let rest: String = chars[i..].iter().collect();

        if let Some(directive) = directive_in(&rest) {
            return Some((directive.to_string(), i));
        }

        if rest.starts_with("#math.equation") {
            return Some(("#math.equation".to_string(), i));
        }

        i += 1;
    }

    None
}

/// First standalone `zws` placeholder, with its character position.
///
/// Standalone means not glued to an identifier and not inside brackets or
/// quotes: `frac(zws, zws)` and `zws/zws` are placeholders, while the text
/// group `#text[zws]` (from `\text{zws}`) is real content.
fn find_placeholder(chars: &[char]) -> Option<usize> {
    let needle: Vec<char> = EMPTY_ARG_PLACEHOLDER.chars().collect();
    let matches_here = |i: usize| chars[i..i + needle.len()] == needle[..];

    let mut i = 0usize;
    while i + needle.len() <= chars.len() {
        if matches_here(i) {
            let before = i.checked_sub(1).and_then(|j| chars.get(j));
            let after = chars.get(i + needle.len());

            if !before.is_some_and(is_identifier_or_bracket)
                && !after.is_some_and(is_identifier_or_bracket)
            {
                return Some(i);
            }
        }

        i += 1;
    }

    None
}

/// Characters that make an adjacent placeholder occurrence part of a larger
/// token rather than a substituted empty argument.
fn is_identifier_or_bracket(c: &char) -> bool {
    c.is_alphanumeric() || *c == '_' || *c == '[' || *c == ']' || *c == '"'
}

// ---------------------------------------------------------------------------
// Error messages
// ---------------------------------------------------------------------------

/// Reason plus the offending snippet of the whole input, in backticks.
fn indict(reason: &str, chars: &[char]) -> String {
    indict_at(reason, chars, 0)
}

/// Reason plus the snippet around `at` (a character offset), in backticks.
fn indict_at(reason: &str, chars: &[char], at: usize) -> String {
    let snippet = snippet_at(chars, at);

    if snippet.is_empty() {
        reason.to_string()
    } else {
        format!("{reason}: `{snippet}`")
    }
}

/// A single-line window of at most [`MAX_SNIPPET_CHARS`] characters around `at`.
fn snippet_at(chars: &[char], at: usize) -> String {
    if chars.is_empty() {
        return String::new();
    }

    let lead = MAX_SNIPPET_CHARS / 3;
    let end = (at.saturating_sub(lead) + MAX_SNIPPET_CHARS).min(chars.len());
    let start = end.saturating_sub(MAX_SNIPPET_CHARS);

    chars[start..end]
        .iter()
        .map(|c| if c.is_control() { ' ' } else { *c })
        .collect()
}

// ---------------------------------------------------------------------------
// Scanning helpers
// ---------------------------------------------------------------------------

/// Byte offset of `chars` when rendered as a `String`.
fn byte_len(chars: &[char]) -> usize {
    chars.iter().map(|c| c.len_utf8()).sum()
}

/// Character offset of the first unescaped `needle`.
fn find_unescaped(text: &str, needle: char) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();

    (0..chars.len()).find(|i| chars[*i] == needle && !is_escaped(&chars, *i))
}

/// `true` when the character at `at` is preceded by an odd number of
/// backslashes, i.e. `\$` rather than a bare `$`.
fn is_escaped(chars: &[char], at: usize) -> bool {
    let mut backslashes = 0usize;

    while let Some(previous) = at.checked_sub(backslashes + 1).and_then(|i| chars.get(i)) {
        if *previous != '\\' {
            break;
        }
        backslashes += 1;
    }

    backslashes % 2 == 1
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_fraction() -> Result<(), String> {
        let body = latex_to_typst(r"\frac{1}{2}")?;
        assert!(
            body.contains('/') || body.contains("frac"),
            "unexpected fraction body: {body}"
        );
        assert!(
            !body.contains('\\') && !body.contains('$'),
            "markup left in body: {body}"
        );

        let compound = latex_to_typst(r"\frac{a+b}{c-d}")?;
        assert!(
            compound.contains("frac") || compound.contains('/'),
            "unexpected fraction body: {compound}"
        );
        assert!(!compound.contains('$'), "fences left in body: {compound}");

        Ok(())
    }

    #[test]
    fn converts_sqrt() -> Result<(), String> {
        let body = latex_to_typst(r"\sqrt{2}")?;
        assert!(body.contains("sqrt"), "unexpected root body: {body}");
        assert!(body.contains('2'), "unexpected root body: {body}");
        assert!(!body.contains('\\'), "LaTeX left in body: {body}");

        let nested = latex_to_typst(r"\sqrt{x^2 + y^2}")?;
        assert!(nested.contains("sqrt"), "unexpected root body: {nested}");
        assert!(nested.contains('x') && nested.contains('y'), "{nested}");

        Ok(())
    }

    #[test]
    fn converts_pmatrix() -> Result<(), String> {
        let body = latex_to_typst(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}")?;

        assert!(body.contains("mat("), "matrix must become mat(...): {body}");
        assert!(body.contains(';'), "rows must stay separated: {body}");
        for cell in ["a", "b", "c", "d"] {
            assert!(body.contains(cell), "cell `{cell}` lost: {body}");
        }
        assert!(
            !body.contains("\\begin") && !body.contains("\\end"),
            "environment left in body: {body}"
        );
        assert!(!body.contains('$'), "fences left in body: {body}");

        Ok(())
    }

    #[test]
    fn converts_aligned_environment() -> Result<(), String> {
        let body = latex_to_typst(r"\begin{aligned} a &= b \\ c &= d \end{aligned}")?;

        assert!(body.contains('&'), "alignment markers lost: {body}");
        for cell in ["a", "b", "c", "d"] {
            assert!(body.contains(cell), "cell `{cell}` lost: {body}");
        }
        assert!(
            !body.contains("\\begin") && !body.contains("\\end"),
            "environment left in body: {body}"
        );
        assert!(!body.contains('$'), "fences left in body: {body}");

        Ok(())
    }

    #[test]
    fn converts_greek_letters() -> Result<(), String> {
        let body = latex_to_typst(r"\alpha + \beta")?;

        assert!(
            body.contains("alpha") || body.contains('α'),
            "alpha lost: {body}"
        );
        assert!(
            body.contains("beta") || body.contains('β'),
            "beta lost: {body}"
        );
        assert!(!body.contains('\\'), "LaTeX left in body: {body}");
        assert!(!body.contains('$'), "fences left in body: {body}");

        Ok(())
    }

    #[test]
    fn converts_sub_and_superscripts() -> Result<(), String> {
        let body = latex_to_typst(r"x_i^2 + y^{n+1}")?;

        for token in ['x', 'i', 'y', 'n', '2', '1'] {
            assert!(body.contains(token), "`{token}` lost: {body}");
        }
        assert!(
            body.contains('^') && body.contains('_'),
            "attachments lost: {body}"
        );
        assert!(!body.contains('$'), "fences left in body: {body}");

        Ok(())
    }

    #[test]
    fn converts_text_with_cjk() -> Result<(), String> {
        let body = latex_to_typst(r"\text{中文}")?;

        assert!(body.contains("中文"), "text content lost: {body}");
        assert!(body.contains("#text["), "unexpected text body: {body}");
        assert!(!body.contains("\\text"), "LaTeX left in body: {body}");
        assert!(!body.contains('$'), "fences left in body: {body}");

        Ok(())
    }

    #[test]
    fn rejects_empty_input() {
        for src in ["", " ", "\n\t ", "$ $", "$$", "$$$$"] {
            assert!(
                latex_to_typst(src).is_err(),
                "expected an error for {src:?}"
            );
        }
    }

    #[test]
    fn rejects_malformed_braces() -> Result<(), String> {
        let cases = [
            (r"\frac{1}{2", "unclosed"),
            (r"\frac{1}}{2}", "unmatched"),
            ("{", "unclosed"),
            ("}", "unmatched"),
            (r"\text{中文", "unclosed"),
        ];

        for (src, expected) in cases {
            match latex_to_typst(src) {
                Ok(body) => return Err(format!("expected an error for {src:?}, got `{body}`")),
                Err(reason) => assert!(
                    reason.contains(expected),
                    "reason {reason:?} does not mention {expected:?}"
                ),
            }
        }

        Ok(())
    }

    #[test]
    fn rejects_malformed_environments_and_delimiters() -> Result<(), String> {
        let cases = [
            (r"\begin{aligned} a & b", "unclosed"),
            (r"\end{aligned}", "without a matching"),
            (r"\begin{pmatrix} a \end{bmatrix}", "mismatch"),
            (r"\begin{}", "empty environment name"),
            (r"\left( x", "unbalanced"),
            (r"\right)", "unbalanced"),
        ];

        for (src, expected) in cases {
            match latex_to_typst(src) {
                Ok(body) => return Err(format!("expected an error for {src:?}, got `{body}`")),
                Err(reason) => assert!(
                    reason.contains(expected),
                    "reason {reason:?} does not mention {expected:?}"
                ),
            }
        }

        Ok(())
    }

    #[test]
    fn strips_fences_and_wrapper_environments() -> Result<(), String> {
        let bare = latex_to_typst(r"x^2 + y")?;

        assert_eq!(latex_to_typst("$$x^2 + y$$")?, bare);
        assert_eq!(latex_to_typst("$x^2 + y$")?, bare);
        assert_eq!(latex_to_typst(r"\[x^2 + y\]")?, bare);
        assert_eq!(
            latex_to_typst(r"\begin{equation} x^2 + y \end{equation}")?,
            bare
        );
        assert_eq!(latex_to_typst(r"   x^2 + y   ")?, bare);
        assert!(!bare.contains('$'));

        assert!(
            latex_to_typst("$x$ + $y$").is_err(),
            "two expressions in one call must be rejected"
        );

        Ok(())
    }

    #[test]
    fn accepts_balanced_delimiters_and_comments() {
        assert!(check_structure(r"\frac{1}{2}").is_ok());
        assert!(check_structure(r"\left\{ x \right\}").is_ok());
        assert!(check_structure(r"\leftarrow x \rightarrow y").is_ok());
        assert!(check_structure(r"\begin{array}{ll} a & b \end{array}").is_ok());
        // A `%` comment hides the rest of the line, braces included.
        assert!(check_structure("x % }\n + y").is_ok());
    }

    #[test]
    fn extracts_math_body_from_document_shaped_output() -> Result<(), String> {
        assert_eq!(
            finalize_math("#math.equation(block: true, numbering: none)[\n$ x + y $\n]")?,
            "x + y"
        );
        assert_eq!(
            finalize_math("#set math.equation(numbering: none)\n$ a = b $")?,
            "a = b"
        );
        assert_eq!(
            finalize_math("#let f(x) = x\n#set math.equation(numbering: none)\n$ f(1) = 1 $")?,
            "f(1) = 1"
        );
        // A `[` inside a string argument must not open the content block early,
        // and a string `]` must not close it.
        assert_eq!(
            finalize_math("#math.equation(block: true)[ mat(delim: \"[\", a, b ; c, d) ]")?,
            "mat(delim: \"[\", a, b ; c, d)"
        );

        Ok(())
    }

    #[test]
    fn document_wrappers_without_math_are_rejected() {
        assert!(finalize_math("#set math.equation(numbering: none)\n#let a = 1").is_err());
        assert!(finalize_math("#math.equation(block: true, numbering: none)[$ x $").is_err());
    }

    #[test]
    fn rejects_placeholder_from_empty_arguments() -> Result<(), String> {
        match latex_to_typst(r"\frac{}{}") {
            Ok(body) => Err(format!("expected an error, got `{body}`")),
            Err(reason) => {
                assert!(
                    reason.contains("placeholder"),
                    "unexpected reason: {reason}"
                );
                Ok(())
            }
        }
    }

    #[test]
    fn text_groups_are_not_mistaken_for_placeholders() -> Result<(), String> {
        assert_eq!(finalize_math("frac(a, #text[zws])")?, "frac(a, #text[zws])");
        assert!(finalize_math("zws/zws").is_err());
        assert!(finalize_math("frac(zws, 1)").is_err());

        // The real converter path: an empty mandatory argument becomes `zws`.
        match latex_to_typst(r"\sqrt{}") {
            Ok(body) => Err(format!("expected an error, got `{body}`")),
            Err(reason) => {
                assert!(
                    reason.contains("placeholder"),
                    "unexpected reason: {reason}"
                );
                Ok(())
            }
        }
    }

    #[test]
    fn errors_quote_a_bounded_snippet() -> Result<(), String> {
        let long = format!(r"\frac{{1}}{{2{}", "x".repeat(200));

        match latex_to_typst(&long) {
            Ok(body) => Err(format!("expected an error, got `{body}`")),
            Err(reason) => {
                assert!(reason.contains("unclosed"), "unexpected reason: {reason}");
                assert_eq!(
                    reason.lines().count(),
                    1,
                    "reason must be one line: {reason}"
                );

                let quoted = reason.split('`').nth(1).unwrap_or_default();
                assert!(!quoted.is_empty(), "no snippet quoted: {reason}");
                assert!(
                    quoted.chars().count() <= MAX_SNIPPET_CHARS,
                    "snippet longer than {MAX_SNIPPET_CHARS} chars: {quoted}"
                );

                Ok(())
            }
        }
    }

    #[test]
    fn short_errors_quote_the_offending_expression() -> Result<(), String> {
        match latex_to_typst(r"\frac{1}{2") {
            Ok(body) => Err(format!("expected an error, got `{body}`")),
            Err(reason) => {
                assert!(
                    reason.contains(r"\frac{1}{2"),
                    "offending snippet missing from reason: {reason}"
                );
                Ok(())
            }
        }
    }

    #[test]
    fn fence_and_whitespace_helpers() {
        assert_eq!(strip_dollar_fences("$a$"), "a");
        assert_eq!(strip_dollar_fences(r"\$"), r"\$");
        assert_eq!(strip_dollar_fences("$a$ + b"), "a + b");
        assert_eq!(collapse_whitespace(" a \n\t b  c "), "a b c");
        assert_eq!(collapse_whitespace("   "), "");
        assert_eq!(trim_trailing_line_breaks(r"a & b \ "), "a & b");
    }

    #[test]
    fn pathological_inputs_never_panic() {
        let cases = [
            "",
            " ",
            "$",
            "$$",
            "$$$",
            "\\",
            "\\\\",
            "{",
            "}",
            "{{}}",
            "{{{}}}",
            "\\frac",
            "\\frac{1}{2",
            "\\frac{1}}{2}",
            "\\frac{}{}",
            "\\sqrt",
            "\\begin",
            "\\begin{",
            "\\end",
            "\\end{x}",
            "\\begin{aligned}",
            "\\end{aligned}",
            "\\begin{aligned} a & b",
            "\\begin{equation}",
            "\\begin{equation} a",
            "\\begin{pmatrix} a & b",
            "\\begin{pmatrix}",
            "\\begin{array}{ccc} a",
            "\\begin{cases} x & y",
            "\\left(",
            "\\right)",
            "\\left\\{",
            "\\left.",
            "\\right.",
            "\\leftarrow x",
            "\\text{",
            "\\text{}",
            "\\text{中文",
            "\\frac{中文}{2",
            "%",
            "%}",
            "\\%",
            "\\#",
            "&",
            "#",
            "#set",
            "#math.equation",
            "#math.equation(",
            "#math.equation(block: true)[",
            "x^2 + y^{",
            "\\hbox{a",
            "\\verb|a{b|",
            "\\color{red}{x",
            "\\mathchoice{}{}{}{",
            "\\def\\x{1}",
            "🙂_{x}",
            "\\begin{verbatim}{",
            "0",
            "=",
            "\\2000",
        ];

        for src in cases {
            let _ = latex_to_typst(src);
        }
    }
}
