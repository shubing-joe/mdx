# Adversarial input

Markdown that fights the pipeline and must still leave a readable document: raw
HTML blocks, inline HTML, quotes and lists nested deep enough to matter, escapes
that collide with the markup syntax, and text that starts a line with a
character the Typst writer would otherwise read as markup.

## Raw HTML blocks

<div class="wrapper">

Blank lines around the tags keep the middle a Markdown paragraph: the `<div>`
line itself is an HTML block, and so is the closing `</div>` line.

</div>

<table>
  <tr><th>html</th><td>table</td></tr>
</table>

<script>const not = "javascript";</script>

<details>
<summary>Details summary</summary>
</details>

An HTML block that never closes still ends at the blank line below.

<div>

## Inline HTML

Inline HTML is dropped and reported as `W_UNSUPPORTED_HTML`:
<kbd>Ctrl</kbd>+<kbd>C</kbd>, a self-closing <br/> break, a tag with attributes
<span style="color: red">spanning text</span>, and an entity written as
&amp;amp; which must survive as the literal text `&amp;`.

## Nested quotes and lists

> level one
>
> > level two
> >
> > > level three
> >
> > - back to a list inside the second quote
> >   1. ordered item inside a bullet
> >      - and one level deeper
> >        1. deepest ordered item

1. ordered item with a quote inside it:
   > quoted inside an ordered item
   >
   > - a list inside that quote
   >   1. ordered inside the nested list
2. ordered item with a fenced block:

   ```rust
   let depth = 4;
   if depth > 3 { /* nested */ }
   ```

- bullet with an indented code block:

      indented code, never a block quote

- bullet with a table:

  | a | b |
  |---|---|
  | 1 | `x\|y` |

## Escapes that collide with markup

Escaped punctuation must render as itself: \*not emphasis\*, \_not emphasis\_,
\`not code\`, \[not a link\], \#not a heading, \$not math\$, \\a backslash,
\<not a tag\>, and \-\- two hyphens.

A text run may also begin a line with a character that means markup in Typst or
in Markdown; both have to be escaped before it reaches the renderer:

= an equals sign at the start of a line
/not a term
\+ not a bullet
\- not a list marker
1\. not an enumeration

Prose around the dangerous characters: a < b > c, an array index a[i], a lone
dollar sign $ stays text, and a backslash at the end of a line is literal too.

## Unicode noise

Emoji with no glyph in the shipped fonts: 🚀 🧪 ✅ 🇯🇵 👨‍👩‍👧‍👦, a
right-to-left mark &rlm; between two Latin words, and a zero-width space:
a&#8203;b.

## Everything at once

> - a list in a quote with another quote inside it
>   > quoted once more, with **strong** and `code`
>
> ```text
> code in the quote, after the list
> ```

Text after the block: **bold**, *italic*, ~~struck through~~, `code`, and a
[link](https://commonmark.org) that resolves without any asset work.

<!-- an HTML comment block -->

---

Thematic break above, and a final paragraph with a hard break at the end.  
After the hard break.
