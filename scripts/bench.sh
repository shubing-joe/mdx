#!/usr/bin/env bash
#
# Time `mdx render` end to end on synthetic documents of two sizes.
#
# The script builds the CLI in release mode, generates two Markdown inputs from
# one ASCII template (default 1 MiB and 8 MiB, the small one being a literal
# prefix of the large one), then renders each of them `BENCH_RUNS` times with
# `--jobs 1` and `--jobs 4`, reporting the median wall-clock time, the median
# peak resident set size and the size of the PDF that came out.
#
# Peak RSS needs GNU time (`/usr/bin/time -v`), which is a separate package on
# most distributions (Arch: `pacman -S time`, Debian: `apt install time`). When
# it is missing the script still measures wall time and prints `n/a` for RSS.
#
# This measures the *process*, including interpreter-free start-up, font
# registration and Typst compilation; `benchmarks/mdx-bench` measures the stages
# inside one process. Use both: this one answers "how long does `mdx render`
# take for a user", that one answers "which stage got slower".
#
# The diagram cache is switched off for every timed render: `MDX_CACHE_DIR`
# points at a directory under BENCH_WORK that is never created, so a lookup
# always misses (and the advisory write always fails silently). Without that,
# run 1 would render the diagrams and runs 2..N would read SVGs from the cache,
# and the median would describe neither case.
#
# Usage: scripts/bench.sh
#
# Environment:
#   BENCH_RUNS          timed runs per configuration                    (3)
#   BENCH_SMALL_BYTES   size of the small input, in bytes          (1048576)
#   BENCH_LARGE_BYTES   size of the large input, in bytes          (8388608)
#   BENCH_JOBS          job counts to compare, space separated        ("1 4")
#   BENCH_TIME_BIN      GNU time binary used for peak RSS      (/usr/bin/time)
#   BENCH_WORK          directory to generate into             (a temp dir)
#   BENCH_CACHE_DIR     diagram cache directory for the renders  (a path under
#                       BENCH_WORK that is never created, which makes every
#                       lookup miss; see the note below)
#   BENCH_KEEP          1 keeps the generated inputs and PDFs           (0)
#   BENCH_VERBOSE       1 prints every individual run                   (0)
#
# Exit status: 0 the benchmark ran, 2 it could not run (missing tool, failed
# build, bad BENCH_* value).
#
# Known numbers must never be typed into this script: the baseline table in
# benchmarks/README.md is the only place a measurement belongs, and it is filled
# in by hand from a real run.

set -euo pipefail
export LC_ALL=C

# ---------------------------------------------------------------------------
# configuration
# ---------------------------------------------------------------------------

runs="${BENCH_RUNS:-3}"
small_bytes="${BENCH_SMALL_BYTES:-1048576}"
large_bytes="${BENCH_LARGE_BYTES:-8388608}"
jobs_list="${BENCH_JOBS:-1 4}"
time_bin="${BENCH_TIME_BIN:-/usr/bin/time}"
keep="${BENCH_KEEP:-0}"
verbose="${BENCH_VERBOSE:-0}"

# Sections per stage trigger. Math is cheap per item and every fourth section
# carries two unique formulas; a diagram costs an order of magnitude more, so
# one section in sixty-four carries a Mermaid block. Both strides are what makes
# `--jobs 1` versus `--jobs 4` measurable at all.
math_stride=4
diagram_stride=64

case "${1:-}" in
    -h | --help)
        # The header comment block, up to the first line that is not a comment.
        awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' \
            "${BASH_SOURCE[0]}"
        exit 0
        ;;
    "") ;;
    *)
        printf 'bench: unknown argument `%s` (try --help)\n' "$1" >&2
        exit 2
        ;;
esac

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="$root/target"
bin="$target_dir/release/mdx"

for tool in cargo date sort awk; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'bench: %s is required but was not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

if [ ! -x "$time_bin" ]; then
    printf 'bench: %s not found - peak RSS will be reported as n/a\n' "$time_bin" >&2
    printf 'bench: install GNU time (Arch: pacman -S time, Debian: apt install time)\n' >&2
    time_bin=""
fi

for count in "$runs" "$small_bytes" "$large_bytes"; do
    case "$count" in
        '' | *[!0-9]*)
            printf 'bench: expected a whole number, got `%s`\n' "$count" >&2
            exit 2
            ;;
    esac
