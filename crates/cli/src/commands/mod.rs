//! One file per command: running a command, `check`, `init` (and `init
//! slurm`) and `report`.

mod check;
mod init;
mod report;
mod run;
mod slurm;

pub use check::check;
pub use init::init;
pub use report::report;
pub use run::track;

/// `init slurm`, on stdin and stdout.
pub fn init_slurm(args: &crate::args::Args, script: Option<&std::path::Path>) -> Result<i32, String> {
    let mut console = init::Std::new();
    let interactive = console.terminal;
    slurm::slurm(args, script, &mut console, interactive)
}
