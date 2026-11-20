//! `carbontracker init slurm`: a wrapper that runs a Slurm script under
//! carbontracker, written next to it. The script itself is never changed.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::init::Console;
use crate::args::Args;

/// Marks a wrapper `init slurm` wrote, so it is replaced, never wrapped.
const MARK: &str = "Written by carbontracker init";
/// Folders never searched for scripts.
const SKIPPED: [&str; 6] = [".git", "target", "node_modules", ".venv", "venv", "__pycache__"];
/// How many folders below the current one the search goes.
const MAX_DEPTH: usize = 3;
/// Larger files are not read.
const MAX_BYTES: u64 = 256 * 1024;

/// Wrap `script`; without one, list the scripts here and, when
/// `interactive`, ask which.
pub fn slurm(args: &Args, script: Option<&Path>, console: &mut dyn Console, interactive: bool) -> Result<i32, String> {
    let script = match script {
        Some(script) => script.to_owned(),
        None => match pick(console, interactive, &current_dir()?)? {
            Some(script) => script,
            None => return Ok(0),
        },
    };
    wrap_file(args, &script, console)?;
    Ok(0)
}

fn current_dir() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|error| format!("cannot read the current folder: {error}"))
}

/// List the scripts under `here`; when `interactive`, the one chosen,
/// relative to `here`, or `None` to go back.
fn pick(console: &mut dyn Console, interactive: bool, here: &Path) -> Result<Option<PathBuf>, String> {
    let scripts = find(here);
    if scripts.is_empty() {
        return Err(format!(
            "no Slurm scripts found under {}; name one: carbontracker init slurm <SCRIPT>",
            here.display()
        ));
    }
    console.say(&format!("Slurm scripts under {}, newest first:", here.display()));
    for (number, script) in scripts.iter().enumerate() {
        console.say(&format!("  {}) {}", number + 1, describe(script, here)));
    }
    if !interactive {
        return Err("name the script: carbontracker init slurm <SCRIPT>".to_owned());
    }
    let prompt = format!("Which script? 1-{} (Enter to go back): ", scripts.len());
    let answer = console.line(&prompt).map_err(|error| format!("cannot read the input: {error}"))?.unwrap_or_default();
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(None);
    }
    let chosen = answer.parse::<usize>().ok().and_then(|number| scripts.get(number.checked_sub(1)?));
    let chosen = chosen.ok_or_else(|| format!("no script numbered {answer}"))?;
    Ok(Some(chosen.strip_prefix(here).unwrap_or(chosen).to_owned()))
}

fn wrap_file(args: &Args, given: &Path, console: &mut dyn Console) -> Result<(), String> {
    let text = std::fs::read_to_string(given).map_err(|error| format!("cannot read {}: {error}", given.display()))?;
    if text.contains(MARK) {
        return Err(format!("{} is a wrapper carbontracker init wrote; wrap the original script", given.display()));
    }
    let script = read(&text);
    if script.directives.is_empty() {
        console.warn(&format!("! {} has no #SBATCH lines; is it a Slurm script? Wrapping it anyway.", given.display()));
    }
    let absolute = given.canonicalize().map_err(|error| format!("cannot read {}: {error}", given.display()))?;
    let target = wrapper_path(&absolute);
    let shown = wrapper_path(given);
    let replacing = match std::fs::read_to_string(&target) {
        Ok(existing) if existing.contains(MARK) => true,
        Ok(_) => {
            return Err(format!("{} exists and was not written by carbontracker init; left alone", shown.display()));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err(format!("{} exists and cannot be read; left alone", shown.display())),
    };
    let carbontracker = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|error| format!("cannot find carbontracker's own path: {error}"))?;
    let logging = match args.log_dir() {
        Some(dir) => format!("--log-dir {}", quote(&dir.display().to_string())),
        None => "--no-log".to_owned(),
    };
    let wrapper = wrap(&script, &absolute, &carbontracker, &logging);
    write_executable(&target, &wrapper).map_err(|error| format!("cannot write {}: {error}", shown.display()))?;
    let verb = if replacing { "Replaced" } else { "Wrote" };
    console.say(&format!("{verb} {}. Submit it with:\n    sbatch {}", shown.display(), shown.display()));
    Ok(())
}

#[cfg(unix)]
fn write_executable(path: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn write_executable(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)
}

/// What reading a script found.
#[derive(Debug, PartialEq, Eq)]
struct Script {
    /// The interpreter line without `#!`, e.g. `/bin/bash -l`.
    interpreter: Option<String>,
    /// The `#SBATCH` lines Slurm reads: those before the first command.
    directives: Vec<String>,
}

