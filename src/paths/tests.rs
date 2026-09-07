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
fn an_empty_override_is_treated_as_unset() {
    // BATMON_SYSFS_PATH="" would otherwise yield an empty battery path, so
    // is_present would report false and the daemon would record nothing
    // while exiting successfully.
    let tmp = TempDir::new().unwrap();
    let bat0 = device(tmp.path(), "BAT0", "Battery", None);

    assert_eq!(
        discover_battery_path(tmp.path(), Some(Path::new(""))),
        bat0,
        "an empty override should fall through to discovery"
    );
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
