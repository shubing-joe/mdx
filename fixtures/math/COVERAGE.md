# Math conversion coverage (`fixtures/math`)

What the `mdx` pipeline can do with a LaTeX formula, per construct, and what it
does when it cannot. The corpus is the evidence: every construct listed as
supported has a fixture in this directory, and both backends are asserted
against the whole corpus.

```
cargo test -p mdx-math
```

* `crates/mdx-math/src/svg.rs` — RaTeX: `latex_to_svg` renders each of the 85
  valid fixtures and rejects each of the 10 malformed ones.
* `crates/mdx-math/tests/coverage.rs` — Typst: `latex_to_typst` converts the same
  85 and rejects the same 10, with the counts printed to stderr.

## Two backends, one corpus

| Backend | Entry point | Role |
| --- | --- | --- |
| Typst (primary) | `mdx_math::latex_to_typst` | converts LaTeX to a Typst math *expression body* — the text that belongs between `$…$` in a Typst equation, which is what the writers receive |
| RaTeX (fallback) | `mdx_math::latex_to_svg` | renders LaTeX straight to a standalone SVG, used when the Typst path cannot express a formula |

For a document, `mdx-cli` tries the Typst path first and, on failure, asks
RaTeX whether the formula is drawable at all (`crates/mdx-cli/src/pipeline.rs`,
`convert_math`):

* Typst conversion succeeded → the body goes into the document.
* Typst failed, RaTeX rendered → an `E_MATH_CONVERT` warning, and the Typst
  writer keeps the original LaTeX source instead of a converted body.
* Both failed → an `E_MATH_CONVERT` warning naming both reasons.