done
if [ "$runs" -lt 1 ] || [ "$small_bytes" -lt 1 ] || [ "$large_bytes" -lt 1 ]; then
    printf 'bench: runs and input sizes must be at least 1\n' >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# synthetic input
# ---------------------------------------------------------------------------
#
# One section is pure ASCII, so `${#text}` is its byte length and the generator
# can stop at the requested size without measuring the file on every pass. Every
# `@N@` becomes the 1-based section number: the heading text, the inline code,
# the math "n" and the generated code block all differ per section, and the
# generators below have to do real work instead of folding thousands of
# repetitions of one key.

IFS= read -r -d '' section_template <<'SECTION' || true
## Section @N@

Paragraph @N@ of the synthetic body. This template is repeated until the file
reaches the requested size, so every stage of the pipeline sees a document whose
body grows linearly in the number of blocks. The prose is deliberately padded:
sections that were a few hundred bytes long would make the run dominated by the
per-process constants of the toolchain rather than by the work, and the whole
point of a size benchmark is the part that scales. Nothing here is derived from
the wall clock, the environment or a random source, so two runs on the same
machine render byte-identical PDFs - which is also what `--deterministic` tells
the renderer, and what makes the numbers below comparable between runs.

### Notes @N@

- bullet @N@.1, short
- bullet @N@.2, a little longer than the first one
- bullet @N@.3, with `inline code @N@`, *emphasis* and a [link][ref-@N@]

[ref-@N@]: https://example.invalid/@N@

```text
line @N@ of the generated body
```

| key | value |
|-----|-------|
| n | @N@ |
| n squared | @N@ |

SECTION

IFS= read -r -d '' math_template <<'MATH' || true

Inline math $x_{@N@} = @N@ y$ with a display formula between the paragraphs:

$$ \int_{0}^{@N@} \frac{x^{2}+1}{\sqrt{1+x}}\, dx = \sum_{k=1}^{@N@} \frac{k^{2}}{k+1} $$

The two formulas above are unique per section, so the math stage converts one
distinct expression per occurrence instead of reusing a deduplicated handful.
MATH

IFS= read -r -d '' diagram_template <<'DIAGRAM' || true

```mermaid
flowchart LR
  A@N@[Section @N@] --> B@N@{ok?}
  B@N@ -- yes --> C@N@[render]
  B@N@ -- no --> D@N@[report]
```
DIAGRAM

