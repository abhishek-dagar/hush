//! The machine the gate runs on (spec 0007, AC-14).
//!
//! Detects CPU, RAM and OS version, and compares against the `[machine]`
//! table in budgets.toml. Timing and memory rows become `info` (never
//! failing) on a different machine; size rows still gate.

use std::process::Command;

/// The machine this gate is running on.
#[derive(Debug, Clone)]
pub struct Machine {
    /// CPU brand string, e.g. "Apple M4".
    pub cpu: String,
    /// RAM in gigabytes.
    pub ram_gb: u64,
    /// OS version string, e.g. "macOS 26.0".
    pub os_version: String,
}

impl Machine {
    /// Detects the local machine. On macOS this reads `sysctl` and
    /// `sw_vers`; on other platforms it returns best-effort unknowns so the
    /// gate still runs (size still gates, timing becomes info).
    pub fn detect() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                cpu: sysctl("machdep.cpu.brand_string"),
                ram_gb: sysctl_u64("hw.memsize") / (1024 * 1024 * 1024),
                os_version: sw_vers(),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = ();
            Self {
                cpu: "unknown".to_owned(),
                ram_gb: 0,
                os_version: "unknown".to_owned(),
            }
        }
    }
}

/// Whether the detected machine matches the budgets' `[machine]` table
/// (AC-14): CPU name, RAM and OS name all match. On any difference, timing
/// and memory rows become `info` (reported, never failing) while size rows
/// still gate.
pub fn machine_matches(budget_machine: &crate::budgets::Machine, actual: &Machine) -> bool {
    budget_machine.os.eq_ignore_ascii_case(&actual.os_name())
        && budget_machine
            .cpu
            .trim()
            .eq_ignore_ascii_case(actual.cpu.trim())
        && budget_machine.ram_gb == actual.ram_gb
}

/// The OS name to compare against budgets, lowercased.
impl Machine {
    fn os_name(&self) -> String {
        if self.os_version.to_ascii_lowercase().contains("macos")
            || self.os_version.to_ascii_lowercase().contains("mac os")
        {
            "macos".to_owned()
        } else if self.os_version.contains("linux") {
            "linux".to_owned()
        } else if self.os_version.contains("windows") {
            "windows".to_owned()
        } else {
            "unknown".to_owned()
        }
    }
}

#[cfg(target_os = "macos")]
fn sysctl(key: &str) -> String {
    let output = Command::new("sysctl").arg("-n").arg(key).output();
    let Ok(output) = output else {
        return "unknown".to_owned();
    };
    let Ok(text) = String::from_utf8(output.stdout) else {
        return "unknown".to_owned();
    };
    text.trim().to_owned()
}

#[cfg(target_os = "macos")]
fn sysctl_u64(key: &str) -> u64 {
    sysctl(key).parse().unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn sw_vers() -> String {
    let output = Command::new("sw_vers").arg("-productVersion").output();
    let Ok(output) = output else {
        return "unknown".to_owned();
    };
    let Ok(text) = String::from_utf8(output.stdout) else {
        return "unknown".to_owned();
    };
    format!("macOS {}", text.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_os_name() {
        let m = Machine {
            cpu: "x".to_owned(),
            ram_gb: 16,
            os_version: "macOS 26.0".to_owned(),
        };
        assert_eq!(m.os_name(), "macos");
    }

    #[test]
    fn machine_matches_on_macos() {
        let budget = crate::budgets::Machine {
            cpu: "Apple M4".to_owned(),
            ram_gb: 16,
            os: "macos".to_owned(),
        };
        let actual = Machine {
            cpu: "Apple M4".to_owned(),
            ram_gb: 16,
            os_version: "macOS 26.0".to_owned(),
        };
        assert!(machine_matches(&budget, &actual));
    }

    #[test]
    fn machine_mismatch_on_os() {
        let budget = crate::budgets::Machine {
            cpu: "Apple M4".to_owned(),
            ram_gb: 16,
            os: "macos".to_owned(),
        };
        let actual = Machine {
            cpu: "x".to_owned(),
            ram_gb: 16,
            os_version: "Ubuntu 24.04".to_owned(),
        };
        assert!(!machine_matches(&budget, &actual));
    }

    #[test]
    fn machine_mismatch_on_cpu_or_ram() {
        let budget = crate::budgets::Machine {
            cpu: "Apple M4".to_owned(),
            ram_gb: 16,
            os: "macos".to_owned(),
        };
        // Same OS, different CPU: timing and memory become info (AC-14).
        let actual = Machine {
            cpu: "Apple M1".to_owned(),
            ram_gb: 16,
            os_version: "macOS 26.0".to_owned(),
        };
        assert!(!machine_matches(&budget, &actual));
        // Same OS and CPU, different RAM: also a mismatch.
        let actual = Machine {
            cpu: "Apple M4".to_owned(),
            ram_gb: 32,
            os_version: "macOS 26.0".to_owned(),
        };
        assert!(!machine_matches(&budget, &actual));
    }
}
