//! CPU clock frequency and GPU utilisation.
//!
//! Both are stateless reads with a fallback, so they are free functions rather
//! than a reader struct. Neither is expensive enough to justify caching resolved
//! paths the way the hwmon scan needed.

use std::path::Path;

use crate::parity::{js_number, round_js, round_to};

/// Mean current clock across every online CPU, in megahertz.
///
/// Averaged rather than taken from one core because modern schedulers park and
/// boost cores independently: a single core's reading says more about which core
/// was sampled than about what the machine is doing.
///
/// Prefers `cpufreq`, which reports what the hardware is actually running at.
/// Falls back to `/proc/cpuinfo`, which is what virtual machines and some ARM
/// platforms expose instead. Returns [`None`] when neither is available.
#[must_use]
pub fn read_cpu_freq_mhz(cpu_base: &Path, proc_base: &Path) -> Option<f64> {
    if let Some(mhz) = read_cpufreq(cpu_base) {
        return Some(mhz);
    }
    read_cpuinfo_freq(proc_base)
}

fn read_cpufreq(cpu_base: &Path) -> Option<f64> {
    let entries = std::fs::read_dir(cpu_base).ok()?;

    let mut total_khz = 0.0;
    let mut cores = 0u32;

    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        if !is_cpu_directory(&name.to_string_lossy()) {
            continue;
        }

        let path = cpu_base.join(&name).join("cpufreq/scaling_cur_freq");
        // A core that is offline has no cpufreq directory at all, and one being
        // read mid-transition can report zero. Both are excluded rather than
        // averaged in as a zero, which would drag the mean toward nothing.
        if let Some(khz) = read_positive(&path) {
            total_khz += khz;
            cores += 1;
        }
    }

    (cores > 0).then(|| round_js(total_khz / f64::from(cores) / 1_000.0))
}

fn read_cpuinfo_freq(proc_base: &Path) -> Option<f64> {
    let cpuinfo = std::fs::read_to_string(proc_base.join("cpuinfo")).ok()?;

    let mut total_mhz = 0.0;
    let mut cores = 0u32;

    for line in cpuinfo.lines() {
        if !line.starts_with("cpu MHz") {
            continue;
        }
        let Some((_, value)) = line.split_once(':') else {
            continue;
        };
        if let Some(mhz) = js_number(value.trim()).filter(|mhz| *mhz > 0.0) {
            total_mhz += mhz;
            cores += 1;
        }
    }

    (cores > 0).then(|| round_js(total_mhz / f64::from(cores)))
}

/// Directory names of the form `cpu0`, `cpu1`, and so on.
///
/// The check has to be exact: `/sys/devices/system/cpu` also contains `cpuidle`
/// and `cpufreq`, which are not cores.
fn is_cpu_directory(name: &str) -> bool {
    let Some(index) = name.strip_prefix("cpu") else {
        return false;
    };
    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
}

/// GPU shader utilisation, percent.
///
/// Only AMD exposes this through the DRM interface; Intel and NVIDIA report
/// nothing here, so [`None`] is the normal answer on most machines rather than a
/// failure. Cards are visited in sorted order so a multi-GPU machine reports the
/// same one every tick — the original relied on directory enumeration order,
/// which is not stable.
#[must_use]
pub fn read_gpu_pct(drm_base: &Path) -> Option<f64> {
    let entries = std::fs::read_dir(drm_base).ok()?;
    let mut cards: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| is_card_directory(&name.to_string_lossy()))
        .collect();
    cards.sort();

    for card in cards {
        let path = drm_base.join(card).join("device/gpu_busy_percent");
        if let Some(percent) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|contents| js_number(contents.trim()))
            .filter(|percent| *percent >= 0.0)
        {
            return Some(round_to(percent, 1));
        }
    }

    None
}

/// Directory names of the form `card0`, excluding connector nodes like
/// `card0-eDP-1`.
fn is_card_directory(name: &str) -> bool {
    let Some(index) = name.strip_prefix("card") else {
        return false;
    };
    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
}

fn read_positive(path: &Path) -> Option<f64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| js_number(contents.trim()))
        .filter(|value| *value > 0.0)
}

#[cfg(test)]
mod tests {
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
}
