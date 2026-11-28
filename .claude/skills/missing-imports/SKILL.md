---
name: missing-imports
description: >-
  Turn the imports and D3D12.X methods the title needs but Leonida Legacy does not implement into a ranked
  list of fixes, each with the file it belongs in: an hle_export! in a leonida-hle module, a new module, a
  method of a leonida-gpu object, or a console path. Reads leonida census, the bind report of leonida run and
  the run's log. Use when the user says "unimplemented import", "missing import", "what does the game still
  need", "method not implemented", "E_NOTIMPL", or after boot-triage ranks a missing import as the stop.
---

# Missing imports

The title's code runs unchanged; everything it expects from the console (the system DLLs, the GDK runtime,
the D3D12.X device) has to come from us. Every missing piece shows up as an import with no implementation,
a method that answers `E_NOTIMPL`, or a call that fails. This skill ranks them and says where each fix goes.
The method comes from the backend skill of
[Jakeschincariol/console-skill](https://github.com/Jakeschincariol/console-skill) (MIT, see
[LICENSE](LICENSE)), rewritten for this repository. Read [CLAUDE.md](../../../CLAUDE.md) first.

It reads; it does not write the code for you. Some fixes are one line in a table, others a whole area.

## What the title needs, before running it

None of these run title code.

```powershell
target\release\leonida.exe census --missing            # every missing dll!function, most used first
target\release\leonida.exe census --strict --missing   # the same, host pass-through not counted as covered
target\release\leonida.exe run                         # bind only: "bound ...: N hle, N host, N title, N missing"
target\release\leonida.exe inspect <image> --imports   # one image's imports
```

`census` with no paths reads the configured game folder. Its table has one row per DLL: functions
imported, served by us (`hle`), passed to the host, missing.

Rank by:

1. the import that trapped in the last run (`--unimplemented trap`, the default, aborts at the first call;
   [boot-triage](../boot-triage/SKILL.md) finds it in `crashes.log`);
2. imports that returned 0 during a run with `--unimplemented zero` (each is logged once), in the order
   they were called;
3. `method not implemented` warnings from `crates/leonida-gpu/src/object.rs`: object, slot, caller;
4. the rest of `census --missing`, by number of images using them.

## Where each fix goes

| What is missing | Where the fix goes |
|---|---|
| an export of a DLL a module already serves | an `extern "win64"` function named exactly like the import, listed with `hle_export!` in that module's `module()`, as `crates/leonida-hle/src/modules/xmem.rs` does |
| a DLL no module serves | a new file in `crates/leonida-hle/src/modules/`, declared in `mod.rs` and pushed in `all()`; its `dlls` list may end a name with `*` to cover versioned API sets (`crates/leonida-hle/src/registry.rs`) |
| a D3D12.X method that answers `E_NOTIMPL` | the object's table in `crates/leonida-gpu/src/` (`object.rs` names the kind and the slot); device methods and pipeline creation are in `device.rs` |
| a file the title cannot open | `crates/leonida-hle/src/modules/files.rs`: the package is mounted as `G:` and temporary files as `T:` (`apps/leonida/src/run.rs`) |
| an import that should go to the PC's own DLL | the host pass-through; `leonida run --strict` turns it off to check what still depends on it |

A new function comes with a test that needs no game, in the module's own `#[cfg(test)]` block as
`xmem.rs` has, or under `crates/leonida-hle/tests/`.

## The method

1. **Read the state file** (`%LOCALAPPDATA%\leonida\agent-state.md`, see
   [boot-triage](../boot-triage/SKILL.md)): the run command, the open stops, the fixes already tried.
2. **Rank** with the commands above and the latest run's log.
3. **Find out what the function has to do before writing it.** Its public reference page gives the
   signature; the title's call sites give the arguments it passes and what it does with the result
   ([trace-import](../trace-import/SKILL.md)). Read `docs/notes/` and `docs/fork-notes.md` too: much is measured there.
   Quote where each fact comes from, or call it a guess.
4. **Write the smallest real implementation.** Returning success with the outputs unset is the bug the
   `trap` default exists to catch; return a documented failure the title handles rather than a success it
   cannot use. Code that approximates the console gets a `shortcut:` comment; a behaviour that might break
   a picture or lose the device goes behind a `LEONIDA_*` switch, off by default, noted in
   a note under `docs/notes/`. Every new `unsafe` block gets a `// SAFETY:` comment.
5. **Test without the game:** `cargo test -p leonida-hle --locked` (or the crate you changed), then
   `cargo clippy --workspace --all-targets --locked -- -D warnings` and compare with `main`: no new warning.
6. **Run again with [boot-triage](../boot-triage/SKILL.md)** and compare with the run before. Fixed:
   update the state file. Same stop after the fix: count a strike; at three, stop guessing.
7. **Write it down** in a new note under `docs/notes/` when something was measured, with how.

Before writing code, say in one paragraph what will change, why it fixes the cause, what it could break,
and whether that comes from reading the code or from memory.

## Do not

- make an import succeed silently to get past it, or switch a run to `--unimplemented zero` and call that
  a fix;
- add a network call: the title must not reach the network (`crates/leonida-hle/src/modules/net.rs`);
- touch keys, licensing or protection: if a call turns out to need one, write that down and stop;
- quote the title's bytes, strings or disassembly in code, comments, tests or pull requests. Import names
  and `module+offset` addresses are fine.
