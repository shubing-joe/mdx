# Diagrams

A `mermaid` fenced block becomes a rendered SVG figure instead of a code
listing; every other info string stays a code block.

## Flowchart

```mermaid
flowchart TD
    A[doc.md] --> B{Resolve assets}
    B -->|present| C[Typst]
    B -->|missing| D[E_ASSET_MISSING]
    C --> E[doc.pdf]
    D --> E
```

## Sequence diagram

```mermaid
sequenceDiagram
    participant CLI
    participant Core
    CLI->>Core: mdx render doc.md
    Core-->>CLI: doc + report
    CLI->>CLI: exit_code(report)
```

## Pie chart

```mermaid
pie title Time in the pipeline
    "parse" : 5
    "assets" : 10
    "typst" : 75
    "diagrams" : 10
```

## Not a diagram

An info string other than `mermaid` keeps the block as code:

```text
flowchart LR
    A --> B
```
