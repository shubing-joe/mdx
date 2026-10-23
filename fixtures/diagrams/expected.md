# Diagram corpus — expected values

Fixtures for the Mermaid renderer (`crates/mdx-diagram`) and its content cache
(`crates/mdx-diagram/src/cache.rs`). Each `.mmd` file holds one diagram type and
contains no comments, so the whole file body is the diagram source fed to
`render_svg`.

## Cache keys

`cache::key(src)` = `"mmd-"` + the 16 lowercase hex digits of an FNV-1a 64-bit
digest of the source bytes (`crates/mdx-diagram/src/cache.rs`). Every key is
exactly 20 characters. Keys below are computed over each file's exact bytes,
including the trailing newline.

| File | Diagram type | Expected cache key |
|------|--------------|--------------------|
| `flowchart.mmd` | flowchart | `mmd-d1151423208c1930` |
| `sequence.mmd` | sequenceDiagram | `mmd-2e8f4359bf2885fe` |
| `class.mmd` | classDiagram | `mmd-de7bda44b0c8d9a2` |
| `state.mmd` | stateDiagram-v2 | `mmd-24304efb83344c62` |
| `gantt.mmd` | gantt | `mmd-52a411ceb0889132` |
| `pie.mmd` | pie | `mmd-f07d112f0759c6c2` |
| `mindmap.mmd` | mindmap | `mmd-36f04117a92f0803` |
| `er.mmd` | erDiagram | `mmd-f36193502f211456` |
| `journey.mmd` | journey | `mmd-362c0ddacdf06a11` |
| `gitgraph.mmd` | gitGraph | `mmd-5051c5d77d473c21` |
| `timeline.mmd` | timeline | `mmd-ab963a06717319b8` |
| `quadrant.mmd` | quadrantChart | `mmd-43faeb7d3fa12ccd` |

## Expected render results

Every file must render through the resvg-safe pipeline: the returned string
starts with `<svg` and contains no `foreignObject` element. All twelve are
well-formed Mermaid sources, so `render_svg` returns `Ok` for each; none of them
is expected to produce `E_DIAGRAM_PARSE`.

## Keyword check

The first non-empty line of each file is the diagram keyword listed above
(`flowchart` files additionally carry their direction on the same line, and
`pie`/`gantt`/`journey`/`timeline` may carry a `title`).
