---
title: Math in tables and footnotes
author: mdx maintainers
date: "2026-01-01"
---

# Math in tables and footnotes

Math is inline content, so it may sit in any container that holds inlines: table
cells, footnote bodies, quotes and list items. The `latex -> typst` map is
collected from every formula in the document, including the ones nested below.

## Table cells

| step | formula | result |
|:-----|:--------|-------:|
| sum of squares | $\sum_{k=1}^{n} k^2$ | $\frac{n(n+1)(2n+1)}{6}$ |
| geometric series | $\sum_{k=0}^{n} r^k$ | $\frac{1 - r^{n+1}}{1 - r}$ |
| binomial | $\binom{n}{k}$ | $\frac{n!}{k!(n-k)!}$ |
| matrix | $\begin{pmatrix} a & b \\ c & d \end{pmatrix}$ | $ad - bc$ |
| cube root | $\sqrt[3]{x^3 + y^3}$ | $x + y$ |
| limit | $\lim_{x \to 0} \frac{\sin x}{x}$ | $1$ |

A cell may carry text around its math (Euler's identity $e^{i\pi} + 1 = 0$) and
the column alignment applies to the whole cell, math included:

| left $\alpha$ | center $\beta$ | right $\gamma$ |
|:--------------|:--------------:|---------------:|
| $\alpha_1$ | $\beta_1$ | $\gamma_1$ |
| plain text | $x \mapsto x^2$ | $\pi \approx 3.14159$ |

## Footnotes holding math

The definition of the integral[^integral] and the Euler identity[^euler] both
carry formulas, and a reference may repeat itself[^integral].

[^integral]:
    The definite integral sums infinitely many slices:

    $$ \int_a^b f(x) \, \mathrm{d}x = F(b) - F(a) $$

    where $F$ is an antiderivative of $f$.

[^euler]: Euler's identity $e^{i\pi} + 1 = 0$ holds for every $i$ and $\pi$ the
    note mentions.

## Math in other containers

> A quote holding a formula, $\frac{\mathrm{d}}{\mathrm{d}x} \ln x = \frac{1}{x}$,
> and one more note[^euler].

- a list item with $\vec{v} \cdot \vec{w} = 0$
  1. a nested ordered item with $\hat{H}\psi = E\psi$
- another item repeating $\vec{v} \cdot \vec{w} = 0$

Display math inside a quote:

> $$
> \nabla \cdot \vec{E} = \frac{\rho}{\varepsilon_0}
> $$

| threshold | value |
|-----------|-------|
| $\varepsilon$ | $10^{-9}$ |
| $\pi$ | $3.14159$ |
