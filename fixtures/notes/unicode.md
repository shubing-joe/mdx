# Unicode stress

A document whose script coverage is wider than any single font: CJK ideographs
and kana, Arabic written right to left, and Latin letters carrying combining
marks. Nothing here may depend on a font installed on the machine that renders
it; a glyph the fonts do not cover becomes tofu, never a failure.

## CJK

简体中文：排版引擎负责断行，标点不应落在行首。
繁體中文：「引號」、『單引號』與全形標點都應正確斷行。
日本語：ひらがな、カタカナ、漢字、長音符「ー」を混ぜます。
한국어: 한글 자모와 한자를 함께 씁니다.

Latin and CJK on the same line: the quick brown fox 跳過 the lazy dog 躍過.

## Arabic, right to left

مرحبا بالعالم
العربية تكتب من اليمين إلى اليسار، والأرقام 12345 تبقى في مكانها.

A Latin sentence with an Arabic run inside it مرحبا continues after the run, and
a bracket pair (مرحبا) keeps its own direction.

## Combining marks

The same letters spelled precomposed and decomposed must both survive:

- precomposed: résumé naïve Zürich Ångström
- decomposed: résumé naïve Zürich Ångström
- stacked: á̈ carries two marks on one base letter
- Greek: ά and Ώ̈

Directional and zero-width controls: a​b keeps the words apart, a‌b
joins them, and ‏a Latin run after a right-to-left mark stays Latin, as does
a‎b.

## Everything in one table

| script | sample | marks |
|--------|--------|-------|
| Han | 永和九年 | none |
| Hiragana | あいうえお | none |
| Hangul | 한글 | none |
| Arabic | السلام عليكم | none |
| Latin | é ä | combining |
| Mixed | 🚀 👨‍👩‍👧 🇯🇵 | emoji |