fn read(text: &str) -> Script {
    let mut lines = text.lines().peekable();
    let interpreter = lines.peek().and_then(|line| line.strip_prefix("#!")).map(|rest| rest.trim().to_owned());
    if interpreter.is_some() {
        lines.next();
    }
    // Slurm stops reading directives at the first line that is neither a comment nor blank.
    let directives = lines
        .take_while(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
        .filter(|line| line.trim_start().starts_with("#SBATCH"))
        .map(|line| line.trim().to_owned())
        .collect();
    Script { interpreter, directives }
}

/// The wrapper: the script's directives, then the script run by its own
/// interpreter under carbontracker.
fn wrap(script: &Script, original: &Path, carbontracker: &Path, logging: &str) -> String {
    let interpreter = script.interpreter.as_deref().unwrap_or("/bin/bash");
    let mut out = format!("#!{interpreter}\n");
    for directive in &script.directives {
        out.push_str(directive);
        out.push('\n');
    }
    let name = original.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    out.push_str(&format!("\n# Runs {name} under carbontracker. {MARK}; delete to undo.\n"));
    out.push_str(&format!(
        "exec {} {logging} -- {interpreter} {} \"$@\"\n",
        quote(&carbontracker.display().to_string()),
        quote(&original.display().to_string()),
    ));
    out
}

/// `train.sbatch` gives `train.ct.sbatch`; `job` gives `job.ct`.
fn wrapper_path(original: &Path) -> PathBuf {
    let stem = original.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match original.extension() {
        Some(extension) => format!("{stem}.ct.{}", extension.to_string_lossy()),
        None => format!("{stem}.ct"),
    };
    original.with_file_name(name)
}

fn quote(value: &str) -> String {
    if !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+,=:@".contains(c)) {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

/// Slurm scripts under `root` that are not wrappers, newest first.
fn find(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, 0, &mut found);
    found.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    found.into_iter().map(|(path, _)| path).collect()
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<(PathBuf, SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if depth < MAX_DEPTH && !SKIPPED.contains(&entry.file_name().to_string_lossy().as_ref()) {
                walk(&path, depth + 1, found);
            }
        } else if meta.is_file()
            && meta.len() <= MAX_BYTES
            && let Ok(text) = std::fs::read_to_string(&path)
            && !read(&text).directives.is_empty()
            && !text.contains(MARK)
        {
            found.push((path, meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
        }
    }
}

/// `jobs/train.sh   train · gpu · 02:00:00  (wrapped)`.
fn describe(path: &Path, here: &Path) -> String {
    let directives = std::fs::read_to_string(path).map(|text| read(&text).directives).unwrap_or_default();
    let value = |long: &str, short: &str| {
        directives.iter().find_map(|directive| {
            let rest = directive.trim_start_matches("#SBATCH").trim();
            let rest = rest.strip_prefix(long).or_else(|| rest.strip_prefix(short))?;
            let rest = rest.strip_prefix('=').unwrap_or(rest).trim();
            rest.split_whitespace().next().map(str::to_owned)
        })
    };
    let details: Vec<String> =
        [("--job-name", "-J"), ("--partition", "-p"), ("--time", "-t")].iter().filter_map(|(l, s)| value(l, s)).collect();
    let shown = path.strip_prefix(here).unwrap_or(path).display().to_string();
    let mut line = format!("{shown:<32} {}", details.join(" · "));
    if wrapper_path(path).exists() {
        line.push_str("  (wrapped)");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init::tests::Scripted;

    const SCRIPT: &str =
        "#!/bin/bash -l\n#SBATCH --gres=gpu:1\n\n# note\n#SBATCH -J eval\nmodule load x\n#SBATCH --ignored\npython train.py\n";

    #[test]
    fn directives_stop_at_the_first_command() {
        let script = read(SCRIPT);
        assert_eq!(script.interpreter.as_deref(), Some("/bin/bash -l"));
        assert_eq!(script.directives, ["#SBATCH --gres=gpu:1", "#SBATCH -J eval"]);
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        let text = wrap(&read(SCRIPT), Path::new("/home/u/my job.sh"), Path::new("/opt/ct/carbontracker"), "--no-log");
        assert!(text.ends_with("exec /opt/ct/carbontracker --no-log -- /bin/bash -l '/home/u/my job.sh' \"$@\"\n"), "{text}");
    }

    #[test]
    fn short_flags_are_described_too() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("eval.sh"), SCRIPT).unwrap();
        assert!(describe(&dir.path().join("eval.sh"), dir.path()).ends_with(" eval"));
    }

    /// AC/FE-75
    #[test]
    fn on_a_terminal_the_chosen_script_is_returned() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("eval.sh"), SCRIPT).unwrap();
        let mut console = Scripted::new(&["1"]);
        let chosen = pick(&mut console, true, dir.path());
        let mut back = Scripted::new(&[""]);
        let none = pick(&mut back, true, dir.path());
        assert_eq!(chosen, Ok(Some(PathBuf::from("eval.sh"))));
        assert!(console.out.contains("1) eval.sh"), "{}", console.out);
        assert!(console.out.contains("Which script? 1-1"), "{}", console.out);
        assert_eq!(none, Ok(None));
    }
}
