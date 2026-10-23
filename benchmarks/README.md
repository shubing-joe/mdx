# Benchmarks

Two entry points measure the same pipeline at two levels. Neither is part of
CI: both are `bash -n`-clean scripts you run by hand on a quiet machine, and
neither types numbers into any file — measurements belong in the tables at the
bottom of this file, filled in from a real run.

| Entry point | What it measures | Use it to answer |
|---|---|---|
| `scripts/bench.sh` | one `mdx render` process per input, wall clock + peak RSS of the whole process | "how long does a user wait?" |
| `benchmarks/mdx-bench` (Rust) | every stage inside one process: parse, assets, math, diagram, typst-pdf | "which stage got slower?" |

Nothing here is wired into `cargo test`: the crate in this directory is excluded
from the workspace, so the root build never compiles it and `scripts/bench.sh`
is only run when a human asks for it.

## What `scripts/bench.sh` does

1. Builds the CLI: `cargo build --release --target-dir target -p mdx-cli`.
2. Generates two synthetic Markdown inputs from one ASCII template — by default
   1 MiB and 8 MiB — by repeating a ~1.1 kB section (`## Section n`, prose,
   bullets, a fenced code block, a table, a link reference). The small input is
   a literal prefix of the large one, so a super-linear result is a real
   regression and not a difference between two corpora.
3. Renders each input `BENCH_RUNS` (default 3) times with `--jobs 1` and
   `--jobs 4`, plus one untimed warm-up on the small input.
4. Prints the median wall-clock time, the median peak RSS and the size of the
   PDF from the last run.

Every section carries two unique formulas every fourth section
(`MATH_STRIDE=4` in the script) and a Mermaid diagram every sixty-fourth
(`DIAGRAM_STRIDE=64`). Deduplication happens before conversion, so the strides
are what make the `--jobs` comparison mean something: at `--jobs 1` the
thousands of formulas and the ~120 diagrams convert on one thread, at `--jobs 4`
they are split four ways.

Determinism: the document is generated from indices alone (no clock, no random
source) and rendered with `--deterministic --offline`, so repeated runs on one
machine produce byte-identical PDFs.

### Diagram cache

`mdx-diagram` keeps an advisory on-disk cache of rendered SVGs (path
`$MDX_CACHE_DIR`, else `$XDG_CACHE_HOME/mdx/diagrams`), so the second render of
an unchanged document reads SVG files instead of rendering diagrams. The script
sets `MDX_CACHE_DIR` to a path under `BENCH_WORK` that is never created, which
makes every lookup miss; the numbers below are therefore *cold cache* numbers,
and a warm-cache render is faster than the table says. Override with
`BENCH_CACHE_DIR=/some/dir` to measure a warm cache instead — then keep the
directory out of `$BENCH_WORK` so it survives between runs.

### Requirements

* a Rust toolchain and `cargo` on `PATH`;
* GNU time for peak RSS — `/usr/bin/time -v` is a separate package on most
  distributions (`pacman -S time` on Arch, `apt install time` on Debian). The
  script warns once and reports `n/a` for RSS when it is missing; wall clock is
  measured with `date +%s%N` either way.

### Reproduce

```sh
scripts/bench.sh                       # defaults: 3 runs, 1 MiB and 8 MiB, jobs 1 and 4
BENCH_RUNS=5 scripts/bench.sh          # more runs for a quieter median
BENCH_LARGE_BYTES=20971520 scripts/bench.sh          # 20 MiB instead of 8 MiB
BENCH_JOBS="1 2 4 8" scripts/bench.sh  # more job counts
BENCH_TIME_BIN=/usr/local/bin/time scripts/bench.sh  # non-standard GNU time
BENCH_WORK=/tmp/bench BENCH_KEEP=1 scripts/bench.sh  # keep inputs and outputs
BENCH_SMALL_BYTES=65536 BENCH_LARGE_BYTES=262144 BENCH_RUNS=1 scripts/bench.sh
                                       # fast sanity run of the harness itself
```

Output goes to stdout as a table (progress and warnings go to stderr); nothing
is written outside `$BENCH_WORK`, and a temporary directory is removed unless
`BENCH_KEEP=1`. Exit status: `0` measured, `2` the benchmark could not run.

The 8 MiB configuration renders a document of a few thousand pages and takes
minutes, not seconds. Start with the two-line `BENCH_SMALL_BYTES=…` invocation
above when you only want to check that the harness works.

