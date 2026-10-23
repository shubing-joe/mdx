# Basic blocks

A deliberately boring document that walks every core block of the model through
the whole pipeline. `tests/smoke.rs` renders this file, so it stays free of
anything that needs the network or a font the toolchain does not ship.

## Inline formatting

A paragraph with *emphasis*, **strong emphasis**, ***both at once***,
~~struck through text~~ and `inline code`. A [reference link][spec] and an
autolink <https://typst.app> resolve without any asset work, and a bare URL
https://commonmark.org stays text. Escapes survive too: \*not emphasis\*.

Two spaces at the end of this line  
force a hard break.

[spec]: https://commonmark.org/ "CommonMark specification"

## Lists

- first bullet
- second bullet
  - nested bullet
  - another nested bullet
- third bullet

1. first step
2. second step
3. third step

## Block quote

> A quote keeps its own block structure.
>
> - and can hold a list
>
> > Quotes nest.

## Code

```rust
fn main() {
    let squares: Vec<u32> = (1..4).map(|n| n * n).collect();
    println!("{squares:?}");
}
```

## Repeated headings

### Setup

The first section under this heading.

### Setup

The second one: the parser must deduplicate the id to `setup-1`.

---

Done.
