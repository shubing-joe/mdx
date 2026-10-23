#!/usr/bin/env bash
#
# install-fonts.sh — find a CJK-capable font on this machine and print the
# `--font` argument `mdx` needs to embed it.
#
# `mdx` typesets with the faces it ships plus the files you pass with `--font`;
# it scans the usual font directories for known CJK families on its own unless
# `--ignore-system-fonts` is given, but a font that lives elsewhere — or a render
# that must not depend on the host's font store — needs the path spelled out.
#
# The script is read-only: it never installs, copies or modifies a font, and it
# writes nothing to disk. It asks fontconfig first, because fontconfig knows what
# is actually installed, falls back to the well-known install locations, and
# prefers a Regular-weight face over the Bold or Light faces a font directory is
# mostly made of.
#
# Usage:
#   scripts/install-fonts.sh           print a ready-to-paste `--font` argument
#   scripts/install-fonts.sh --path    print the font file alone
#   scripts/install-fonts.sh --help
#
# Exit status: 0 when a usable font was found, 1 when none was, 2 on bad usage.
set -euo pipefail

# File extensions Typst can read; the same set the diagram renderer accepts.
FONT_EXTENSIONS=(otf otc ttf ttc)

# Substrings that identify a CJK-capable face by name. fontconfig answers a
# language query with a substitution when nothing matches, so a "best match" for
# `:lang=zh-cn` can still be a Latin-only face; these markers keep those out.
# The list mirrors the families `mdx` itself scans for.
CJK_MARKERS=(cjk sourcehan "source han" wqy wenquanyi "noto sans sc" "noto serif sc" simhei simsun pingfang hiragino)

# Well-known locations, in the order they are tried. Only paths that really are
# provided by a distribution package are listed, so a hit here is a font that is
# already installed.
FONT_CANDIDATES=(
  # Arch Linux: noto-fonts-cjk
  /usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc
  /usr/share/fonts/noto-cjk/NotoSerifCJK-Regular.ttc
  /usr/share/fonts/noto-cjk/NotoSansCJK-VF.otf.ttc
  /usr/share/fonts/noto-cjk/NotoSansCJKsc-Regular.otf
  # Debian, Ubuntu, Fedora: fonts-noto-cjk, google-noto-sans-cjk-fonts
  /usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc
  /usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc
  /usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc
  # Arch Linux: adobe-source-han-sans-cn-fonts, adobe-source-han-serif-cn-fonts
  /usr/share/fonts/adobe-source-han-sans-cn/SourceHanSansCN-Regular.otf
  /usr/share/fonts/adobe-source-han-serif-cn/SourceHanSerifCN-Regular.otf
  # Arch Linux: wqy-zenhei, wqy-microhei
  /usr/share/fonts/wenquanyi/wqy-zenhei/wqy-zenhei.ttc
  /usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc
  # macOS
  /System/Library/Fonts/PingFang.ttc
  /System/Library/Fonts/Hiragino Sans GB.ttc
)

usage() {
  cat <<'USAGE'
usage: install-fonts.sh [--path]

Print the `--font PATH` argument for the CJK font installed on this machine.

  --path   print only the font file, without the `mdx render` command line
  --help   print this text

Exit status is 0 when a font was found and 1 when none was.
USAGE
}

