# Tables

GFM tables: alignment, inline formatting inside cells, escaped pipes and CJK
cells.

| Left | Center | Right |
|:-----|:------:|------:|
| `a` | *b* | 3 |
| a longer cell | **bold** | 42.5 |
| escaped \| pipe | `code | span` | 0 |

A table does not need leading and trailing pipes, and its rows may be ragged:

Head one | Head two | Head three
--- | --- | ---
1 | 2 | 3
only one cell

| 语言 | 值 |
|------|----|
| 中文 | 一 |
| 日本語 | 二 |
| 한국어 | 셋 |

Empty cells keep their column:

| Col A | Col B | Col C |
|-------|-------|-------|
| x |  | z |
|  | y |  |
| a | b | c |

A table inside a quote is still a table:

> | Q | A |
> |---|---|
> | 1 | one |
