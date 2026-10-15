# openboardview-mcp

An [MCP](https://modelcontextprotocol.io) server that lets AI assistants read PCB boardview files: look up
parts and nets, follow power rails, find what can short a rail to ground, render the board with highlights,
search the schematic, and leave notes that show up in [OpenBoardView](https://github.com/OpenBoardView/OpenBoardView).

It parses files with OpenBoardView's own C++ parsers (vendored at release 10.0.0), so a board reads exactly as
it does in the OpenBoardView GUI. Everything else is Rust.

## Supported formats

Every format OpenBoardView 10.0.0 reads: Test_Link `.brd` (plain and encoded), BRD2 (`BRDOUT:`), `.bdv`,
BVRAW 1 and 3 (`.bvr`), CAD (`###Panel` files), GenCAD, `.cst`, ASC (`format.asc`/`pins.asc`/`nails.asc`),
Altium/Protel ASCII PCB, and the encrypted `.fz`, `.cae` and XZZPCB `.pcb`. Encrypted formats need the same keys OpenBoardView needs (see [Configuration](#configuration)).
Cadence Allegro `.brd` is detected but, as in OpenBoardView, not supported.

## Tools

| Tool | What it does |
| --- | --- |
| `open_board` | Load a file and summarize it: format, counts, size, part kinds, ground nets, main rails, annotations, schematic. |
| `list_parts` | Parts filtered by name glob/substring, kind, side or part number. Paginated. |
| `get_part` | One part: every pin with its net and position, its nets and the other parts on each. |
| `list_nets` | Nets filtered by name, kind (ground/power/signal) or pin count; sort by name or size. |
| `get_net` | One net: every part and pin on it, counts by kind, test points. |
| `search` | Fuzzy search over part names, net names and part numbers. |
| `parts_between_nets` | Parts with pins on two nets; with one net, parts between it and ground (short suspects). |
| `trace_net` | Walk from a net across resistors, inductors, fuses, diodes and jumpers to connected nets. |
| `nearby_parts` | Parts physically close to a part or point, nearest first. |
| `power_rails` | Likely rails, largest first, with decoupling-cap counts and the inductors, ICs and FETs on each. |
| `render_board` | PNG of the board or a zoomed region, with highlighted parts and nets. |
| `list_annotations`, `add_annotation`, `update_annotation`, `delete_annotation` | Notes in OpenBoardView's own annotation database. |
| `schematic_info`, `link_schematic`, `search_schematic` | Find, link and full-text search the board's schematic PDF. |
| `open_in_viewer` | Open the board in the OpenBoardView GUI. |
| `compare_boards` | Diff two boards (e.g. revisions): parts, nets, part numbers, pin-to-net changes. |

Tools that take `path` default to the most recently opened board. Coordinates are in mils (1 mil = 0.0254 mm)
with Y growing upward, and the bottom side is drawn mirrored, all as in OpenBoardView.

Part kinds (capacitor, jumper, ...) and net kinds (ground, power, signal) are inferred from reference
designators and net names, since board files don't record them. The rules cover the common vendor conventions,
including the `P`-prefixed power-section parts on laptop boards (`PC`, `PR`, `PQ`, `PU`, `PJP`).

## Build

Requirements: Rust 1.88 or newer, a C/C++17 compiler, and git. zlib and SQLite are built in, and no SDL or
OpenGL is needed.

```sh
git clone --recurse-submodules=vendor/OpenBoardView <repo-url> openboardview-mcp
cd openboardview-mcp
git -C vendor/OpenBoardView submodule update --init --depth 1 src/utf8 src/mpc
cargo build --release
```

The binary is `target/release/openboardview-mcp`. To build against a different OpenBoardView checkout, set
`OBV_SRC=/path/to/OpenBoardView`; its `src/utf8` and `src/mpc` submodules must be checked out.

Check that a file parses:

```sh
target/release/openboardview-mcp dump path/to/board.brd          # summary
target/release/openboardview-mcp dump path/to/board.brd --json   # raw parser output
```

## Use with Claude Code

```sh
claude mcp add openboardview -- /absolute/path/to/target/release/openboardview-mcp
```

Other MCP clients use the same command over stdio. For example, in a JSON config:

```json
{
  "mcpServers": {
    "openboardview": { "command": "/absolute/path/to/target/release/openboardview-mcp" }
  }
}
```

Then ask things like *"Open ~/boards/x421ia.cad. +3VS is shorted to ground: what could cause it, and where are
those parts?"*

## Configuration

| Setting | Flag / environment | Default |
| --- | --- | --- |
| Decryption keys (`FZKey`, `CAEKey`, `XZZPCBKey`) | `--obv-config` / `OBV_CONFIG` | OpenBoardView's `obv.conf` (`~/.config/OpenBoardView/obv.conf` on Linux) |
| OpenBoardView executable for `open_in_viewer` | `--viewer` / `OBV_VIEWER` | `openboardview` |
| Parse timeout | `--parse-timeout <seconds>` | `120` |
| Log level (to stderr) | `OBV_MCP_LOG` | `info` |

Keys entered in OpenBoardView's preferences are picked up automatically. They are passed to the parser over a
pipe, never on a command line, and are removed from parser error messages.

`search_schematic` needs `pdftotext` from poppler (`poppler-utils` on Debian/Ubuntu, `poppler` on Arch and
Homebrew).

## Files it writes

Only the annotation and schematic tools write anything, and only where OpenBoardView itself would:

- Annotations go to `<board>_<ext>.sqlite3` next to the board (for example `x421ia.cad` becomes
  `x421ia_cad.sqlite3`), using OpenBoardView's schema. Deleting an annotation hides it, as the GUI does. The GUI
  reads annotations when it opens a board, so reopen the board to see new notes.
- `link_schematic` sets `PDFFilePath` in `<board>.conf`, the board settings file OpenBoardView uses.

## How it works

```
MCP client ──stdio──> openboardview-mcp (Rust: tools, board model, analysis, rendering)
                         │  JSON request on stdin (path + keys)
                         └─> openboardview-mcp parse-worker ──> OpenBoardView C++ parsers (static, via C ABI)
                                JSON board on stdout, parser log on stderr
```

- `build.rs` compiles OpenBoardView's `FileFormats/` sources plus `native/obv_ffi.cpp`, a small C ABI wrapper
  that replicates the GUI's format detection. A stand-in `SDL.h` (`native/shim/`) replaces SDL, which the
  parsers use only for logging.
- Each parse runs in a short-lived child process. The C++ parsers can crash on malformed files and change
  process-global locale state; isolating them means a bad file returns an error instead of taking down the
  server.
- `src/board/` ports OpenBoardView's `BRDBoard` (test pads, `UNCONNECTED` nets, pin numbering, generated
  outline) so parts and nets match the GUI. Parsed boards are cached and reloaded when the file changes.

## Development

```sh
cargo test                       # unit tests + end-to-end MCP test over stdio (tests/mcp_stdio.rs)
cargo clippy --all-targets -- -D warnings
cargo fmt
```

`compile_flags.txt` gives clangd the include paths for the C++ wrapper.

## License

MIT. The vendored OpenBoardView sources are MIT-licensed by their authors (see
`vendor/OpenBoardView/LICENSE`). Statically linked third-party code includes mpc (BSD), utf8.h
(Unlicense), zlib (zlib license) and SQLite (public domain).
