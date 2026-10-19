# HD icon files (`icons-hd.bni`, `icons_STAR-hd.bni`)

An optional extra for bots and modern clients: the same chat icons a server hands out in
`icons.bni` and `icons_STAR.bni`, four times as large. A server that has them serves them over
BNFTP like any other file in its `[files] dir`; nothing in `bnetccd` names them, so classic
clients never see them and keep getting the 28 × 14 icons they expect (`SID_GETICONDATA` still
names `icons.bni`).

| File | Copy of | Icons |
|---|---|---|
| `icons-hd.bni` | `icons.bni` | 112 × 56 |
| `icons_STAR-hd.bni` | `icons_STAR.bni` | 112 × 56 |

## Format

Exactly the BNI of `crates/bnetcc-proto/src/bni.rs`, version 1: the same entries in the same
order with the same chat flags and product codes, each entry's width and height four times the
original's, and one stacked TGA (type 10, 24-bit, top-down, run-length encoded) whose width is
four times the original image's. A parser for `icons.bni` reads it unchanged; only the sizes
differ. Pick an icon by flags or code as usual, and take it from the image at the HD offsets.

## Getting them

Ask for the file by name over BNFTP (protocol byte `0x02`), as for `icons.bni`. `SID_GETFILETIME`
(`0x33`) answers for them too, so a bot can keep a cached copy and refresh it when the date
changes. Ask only servers you know carry them: elsewhere the request simply fails.

## Making them

The icons are upscaled by Real-ESRGAN from the operator's own `icons.bni`, so like it they are
game data: never committed here (see [LEGAL.md](LEGAL.md)). us.bnet.cc carries them since
2026-10-19.