# Appends whole sections to `$2` until it holds at least `$1` bytes.
generate() {
    local target="$1" out="$2" n=0 total=0 text
    : > "$out"
    exec 3>"$out"
    while [ "$total" -lt "$target" ]; do
        n=$((n + 1))
        text="${section_template//@N@/$n}"
        if [ $((n % diagram_stride)) -eq 0 ]; then
            text+="${diagram_template//@N@/$n}"
        elif [ $((n % math_stride)) -eq 0 ]; then
            text+="${math_template//@N@/$n}"
        fi
        printf '%s' "$text" >&3
        total=$((total + ${#text}))
    done
    exec 3>&-
    printf 'bench: wrote %s (%s bytes, %s sections)\n' "$out" "$total" "$n" >&2
}

# ---------------------------------------------------------------------------
# measurement helpers
# ---------------------------------------------------------------------------

# Median of the integer arguments on the command line.
median() {
    local -a values=("$@") sorted
    local count=${#values[@]} middle
    if [ "$count" -eq 0 ]; then
        printf '0'
        return
    fi
    mapfile -t sorted < <(printf '%s\n' "${values[@]}" | sort -n)
    middle=$((count / 2))
    if [ $((count % 2)) -eq 1 ]; then
        printf '%s' "${sorted[middle]}"
    else
        printf '%s' "$(((sorted[middle - 1] + sorted[middle]) / 2))"
    fi
}

# Renders `$1` with `$2` jobs into `$3`; echoes "<wall ms> <rss kb>".
#
# The exit status of `mdx render` is deliberately not fatal: a document whose
# diagrams fail still renders, and the benchmark is about time, not about the
# diagnostics the renderer printed.
measure() {
    local doc="$1" jobs="$2" out="$3" start end wall rss="" time_file="$work/time.txt"
    start=$(date +%s%N)
    if [ -n "$time_bin" ]; then
        "$time_bin" -v -o "$time_file" \
            "$bin" render "$doc" --jobs "$jobs" --offline --deterministic \
            -o "$out" >/dev/null 2>&1 || true
        rss="$(grep -F 'Maximum resident set size' "$time_file" | tr -dc '0-9' || true)"
    else
        "$bin" render "$doc" --jobs "$jobs" --offline --deterministic \
            -o "$out" >/dev/null 2>&1 || true
    fi
    end=$(date +%s%N)
    wall=$(((end - start) / 1000000))
    printf '%s %s\n' "$wall" "${rss:-n/a}"
}

# ---------------------------------------------------------------------------
# run
# ---------------------------------------------------------------------------

printf 'bench: building mdx (release) in %s\n' "$target_dir" >&2
if ! cargo build --release --target-dir "$target_dir" -p mdx-cli >&2; then
    printf 'bench: cargo build failed\n' >&2
    exit 2
fi
if [ ! -x "$bin" ]; then
    printf 'bench: %s was not produced by the build\n' "$bin" >&2
    exit 2
fi

if [ -n "${BENCH_WORK:-}" ]; then
    work="$BENCH_WORK"
    mkdir -p "$work"
else
    work="$(mktemp -d)"
fi
cleanup() {
    if [ "$keep" != "1" ] && [ -z "${BENCH_WORK:-}" ]; then
        rm -rf "$work"
    else
        printf 'bench: inputs and outputs kept in %s\n' "$work" >&2
    fi
}
trap cleanup EXIT

# Timed renders must be cold: point the diagram cache at a directory that is
# never created, so every lookup misses and the advisory write fails silently.
export MDX_CACHE_DIR="${BENCH_CACHE_DIR:-$work/no-cache}"
printf 'bench: diagram cache disabled for the timed runs (MDX_CACHE_DIR=%s)\n' \
    "$MDX_CACHE_DIR" >&2

small_doc="$work/doc-1mb.md"
large_doc="$work/doc-8mb.md"
generate "$small_bytes" "$small_doc"
generate "$large_bytes" "$large_doc"

# One untimed pass over the small document: the first render of the process
# loads and validates the bundled fonts and touches each code path once, which
# would otherwise land entirely on run 1.
printf 'bench: warm-up render\n' >&2
"$bin" render "$small_doc" --jobs 1 --offline --deterministic \
    -o "$work/warmup.pdf" >/dev/null 2>&1 || true

printf '\n'
printf '%-8s %10s %6s %12s %10s %10s %6s\n' \
    "input" "bytes" "jobs" "median ms" "rss MB" "pdf kB" "runs"
printf '%s\n' "-------------------------------------------------------------------"

for document in "1mb:$small_bytes:$small_doc" "8mb:$large_bytes:$large_doc"; do
    label="${document%%:*}"
    rest="${document#*:}"
    bytes="${rest%%:*}"
    doc="${rest#*:}"
    for jobs in $jobs_list; do
        case "$jobs" in
            '' | *[!0-9]*)
                printf 'bench: `%s` in BENCH_JOBS is not a whole number\n' "$jobs" >&2
                exit 2
                ;;
        esac
        walls=()
        rsses=()
        pdf_bytes=0
        for ((run = 1; run <= runs; run++)); do
            out="$work/$label-jobs$jobs-run$run.pdf"
            read -r wall rss < <(measure "$doc" "$jobs" "$out")
            walls+=("$wall")
            rsses+=("$rss")
            if [ "$run" -eq "$runs" ] && [ -f "$out" ]; then
                pdf_bytes=$(wc -c < "$out")
            fi
            if [ "$verbose" = "1" ]; then
                printf 'bench: %s jobs=%s run %s: %s ms, %s kB\n' \
                    "$label" "$jobs" "$run" "$wall" "$rss" >&2
            fi
        done
        median_wall=$(median "${walls[@]}")
        if [ -n "$time_bin" ]; then
            median_rss=$(median "${rsses[@]}")
            rss_column=$((median_rss / 1024))
        else
            rss_column="n/a"
        fi
        printf '%-8s %10s %6s %12s %10s %10s %6s\n' \
            "$label" "$bytes" "$jobs" "$median_wall" \
            "$rss_column" "$((pdf_bytes / 1024))" "$runs"
    done
done

printf '\n'
if [ -n "$time_bin" ]; then
    printf 'peak RSS comes from `%s -v`; PDF size is from the last run.\n' "$time_bin"
else
    printf 'peak RSS is unavailable (GNU time not installed); PDF size is from the last run.\n'
fi
printf 'Record new numbers in benchmarks/README.md, not in this script.\n'
