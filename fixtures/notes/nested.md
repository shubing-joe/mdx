# Nested structures

Containers nest, and the blocks inside them must come out in the same shape they
went in.

## Quote containing a list and code

> A blockquote holds more than a paragraph:
>
> - first item
>   - nested item
>     1. ordered inside unordered
>     2. second ordered
>
> ```rust
> let nested = true;
> ```
>
> > A quote inside a quote.

## List containing quotes and code

1. an ordered item with a quote:
   > quoted inside a list item
   >
   > - and a list inside that quote
2. an ordered item with a fenced block:

   ```text
   plain text inside a list item
   ```

- a bullet with a nested list
  - deeper
    - deepest
      - deeper still

## Everything at once

> - a list in a quote
>   - and a nested list
>
> ```text
> code in the quote, after the list
> ```

## Raw HTML

<div class="fixture">

An HTML block: the parser keeps the tag as raw HTML, and the paragraph between
the tags is still parsed as Markdown.

</div>

Inline HTML such as <kbd>Ctrl</kbd>+<kbd>C</kbd> stays inline, and a
self-closing tag <br/> is inline too.