## What `benchmarks/mdx-bench` does

`benchmarks/` is a standalone std-only crate (no external dependencies) that
links the library crates by path and runs the pipeline in process, exactly as
`mdx render` orders it: parse → resolve assets → convert math → render diagrams
→ write the PDF. It prints median/min/max milliseconds per stage over
`--runs` runs, plus the size of the PDF and the number of failed conversions.

It is *not* a workspace member (`exclude = ["benchmarks"]` in the root
`Cargo.toml`), so it is built on demand only:

```sh
# Reuses the workspace's own build directory and profile:
CARGO_TARGET_DIR=target cargo run --release \
    --manifest-path benchmarks/Cargo.toml -- fixtures/notes/math.md

# Larger input, four jobs, five runs:
BENCH=benchmarks/doc-8mb.md
CARGO_TARGET_DIR=target cargo run --release --manifest-path benchmarks/Cargo.toml -- \
    "$BENCH" --runs 5 --jobs 4
```

Usage: `mdx-bench <INPUT.md> [--runs N] [--jobs N] [--toc] [--offline]`. Exit
codes are `0` success, `1` a render stage failed, `2` bad usage or an
unreadable input. Point it at the inputs `scripts/bench.sh` generates
(`BENCH_WORK=/tmp/bench BENCH_KEEP=1 scripts/bench.sh` then
`/tmp/bench/doc-8mb.md`) to attribute a process-level regression to a stage.

The stage timings include the collection step they belong to (deduplicating and
ordering the unique formulas or diagrams), because that is what the CLI's
pipeline does in the same place. Use `--release`: a debug build measures
unoptimised fractions of the pipeline and is not comparable to anything.

The diagram stage goes through the same advisory SVG cache as the CLI. The
binary reads the environment but cannot set it (`std::env::set_var` is `unsafe`
in edition 2024), so it prints a note when `MDX_CACHE_DIR` is unset; run it as
`MDX_CACHE_DIR=/nonexistent cargo run --release …` for clean per-render
diagram numbers.

## Baseline

Fill a row per configuration from a real run; every cell the run did not print
stays `TBD`. Never copy a number from another machine into this table — record
`/proc/cpuinfo`'s model name, the memory size, `rustc -V`, and the commit the
run was made at, since none of these numbers are portable.

Record a machine header once per host, then one row per measurement:

```text
host: <hostname> | cpu: <model name> | ram: <GiB> | rustc: <rustc -V> | commit: <git rev-parse --short HEAD>
```

### Process level — `scripts/bench.sh`

| host | commit | input | jobs | median wall (ms) | peak RSS (MB) | pdf (kB) |
|------|--------|-------|------|------------------|---------------|----------|
| TBD  | TBD    | 1 MiB | 1    | TBD              | TBD           | TBD      |
| TBD  | TBD    | 1 MiB | 4    | TBD              | TBD           | TBD      |
| TBD  | TBD    | 8 MiB | 1    | TBD              | TBD           | TBD      |
| TBD  | TBD    | 8 MiB | 4    | TBD              | TBD           | TBD      |

### Stage level — `benchmarks/mdx-bench`

Medians in milliseconds, `--runs 3`, same host header as above.

| host | commit | input | jobs | parse | assets | math | diagram | typst-pdf | total |
|------|--------|-------|------|-------|--------|------|---------|-----------|-------|
| TBD  | TBD    | 1 MiB | 1    | TBD   | TBD    | TBD  | TBD     | TBD       | TBD   |
| TBD  | TBD    | 8 MiB | 1    | TBD   | TBD    | TBD  | TBD     | TBD       | TBD   |
| TBD  | TBD    | 8 MiB | 4    | TBD   | TBD    | TBD  | TBD     | TBD       | TBD   |

### Reading the numbers

* `parse` and `assets` should scale linearly in document size; a step change
  there means a quadratic scan or a new allocation per block.
* `math` and `diagram` are the stages `--jobs` accelerates. Their `jobs=1` and
  `jobs=4` columns answer whether the split is actually parallel; identical
  numbers mean the work collapsed into a single item (deduplication) or the
  pipeline is running inline.
* `typst-pdf` is the Typst compile plus PDF export of a multi-thousand-page
  document; it dominates every run and is the number to watch when the writer
  or the world changes.
* Compare only runs made on the same host, with the same `rustc`, at the same
  commit, with the same `BENCH_*` settings.
