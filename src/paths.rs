//! Filesystem locations the daemon reads and writes, resolved once at startup.
//!
//! Every path the daemon touches is a field on [`Paths`] rather than a literal
//! buried in a reader. That is what makes the `/proc` parsers testable against
//! fixture directories — in the TypeScript daemon they hardcoded `/proc` and
//! consequently had no test coverage at all.

use std::path::{Path, PathBuf};

/// Default `power_supply` class directory.
pub const DEFAULT_POWER_SUPPLY_BASE: &str = "/sys/class/power_supply";
/// Default hwmon class directory.
pub const DEFAULT_HWMON_BASE: &str = "/sys/class/hwmon";
/// Default cpufreq/topology directory.
pub const DEFAULT_CPU_BASE: &str = "/sys/devices/system/cpu";
/// Default DRM class directory, used for GPU utilisation.
pub const DEFAULT_DRM_BASE: &str = "/sys/class/drm";
/// Default procfs mount point.
pub const DEFAULT_PROC_BASE: &str = "/proc";

/// Fallback battery directory name when discovery finds nothing.
///
/// Returning a path that does not exist is deliberate: it keeps construction
/// infallible on machines with no battery (CI runners, desktops) and defers the
/// question to runtime, where `is_present` answers it honestly.
const FALLBACK_BATTERY: &str = "BAT0";

/// Resolved filesystem locations for one daemon run.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `/sys/class/power_supply`.
    pub power_supply_base: PathBuf,
    /// The specific battery directory within `power_supply_base`.
    pub battery: PathBuf,
    /// `/sys/class/hwmon`.
    pub hwmon_base: PathBuf,
    /// `/sys/devices/system/cpu`.
    pub cpu_base: PathBuf,
    /// `/sys/class/drm`.
    pub drm_base: PathBuf,
    /// `/proc`.
    pub proc_base: PathBuf,
    /// Directory holding `battery.db` and `debug.db`.
    pub db_dir: PathBuf,
}

impl Paths {
    /// Resolve from the environment, reading each override exactly once.
    ///
    /// Environment access is confined to this constructor. Reading it deeper
    /// would make the readers untestable in parallel — process environment is
    /// global mutable state, and under edition 2024 `set_var` is `unsafe`, which
    /// this crate forbids. Tests construct [`Paths`] directly instead.
    #[must_use]
    pub fn from_env() -> Self {
        let power_supply_base = env_path("BATMON_POWER_SUPPLY_BASE", DEFAULT_POWER_SUPPLY_BASE);
        let battery_override = std::env::var_os("BATMON_SYSFS_PATH").map(PathBuf::from);
        let battery = discover_battery_path(&power_supply_base, battery_override.as_deref());

        // Deliberately $HOME/.local/share, not an XDG-aware resolver. Every
        // existing installation has its databases there; honouring XDG_DATA_HOME
        // would silently start a fresh database beside the real one for anyone
        // who sets it, which reads as data loss.
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);

        Self {
            power_supply_base,
            battery,
            hwmon_base: env_path("BATMON_HWMON_BASE", DEFAULT_HWMON_BASE),
            cpu_base: PathBuf::from(DEFAULT_CPU_BASE),
            drm_base: PathBuf::from(DEFAULT_DRM_BASE),
            proc_base: PathBuf::from(DEFAULT_PROC_BASE),
            db_dir: home.join(".local/share/batmon"),
        }
    }

    /// Permanent downsampled history.
    #[must_use]
    pub fn db_path(&self) -> PathBuf {
        self.db_dir.join("battery.db")
    }

    /// Rolling high-frequency flight recorder.
    #[must_use]
    pub fn debug_db_path(&self) -> PathBuf {
        self.db_dir.join("debug.db")
    }

    /// A named attribute inside the battery directory.
    #[must_use]
    pub fn battery_attr(&self, name: &str) -> PathBuf {
        self.battery.join(name)
    }
}

fn env_path(key: &str, default: &str) -> PathBuf {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(default), PathBuf::from)
}

