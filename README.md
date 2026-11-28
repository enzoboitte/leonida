<p align="center">
  <strong><a href="https://t.me/projectleonidal">Telegram — Leonida Legacy</a></strong>
</p>

<h1 align="center">
  <br>
  <a>
  <img src="https://raw.githubusercontent.com/ShinniUwU/leonida/89b95f3e2215cc7b078618c2878ded1567a3be18/assets/logo.png" alt="Leonida Legacy" width="200">
</a>

  <br>
  <b>Leonida Legacy</b>
  <br>
</h1>

<h4 align="center">Grand Theft Auto VI (Xbox Series) running natively on Windows, from your own copy.
</h4>

<p align="center"><i>Continued by <a href="https://github.com/orkan1k">orkan1k</a>.</i></p>

> *ItsNotRexed just wanted to play on the PC.*

<p align="center">
  <a href="#status">Status</a> |
  <a href="#getting-started">Getting started</a> |
  <a href="#updating">Updating</a> |
  <a href="docs/notes/">Fork notes</a> |
  <a href="#contact">Contact</a> |
  <a href="#credits">Credits</a> |
  <a href="#license">License</a>
</p>

Leonida Legacy is a compatibility layer. The game's x86-64 code runs unchanged inside our process, and Leonida
Legacy supplies the console platform underneath it: the loader, the runtime services, a Direct3D 12 back end and a
shader recompiler that turns the console's GPU code into DXIL on first use. See
[docs/architecture.md](docs/architecture.md).

