use std::path::Path;

use super::*;
use tempfile::TempDir;

/// Build a `/proc/PID/stat` line. Fields after the name start at index 0
/// with the process state, so utime/stime/rss land where the parser expects.
fn stat_line(name: &str, utime: u64, stime: u64, rss_pages: u64) -> String {
    // Index 0 is the process state — field 3 overall — because every parser
    // offset is counted from the field after the name's closing bracket.
    let mut fields = vec!["0".to_owned(); 44];
    fields[0] = "S".to_owned();
    fields[field::UTIME] = utime.to_string();
    fields[field::STIME] = stime.to_string();
    fields[field::RSS] = rss_pages.to_string();
    format!("1 ({}) {}\n", name, fields.join(" "))
}

/// A fake procfs containing the given processes.
fn procfs(processes: &[(u32, &str, u64, u64, u64)]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    for (pid, name, utime, stime, rss) in processes {
        let dir = tmp.path().join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stat"), stat_line(name, *utime, *stime, *rss)).unwrap();
    }
    tmp
}

fn write_process(base: &Path, pid: u32, name: &str, utime: u64, stime: u64, rss: u64) {
    let dir = base.join(pid.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("stat"), stat_line(name, utime, stime, rss)).unwrap();
}

#[test]
fn the_first_scan_reports_no_cpu_because_there_is_no_baseline() {
    // Lifetime CPU totals are not work done during this interval; counting
    // them would make every process present at startup look like a runaway.
    let tmp = procfs(&[(1, "systemd", 5_000, 1_000, 100)]);
    let mut reader = ProcessReader::new(tmp.path());

    let json = reader.read(10_000, 1_000_000).unwrap();

    assert_eq!(json, r#"[{"name":"systemd","cpu":0,"mem":0,"count":1}]"#);
}

#[test]
fn cpu_is_the_share_of_the_intervals_ticks() {
    let tmp = procfs(&[(1, "firefox", 0, 0, 0)]);
    let mut reader = ProcessReader::new(tmp.path());
    reader.read(10_000, 1_000_000).unwrap();

    // 50 ticks of a 1000-tick interval: 5%.
    write_process(tmp.path(), 1, "firefox", 30, 20, 0);
    let json = reader.read(11_000, 1_000_000).unwrap();

    assert!(json.contains(r#""cpu":5"#), "{json}");
}

#[test]
fn memory_is_rss_pages_against_total_memory() {
    // 25,600 pages x 4 KB = 102,400 KB of 1,024,000 KB total: 10%.
    let tmp = procfs(&[(1, "chrome", 0, 0, 25_600)]);
    let mut reader = ProcessReader::new(tmp.path());

    let json = reader.read(10_000, 1_024_000).unwrap();

    assert!(json.contains(r#""mem":10"#), "{json}");
}

#[test]
fn processes_sharing_a_name_are_aggregated_into_one_group() {
    // A browser with sixty renderers is one thing consuming power.
    let tmp = procfs(&[
        (10, "chrome", 0, 0, 100),
        (11, "chrome", 0, 0, 200),
        (12, "chrome", 0, 0, 300),
    ]);
    let mut reader = ProcessReader::new(tmp.path());
    reader.read(10_000, 1_000_000).unwrap();

    write_process(tmp.path(), 10, "chrome", 10, 0, 100);
    write_process(tmp.path(), 11, "chrome", 20, 0, 200);
    write_process(tmp.path(), 12, "chrome", 30, 0, 300);
    let json = reader.read(11_000, 1_000_000).unwrap();

    assert!(json.contains(r#""count":3"#), "{json}");
    // 60 ticks of 1000 aggregated across the group.
    assert!(json.contains(r#""cpu":6"#), "{json}");
}

#[test]
fn groups_are_ranked_by_cpu_then_memory() {
    let tmp = procfs(&[
        (1, "idle", 0, 0, 0),
        (2, "busy", 0, 0, 0),
        (3, "hungry", 0, 0, 0),
    ]);
    let mut reader = ProcessReader::new(tmp.path());
    reader.read(10_000, 1_000_000).unwrap();

    write_process(tmp.path(), 1, "idle", 0, 0, 0);
    write_process(tmp.path(), 2, "busy", 100, 0, 0);
    write_process(tmp.path(), 3, "hungry", 0, 0, 50_000);
    let json = reader.read(11_000, 1_000_000).unwrap();

    let busy = json.find("busy").unwrap();
    let hungry = json.find("hungry").unwrap();
    let idle = json.find("idle").unwrap();
    assert!(busy < hungry, "cpu must outrank memory: {json}");
    assert!(hungry < idle, "memory must break ties: {json}");
}

#[test]
fn only_the_top_five_groups_are_reported() {
    let named: Vec<(u32, String)> = (1..=9u32)
        .map(|index| (index, format!("proc{index}")))
        .collect();

    let tmp = TempDir::new().unwrap();
    for (pid, name) in &named {
        write_process(tmp.path(), *pid, name, 0, 0, 0);
    }
    let mut reader = ProcessReader::new(tmp.path());
    reader.read(10_000, 1_000_000).unwrap();

    for (pid, name) in &named {
        write_process(tmp.path(), *pid, name, u64::from(*pid) * 10, 0, 0);
    }
    let json = reader.read(11_000, 1_000_000).unwrap();

    assert_eq!(json.matches("\"name\"").count(), 5, "{json}");
    // The five busiest are pids 9 down to 5.
    assert!(json.contains("proc9") && json.contains("proc5"), "{json}");
    assert!(!json.contains("proc4"), "{json}");
}

#[test]
fn a_process_that_exits_mid_scan_is_skipped_not_fatal() {
    let tmp = procfs(&[(1, "survivor", 0, 0, 100)]);
    // A PID directory with no stat file, as happens when a process exits
    // between the listing and the read.
    std::fs::create_dir_all(tmp.path().join("999")).unwrap();
    let mut reader = ProcessReader::new(tmp.path());

    let json = reader.read(10_000, 1_000_000).unwrap();

    assert!(json.contains("survivor"), "{json}");
}

#[test]
fn non_pid_entries_are_ignored() {
    let tmp = procfs(&[(1, "init", 0, 0, 0)]);
    for name in ["self", "meminfo", "sys", "12abc"] {
        let path = tmp.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("stat"), "garbage").unwrap();
    }
    let mut reader = ProcessReader::new(tmp.path());

    let json = reader.read(10_000, 1_000_000).unwrap();

    assert_eq!(json.matches("\"name\"").count(), 1, "{json}");
}

#[test]
fn a_process_name_containing_spaces_and_parentheses_parses_correctly() {
    // comm is arbitrary bytes; a field-counting parser would be wrong about
    // everything after it. Locating the last ')' is what makes this work.
    let tmp = TempDir::new().unwrap();
    write_process(tmp.path(), 1, "foo bar)baz", 0, 0, 0);
    let mut reader = ProcessReader::new(tmp.path());
    reader.read(10_000, 1_000_000).unwrap();

    write_process(tmp.path(), 1, "foo bar)baz", 250, 0, 0);
    let json = reader.read(11_000, 1_000_000).unwrap();

    assert!(json.contains(r#""name":"foo bar)baz""#), "{json}");
    assert!(json.contains(r#""cpu":25"#), "{json}");
}

#[test]
fn a_process_name_needing_json_escaping_is_escaped() {
    let tmp = TempDir::new().unwrap();
    write_process(tmp.path(), 1, "say \"hi\"\\", 0, 0, 0);
    let mut reader = ProcessReader::new(tmp.path());

    let json = reader.read(10_000, 1_000_000).unwrap();

    assert!(json.contains(r#""name":"say \"hi\"\\""#), "{json}");
    // And the result must still be valid JSON.
    serde_json::from_str::<serde_json::Value>(&json).unwrap();
}

#[test]
fn whole_numbers_render_without_a_decimal_point() {
    // JSON.stringify emits 6, not 6.0. serde_json would emit 6.0 and break
    // byte-compatible with the rows already on disk.
    let tmp = procfs(&[(1, "app", 0, 0, 0)]);
    let mut reader = ProcessReader::new(tmp.path());
    reader.read(10_000, 1_000_000).unwrap();

    write_process(tmp.path(), 1, "app", 100, 0, 25_000);
    let json = reader.read(11_000, 1_000_000).unwrap();

    assert_eq!(json, r#"[{"name":"app","cpu":10,"mem":10,"count":1}]"#);
}

#[test]
fn fractional_percentages_keep_one_decimal() {
    let tmp = procfs(&[(1, "app", 0, 0, 0)]);
    let mut reader = ProcessReader::new(tmp.path());
    reader.read(10_000, 1_000_000).unwrap();

    write_process(tmp.path(), 1, "app", 63, 0, 0);
    let json = reader.read(11_000, 1_000_000).unwrap();

    assert!(json.contains(r#""cpu":6.3"#), "{json}");
}

#[test]
fn an_interval_with_no_elapsed_ticks_reports_nothing() {
    let tmp = procfs(&[(1, "app", 0, 0, 0)]);
    let mut reader = ProcessReader::new(tmp.path());
    reader.read(10_000, 1_000_000).unwrap();

    assert_eq!(reader.read(10_000, 1_000_000), None);
}

#[test]
fn an_unreadable_proc_directory_reports_nothing() {
    let mut reader = ProcessReader::new("/nonexistent/batmon/proc");
    assert_eq!(reader.read(10_000, 1_000_000), None);
}

#[test]
fn an_empty_proc_directory_reports_nothing() {
    let tmp = TempDir::new().unwrap();
    let mut reader = ProcessReader::new(tmp.path());
    assert_eq!(reader.read(10_000, 1_000_000), None);
}

#[test]
fn a_process_whose_counters_go_backwards_contributes_no_negative_time() {
    // PID reuse: the number is the same, the process is not.
    let tmp = procfs(&[(1, "old", 5_000, 0, 0)]);
    let mut reader = ProcessReader::new(tmp.path());
    reader.read(10_000, 1_000_000).unwrap();

    write_process(tmp.path(), 1, "new", 10, 0, 0);
    let json = reader.read(11_000, 1_000_000).unwrap();

    assert!(json.contains(r#""cpu":0"#), "{json}");
}

#[test]
fn a_disappearing_process_is_dropped_from_the_baseline() {
    let tmp = procfs(&[(1, "a", 100, 0, 0), (2, "b", 100, 0, 0)]);
    let mut reader = ProcessReader::new(tmp.path());
    reader.read(10_000, 1_000_000).unwrap();

    std::fs::remove_dir_all(tmp.path().join("2")).unwrap();
    let json = reader.read(11_000, 1_000_000).unwrap();

    assert!(!json.contains(r#""name":"b""#), "{json}");
}

#[test]
fn a_malformed_stat_line_is_skipped() {
    let tmp = procfs(&[(1, "good", 0, 0, 0)]);
    let dir = tmp.path().join("2");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("stat"), "no parentheses here\n").unwrap();
    let mut reader = ProcessReader::new(tmp.path());

    let json = reader.read(10_000, 1_000_000).unwrap();

    assert_eq!(json.matches("\"name\"").count(), 1, "{json}");
}

#[test]
fn the_reader_allocates_no_growing_state_across_ticks() {
    // The maps are swapped rather than rebuilt; this asserts the baseline
    // tracks the live process set instead of accumulating every PID ever.
    let tmp = TempDir::new().unwrap();
    let mut reader = ProcessReader::new(tmp.path());

    for generation in 0..20u32 {
        std::fs::remove_dir_all(tmp.path()).ok();
        std::fs::create_dir_all(tmp.path()).unwrap();
        write_process(tmp.path(), generation + 1, "churn", 0, 0, 0);
        reader.read(u64::from(generation) * 1_000 + 1_000, 1_000_000);
    }

    assert_eq!(reader.previous.len(), 1, "baseline grew without bound");
}

#[test]
fn nth_field_defaults_to_zero_for_missing_or_unparseable_fields() {
    assert_eq!(nth_field(b"1 2 3", 1), 2);
    assert_eq!(nth_field(b"1 2 3", 9), 0, "missing field");
    assert_eq!(nth_field(b"1 x 3", 1), 0, "unparseable field");
    assert_eq!(nth_field(b"1 2 3\n", 2), 3, "trailing newline trimmed");
    assert_eq!(nth_field(b"", 0), 0);
}
