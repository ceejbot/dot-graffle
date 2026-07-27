# dot-graffle

[![Tests](https://github.com/ceejbot/dot-graffle/actions/workflows/test.yaml/badge.svg)](https://github.com/ceejbot/dot-graffle/actions/workflows/test.yaml) [![Dependencies](https://github.com/ceejbot/dot-graffle/actions/workflows/audit.yaml/badge.svg)](https://github.com/ceejbot/dot-graffle/actions/workflows/audit.yaml)

Convert from dot to omnigraffle and back. Mostly.

[OmniGraffle](https://www.omnigroup.com/omnigraffle) is the best human-usable diagramming software there is and has been for a long time. If you prefer having a computer lay out your diagrams, [graphviz](https://graphviz.org) is the best there is. Why not put peanut butter on your chocolate?

## Install

```shell
brew install ceejbot/tap/dot-graffle graphviz
cargo install dot-graffle
```

You want `graphviz` available to go from `dot` to `graffle`. The tool shells out to graphviz for layout while converting. Without it the nodes land on a plain grid and clusters are dropped.

## Usage

Give it files. Each one is converted to its sibling: a `.dot` becomes a `.graffle` next to it, and a `.graffle` becomes a `.dot`. The direction is chosen per file from its extension, so you can mix the two freely.

```shell
dot-graffle foo.dot bar.dot          # writes foo.graffle and bar.graffle
dot-graffle /path/to/diagrams/*.dot  # writes a .graffle beside each .dot
dot-graffle foo.dot bar.graffle      # writes foo.graffle and bar.dot
```

By default it refuses to overwrite an existing output file — handy when a `.graffle` is something you've been editing by hand. Pass `--force` (`-f`) to overwrite. If one file in a batch fails, the rest still convert; the exit code is nonzero if any failed.

With no files, the tool reads from `stdin` and writes to `stdout`. It infers conversion from content: a `.graffle` bundle vs `.dot` text, so it works on whatever it gets.

```shell
cat input.graffle | dot-graffle > output.dot      # detected as graffle, emits dot
dot-graffle < input.dot > output.graffle          # detected as dot, emits graffle
```

## Limitations

The conversion is faithful but not lossless. The two formats aren't symmetric. DOT describes a graph and lets the layout engine place it, while OmniGraffle stores placed shapes. Each direction drops what it can't express.

`dot` → `graffle` limitations:

- Parallel edges between the same pair of nodes collapse to one (last wins).
- HTML-like table labels keep their text but lose the table's borders.
- Colors resolve hex (`#rgb`, `#rrggbb`) and a small set of named colors; anything else is dropped.

`graffle` → `dot` has more limitations. It drops position and edge routing entirely. DOT has no coordinates, so graphviz re-lays the graph out when you convert back.

- Node names come from each shape's visible label (uniquified when labels repeat), not OmniGraffle's internal identity.
- Rich text flattens to plain text; the font name, size, and color survive, but bold/italic/underline don't.
- Custom stencil shapes fall back to a plain box, and decorated multi-ring variants (`doublecircle`, `Mdiamond`,
  and so on) fold onto their base shape.
- Only the first canvas is converted; a multi-sheet document prints a note and drops the rest. (I can
  probably handle this better in future.)
- Lines not connected to two shapes are decorative, and carry no edge into the graph.

## Hacking

`just setup` installs all dependencies. `cargo build` does the usual. There are other justfile conveniences.

## LICENSE

Parity 7.0.0
