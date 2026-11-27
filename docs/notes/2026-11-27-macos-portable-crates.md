# The platform-independent crates build and test on macOS

These crates check, test and pass clippy with `-D warnings` on macOS (Apple M5, arm64, macOS 26.6,
toolchain 1.98.1 from `rust-toolchain.toml`, 2026-11-27): `leonida-core`, `leonida-pe`,
`leonida-formats`, `leonida-pipeline`, `leonida-game`, `leonida-shader` and `xtask`. So the
translator, the texture and audio formats and the asset-policy check can be worked on from a Mac:

```bash
P=(-p leonida-core -p leonida-pe -p leonida-formats -p leonida-pipeline -p leonida-game -p leonida-shader -p xtask)
cargo test "${P[@]}" --locked
cargo clippy "${P[@]}" --all-targets --locked -- -D warnings
```

P is an array so that zsh, the default shell on a Mac, splits it.

Two changes were needed, both in tests:

- Four `leonida-pe` tests read the test binary itself as a PE image. Off Windows it is a Mach-O or
  ELF file, so they are ignored there, with the reason.
- A `leonida-shader` test only reads its DXIL and two imports from the Windows-only validator and
  host-pipeline blocks. Off Windows they now no longer warn.

Measured on that Mac: 285 tests pass, 10 ignored (4 of them the PE tests above, the rest were
already ignored), none fail. The Windows-only parts of the shader tests (DXIL validation, running on
the host card) do not run there, so a translator change still needs a Windows run before its pull
request.

`leonida-loader`, `leonida-hle`, `leonida-gpu` and `apps/leonida` don't build off Windows: they call
`windows-sys` and the host's D3D12 directly. `cargo check --workspace` therefore still fails on macOS;
use the `-p` list above.