/// A `power_supply` entry that claims to be a battery.
struct Candidate {
    path: PathBuf,
    /// False for peripherals — a wireless mouse or keyboard reports
    /// `scope=Device` and must never be mistaken for the system battery.
    is_system: bool,
    /// Whether the directory is conventionally named `BAT*`.
    is_bat_name: bool,
}

/// Locate the primary system battery under `base_dir`.
///
/// `override_path` short-circuits everything, which is what `BATMON_SYSFS_PATH`
/// binds to. It is a parameter rather than an environment read so the behaviour
/// can be tested without mutating global process state.
///
/// Selection is a three-tier preference: a system-scoped battery with a
/// conventional `BAT*` name, then any system-scoped battery (Apple Silicon
/// reports `macsmc-battery`), then whatever was found. Ties break on sorted
/// name, so `BAT0` wins over `BAT1` deterministically.
///
/// Never fails. On a machine with no battery it returns `base_dir/BAT0`, a path
/// that does not exist, and presence is settled at runtime instead.
#[must_use]
pub fn discover_battery_path(base_dir: &Path, override_path: Option<&Path>) -> PathBuf {
    if let Some(path) = override_path {
        return path.to_path_buf();
    }

    let dir = if base_dir.as_os_str().is_empty() {
        Path::new(DEFAULT_POWER_SUPPLY_BASE)
    } else {
        base_dir
    };

    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return dir.join(FALLBACK_BATTERY);
    };

    let mut names: Vec<_> = read_dir
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    names.sort();

    let mut candidates = Vec::new();
    for name in names {
        let entry = dir.join(&name);

        // A missing or unreadable `type` means this is not a device we can
        // classify; sysfs entries also come and go as peripherals connect.
        let Ok(kind) = std::fs::read_to_string(entry.join("type")) else {
            continue;
        };
        if !kind.trim().eq_ignore_ascii_case("battery") {
            continue;
        }

        let is_system = std::fs::read_to_string(entry.join("scope"))
            .map_or(true, |scope| !scope.trim().eq_ignore_ascii_case("device"));

        // The TypeScript checked /^bat\d*$/i *or* a case-insensitive "bat"
        // prefix; the first is a strict subset of the second, so only the
        // prefix survives the port.
        let is_bat_name = name.to_string_lossy().to_lowercase().starts_with("bat");

        candidates.push(Candidate {
            path: entry,
            is_system,
            is_bat_name,
        });
    }

    let best = candidates
        .iter()
        .find(|c| c.is_system && c.is_bat_name)
        .or_else(|| candidates.iter().find(|c| c.is_system))
        .or_else(|| candidates.first());

    match best {
        Some(candidate) => candidate.path.clone(),
        None => dir.join(FALLBACK_BATTERY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Build a `power_supply` entry: `<name>/type` plus an optional `scope`.
    fn device(base: &Path, name: &str, kind: &str, scope: Option<&str>) -> PathBuf {
        let dir = base.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
        if let Some(scope) = scope {
            fs::write(dir.join("scope"), format!("{scope}\n")).unwrap();
        }
        dir
    }

    #[test]
    fn discovers_bat0_alongside_an_ac_adapter() {
        let tmp = TempDir::new().unwrap();
        device(tmp.path(), "ADP1", "Mains", None);
        let bat0 = device(tmp.path(), "BAT0", "Battery", None);

        assert_eq!(discover_battery_path(tmp.path(), None), bat0);
    }

    #[test]
    fn discovers_bat1_when_it_is_the_only_battery() {
        let tmp = TempDir::new().unwrap();
        device(tmp.path(), "AC", "Mains", None);
        let bat1 = device(tmp.path(), "BAT1", "Battery", None);

        assert_eq!(discover_battery_path(tmp.path(), None), bat1);
    }

    #[test]
    fn prefers_the_system_battery_over_a_peripheral() {
        let tmp = TempDir::new().unwrap();
        // A wireless mouse, sorting first alphabetically so a naive scan picks it.
        device(
            tmp.path(),
            "hid-0005:004c:0269-battery",
            "Battery",
            Some("Device"),
        );
        let bat0 = device(tmp.path(), "BAT0", "Battery", Some("System"));

        assert_eq!(discover_battery_path(tmp.path(), None), bat0);
    }

    #[test]
    fn discovers_a_system_battery_that_is_not_named_bat() {
        let tmp = TempDir::new().unwrap();
        let mac = device(tmp.path(), "macsmc-battery", "Battery", Some("System"));

        assert_eq!(discover_battery_path(tmp.path(), None), mac);
    }

    #[test]
    fn breaks_ties_between_system_batteries_on_sorted_name() {
        let tmp = TempDir::new().unwrap();
        let bat0 = device(tmp.path(), "BAT0", "Battery", None);
        device(tmp.path(), "BAT1", "Battery", None);

        assert_eq!(discover_battery_path(tmp.path(), None), bat0);
    }

    #[test]
    fn falls_back_when_the_base_directory_is_missing() {
        let missing = Path::new("/nonexistent/batmon-power-supply");

        assert_eq!(
            discover_battery_path(missing, None),
            missing.join("BAT0"),
            "construction must stay infallible on battery-less machines"
        );
    }

    #[test]
    fn falls_back_when_no_entry_is_a_battery() {
        let tmp = TempDir::new().unwrap();
        device(tmp.path(), "ADP1", "Mains", None);
        device(tmp.path(), "ucsi-source-psy-USBC000:001", "USB", None);

        assert_eq!(
            discover_battery_path(tmp.path(), None),
            tmp.path().join("BAT0")
        );
    }

    #[test]
    fn skips_entries_with_no_type_file_and_keeps_scanning() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("empty_dev")).unwrap();
        let bat0 = device(tmp.path(), "BAT0", "Battery", None);

        assert_eq!(discover_battery_path(tmp.path(), None), bat0);
    }

    #[test]
    fn an_override_short_circuits_discovery_entirely() {
        let tmp = TempDir::new().unwrap();
        device(tmp.path(), "BAT0", "Battery", None);
        let forced = Path::new("/custom/mock/battery/BAT99");

        assert_eq!(discover_battery_path(tmp.path(), Some(forced)), forced);
    }

    #[test]
    fn an_empty_base_directory_means_the_default() {
        let discovered = discover_battery_path(Path::new(""), None);

        assert!(
            discovered.starts_with(DEFAULT_POWER_SUPPLY_BASE),
            "{discovered:?}"
        );
    }

    #[test]
    fn type_and_scope_comparisons_ignore_case_and_trailing_newlines() {
        let tmp = TempDir::new().unwrap();
        let bat = device(tmp.path(), "BAT0", "  BaTtErY  ", Some("  SYSTEM  "));

        assert_eq!(discover_battery_path(tmp.path(), None), bat);
    }

    #[test]
    fn a_device_scoped_battery_is_still_used_when_it_is_the_only_one() {
        let tmp = TempDir::new().unwrap();
        let mouse = device(tmp.path(), "hid-mouse", "Battery", Some("Device"));

        assert_eq!(discover_battery_path(tmp.path(), None), mouse);
    }

    #[test]
    fn database_paths_hang_off_the_database_directory() {
        let paths = Paths {
            power_supply_base: PathBuf::from("/ps"),
            battery: PathBuf::from("/ps/BAT0"),
            hwmon_base: PathBuf::from("/hwmon"),
            cpu_base: PathBuf::from("/cpu"),
            drm_base: PathBuf::from("/drm"),
            proc_base: PathBuf::from("/proc"),
            db_dir: PathBuf::from("/data/batmon"),
        };

        assert_eq!(paths.db_path(), Path::new("/data/batmon/battery.db"));
        assert_eq!(paths.debug_db_path(), Path::new("/data/batmon/debug.db"));
        assert_eq!(
            paths.battery_attr("capacity"),
            Path::new("/ps/BAT0/capacity")
        );
    }

    #[test]
    fn from_env_resolves_the_documented_defaults() {
        let paths = Paths::from_env();

        assert!(paths.db_dir.ends_with(".local/share/batmon"));
        assert_eq!(paths.proc_base, Path::new(DEFAULT_PROC_BASE));
        assert_eq!(paths.cpu_base, Path::new(DEFAULT_CPU_BASE));
        assert_eq!(paths.drm_base, Path::new(DEFAULT_DRM_BASE));
    }
}
