# Shader fixes from trustno1-coder/VIctory-NVIDIA

[trustno1-coder/VIctory-NVIDIA](https://github.com/trustno1-coder/VIctory-NVIDIA) (GPL-3.0) shares
an ancestor with this tree (`a26859f`, 2026-11-25); since then it has 1,253 commits, 152 touching
`victory-shader`. Their translator fixes help every host, the Mac port too, since the shaders are
translated by the same code there. Ported here, renamed as `scripts/rename-from-upstream.ps1` does,
each with its origin in its message:

| Theirs | What |
|--------|------|
| `a12e0ae` | `unspill`'s table covers every scalar code: a VOP3 compare whose destination field holds a constant's code panicked (index 215 of 128) instead of failing to translate. Its test panicked on this tree first |
| `c1d8179` | in "halves" mode (a console wave of 64 as two host waves of 32) a mask read as a number gives lanes 32-63 its high dword's bits |
| `f780f33` | MD5 round constants precomputed, no allocation per block |
| `747805e` | mesh programs' stage I/O does not parse vertex inputs |
| `7183796` | empty loop latches holding only dead phis folded |
| `8d30f03` | replaced phi chains compressed in `remove_trivial_phis` |
| `df526e5` | a card test of the integer `v_min3`/`v_max3`/`v_med3` |
| `81fa73c` | SP3 text prints only the operands an instruction has |
| `f084578` | lane guards reused for unchanged EXEC within a block |

Left out: `0745a5d` and `feb90ad` fix code this tree does not have (their wide scalar loads and
their division fixup came in other commits); `eea3b01` does not build here; `a850b06` (folding
conditional branches to their fallthrough) breaks one of this tree's tests. The rest of their
shader commits are large and tangled with their D3D12 changes; they are for one at a time.

## Tested

On a Mac only (Apple M5, macOS 26.6): `cargo test -p leonida-shader --locked`: 218 passed, 2
ignored, before each next port and after the last. The tests that run on a D3D12 card (the halves
test, the `v_min3` test) were not run. Clippy for `x86_64-pc-windows-msvc` from the Mac: clean.
**Not measured in the game**: a translator change re-translates every shader, so the picture and
the frame time need a Windows run (`scripts/measure-run.ps1`) before this is merged.
