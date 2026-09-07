//! CPU clock frequency and GPU utilisation.
//!
//! Both are stateless reads with a fallback, so they are free functions rather
//! than a reader struct. Neither is expensive enough to justify caching resolved
//! paths the way the hwmon scan needed.

use std::path::Path;

use crate::formats::{js_number, round_js, round_to};

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
mod tests;
