# packaging

Install recipes and desktop integration for the `mdx` workspace. Everything here
is derived from the tree: `url` and `license` come from `[workspace.package]` in
`Cargo.toml`, and the installed binary names are the `[[bin]]` targets of
`crates/mdx-cli`, `crates/mdx-mcp` and `crates/mdx-gui`.

| file | role |
|------|------|
| `PKGBUILD` | Arch Linux recipe: builds the workspace in release mode and installs all three binaries |
| `mdx.desktop` | freedesktop entry for `mdx-gui`, registered for `text/markdown` |
| `mdx.svg` | placeholder application icon (drawn with primitives, no font needed) |
| `mdx-mcp.service` | systemd **user** unit for `mdx-mcp`, with the stdio contract spelled out |
| `.gitignore` | keeps the `src/`, `pkg/` and tarball `makepkg` writes here out of commits |

## Arch Linux

```console
$ cd packaging
$ makepkg -si
```

`makepkg` fetches the tag tarball for `pkgver` from the repository URL in
`Cargo.toml`, builds `mdx`, `mdx-mcp` and `mdx-gui` with
`cargo build --release --locked --workspace`, and installs:

```
/usr/bin/mdx
/usr/bin/mdx-mcp
/usr/bin/mdx-gui
/usr/share/applications/mdx.desktop
/usr/share/icons/hicolor/scalable/apps/mdx.svg
/usr/lib/systemd/user/mdx-mcp.service
```

`sha256sums` is `SKIP` because the forges generate tag tarballs on demand; run
`updpkgsums` (or `makepkg -g`) once, at release time, and paste the digest into
the recipe. `cargo` is the only declared build dependency; install the Rust
toolchain from the Arch repositories — the workspace is edition 2024 and pins
`rust-version = "1.95"`.

Two system-level requirements are deliberately left to the packager rather than
hard-coded in `depends`:

- **`mdx-gui`** links against a GL/EGL stack and a window system. `libglvnd`,
  `libx11`, `libxkbcommon` and `libwayland` must be present when the workspace is
  built and when the GUI runs.
- **CJK PDF text** needs a CJK font on the host, hence
  `optdepends=('noto-fonts-cjk: CJK PDF text')`. `mdx` scans the usual font
  directories for known CJK families on its own; pass the font explicitly when
  you want a hermetic render (see below).

To package an uncommitted checkout instead of the released tag, point `source`
at a local repository — `makepkg` accepts git sources:

```
source=("mdx::git+file:///absolute/path/to/mdx")
```

and add `git` to `makedepends` while you build that way.

## CJK fonts and `--font`

`--font PATH` is repeatable: it adds the listed faces to the font book, or
overrides the family they name, and the same files are also used to measure
Mermaid label text. `scripts/install-fonts.sh` prints the argument for the CJK
font it finds on this machine:

```console
$ scripts/install-fonts.sh
mdx render doc.md -o doc.pdf --font /usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc
```

Add `--ignore-system-fonts` when the document must render identically on every
machine; see the CLI section of the top-level `README.md`.

## The systemd user unit

`mdx-mcp` speaks MCP over stdio: it reads JSON-RPC from stdin, writes it to
stdout and exits when that transport closes. It has no socket, no port and no
daemon mode, so the normal deployment is a client configuration (editors and
agents spawn the binary themselves) rather than a service — the JSON snippet in
the top-level `README.md` is the supported way to run it.

`mdx-mcp.service` exists for the cases where a service manager still has to own
the process. Copy it into `~/.config/systemd/user/` and override the streams
there, for example `StandardInput=socket` / `StandardOutput=socket`. With the
unit's own `StandardInput=null` the server starts, sees end of input and exits
with status 0 immediately: expected for a stdio server with nothing attached,
which is why the unit sets `Restart=no` instead of looping.

## Release archives

`scripts/release.sh` is the free-standing packager: it runs
`cargo build --release --workspace`, strips the three binaries and writes
`dist/mdx-<version>-<target>.tar.gz` with the binaries, the desktop entry, the
`icon` and the README. Run it from the repository root (`scripts/release.sh`);
treat `dist/` as build output and keep it out of commits.
