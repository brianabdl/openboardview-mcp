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


## Build

Requirements: Rust 1.88 or newer, a C/C++17 compiler, and git. zlib and SQLite are built in, and no SDL or
OpenGL is needed.

```sh
git clone --recurse-submodules=vendor/OpenBoardView https://github.com/brianabdl/openboardview-mcp.git
cd openboardview-mcp
git -C vendor/OpenBoardView submodule update --init --depth 1 src/utf8 src/mpc
cargo build --release
```

The binary is `target/release/openboardview-mcp`. To build against a different OpenBoardView checkout, set
`OBV_SRC=/path/to/OpenBoardView`; its `src/utf8` and `src/mpc` submodules must be checked out.

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

## License

MIT. The vendored OpenBoardView sources are MIT-licensed by their authors (see
`vendor/OpenBoardView/LICENSE`). Statically linked third-party code includes mpc (BSD), utf8.h
(Unlicense), zlib (zlib license) and SQLite (public domain).
