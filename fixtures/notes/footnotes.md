# Footnotes

A footnote reference[^alpha] sits inline, and one paragraph can carry
two[^beta] or repeat a reference[^alpha] without duplicating the note.

[^alpha]: The first definition, simple and short.

[^beta]: The second definition, with a continuation paragraph.

    The continuation is indented four spaces, so it belongs to the definition
    rather than to the document body.

    - a list inside the definition
    - a second item

Footnotes may also be defined with the label on its own line:

[^gamma]:
    A definition written with a continuation line, plus *inline formatting* and
    `code`.

    | col | value |
    |-----|-------|
    | a | 1 |

References can come before the definition[^gamma] and inside other blocks:

> A quote with a citation[^alpha].

- a list item with a citation[^beta]

Footnotes are ordinary blocks once defined: `mdx ast` reports each definition as
a `footnote_def` block carrying its own nested blocks.
