# Math fixtures (`fixtures/math`)

Corpus for both math backends: `mdx_math::latex_to_typst`
(`crates/mdx-math/src/typst.rs`, the pipeline's primary path) and
`mdx_math::latex_to_svg` (`crates/mdx-math/src/svg.rs`, the RaTeX fallback). One
LaTeX formula per file, UTF-8, LF-terminated, no `$`/`$$` fences (both functions
strip fences, but the corpus is the unfenced form the parser hands over).
`COVERAGE.md` records which constructs each path supports, which fall back to
RaTeX, and the known gaps.

* **85 valid fixtures** — `NNN-name.tex`, `NNN` = `001`…`085`. Each one must
  convert: `latex_to_typst(contents)` returns `Ok(body)` with a Typst math
  expression body, and each one must render: `latex_to_svg(contents)` returns
  `Ok(svg)` with `svg` starting with `<svg `.
* **10 deliberately malformed fixtures** — `9nn-bad-*.tex`, `9nn` = `901`…`910`.
  Each one must fail on both paths: `Err` with a reason, never a panic, never a
  partial document.
* Total: 95 fixtures, one formula each. Two tests walk this directory and assert
  the counts: `crates/mdx-math/src/svg.rs`
  (`fixture_corpus_renders_and_rejects_as_documented`, RaTeX: 85 renders and 10
  rejections) and `crates/mdx-math/tests/coverage.rs`
  (`corpus_converts_or_rejects_as_documented`, Typst: the same split, plus a
  naming check that the numbering has no gap or repeat).

The corpus deliberately exercises the constructs the milestone pipeline has to
survive: fractions and binomials, roots, sums/products/limits, integrals,
matrices in every delimiter flavour, `array`, `cases`, `aligned`/`alignedat`,
`\substack`, `\overset`/`\underset`, `\mathbb`/`\mathcal`/`\operatorname`,
`\text`, accents, delimiter sizing, and CJK text (which the bundled KaTeX fonts
do not cover, so layout falls back to placeholder metrics and the host's Unicode
font chain).

## Valid fixtures

| File | Coverage | Formula |
| --- | --- | --- |
| `001-fraction-basic.tex` | fractions | `\frac{a}{b}` |
| `002-fraction-nested.tex` | fractions, nesting | `\frac{1}{1+\frac{1}{x}}` |
| `003-fraction-quadratic.tex` | fractions, roots, operators | `\frac{-b \pm \sqrt{b^2-4ac}}{2a}` |
| `004-fraction-display-style.tex` | fractions, size command | `\dfrac{a}{b}` |
| `005-binomial-coefficient.tex` | binomials | `\binom{n}{k}` |
| `006-root-square.tex` | roots | `\sqrt{x}` |
| `007-root-cube.tex` | roots, optional index | `\sqrt[3]{x}` |
| `008-root-of-fraction.tex` | roots, fractions | `\sqrt{\frac{a}{b}}` |
| `009-pythagorean-theorem.tex` | superscripts, relations | `a^2 + b^2 = c^2` |
| `010-euler-identity.tex` | superscripts, Greek, constants | `e^{i\pi} + 1 = 0` |
| `011-trigonometric-identity.tex` | function names, powers | `\sin^2(x) + \cos^2(x) = 1` |
| `012-sum-with-limits.tex` | big operator, `\limits` | `\sum\limits_{i=0}^{n}` |
| `013-sum-with-nolimits.tex` | big operator, `\nolimits` | `\sum\nolimits_{i=0}^{n}` |
| `014-geometric-series.tex` | sums, fractions, `\infty` | `\sum_{n=0}^{\infty} \frac{1}{2^n}` |
| `015-product-range.tex` | big operator | `\prod_{i=1}^{n} i` |
| `016-limit-sinc.tex` | limits, fractions | `\lim_{x \to 0} \frac{\sin x}{x}` |
| `017-definite-integral.tex` | integrals, thin space | `\int_0^1 x^2 \, dx` |
| `018-gaussian-integral.tex` | integrals, improper bounds | `\int_{-\infty}^{\infty} e^{-x^2} \, dx` |
| `019-double-integral.tex` | multi-integral, subscripts | `\iint_D f(x,y) \, dA` |
| `020-contour-integral.tex` | contour integral, accents, `\cdot` | `\oint_C \vec{F} \cdot d\vec{r}` |
| `021-union-of-sets.tex` | big operator over sets | `\bigcup_{i=1}^{n} A_i` |
| `022-partial-derivatives.tex` | fractions, `\partial`, `\nabla` | `\frac{\partial f}{\partial x} + \nabla g` |
| `023-matrix-two-rows.tex` | `matrix` | `\begin{matrix} a & b \\ c & d \end{matrix}` |
| `024-pmatrix-two-by-two.tex` | `pmatrix` | `\begin{pmatrix} a & b \\ c & d \end{pmatrix}` |
| `025-pmatrix-fractions.tex` | `pmatrix`, fractions in cells | `\begin{pmatrix} \frac{1}{2} & 0 \\ 0 & \frac{3}{4} \end{pmatrix}` |
| `026-bmatrix-integers.tex` | `bmatrix` | `\begin{bmatrix} 1 & 2 \\ 3 & 4 \end{bmatrix}` |
| `027-bmatrix-expressions.tex` | `bmatrix`, mixed inline math | `\begin{bmatrix} a+b & c^2 \\ \sqrt{d} & e_i \end{bmatrix}` |
| `028-vmatrix-determinant.tex` | `vmatrix` (determinant bars) | `\begin{vmatrix} a & b \\ c & d \end{vmatrix}` |
| `029-Vmatrix-curly.tex` | `Vmatrix` (curly braces) | `\begin{Vmatrix} a & b \\ c & d \end{Vmatrix}` |
| `030-Bmatrix-curly.tex` | `Bmatrix`, single column | `\begin{Bmatrix} a \\ b \end{Bmatrix}` |
| `031-smallmatrix.tex` | `smallmatrix` (script size) | `\begin{smallmatrix} a & b \\ c & d \end{smallmatrix}` |
| `032-cases-two-branches.tex` | `cases` | `\begin{cases} a \\ b \end{cases}` |
| `033-cases-piecewise-absolute.tex` | `cases` + `\text` + alignment | `\begin{cases} x & \text{if } x > 0 \\ -x & \text{otherwise} \end{cases}` |
| `034-rcases-hook.tex` | `rcases` | `\begin{rcases} a \\ b \end{rcases}` |
| `035-aligned-system.tex` | `aligned`, `&` alignment | `\begin{aligned} x &= 1 \\ y &= 2 \end{aligned}` |
| `036-aligned-with-tag.tex` | `aligned` + `\tag` | `\begin{aligned} x &= 1 \tag{1} \\ y &= 2 \end{aligned}` |
| `037-gathered-lines.tex` | `gathered`, 3 rows | `\begin{gathered} a \\ b \\ c \end{gathered}` |
| `038-text-plain.tex` | `\text` | `\text{hello}` |
| `039-text-in-expression.tex` | `\text` words, `\quad` spacing | `x = y \quad \text{if and only if} \quad z` |
| `040-text-weights.tex` | `\textbf`, `\textit`, `\texttt` | `\textbf{a} + \textit{b} + \texttt{c}` |
| `041-operatorname.tex` | `\operatorname` | `\operatorname{sgn}(x)` |
| `042-text-cjk-chinese.tex` | CJK inside `\text` (Simplified) | `\text{中文测试}` |
| `043-text-cjk-japanese.tex` | CJK inside `\text` (Japanese, kana+kanji) | `\text{日本語のテキスト}` |
| `044-text-cjk-korean.tex` | CJK inside `\text` (Hangul) | `\text{한국어 텍스트}` |
| `045-text-cjk-subscript.tex` | CJK inside a subscript `\text` | `x_{\text{中文}}` |
| `046-accent-widehat.tex` | wide accent over a word | `\widehat{ABC}` |
| `047-accent-mixed.tex` | `\hat`, `\vec`, `\bar`, `\tilde` | `\hat{x} + \vec{v} + \bar{y} + \tilde{n}` |
| `048-accent-dots.tex` | `\dot`, `\ddot`, `\breve`, `\check` | `\dot{x} + \ddot{x} + \breve{x} + \check{x}` |
| `049-accent-lines.tex` | `\overline`, `\underline` | `\overline{x} + \underline{x}` |
| `050-accent-braces.tex` | `\overbrace`, `\underbrace` with limits | `\overbrace{a+b}^{n} + \underbrace{c+d}_{m}` |
| `051-accent-arrow.tex` | stretchy `\overrightarrow` | `\overrightarrow{AB}` |
| `052-delimiter-parentheses.tex` | `\left…\right` | `\left( x \right)` |
| `053-delimiter-middle.tex` | `\left…\middle…\right`, fraction | `\left(\frac{x}{y}\middle|z\right)` |
| `054-delimiter-big-angle.tex` | `\bigl`/`\bigr`, `\langle`, fraction | `\bigl\langle \frac{x}{n} \bigr\rangle` |
| `055-delimiter-set-norm.tex` | `\left\{…\right\}`, `\mathbb`, `\mid`, `\lVert` | `\left\{ x \in \mathbb{R} \mid \lVert x \rVert_2 = 1 \right\}` |
| `056-matrix-nested-cells.tex` | `matrix`, fractions/roots in cells | `\begin{matrix} a_{1} & b^{2} \\ \frac{c}{d} & \sqrt{e} \end{matrix}` |
| `057-pmatrix-three-by-three.tex` | `pmatrix`, identity, 3 rows | `\begin{pmatrix} 1 & 0 & 0 \\ 0 & 1 & 0 \\ 0 & 0 & 1 \end{pmatrix}` |
| `058-bmatrix-inverse.tex` | `bmatrix` + trailing script | `\begin{bmatrix} a & b \\ c & d \end{bmatrix}^{-1}` |
| `059-vmatrix-three-by-three.tex` | `vmatrix`, 3 rows | `\begin{vmatrix} a & b & c \\ d & e & f \\ g & h & i \end{vmatrix}` |
| `060-Vmatrix-fractions.tex` | `Vmatrix`, fractions in cells | `\begin{Vmatrix} \frac{1}{2} & 0 \\ 0 & \frac{1}{3} \end{Vmatrix}` |
| `061-Bmatrix-column-fractions.tex` | `Bmatrix`, single column | `\begin{Bmatrix} \frac{1}{2} \\ \frac{3}{4} \end{Bmatrix}` |
| `062-matrix-left-right-delimiters.tex` | `\left…\right` around `matrix` | `\left(\begin{matrix} a & b \\ c & d \end{matrix}\right)` |
| `063-pmatrix-two-by-three.tex` | non-square `pmatrix` + script | `\begin{pmatrix} a & b & c \\ d & e & f \end{pmatrix}^{T}` |
| `064-bmatrix-column-vector.tex` | column vector | `\begin{bmatrix} x \\ y \\ z \end{bmatrix}` |
| `065-bmatrix-row-vector.tex` | row vector | `\begin{bmatrix} x & y & z \end{bmatrix}` |
| `066-cases-three-branches.tex` | `cases`, three branches, relations | `\begin{cases} 0 & x < 0 \\ x & 0 \le x \le 1 \\ 1 & x > 1 \end{cases}` |
| `067-cases-fraction-branches.tex` | `cases`, fractions, `\ne` | `\begin{cases} \frac{1}{x} & x \ne 0 \\ 0 & x = 0 \end{cases}` |
| `068-cases-text-conditions.tex` | `cases` with `\text` conditions | `\begin{cases} 1 & \text{if } n \text{ is even} \\ 0 & \text{otherwise} \end{cases}` |
| `069-array-column-spec.tex` | `array` with a column spec | `\begin{array}{lcr} a & b & c \\ 1 & 2 & 3 \end{array}` |
| `070-array-math-cells.tex` | `array`, fractions, Greek cells | `\begin{array}{cc} \frac{1}{2} & \sqrt{2} \\ \alpha & \beta \end{array}` |
| `071-array-single-column.tex` | `array`, single column | `\begin{array}{c} x \\ y \end{array}` |
| `072-substack-sum-two-indices.tex` | `\substack` in a sum | `\sum_{\substack{i=1 \\ j=1}}^{n} a_{ij}` |
| `073-substack-conditional.tex` | `\substack` with a condition | `\sum_{\substack{i=0 \\ i \ne j}}^{m} x_{ij}` |
| `074-substack-max.tex` | `\substack` on `\max` | `\max_{\substack{x \in A \\ y \in B}} f(x,y)` |
| `075-overset-definition.tex` | `\overset` with `\text`, `=`-definition | `\overset{\text{def}}{=}` |
| `076-overset-question.tex` | `\overset` with a bare marker | `\overset{?}{=}` |
| `077-underset-limit.tex` | `\underset` under `\lim` | `\underset{n \to \infty}{\lim} a_n` |
| `078-underset-argmin.tex` | `\underset` + `\operatorname` + `\mathbb` | `\underset{x \in \mathbb{R}}{\operatorname{argmin}} f(x)` |
| `079-mathbb-number-sets.tex` | `\mathbb` number sets, `\subset` | `\mathbb{N} \subset \mathbb{Z} \subset \mathbb{Q} \subset \mathbb{R} \subset \mathbb{C}` |
| `080-mathbb-vector-spaces.tex` | `\mathbb` with powers, `\to` | `\mathbb{R}^{n} \to \mathbb{R}^{m}` |
| `081-mathcal-laplace-transform.tex` | `\mathcal`, escaped braces | `\mathcal{L}\{f(t)\} = F(s)` |
| `082-mathcal-power-set.tex` | `\mathcal` twice, `\cup` | `\mathcal{P}(A) \cup \mathcal{P}(B)` |
| `083-operatorname-argmax.tex` | `\operatorname` with a bound | `\operatorname{argmax}_{x \in X} f(x)` |
| `084-nested-fraction-triple.tex` | three levels of nesting | `\frac{1}{1+\frac{1}{1+\frac{1}{x}}}` |
| `085-alignedat-two-columns.tex` | `alignedat`, two alignment pairs | `\begin{alignedat}{2} x &= 1 & \quad y &= 2 \\ z &= 3 & \quad w &= 4 \end{alignedat}` |

## Expected failure set

All ten malformed fixtures must return `Err` from both backends — from
`latex_to_svg` (`ratex-parser` error text in parentheses) and from
`latex_to_typst` — and none of them may panic or emit a partial result. The
"Typst reason" column names the check in `crates/mdx-math/src/typst.rs` that
catches it.

| File | Malformed because | Contents | Expected outcome | Typst reason |
| --- | --- | --- | --- | --- |
| `901-bad-missing-argument.tex` | `\frac` is called with one argument instead of two; the denominator is missing at end of input | `\frac{a}` | `Err` — missing/null argument for `\frac` | `empty argument` (the emitted call is `frac(a,)`) |
| `902-bad-double-superscript.tex` | the same base is given two superscripts, which TeX rejects (`Double superscript`) | `x^2^3` | `Err` — double superscript | `double superscript on one base` |
| `903-bad-unknown-command.tex` | `\nonexistentcommand` is not a KaTeX/RaTeX control sequence | `\nonexistentcommand{x}` | `Err` — `Undefined control sequence` | `undefined control sequence` (the name is copied into the body) |
| `904-bad-unmatched-right.tex` | `\right)` appears without a matching `\left` | `\right)` | `Err` — unexpected `\right` | `unbalanced \left / \right` |
| `905-bad-unmatched-left.tex` | `\left(` never gets its `\right`; the input ends first | `\left( x` | `Err` — `Expected \right after \left` | `unbalanced \left / \right` |
| `906-bad-unclosed-group.tex` | `\sqrt{x}` loses its closing brace, so the group is unterminated | `\sqrt{x` | `Err` — unclosed group | `unclosed '{'` |
| `907-bad-unclosed-environment.tex` | `cases` is opened and never closed; the input ends inside it | `\begin{cases} a \\ b` | `Err` — `Expected \end` | `unclosed \begin{cases}` |
| `908-bad-mismatched-environment.tex` | `pmatrix` is closed by `\end{bmatrix}` | `\begin{pmatrix} a \end{bmatrix}` | `Err` — `Mismatch` | `environment mismatch` |
| `909-bad-double-subscript.tex` | the same base is given two subscripts (`Double subscript`) | `x_1_2` | `Err` — double subscript | `double subscript on one base` |
| `910-bad-extra-closing-brace.tex` | a stray `}` closes a group that was never opened after `\frac{a}{b}` | `\frac{a}{b}}` | `Err` — stray closing brace | `unmatched '}'` |

The malformed set is deliberate about which layer catches what: `901`, `902`,
`903` and `909` are all cases the converter itself accepts and would emit
unusable Typst for, which is why the Typst path validates the structure *and*
the emitted body, not just the parse. The remaining six are structural mistakes
that both backends reject outright.

The first five failure modes are asserted in `ratex-parser`'s own test suite
(`ratex-parser/src/tests.rs`: `\frac{a}`, `x^2^3`, `\nonexistentcommand`,
`\right)`, and the `\left` chain cases), and so are the new ones — the
`error_handling` module covers `{x` (`unclosed_brace`) and `x}`
(`extra_close_brace`), `env_name_mismatch_error` covers the mismatched pair, and
`unknown_environment_error` covers an undefined environment — so the corpus
stays valid across `ratex-*` patch releases.

## Rendering notes

* Glyphs are emitted as `<text>` with KaTeX CSS `font-family` names, so an
  embedding host must load the KaTeX stylesheet (webfonts included) — see the
  module docs of `crates/mdx-math/src/svg.rs`. The SVG is a standalone document
  (own `viewBox`/`width`/`height`) but is **not** font-self-contained unless the
  workspace enables `ratex-svg`'s `standalone`/`embed-fonts` features.
* Geometry is computed at 40 user units per em with 10 units of padding, display
  math style, black ink; output is deterministic.
* CJK fixtures (`042`–`045`) render through RaTeX's missing-glyph fallback
  metrics and rely on the host's font chain for the actual shapes, which is the
  documented behaviour for text the bundled KaTeX fonts do not cover.
