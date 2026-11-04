//! One file per command: running a command, `check`, `init` and `report`.

mod check;
mod init;
mod report;
mod run;

pub use check::check;
pub use init::init;
pub use report::report;
pub use run::track;