Both paths are total on malformed input: an `Err` with a one-line reason, never
a panic. A formula that converts but whose body the Typst engine cannot
evaluate is not a build failure either — the writer falls back to the source
LaTeX for that equation and the document still renders, so the symptom of a
conversion gap is a formula that looks like text on the page rather than an
error. The [known gaps](#known-gaps) table lists each one with what it looks
like.

## Supported constructs

Every fixture in this table converts with `latex_to_typst`; the "body" column is
the observed conversion of the fixture next to it, which is what a writer
receives.

| Construct | Fixtures | Example input → body |
| --- | --- | --- |
| Fractions, nested fractions | `001`–`004`, `084` | `\frac{1}{1+\frac{1}{x}}` → `frac(1, 1 + 1/x)` |
| Binomials | `005` | `\binom{n}{k}` → `binom(n, k)` |
| Roots, root index | `006`–`008` | `\sqrt[3]{x}` → `root(3, x)` |
| Superscripts, subscripts, both | `009`–`011`, `045` | `sin^2(x)` → `sin^(2)(x)` |
| Big operators with limits | `012`–`015`, `021` | `\sum_{i=0}^{n}` → `sum_(i = 0)^(n)` |
| Integrals (single, multiple, contour) | `017`–`020` | `\iint_D f(x,y) \, dA` → `integral.double_(D) f (x,y) thin d A` |
| Limits, partials, `\nabla` | `016`, `022` | `\frac{\partial f}{\partial x}` → `frac(partial f, partial x)` |
| Matrices, all delimiter flavours (`matrix`, `pmatrix`, `bmatrix`, `Bmatrix`, `vmatrix`, `Vmatrix`, `smallmatrix`) | `023`–`031`, `056`–`065` | `\begin{pmatrix} a & b \\ c & d \end{pmatrix}` → `mat(delim: "(", a, b ; c, d)` |
| `\left…\right` around a matrix | `062` | `\left(\begin{matrix} a & b \\ c & d \end{matrix}\right)` → `mat(delim: "(", a, b ; c, d)` |
| Matrix with a script on it | `058`, `063` | `\begin{bmatrix} a & b \\ c & d \end{bmatrix}^{-1}` → `mat(delim: "[", a, b ; c, d)^(- 1)` |
| Empty matrix cell | `\begin{matrix} a & & b \end{matrix}` | `mat(delim: #none, a,, b)` — accepted: only a *first* or *last* empty argument is treated as a missing argument |
| `cases` (and `rcases` input) | `032`–`034`, `066`–`068` | `\begin{cases} x & \text{if } x > 0 \\ -x & \text{otherwise} \end{cases}` → `cases(x & #text[if] x > 0, - x & #text[otherwise])` |
| `array` with a column spec | `069`–`071` | `\begin{array}{lcr} a & b & c \\ 1 & 2 & 3 \end{array}` → `mat(delim: #none, a, b, c ; 1, 2, 3)` |
| `aligned`, `gathered`, `\tag` | `035`–`037` | `\begin{aligned} x &= 1 \\ y &= 2 \end{aligned}` → `x & = 1 \\ y & = 2` |
| `alignedat` | `085` | see [known gaps](#known-gaps) — it converts, with leaked environment markers |
| `\substack` | `072`–`074` | `\sum_{\substack{i=1 \\ j=1}}^{n} a_{ij}` → `sum_(i = 1 \\ j = 1)^(n) a_(i j)` (see gaps) |
| `\overset`, `\underset`, `\stackrel` | `075`–`078` | `\underset{x \in \mathbb{R}}{\operatorname{argmin}} f(x)` → `limits(op("argmin"))_(x in RR) f (x)` |
| `\overset{\text{def}}{=}` | `075` | → `eq.def` |
| Blackboard bold, calligraphic, `\operatorname` | `041`, `079`–`083` | `\mathbb{N} \subset \mathbb{R}` → `NN subset RR`; `\mathcal{L}\{f(t)\}` → `cal(L) {f (t)}` |
| `\text`, `\textbf`, `\textit`, `\texttt` | `038`–`040` | `\text{hello}` → `#text[hello]`; `\textbf{a}` → `*a*` |
| CJK inside `\text` | `042`–`045` | `x_{\text{中文}}` → `x_(#text[中文])` |
| Accents (`\hat`, `\vec`, `\bar`, `\tilde`, `\dot`, `\ddot`, `\breve`, `\check`, `\widehat`) | `046`–`048` | `\widehat{ABC}` → `hat(A B C)` |
| Lines and braces over/under a group | `049`, `050` | `\overbrace{a+b}^{n}` → `overbrace(a + b, n)` |
| Stretchy arrows | `051` | `\overrightarrow{AB}` → `arrow(A B)` |
| Delimiters (`\left…\right`, `\bigl`, `\langle`, set braces, `\lVert`) | `052`–`055` | `\left( x \right)` → `(x)` (see gaps for `\middle` and sizing) |
| Spacing (`\quad`, `\,`) and relations | `039`, `017` | `x = y \quad \text{if}` → `x = y quad #text[if]` |
| Greek letters, symbols, `\infty`, `\partial`, `\nabla` | `010`, `079` | `\mathbb{R}^n \to \mathbb{R}^m` → `RR^(n) -> RR^(m)` |

Constructs were checked one formula at a time; a formula that needs a construct
from two rows at once (a fraction inside a matrix cell, a `cases` branch inside
an `aligned`) is covered by the fixtures in `056`–`085`.

## What the Typst path rejects

The 10 `9NN-bad-*.tex` fixtures must fail: an undefined control sequence or a
structurally broken formula is not something a writer should receive as a body.
The Typst path rejects:

| Fixture | Input | Detected as |
| --- | --- | --- |
| `901` | `\frac{a}` | `empty argument`: the emitted call has no second argument |
| `902` | `x^2^3` | `double superscript` |
| `903` | `\nonexistentcommand{x}` | `undefined control sequence`: the name is copied into the body |
| `904` | `\right)` | `unbalanced \left / \right` |
| `905` | `\left( x` | `unbalanced \left / \right` |
| `906` | `\sqrt{x` | `unclosed '{'` |
| `907` | `\begin{cases} a \\ b` | `unclosed \begin{cases}` |
| `908` | `\begin{pmatrix} a \end{bmatrix}` | `environment mismatch` |
| `909` | `x_1_2` | `double subscript` |
| `910` | `\frac{a}{b}}` | `unmatched '}'` |

The structural checks run **before** the converter, so a broken formula never
reaches third-party parsing code; the empty-argument and copied-name checks run
on the emitted body, which is the only place where a silently wrong conversion
can still be caught.

Rejection is deliberately conservative — it costs a fallback, not a wrong
document. Two of the three body checks cannot carry a command inventory (the
converter has no public one), so they key on what can be observed: an empty term
in the emitted call, and a control word of the input surviving in the body fused
with what follows it. See [known gaps](#known-gaps) for what that leaves
undetected.

## RaTeX fallback boundary

`latex_to_svg` renders formulas the Typst path cannot express, so a construct
the Typst path rejects is not necessarily unusable: `mdx render` of a document
containing `x^2^3` warns once with `E_MATH_CONVERT` and
(`%PDF` output) still builds, because the writer keeps the LaTeX source. The
fallback's own contract and failure modes are documented in
`crates/mdx-math/src/svg.rs`.

## Known gaps

Each gap below was reproduced against the corpus: the *body* column is what
`latex_to_typst` returns (the crate's contract, asserted by the corpus test),
and the *observed* column is what `mdx render` writes for a document containing
that fixture's formula. Nothing here is a compile failure — a body the Typst
engine cannot evaluate is rendered as the original LaTeX text, which is why a
document with a gap in it still builds and looks wrong in exactly the place the
gap names.

| Gap | Fixture | Body | Observed |
| --- | --- | --- | --- |
| `\nolimits` is copied through as an identifier call | `013` | `nolimits(sum)_(i = 0)^(n)` — Typst has no `nolimits` | the PDF shows the LaTeX source text, not a sum. `\limits` (`012`) is fine: `limits(sum)_(i = 0)^(n)` renders ∑ with its limits. Recommended fix: drop the wrapper (`nolimits(X)` → `X`), since Typst's inline math has no limits to begin with. |
| `\middle` is dropped | `053` | `(x/yz)` — the `\|` is gone, and Typst reads `yz` as one identifier | the PDF shows the LaTeX source text. |
| `\hline` inside `array`/`matrix` leaks a converter sentinel | none (probed) | `mat(delim: #none, a, b ; \|\|\|HLINE\|\|\| c, d)` | the PDF shows the LaTeX source text. |
| Delimiter-sizing commands are dropped | `054` (`\bigl\langle`, `\bigr\rangle`), `\big`, `\Big` | `x/n` — the angle brackets disappear instead of growing | the PDF shows the fraction without its brackets. `\left…\right` (`052`, `062`) is unaffected. |
| Unknown environments are passed through with marker comments | `085` (`alignedat`) | `/* Begin alignedat */ x &= 1 & quad y &= 2 \\ z &= 3 & quad w &= 4 /* End alignedat */` | the four results render on one line: Typst has no alignment columns outside `mat`/`cases`, and the `alignedat` `{2}` argument is dropped. `aligned` (`035`) converts properly. |
| Marker comments and row separators leak with `gathered` | `037` | `/* Begin gathered */ a \\ b \\ c /* End gathered */` | renders as three lines — `\\` is Typst's line-break operator, so this one is only untidy in the body, not wrong on the page. |
| An unknown environment name is accepted, not rejected | none (probed) | `\begin{foobar} a + b \end{foobar}` → `/* Begin foobar */ a + b /* End foobar */` | renders the content and drops the (invisible) markers; RaTeX rejects the same input, so the two backends disagree about it and a document using it reports no diagnostic. |
| `rcases` loses its mirrored brace | `034` | `cases(a, b)` | renders as a left-braced `cases` block. |
| `\substack` rows are not stacked | `072`–`074` | `sum_(i = 1 \\ j = 1)^(n) a_(i j)` | the two rows appear, joined by Typst's line break rather than a stack, with Typst's own row spacing. |
| Empty first/last call argument is rejected | none (probed) | `\limits` alone → `limits()`, rejected as `empty argument` | a bare `\limits` is not a formula, and no fixture uses one. An empty *middle* term is still accepted (`a & & b` → `mat(delim: #none, a,, b)`). |
| Copied-command detection is name-length limited | none (probed) | `\foo + x` → `foo+ x` (accepted; `\foo{x}` is rejected) | a typo in a command of fewer than four characters, or in one without a braced argument, reaches the page as an identifier instead of failing. |

## Corpus layout

* `001`–`085` — valid fixtures (`NNN-name.tex`). Each one must convert with
  `latex_to_typst` **and** render with `latex_to_svg`.
* `901`–`910` — malformed fixtures (`9NN-bad-name.tex`). Each one must fail
  cleanly on both paths.
* One formula per file, UTF-8, LF-terminated, no `$`/`$$` fences: the corpus is
  the unfenced form `mdx-parse` hands to the math crate.

Filenames are part of the contract (the counts and the `9NN-bad-` marker are
asserted by both tests), so a fixture's *contents* may be extended but its name
must not change. Adding a fixture means increasing `EXPECTED_VALID` in
`crates/mdx-math/tests/coverage.rs`, `checked` in `crates/mdx-math/src/svg.rs`,
and the tables above.
