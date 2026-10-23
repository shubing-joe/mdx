# CJK text

This fixture exercises text the parser cannot tokenise into words: line breaking,
punctuation squeezing and the spacing between CJK runs and the Latin text around
them.

## 简体中文

简体中文的一段文字，包含全角标点：「引号」、《书名》、（括号）、【方括号】、顿号、
以及省略号……句号与逗号不应落在行首，换行由排版引擎决定，而不是由 Markdown
源文件里的换行决定。

## 繁體中文

繁體中文段落，使用「引號」、『單引號』、全形標點與破折號——都應該正確斷行。

## 日本語

日本語の文章です。ひらがな、カタカナ、漢字を混在させます。長音符「ー」や
約物「、」「。」も含みます。「こんにちは、世界。」という挨拶も書きます。

## 한국어

한국어 문장입니다. 한글 자모와 한자를 함께 사용합니다. "안녕하세요, 세계."라는
인사말과 물음표·느낌표도 포함합니다.

## Mixed inline content

Inline code such as `变量名 = 42` and **强调文本** sit next to `ASCII` text, and an
emoji 🚀 survives the round trip. 半角カナ ｱｲｳ も残ります。

| 语言 | 标签 | 说明 |
|------|------|------|
| 中文 | `zh` | 简体与繁体 |
| 日本語 | `ja` | かなと漢字 |
| 한국어 | `ko` | 한글 |
| English | `en` | Latin, obviously |

- 中文列表项
- 日本語のリスト項目
- 한국어 목록 항목

> 引用の中にも CJK テキストが入ります。
