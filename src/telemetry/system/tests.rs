#![allow(clippy::float_cmp)]

use super::*;
use tempfile::TempDir;

fn core(base: &Path, name: &str, khz: Option<&str>) {
    let dir = base.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(khz) = khz {
        let freq = dir.join("cpufreq");
        std::fs::create_dir_all(&freq).unwrap();
        std::fs::write(freq.join("scaling_cur_freq"), format!("{khz}\n")).unwrap();
    }
}

fn card(base: &Path, name: &str, busy: Option<&str>) {
    let dir = base.join(name).join("device");
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(busy) = busy {
        std::fs::write(dir.join("gpu_busy_percent"), format!("{busy}\n")).unwrap();
    }
}

#[test]
fn cpu_frequency_is_the_mean_across_cores_in_megahertz() {
    let tmp = TempDir::new().unwrap();
    core(tmp.path(), "cpu0", Some("1600000"));
    core(tmp.path(), "cpu1", Some("2400000"));
    core(tmp.path(), "cpu2", Some("3200000"));

    assert_eq!(
        read_cpu_freq_mhz(tmp.path(), Path::new("/nonexistent")),
        Some(2400.0)
    );
}

#[test]
fn non_core_directories_are_not_mistaken_for_cores() {
    // cpuidle and cpufreq live alongside the cores and would otherwise be
    // counted as cores reporting nothing.
    let tmp = TempDir::new().unwrap();
    core(tmp.path(), "cpu0", Some("2000000"));
    core(tmp.path(), "cpuidle", Some("9999999"));
    core(tmp.path(), "cpufreq", Some("9999999"));
    std::fs::write(tmp.path().join("online"), "0-1\n").unwrap();

    assert_eq!(
        read_cpu_freq_mhz(tmp.path(), Path::new("/nonexistent")),
        Some(2000.0)
    );
}

#[test]
fn offline_and_zero_reporting_cores_are_excluded_from_the_mean() {
    let tmp = TempDir::new().unwrap();
    core(tmp.path(), "cpu0", Some("2000000"));
    core(tmp.path(), "cpu1", None); // offline: no cpufreq directory
    core(tmp.path(), "cpu2", Some("0")); // mid-transition

    assert_eq!(
        read_cpu_freq_mhz(tmp.path(), Path::new("/nonexistent")),
        Some(2000.0),
        "a zero must not drag the mean down"
    );
}

#[test]
fn cpu_frequency_falls_back_to_cpuinfo() {
    let tmp = TempDir::new().unwrap();
    let proc = TempDir::new().unwrap();
    std::fs::write(
        proc.path().join("cpuinfo"),
        "processor\t: 0\ncpu MHz\t\t: 2100.000\nprocessor\t: 1\ncpu MHz\t\t: 2300.000\n",
    )
    .unwrap();

    assert_eq!(read_cpu_freq_mhz(tmp.path(), proc.path()), Some(2200.0));
}

#[test]
fn cpufreq_wins_over_cpuinfo_when_both_exist() {
    let tmp = TempDir::new().unwrap();
    core(tmp.path(), "cpu0", Some("3000000"));
    let proc = TempDir::new().unwrap();
    std::fs::write(proc.path().join("cpuinfo"), "cpu MHz\t\t: 800.000\n").unwrap();

    assert_eq!(read_cpu_freq_mhz(tmp.path(), proc.path()), Some(3000.0));
}

#[test]
fn no_frequency_source_yields_nothing() {
    assert_eq!(
        read_cpu_freq_mhz(
            Path::new("/nonexistent/cpu"),
            Path::new("/nonexistent/proc")
        ),
        None
    );
}

#[test]
fn an_empty_cpu_tree_yields_nothing() {
    let tmp = TempDir::new().unwrap();
    let proc = TempDir::new().unwrap();
    std::fs::write(proc.path().join("cpuinfo"), "processor\t: 0\n").unwrap();

    assert_eq!(read_cpu_freq_mhz(tmp.path(), proc.path()), None);
}

#[test]
fn gpu_utilisation_is_read_from_the_drm_interface() {
    let tmp = TempDir::new().unwrap();
    card(tmp.path(), "card0", Some("37"));

    assert_eq!(read_gpu_pct(tmp.path()), Some(37.0));
}

#[test]
fn an_idle_gpu_reports_zero_rather_than_nothing() {
    let tmp = TempDir::new().unwrap();
    card(tmp.path(), "card0", Some("0"));

    assert_eq!(read_gpu_pct(tmp.path()), Some(0.0));
}

#[test]
fn cards_are_visited_in_sorted_order_for_a_stable_answer() {
    // Directory enumeration order is not stable, so a multi-GPU machine
    // would otherwise report a different card from tick to tick.
    let tmp = TempDir::new().unwrap();
    card(tmp.path(), "card1", Some("90"));
    card(tmp.path(), "card0", Some("10"));

    assert_eq!(read_gpu_pct(tmp.path()), Some(10.0));
}

#[test]
fn a_card_without_the_attribute_is_skipped_for_one_that_has_it() {
    // Intel and NVIDIA expose nothing here; an AMD card alongside them still
    // gets reported.
    let tmp = TempDir::new().unwrap();
    card(tmp.path(), "card0", None);
    card(tmp.path(), "card1", Some("55"));

    assert_eq!(read_gpu_pct(tmp.path()), Some(55.0));
}

#[test]
fn connector_nodes_are_not_mistaken_for_cards() {
    // /sys/class/drm holds card0-eDP-1 and friends beside the cards.
    let tmp = TempDir::new().unwrap();
    card(tmp.path(), "card0-eDP-1", Some("99"));
    card(tmp.path(), "card0", Some("12"));

    assert_eq!(read_gpu_pct(tmp.path()), Some(12.0));
}

#[test]
fn a_machine_with_no_reporting_gpu_yields_nothing() {
    let tmp = TempDir::new().unwrap();
    card(tmp.path(), "card0", None);

    assert_eq!(read_gpu_pct(tmp.path()), None);
    assert_eq!(read_gpu_pct(Path::new("/nonexistent/drm")), None);
}

#[test]
fn a_malformed_utilisation_value_is_ignored() {
    let tmp = TempDir::new().unwrap();
    card(tmp.path(), "card0", Some("unavailable"));

    assert_eq!(read_gpu_pct(tmp.path()), None);
}
