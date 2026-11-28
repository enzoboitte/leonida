//! `leonida`: the command-line front end.
//!
//! Every command reads the user's own files and writes only to stdout or to
//! the user's cache directory (outside the repository).

mod census;
mod graphics;
mod inspect;
mod run;
#[cfg(target_os = "macos")]
mod run_macos;
mod shader;
mod translate;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "leonida",
    version,
    about = "Run an Xbox Series title natively on Windows"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Summarise one PE image: headers, sections, imports, TLS, build id.
    Inspect {
        file: PathBuf,
        /// List every imported function.
        #[arg(long)]
        imports: bool,
        /// List every exported function.
        #[arg(long)]
        exports: bool,
    },
    /// Aggregate the imports of every PE image under the given paths and
    /// show which ones our runtime covers. Defaults to the configured
    /// game and test-title directories.
    Census {
        paths: Vec<PathBuf>,
        /// Do not count host pass-through as covered.
        #[arg(long)]
        strict: bool,
        /// List every missing `dll!function`, most used first.
        #[arg(long)]
        missing: bool,
    },
    /// List the `dll!export` pairs an import library describes.
    LibExports {
        lib: PathBuf,
        /// List every export, not just per-DLL counts.
        #[arg(long)]
        list: bool,
    },
    /// Find shader containers in files or directories (also inside engine
    /// archives, if uncompressed) and report their layout.
    ShaderInfo {
        paths: Vec<PathBuf>,
        /// Print one line per container found.
        #[arg(long)]
        verbose: bool,
        /// Also list the decoded native instructions (implies --verbose).
        #[arg(long)]
        disasm: bool,
    },
    /// Run every shader found (containers, SGD5 files) through the
    /// translator and report what it cannot handle yet.
    Translate {
        paths: Vec<PathBuf>,
        /// Check emitted DXIL with dxil.dll's validator (`LEONIDA_DXIL`,
        /// else the newest Windows SDK's).
        #[arg(long)]
        validate: bool,
        /// Also create a host D3D12 pipeline for each emitted shader.
        #[arg(long)]
        pipelines: bool,
        /// Print one line per program and outcome.
        #[arg(long)]
        list: bool,
        /// Worker threads (default: every logical core).
        #[arg(long)]
        jobs: Option<usize>,
        /// Warm the run's shader cache: a file of shader hashes (the 16
        /// hex digits a run's log names a shader by, anywhere in a line);
        /// the containers with those hashes are translated into the
        /// cache and nothing else is done. Set `LEONIDA_HOST_WAVE` and
        /// `LEONIDA_VIEW_LIMIT` to what the run's log says of the host.
        #[arg(long)]
        warm: Option<PathBuf>,
    },
    /// Prepare, map and bind a title executable; with --execute, run it.
    Run {
        /// The title's executable. Default: the one the game folder's
        /// MicrosoftGame.config names (the folder is asked for once and
        /// saved to leonida.toml).
        exe: Option<PathBuf>,
        /// What unimplemented imports do when called.
        #[arg(long, value_enum, default_value_t = Unimplemented::Trap)]
        unimplemented: Unimplemented,
        /// Disable host pass-through (everything must be implemented by us).
        #[arg(long)]
        strict: bool,
        /// Actually start the title. Without it, stop after binding.
        #[arg(long)]
        execute: bool,
        /// The console model the title sees (device type, memory).
        #[arg(long, value_enum, default_value_t = Console::SeriesX)]
        console: Console,
        /// Arguments for the title, after `--` (it reads the whole command
        /// line; a title that relaunches itself passes some).
        #[arg(last = true)]
        title_args: Vec<String>,
    },
    /// Show the resolved configuration and check it.
    Config {
        /// First save this folder as the game folder in leonida.toml.
        #[arg(long)]
        game_dir: Option<PathBuf>,
        /// Print the effective graphics settings as JSON, without running a title.
        #[arg(long, conflicts_with = "game_dir")]
        graphics: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Unimplemented {
    Trap,
    Zero,
}

#[derive(Clone, Copy, ValueEnum)]
enum Console {
    SeriesX,
    SeriesS,
}

fn main() -> Result<()> {
    leonida_core::logging::init("info");
    // The audio recorder `run` starts for `LEONIDA_AUDIO_CAPTURE`.
    #[cfg(all(windows, feature = "diagnostics"))]
    if leonida_hle::modules::audio::capture::child() {
        return Ok(());
    }
    let cli = Cli::parse();
    match cli.command {
        Command::Inspect {
            file,
            imports,
            exports,
        } => inspect::run(&file, imports, exports),
        Command::Census {
            paths,
            strict,
            missing,
        } => census::run(paths, strict, missing),
        Command::LibExports { lib, list } => inspect::lib_exports(&lib, list),
        Command::ShaderInfo {
            paths,
            verbose,
            disasm,
        } => shader::run(paths, verbose || disasm, disasm),
        Command::Translate {
            paths,
            validate,
            pipelines,
            list,
            jobs,
            warm,
        } => {
            graphics::initialize(&leonida_core::Config::load()?)?;
            // The warm-up keys its translations on the same host facts a run
            // gets from the device; the switches below still override them.
            if warm.is_some() && !leonida_hle::modules::d3d12x::probe_translation_host() {
                eprintln!("no host D3D12 device: the warm-up's shaders may not match a run's cache");
            }
            // `LEONIDA_VIEW_LIMIT=<null slot>`: translate as on a host whose view heap is capped
            if let Some(n) = std::env::var("LEONIDA_VIEW_LIMIT").ok().and_then(|v| v.parse().ok()) {
                leonida_shader::emit::set_view_heap_limit(n);
            }
            // `LEONIDA_HOST_WAVE=32`: translate as for a GPU whose waves are at most 32 lanes
            if let Some(n) = std::env::var("LEONIDA_HOST_WAVE").ok().and_then(|v| v.parse().ok()) {
                leonida_shader::emit::set_host_max_wave(n);
            }
            if let Some(file) = warm {
                return translate::warm(paths, &file, jobs);
            }
            translate::run(paths, validate, pipelines, list, jobs)
        }
        Command::Run {
            exe,
            unimplemented,
            strict,
            execute,
            console,
            title_args: _,
        } => {
            let exe = match exe {
                Some(exe) => exe,
                None => default_exe()?,
            };
            run::run(&exe, unimplemented, strict, execute, console)
        }
        Command::Config { game_dir, graphics } => {
            if graphics {
                let plan = graphics::plan(&leonida_core::Config::load()?)?;
                println!("{}", plan.to_json()?);
                return Ok(());
            }
            if let Some(dir) = game_dir {
                check_game_dir(&dir)?;
                let saved = leonida_core::Config::save_game_dir(&dir)?;
                println!("game folder saved to {}", saved.display());
            }
            show_config()
        }
    }
}

/// The executable the game folder's MicrosoftGame.config names. With no
/// folder configured and a terminal to ask in, the folder is asked for and
/// saved to the config file.
fn default_exe() -> Result<PathBuf> {
    use std::io::IsTerminal;
    let cfg = leonida_core::Config::load()?;
    let dir = if let Some(dir) = cfg.game_dir.clone() {
        dir
    } else if std::io::stdin().is_terminal() {
        ask_game_dir()?
    } else {
        cfg.require_game_dir()?.to_owned()
    };
    let manifest = dir.join("MicrosoftGame.config");
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let name = manifest_exe(&text)
        .with_context(|| format!("{} names no executable", manifest.display()))?;
    Ok(dir.join(name))
}

/// Asks for the game folder until one with a MicrosoftGame.config is given,
/// then saves it.
fn ask_game_dir() -> Result<PathBuf> {
    use std::io::{BufRead, Write};
    println!("No game folder is set. Enter the folder of your copy of the game");
    println!("(the one with MicrosoftGame.config in it):");
    loop {
        print!("> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line)? == 0 {
            anyhow::bail!("no game folder given");
        }
        let dir = PathBuf::from(line.trim().trim_matches('"'));
        if dir.as_os_str().is_empty() {
            continue;
        }
        if let Err(e) = check_game_dir(&dir) {
            println!("{e:#}, try again:");
            continue;
        }
        let saved = leonida_core::Config::save_game_dir(&dir)?;
        println!("game folder saved to {}", saved.display());
        return Ok(dir);
    }
}

/// A game folder holds the title's MicrosoftGame.config and lies outside
/// the repository.
fn check_game_dir(dir: &Path) -> Result<()> {
    anyhow::ensure!(
        dir.join("MicrosoftGame.config").is_file(),
        "no MicrosoftGame.config in {}",
        dir.display()
    );
    leonida_core::ensure_outside_repo(dir)?;
    Ok(())
}

/// The `Name` of the manifest's first `<Executable>` element (not
/// `<ExecutableList>`, which holds them).
fn manifest_exe(manifest: &str) -> Option<&str> {
    let mut rest = manifest;
    while let Some(start) = rest.find("<Executable") {
        rest = &rest[start + "<Executable".len()..];
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let tag = &rest[..rest.find('>')?];
        let (at, _) = tag
            .match_indices("Name=\"")
            .find(|(i, _)| tag[..*i].ends_with(char::is_whitespace))?;
        let value = &tag[at + "Name=\"".len()..];
        let name = &value[..value.find('"')?];
        return (!name.is_empty()).then_some(name);
    }
    None
}

fn show_config() -> Result<()> {
    let cfg = leonida_core::Config::load()?;
    let status = |p: &Option<PathBuf>| match p {
        Some(p) if p.exists() => format!("{}", p.display()),
        Some(p) => format!("{}  (does not exist)", p.display()),
        None => "(not set)".to_owned(),
    };
    println!(
        "config file : {}",
        cfg.source
            .as_ref()
            .map_or("(none)".into(), |p| p.display().to_string())
    );
    println!("game_dir    : {}", status(&cfg.game_dir));
    println!("cache_dir   : {}", cfg.cache_dir.display());
    println!("data_dir    : {}", cfg.data_dir.display());
    for t in &cfg.test_titles {
        println!("test_title  : {}", status(&Some(t.clone())));
    }
    for p in cfg
        .game_dir
        .iter()
        .chain(&cfg.test_titles)
        .chain([&cfg.cache_dir, &cfg.data_dir])
    {
        if let Err(e) = leonida_core::ensure_outside_repo(p) {
            println!("POLICY VIOLATION: {e}");
        }
    }
    Ok(())
}

/// Paths given on the command line, or the configured game + test titles.
pub(crate) fn default_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    if !paths.is_empty() {
        return Ok(paths);
    }
    let cfg = leonida_core::Config::load()?;
    let all: Vec<PathBuf> = cfg.game_dir.into_iter().chain(cfg.test_titles).collect();
    anyhow::ensure!(
        !all.is_empty(),
        "no paths given and none configured (see leonida.example.toml)"
    );
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::manifest_exe;

    #[test]
    fn manifest_exe_is_the_first_executable() {
        let text = r#"<Game><ExecutableList><Executable Name="title.exe" Id="Game" OverrideDisplayName="The Title" /><Executable Name="other.exe" /></ExecutableList></Game>"#;
        assert_eq!(manifest_exe(text), Some("title.exe"));
        assert_eq!(manifest_exe("<Game></Game>"), None);
        assert_eq!(manifest_exe(r#"<Executable Name="" />"#), None);
        assert_eq!(manifest_exe("<Executable
  OverrideName=\"x\"
  Name=\"a.exe\"/>"), Some("a.exe"));
    }
}
