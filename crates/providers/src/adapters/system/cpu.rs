//! This host's CPU, by the name every CPU candidate shares.

/// Host CPU name. All CPU candidates must use this so they share one coverage slot.
pub(crate) fn device_name() -> String {
    cpu_brand().unwrap_or_else(|| "Unknown CPU".to_owned())
}

/// The OS's name for this host's CPU.
pub(crate) fn cpu_brand() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output().ok()?;
        let brand = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        (!brand.is_empty()).then_some(brand)
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/cpuinfo").ok()?
            .lines()
            .find_map(|l| l.strip_prefix("model name"))
            .and_then(|v| v.split_once(':'))
            .map(|(_, b)| b.trim().to_owned())
            .filter(|b| !b.is_empty())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    None
}
