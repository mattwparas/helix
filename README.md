<div align="center">

<h1>
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="logo_dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="logo_light.svg">
  <img alt="Helix" height="128" src="logo_light.svg">
</picture>
</h1>

[![Build status](https://github.com/helix-editor/helix/actions/workflows/build.yml/badge.svg)](https://github.com/helix-editor/helix/actions)
[![GitHub Release](https://img.shields.io/github/v/release/helix-editor/helix)](https://github.com/helix-editor/helix/releases/latest)
[![Documentation](https://shields.io/badge/-documentation-452859)](https://docs.helix-editor.com/)
[![GitHub contributors](https://img.shields.io/github/contributors/helix-editor/helix)](https://github.com/helix-editor/helix/graphs/contributors)
[![Matrix Space](https://img.shields.io/matrix/helix-community:matrix.org)](https://matrix.to/#/#helix-community:matrix.org)

</div>

![Screenshot](./screenshot.png)

A [Kakoune](https://github.com/mawww/kakoune) / [Neovim](https://github.com/neovim/neovim) inspired editor, written in Rust.

The editing model is very heavily based on Kakoune; during development I found
myself agreeing with most of Kakoune's design decisions.

For more information, see the [website](https://helix-editor.com) or
[documentation](https://docs.helix-editor.com/).

All shortcuts/keymaps can be found [in the documentation on the website](https://docs.helix-editor.com/keymap.html).

[Troubleshooting](https://github.com/helix-editor/helix/wiki/Troubleshooting)

# Changes in this fork

This repo tracks [mattwparas/helix](https://github.com/mattwparas/helix)'s
`steel-event-system` branch (upstream Helix plus the
[Steel](https://github.com/mattwparas/steel) plugin system, customizable
statusline, and code actions on save) and adds the following on top:

- **File watching with external-change auto-reload** — unmodified buffers
  reload automatically when their files change on disk (macOS backend via
  filesentry); modified buffers surface a conflict warning instead
  (`helix-view/src/document.rs`, `[editor.auto-reload]`).
- **Vim-style handling of externally deleted files** — when an open file is
  deleted from disk the buffer is kept and marked modified rather than
  closed; `:w` recreates the file.
- **Snappy `:q` with LSP servers running** — the shutdown flush wait is
  capped so quitting no longer hangs ~1s per language server.
- **Image and PDF rendering in the editor** — `:open` on an image
  (png/jpeg/gif/webp/svg/…) or PDF renders it in the view via the kitty
  graphics protocol (unicode placeholder placements), scaled to fit,
  aspect-preserved, read-only. PDFs support `:media-next-page`,
  `:media-prev-page`, and `:media-goto-page N` (bindable like any typable
  command). Auto-detected on kitty/ghostty-family terminals; configurable
  with `editor.image-rendering = "auto" | "kitty" | "disabled"`. PDF pages
  rasterize through `pdftoppm` (poppler); non-PNG images convert through
  `magick`/`convert`/`sips`, whichever is present. Terminals without
  graphics support get a text fallback instead of binary garbage.
- **Steel/event fixes** — `send_blocking` no longer panics without a tokio
  reactor; Steel pinned past a thread-spawning bug.

# Features

- Vim-like modal editing
- Multiple selections
- Built-in language server support
- Smart, incremental syntax highlighting and code editing via tree-sitter

Although it's primarily a terminal-based editor, I am interested in exploring
a custom renderer (similar to Emacs) using wgpu.

Note: Only certain languages have indentation definitions at the moment. Check
`runtime/queries/<lang>/` for `indents.scm`.

# Installation

[Installation documentation](https://docs.helix-editor.com/install.html).

[![Packaging status](https://repology.org/badge/vertical-allrepos/helix-editor.svg?exclude_unsupported=1)](https://repology.org/project/helix-editor/versions)

# Contributing

Contributing guidelines can be found [here](./docs/CONTRIBUTING.md).

# Getting help

Your question might already be answered on the [FAQ](https://github.com/helix-editor/helix/wiki/FAQ).

Discuss the project on the community [Matrix Space](https://matrix.to/#/#helix-community:matrix.org) (make sure to join `#helix-editor:matrix.org` if you're on a client that doesn't support Matrix Spaces yet).

# Credits

Thanks to [@jakenvac](https://github.com/jakenvac) for designing the logo!