It is a fork of [victory-gta6](https://github.com/fxceless/victory-gta6), itself a fork of
[VIctory](https://github.com/Marvisak/VIctory).

> [!IMPORTANT]
> This repository **does not contain the game, keys or an executable**. You need your own copy of the game.
> Everything the tools make from your files (translated shaders, caches, saves, logs) stays outside the repository
> and on your PC. Never share those files.

**Made for everyone's PC, not just ours.** Many projects like this are tuned on the developer's own machine and
nowhere else. Leonida Legacy aims to run as well as it can on your hardware, and to be usable by people who can't
code and don't have an AI assistant to help them. The program picks its settings for your graphics card
(`gpu-profiles.json`), so a change that helps one card is not forced on everyone. If it runs badly on your PC, tell us
your CPU, GPU, driver version and frame rate (see [Contact](#contact)); that is how new cards get a profile.

## Status

**Not playable yet: frame rates are low, and shadowed areas are black.** Tested on Windows 11 Pro,
AMD Ryzen 7 7800X3D, NVIDIA RTX 3080 12 GB, 32 GB RAM, Series X profile. The world frame rates below were measured
by victory-gta6 on an RTX 4070 and have not been measured again on this machine yet.

| Stage | State |
|-------|-------|
| Boot, menus, start screen | works, 30 fps |
| Opening cutscene | plays from start to finish, lit |
| Saves | the game autosaves a checkpoint and loads it again |
| First gameplay scene | renders characters, textures, sky and the sunlit exterior; HUD, pause menu and pad input work |
| Light in shadow | missing: ray-traced bounce light and shadows don't run yet, so anything out of direct sun is black |
| Frame rate in the world | Earlier measurements: 2-4 fps without the newer switches, about 10 fps in the first rooms with them; the measured base profile now applies to direct launches too |

Known problems:

- The first gameplay room is sometimes not drawn after the cutscene hands over to gameplay.
- Around 24 GB of memory is in use during play.
- Many changes are specific to this game and this GPU; some are marked `shortcut:` in the code.

The full measurement log, what each switch does and the debugging options are in
[docs/notes/](docs/notes/) (newest) and [docs/fork-notes.md](docs/fork-notes.md) (up to 2026-11-27).

## Getting started

Requirements: Windows 10/11 x64, a CPU with AVX2, a Direct3D 12 GPU with shader model 6.6 and mesh shaders, Rust
(pinned in `rust-toolchain.toml`), and several GiB of free Windows commit at startup (if `crashes.log` reports
Windows error 1455, close other applications or make the paging file bigger).

```powershell
cargo xtask install-hooks                # asset-policy check on every commit
cargo test --workspace                   # needs no game data

cargo build --release -p leonida
target\release\leonida.exe run --execute # asks for your game folder the first time
```

The first run asks for the folder of your copy of the game (the one with `MicrosoftGame.config` in it) and saves it to
`leonida.toml`, which git ignores. `leonida config` shows what is set, and `leonida.example.toml` lists the other
settings.

If startup names a missing game DLL, restore that file from your complete copy of the game, or choose the
folder containing the complete installation. Leonida checks required DLLs, including dependencies of other
DLLs, before running title code.

`run-gta6.ps1` does the build and the run in one step. The program itself selects the frame-rate settings from
`gpu-profiles.json`, including when launched directly through Wine. It asks for the game folder too, and whether
to remember it:

```powershell
powershell -ExecutionPolicy Bypass -File .\run-gta6.ps1
```

To switch between branches and start `run-gta6.ps1` from a window instead, build the launcher once:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-launcher.ps1
```

It creates `Leonida-Launcher.exe` in the repository's root folder (git ignores it; it stays when you switch
branches). Start it, pick a branch and click **Load and start**: it switches to that branch and
runs `run-gta6.ps1`. Switching discards your changes to tracked files, after asking. If Git is
missing, the launcher offers to install it with winget; if the Visual Studio C++ build tools or
Rust are missing, the window that starts the game offers to install them first (the same check as
`start-measure.cmd`). Under **Settings** you can set your game folder (passed as `-GameDir`) and
the launcher's language (English or French); settings are kept in
`%APPDATA%\LeonidaLauncher\config.json`. The first build installs the `ps2exe` module for your
Windows user and downloads the logo; without network it uses a plain logo.

`scripts\measure-gta6.ps1` does the same with the diagnostics build, the frame log and the card meter on, and saves
everything the game prints to `%LOCALAPPDATA%\leonida\logs`. `-SpeedSwitches off` runs it without the frame-rate
switches or your graphics overrides, for a comparison in the same place.

### Experimental Linux launch with Steam Flatpak and Proton

Leonida still builds as a Windows executable; Proton runs it on Linux. On Pop!_OS 22.04 with
Proton 11.0, an RTX 5090 and NVIDIA 580.173.02, this path reached the opening scene with a
newer vkd3d-proton build, then stopped at a game resource-pool breakpoint. Gameplay on Linux
has not been validated. You need the Steam Flatpak with Proton installed, a working
PulseAudio-compatible desktop socket, and an x64 `d3dcompiler_47.dll` from a Windows SDK
installation. [The Windows SDK includes this DLL][sdk-d3dcompiler].

Build the Windows executable on Linux with [cargo-xwin][cargo-xwin], or use a Windows release
build you made yourself. Follow cargo-xwin's setup instructions for Clang/LLVM, then run in this
repository:

```sh
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin --locked
cargo xwin build --release --locked --target x86_64-pc-windows-msvc -p leonida
```

Put the game executable named by `MicrosoftGame.config` beside that manifest and the rest of your
game files. On the tested NVIDIA PC, Proton 11.0's bundled vkd3d-proton 3.0.0 exited while
creating compute pipelines during world loading. For that combination, download vkd3d-proton
3.2.0 or newer from the [official package workflow][vkd3d-proton] and extract it outside this
repository. Its `x64` folder must contain matching `d3d12.dll` and `d3d12core.dll` files.
Then run:

```sh
./scripts/run-gta6-linux.sh \
  --game-dir "$HOME/Games/GTA6" \
  --proton "$HOME/.var/app/com.valvesoftware.Steam/data/Steam/steamapps/common/Proton 11.0" \
  --compiler "$HOME/WindowsSDK/x64/d3dcompiler_47.dll" \
  --vkd3d-dir "$HOME/Downloads/vkd3d-proton/x64"
```

Replace those four paths with yours. `--exe` selects another Leonida Windows executable. The
launcher reads the repository's `leonida.toml` if present; `--config` selects another settings
file. It also reads `gpu-profiles.local.json` beside that file. It creates a dedicated Wine prefix
with Proton's DXVK/vkd3d graphics DLLs and your compiler DLL. The prefix, shader cache and saves
stay under `~/.local/share/leonida/proton` (or `$XDG_DATA_HOME/leonida/proton`). `--state-dir`
chooses another location outside the repository. For symlinks to files in another directory,
pass `--read-only-dir` for each target directory. `--bind-only` checks imports without starting
the title.

The launcher copies the newer vkd3d-proton DLL pair only into its dedicated Wine prefix. If
your Proton already includes a working version, omit `--vkd3d-dir` to use its bundled pair.

The script grants [Flatpak's PulseAudio socket][flatpak-sockets] and checks for its presence inside
Steam's sandbox before starting the game. If the check fails, restore your desktop's
PulseAudio-compatible audio service and Steam's audio permission. The title stays offline. This
launcher is for compatibility, **not malware isolation**: Steam's app data can still be visible
to Wine. Only run game files you trust.

[sdk-d3dcompiler]: https://learn.microsoft.com/en-us/windows/desktop/directx-sdk--august-2009-
[cargo-xwin]: https://github.com/rust-cross/cargo-xwin
[flatpak-sockets]: https://docs.flatpak.org/en/latest/flatpak-command-reference.html
[vkd3d-proton]: https://github.com/HansKristian-Work/vkd3d-proton/actions/workflows/artifacts.yml

### Experimental: macOS on Apple Silicon (no picture yet)

Leonida Legacy can load the game on a Mac and start its code under Rosetta 2, but **it cannot show
a picture yet**: there is no Metal back end, so the game stops at its first graphics call. This is
for people who want to help with the port, not for playing. What works and what is missing is in
`docs/notes/` (the files with `macos` in their name).

1. Install the Xcode command line tools, Rosetta 2 and Rust. In Terminal:

   ```bash
   xcode-select --install
   softwareupdate --install-rosetta --agree-to-license
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. In the Leonida Legacy folder, build the Mac version:

   ```bash
   rustup target add x86_64-apple-darwin x86_64-pc-windows-msvc
   cargo build --release -p leonida --target x86_64-apple-darwin
   ```

3. Check what the game still needs, without running it (replace the path with the game's `.exe` in
   your own copy of the **Xbox Series** game; a PlayStation copy does not work):

   ```bash
   ./target/x86_64-apple-darwin/release/leonida run "/path/to/your/game/folder/the-game.exe"
   ```

   It lists every `missing` import. Add `--execute` to start the game; it stops at the first
   missing one and says which.

4. The game also needs Microsoft's C runtime DLLs, which a Windows PC has and a Mac does not:
   `ucrtbase.dll`, `vcruntime140.dll`, `vcruntime140_1.dll` and `msvcp140.dll`, the x64 ones.
   Microsoft lets them be copied beside a program (the Windows SDK has them under
   `Redist\ucrt\DLLs\x64`, the Visual C++ Redistributable installs the others). Put them in
   `~/.cache/leonida/data/runtime/`; `leonida run` prints that folder as `runtime dlls`.

### Graphics settings

The measured base profile is automatic. Experimental profiles such as `card-windows` remain opt-in.
To change a setting, add a `[graphics]` section to your `leonida.toml`; the example file lists every option:

```toml
[graphics]
async_submit = false
queue2 = 0
```

Only the options you write change. To disable automatic profiles, use `profiles = ["none"]` (or `profiles = []`)
in that section. Other settings you explicitly write still apply. To choose profiles yourself, use a list such
as `profiles = ["base", "my-test"]`; named profiles include the base settings and apply in catalog order.

For the optional NVIDIA shader compatibility fix, select `nvidia-code-tables`:

```toml
[graphics]
profiles = ["nvidia-code-tables"]
```

If you already have a `[graphics]` section, add or update its `profiles` line instead of adding
another section. Keep any profile names you already selected, then launch normally. Remove
`nvidia-code-tables` to disable it. This option is off by default. On the tested RTX 5090 with
Proton 11.0, it restored creation of six refused compute
pipelines; it does not establish correct gameplay or a frame-rate improvement. See the
[validation details](docs/fork-notes.md#r-7-large-code-literal-tables-on-nvidia-2026-11-26).

Environment overrides take precedence over TOML settings, which take precedence over profiles. For example,
an explicit `LEONIDA_QUEUE2=0` still wins. `LEONIDA_PROFILE` overrides the profile list, except the existing
diagnostics value `1`, which enables the CPU sampler and leaves graphics selection to the config. Run
`leonida config --graphics` to see the selected adapter, profiles, effective values and their sources as JSON;
it needs no game files. The scripts use this same result and leave your environment overrides intact.

The standard catalog is built into the executable. Put local profiles in `gpu-profiles.local.json` beside
your selected config file (the current folder when no config exists). They apply after the built-in catalog.
Profiles match the adapter used by Direct3D, including Wine's reported adapter identity. If discovery fails,
only profiles without card-specific conditions match. A local profile can cancel an earlier profile value
with `null`, but cannot override an explicit TOML or environment value. Game/cache/data paths, logging, RenderDoc startup
and WARP selection remain separate from graphics profiles.
GPU name patterns support case-insensitive `*`, `?`, `[abc]` and `[a-z]`; malformed patterns and unsupported
backtick escapes or negated character classes report an error. Unknown profile names also report an error.

### Testing for the project, with no programming experience

1. Download this repository (Code, Download ZIP) and unpack it, or clone it.
2. Double-click `start-measure.cmd`. If the Visual Studio C++ build tools or Rust are missing, it offers to install
   them with winget (about 3 GB, 5 to 15 minutes, asks first). The first build takes about 5 to 10 minutes.
3. Say yes when it offers to look for your copy of the game, and check the folder it found. If it finds nothing, drag
   the game's `.exe` from File Explorer into the window and press Enter. Answer its other questions, and play until
   the place you want to test.
4. Close the game window. The script offers a short test report: your graphics card, driver, CPU, RAM, Windows
   version, the switches that were on and the frame rates. It shows all of it before anything is shared, and never
   includes log files, game files, paths or your user name.
5. If you agree, it copies the report and opens `#test-results` on our [Discord](https://discord.gg/dxMD2XqyEJ).
   Paste it with Ctrl+V and send it. Nothing is uploaded unless you post it yourself.

Run it once with the speed switches on and once with them off, in the same place: the pair is what lets us add a
profile for your card to `gpu-profiles.json`. Reports are kept in `%LOCALAPPDATA%\leonida\reports`.

`scripts\shot-run.ps1` runs the game unattended and takes frame shots (`-ShotAt`, with `-Keys` to get through the menus) into
`%LOCALAPPDATA%\leonida\shots`. Shots come from your game: keep them on your PC.

If the GPU driver crashes while building one shader's pipeline, `LEONIDA_SKIP_PIPELINES=<16-digit shader hash>` keeps
that pipeline from ever reaching the driver (its draws are dropped).

Saves, settings and `crashes.log` are kept in `%LOCALAPPDATA%\leonida\data`, derived data in
`%LOCALAPPDATA%\leonida\cache`.

Input: XInput pads. With none connected, the keyboard acts as a pad while the game's window has focus:

| Keys | Pad |
|------|-----|
| W A S D / I J K L | left / right stick |
| arrows | D-pad |
| Enter, Backspace, X, Y | A, B, X, Y |
| Q / E, Z / C | shoulders, triggers |
| F / H | stick clicks |
| Esc, Tab | Menu, View |

### Testing the optional one-record clamp

An experimental optimization reduces GPU work before individual indirect draws. It improved a synthetic
RTX 5090 benchmark by 19.4%; gameplay and other cards are unmeasured, so it stays off by default.
[The measurements and limits are here](docs/fork-notes.md#newest-2026-11-26).

To compare it using your configured game, create `gpu-profiles.local.json` beside your selected config file
(normally in the checkout) with the following content. If
the file already exists, add only this profile object to its existing `profiles` array:

```json
{
  "profiles": [
    {
      "name": "single-clamp-test",
      "opt_in": true,
      "match": {},
      "env": { "LEONIDA_INDIRECT_SINGLE": "1" }
    }
  ]
}
```

Run this pair in PowerShell from the checkout. Both use the base settings; the second adds the new clamp:

```powershell
cargo build --profile diag -p leonida --features diagnostics
$clampSeries = Join-Path $env:LOCALAPPDATA 'leonida\runs\single-clamp'
powershell -ExecutionPolicy Bypass -File scripts\measure-run.ps1 `
  -Root $clampSeries -Label 01-base -Profile base
powershell -ExecutionPolicy Bypass -File scripts\measure-run.ps1 `
  -Root $clampSeries -Label 02-single -Profile single-clamp-test
```

Repeat the pair with new labels before judging frame times. Compare the pictures locally and share only
the figures from `summary.txt`. Ordinary launches keep this profile off.

## Updating

Get the newest version before you report a problem: it may already be fixed. In PowerShell, in the folder you
cloned:

```powershell
git switch main
git pull
powershell -ExecutionPolicy Bypass -File .\run-gta6.ps1
```

`run-gta6.ps1` builds the new version before it starts the game. Your game folder, saves and settings are outside
the code (`leonida.toml` and `%LOCALAPPDATA%\leonida`), so updating keeps them.

- **First time only:** if you cloned before the project moved to ProjectLeonidaL, point your copy at it once:
  `git remote set-url origin https://github.com/ProjectLeonidaL/leonida.git`. If `git pull` then says your branch
  has no upstream, run `git branch -u origin/main` once.
- **`git pull` refuses because files changed:** you edited something in the folder. `git stash` puts your changes
  aside, then `git pull` again; `git stash pop` brings them back. Your own GPU settings belong in
  `gpu-profiles.local.json`, which git leaves alone.
- **The first run after an update stutters** while shaders are translated again. Let the warm-up run when the script
  asks, or play a minute and start again.
- **Don't have git?** Download the code as a ZIP from the green *Code* button on GitHub, unpack it over a fresh
  folder, and copy your `leonida.toml` across.

## Contact

- **Discord:** [discord.gg/dxMD2XqyEJ](https://discord.gg/dxMD2XqyEJ)
- **Telegram:** [@wouldscams](https://t.me/wouldscams)
- **News & updates:** [Leonida Legacy on Telegram](https://t.me/projectleonidal)

## Credits

- **[victory-gta6](https://github.com/fxceless/victory-gta6)** by fxceless is the fork this repository continues
  under the name Leonida Legacy, with its history kept.
- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and its contributors is the base of everything
  here: the loader, the runtime services, the Direct3D 12 back end and the shader recompiler.
- **[XCL](https://github.com/ivall/xcl)** by ivall and its contributors (GPL-2.0-only) was used as a reference for
  how the same game behaves under another layer. Fixes inspired by it were written anew in VIctory's code.
- The guard over every view of a memory page is code by **myokex**
  ([victory-amd](https://github.com/myokex/victory-amd), commit `c6dc4d8`), which adapts pull request #115 by
  Spexivus, its correction by joroyt and a part of VIctory #1122 by tightfall. The read-beside fix for kernel reads
  into title memory is by **Joshua Odom** (commit `10ea38e` of the same fork family).
- Logo: ["Grand Theft Auto VI" icon by SlyDevil](https://www.steamgriddb.com/icon/122618) on SteamGridDB.
- The victory-gta6 changes were developed with [Claude Code](https://claude.com/claude-code).

Grand Theft Auto and all related names are trademarks of their owners. This project is not affiliated with or
endorsed by them.

## License

GPL-3.0-or-later, like victory-gta6 and VIctory. See [LICENSE](LICENSE).