# A path is usable when it is a readable file with an extension Typst reads.
is_usable_font() {
  local file=$1
  if [[ ! -f $file || ! -r $file ]]; then
    return 1
  fi
  local extension=${file##*.}
  extension=${extension,,}
  local known
  for known in "${FONT_EXTENSIONS[@]}"; do
    if [[ $extension == "$known" ]]; then
      return 0
    fi
  done
  return 1
}

# A face is CJK by name when its file name carries one of the markers above.
is_cjk_name() {
  local name=${1##*/}
  name=${name,,}
  local marker
  for marker in "${CJK_MARKERS[@]}"; do
    if [[ $name == *"$marker"* ]]; then
      return 0
    fi
  done
  return 1
}

# The CJK faces fontconfig ranks for Chinese, then Japanese and Korean text, best
# match first (`fc-match -s` sorts the whole match list). Duplicates are dropped
# without reordering, because the rank is the answer.
fontconfig_ranked() {
  if ! command -v fc-match >/dev/null 2>&1; then
    return 1
  fi
  local file
  while IFS= read -r file; do
    if is_cjk_name "$file" && is_usable_font "$file"; then
      printf '%s\n' "$file"
    fi
  done < <(
    for pattern in ':lang=zh-cn' ':lang=ja' ':lang=ko'; do
      fc-match -s --format '%{file}\n' "$pattern" 2>/dev/null
    done | awk '!seen[$0]++'
  )
}

# Every face fontconfig reports for Simplified Chinese, in sorted order so the
# result does not depend on the order the configuration files were read. The
# `:lang` filter comes from fontconfig's own coverage data, so no name guard is
# needed here.
fontconfig_list() {
  if ! command -v fc-list >/dev/null 2>&1; then
    return 1
  fi
  fc-list --format '%{file}\n' ':lang=zh-cn' 2>/dev/null | LC_ALL=C sort -u
}

# Picks one path from the candidates read on stdin: the first Regular-weight face
# when there is one, otherwise the first usable candidate. Font directories are
# full of Bold and Italic faces, and a document set in a bold fallback face is
# not what anyone asked for.
pick_font() {
  local first=
  local file lower
  while IFS= read -r file; do
    if [[ -n $file ]] && is_usable_font "$file"; then
      if [[ -z $first ]]; then
        first=$file
      fi
      lower=${file,,}
      if [[ $lower == *regular* ]]; then
        printf '%s\n' "$file"
        return 0
      fi
    fi
  done
  if [[ -n $first ]]; then
    printf '%s\n' "$first"
    return 0
  fi
  return 1
}

# First usable entry of the static candidate list.
find_in_known_locations() {
  local candidate
  for candidate in "${FONT_CANDIDATES[@]}"; do
    if is_usable_font "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

mode=command
case ${1:-} in
  "") ;;
  --path) mode=path ;;
  -h | --help)
    usage
    exit 0
    ;;
  *)
    printf 'install-fonts.sh: unknown argument %s\n\n' "$1" >&2
    usage >&2
    exit 2
    ;;
esac
if [[ $# -gt 1 ]]; then
  printf 'install-fonts.sh: unexpected extra arguments: %s\n\n' "${*:2}" >&2
  usage >&2
  exit 2
fi

# Tiers, best first: the CJK faces fontconfig ranks for this host, then the
# well-known install locations, then every face fontconfig reports for Simplified
# Chinese. Within a tier a Regular-weight face wins; `|| true` keeps a missing
# fontconfig from ending the script early under `set -e` and `pipefail`.
font=
for tier in fontconfig_ranked find_in_known_locations fontconfig_list; do
  font=$("$tier" 2>/dev/null | pick_font || true)
  if [[ -n $font ]]; then
    break
  fi
done

if [[ -z $font ]]; then
  cat >&2 <<'MISSING'
install-fonts.sh: no CJK font found.

Install one, then run this script again:
  Arch Linux     sudo pacman -S noto-fonts-cjk     (or wqy-zenhei)
  Debian, Ubuntu sudo apt install fonts-noto-cjk
  Fedora         sudo dnf install google-noto-sans-cjk-fonts

Any TrueType/OpenType CJK face works; pass it yourself with
  mdx render doc.md -o doc.pdf --font /path/to/font.ttc
MISSING
  exit 1
fi

if [[ $mode == path ]]; then
  printf '%s\n' "$font"
else
  # `%q` so a path with spaces stays one argument when the line is pasted.
  printf 'mdx render doc.md -o doc.pdf --font %q\n' "$font"
fi
