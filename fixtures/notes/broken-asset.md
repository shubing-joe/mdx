# Broken asset

This fixture points at one image that does not exist, so `mdx check` reports
`E_ASSET_MISSING` for the raw `src` and the process exits with `3`:

![the missing file](missing.png)

The document itself is well-formed Markdown. A resolvable image next to the
missing one still resolves, so the run is a partial success rather than a hard
failure:

![a present file](assets/pixel.png)

The diagnostic carries the source as written (`missing.png`) rather than the
path it resolved to, so the fix is obvious from the output alone.
