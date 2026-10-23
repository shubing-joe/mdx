---
title: Front matter fixture
author: mdx maintainers
date: "2026-01-01"
tags:
  - fixture
  - yaml
draft: false
lang: en
reviewers:
  - alice
  - bob
nested:
  key: value
  numbers: [1, 2, 3]
---

# Front matter

The YAML block above is not Markdown: it becomes `Doc::front`, its known keys
(`title`, `author`, `date`) are typed, and every other key survives verbatim in
`front.extra` for the templates that read it.

`title` reaches the PDF metadata and the HTML `<title>`; the body starts here,
with a paragraph that follows a blank line.

## After the front matter

- the body is parsed exactly like a document without front matter
- a second `---` line far below is a thematic break, not front matter

A stray `---` in the middle of a document is just a rule:

---

That was a thematic break.
