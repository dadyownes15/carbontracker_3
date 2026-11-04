//! Power candidates: stated draws, RAPL, NVML, IOReport counters, macmon,
//! powermetrics and the generic CPU table.

pub(crate) mod constant;
pub(crate) mod generic_cpu;
// Built everywhere, so its read is tested everywhere; only the snapshot is macOS's.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod ioreport;
#[cfg(target_os = "macos")]
pub(crate) mod macmon;
pub(crate) mod nvml;
#[cfg(target_os = "macos")]
pub(crate) mod powermetrics;
// Built everywhere, so its read is tested everywhere; only probing is Linux's.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod rapl;
