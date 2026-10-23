# Math

Inline math such as $E = mc^2$ and $a^2 + b^2 = c^2$ shares a line with ordinary
text. Display math stands on its own:

$$
\int_0^\infty e^{-x^2} \, \mathrm{d}x = \frac{\sqrt{\pi}}{2}
$$

A matrix, a sum and a limit:

$$
A = \begin{pmatrix} a & b \\ c & d \end{pmatrix}
\qquad
\sum_{n=1}^{\infty} \frac{1}{n^2} = \frac{\pi^2}{6}
\qquad
\lim_{x \to 0} \frac{\sin x}{x} = 1
$$

A `math` fenced block is display math as well:

```math
\frac{\partial^2 u}{\partial t^2} = c^2 \nabla^2 u
```

Greek letters, accents and delimiters: $\alpha, \beta, \Gamma, \hat{x}, \vec{v},
\overline{z}, \left( \frac{1}{2} \right]$.

A formula that only RaTeX can draw is reported as `E_MATH_CONVERT` and left
alone instead of failing the run: $\notacommand{1}$.

Text still renders after math: *emphasised* and `code`.
