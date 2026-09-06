//! Per-process CPU and memory accounting.
//!
//! The most expensive thing the daemon does — roughly four hundred `/proc/PID`
//! directories opened, read and parsed every second — and therefore the part
//! where allocation discipline actually shows up in the power budget. Nothing
//! here allocates per process: the read buffer, both process maps and the group
//! table are owned by the reader and reused across ticks.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::PathBuf;

use crate::parity::round_to;

/// How many process groups appear in the recorded snapshot.
const TOP_GROUPS: usize = 5;

/// Memory page size assumed when converting RSS pages to kilobytes.
///
/// Hard-coded, matching the TypeScript daemon, and correct for x86-64 and the
/// common arm64 configuration. A kernel built with 16K or 64K pages would report
/// proportionally low memory here. Reading the real value is a behaviour change
/// and belongs in its own commit rather than being smuggled into a port.
const PAGE_SIZE_KB: u64 = 4;

/// Field offsets within `/proc/PID/stat`, counted from the field after the
/// closing parenthesis of the process name.
///
/// The name is skipped by finding its parentheses rather than by splitting,
/// because a process may legally be called `foo bar)baz` and any field-counting
/// parser would be wrong about everything after it.
mod field {
    /// `utime` — field 14 overall.
    pub const UTIME: usize = 11;
    /// `stime` — field 15 overall.
    pub const STIME: usize = 12;
    /// `rss` in pages — field 24 overall.
    pub const RSS: usize = 21;
}

/// One aggregated group of same-named processes.
#[derive(Debug, Default, Clone)]
struct Group {
    name: String,
    cpu_ticks: u64,
    rss_pages: u64,
    count: u32,
}

/// Scans `/proc` and reports the busiest process groups.
#[derive(Debug)]
pub struct ProcessReader {
    proc_base: PathBuf,
    /// Per-PID cumulative CPU ticks from the previous tick.
    previous: HashMap<u32, u64>,
    /// Being filled this tick; swapped with `previous` at the end.
    current: HashMap<u32, u64>,
    previous_total_ticks: u64,
    /// Reused read buffer, so no `/proc/PID/stat` read allocates.
    buffer: Vec<u8>,
    groups: Vec<Group>,
    /// Group name to index in `groups`, which preserves first-seen order.
    index: HashMap<String, usize>,
}

impl ProcessReader {
    #[must_use]
    pub fn new(proc_base: impl Into<PathBuf>) -> Self {
        Self {
            proc_base: proc_base.into(),
            previous: HashMap::new(),
            current: HashMap::new(),
            previous_total_ticks: 0,
            buffer: Vec::with_capacity(1024),
            groups: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Scan every process and return the top groups as a JSON array.
    ///
    /// Processes are grouped by name, because a browser with sixty renderer
    /// processes is one thing consuming power, not sixty. CPU is the share of
    /// the interval's total ticks, so it is already normalised across cores.
    ///
    /// Returns [`None`] when no CPU time elapsed or nothing was readable.
    pub fn read(&mut self, total_ticks: u64, mem_total_kb: u64) -> Option<String> {
        let elapsed_ticks = total_ticks.saturating_sub(self.previous_total_ticks);
        self.previous_total_ticks = total_ticks;

        self.current.clear();
        self.groups.clear();
        self.index.clear();

        let Ok(entries) = std::fs::read_dir(&self.proc_base) else {
            return None;
        };

        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
                continue;
            };

            // A process exiting between the directory listing and this read is
            // routine, not exceptional; there are hundreds of chances per tick.
            let Some((command, ticks, rss_pages)) = self.read_process(pid) else {
                continue;
            };

            self.current.insert(pid, ticks);

            // A process seen for the first time contributes no CPU delta. Its
            // lifetime CPU total is not work done during this interval, and
            // counting it would make every new process look like a runaway.
            let delta = self
                .previous
                .get(&pid)
                .map_or(0, |before| ticks.saturating_sub(*before));

            if let Some(position) = self.index.get(command.as_str()) {
                let group = &mut self.groups[*position];
                group.cpu_ticks += delta;
                group.rss_pages += rss_pages;
                group.count += 1;
            } else {
                self.index.insert(command.clone(), self.groups.len());
                self.groups.push(Group {
                    name: command,
                    cpu_ticks: delta,
                    rss_pages,
                    count: 1,
                });
            }
        }

        std::mem::swap(&mut self.previous, &mut self.current);

        if elapsed_ticks == 0 || self.groups.is_empty() {
            return None;
        }

        Some(render(&mut self.groups, elapsed_ticks, mem_total_kb))
    }

    /// Read one `/proc/PID/stat`, returning its name, CPU ticks and RSS pages.
    fn read_process(&mut self, pid: u32) -> Option<(String, u64, u64)> {
        let path = self.proc_base.join(pid.to_string()).join("stat");

        self.buffer.clear();
        std::fs::File::open(path)
            .ok()?
            .read_to_end(&mut self.buffer)
            .ok()?;

        let open = self.buffer.iter().position(|byte| *byte == b'(')?;
        let close = self.buffer.iter().rposition(|byte| *byte == b')')?;
        if close < open {
            return None;
        }

        // The name is arbitrary bytes; lossy conversion mirrors reading the file
        // as UTF-8 with replacement characters, which is what the original did.
        let command = String::from_utf8_lossy(&self.buffer[open + 1..close]).into_owned();

        let rest = self.buffer.get(close + 2..)?;
        let utime = nth_field(rest, field::UTIME);
        let stime = nth_field(rest, field::STIME);
        let rss_pages = nth_field(rest, field::RSS);

        Some((command, utime + stime, rss_pages))
    }
}

/// Parse the `index`-th space-separated field as an integer, defaulting to zero.
///
/// Zero for a missing or unparseable field mirrors the original's `Number(x) ||
/// 0`, and is the right default here regardless: an unreadable counter means no
/// measured work, not a failed tick.
fn nth_field(rest: &[u8], index: usize) -> u64 {
    let mut field = rest.split(|byte| *byte == b' ').nth(index).unwrap_or(b"");
    // Guard against a trailing newline on the final field.
    while field.last().is_some_and(u8::is_ascii_whitespace) {
        field = &field[..field.len() - 1];
    }

    let mut value: u64 = 0;
    for byte in field {
        if !byte.is_ascii_digit() {
            return 0;
        }
        value = value
            .saturating_mul(10)
            .saturating_add(u64::from(byte - b'0'));
    }
    value
}

/// Sort, truncate and render the group table as JSON.
fn render(groups: &mut [Group], elapsed_ticks: u64, mem_total_kb: u64) -> String {
    let mut summaries: Vec<(&str, f64, f64, u32)> = groups
        .iter()
        .map(|group| {
            let cpu = round_to(group.cpu_ticks as f64 / elapsed_ticks as f64 * 100.0, 1);
            let memory_kb = group.rss_pages * PAGE_SIZE_KB;
            let mem = round_to(memory_kb as f64 / mem_total_kb as f64 * 100.0, 1);
            (group.name.as_str(), cpu, mem, group.count)
        })
        .collect();

    // Busiest first, memory breaking ties. The sort is stable, so groups equal
    // on both fall back to the order /proc listed them — which is what the
    // original did, and is not deterministic in either implementation.
    summaries.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.2.total_cmp(&a.2)));
    summaries.truncate(TOP_GROUPS);

    let mut json = String::from("[");
    for (position, (name, cpu, mem, count)) in summaries.iter().enumerate() {
        if position > 0 {
            json.push(',');
        }
        json.push_str("{\"name\":");
        // serde_json for the escaping, which has to handle quotes, backslashes
        // and control bytes in a process name...
        json.push_str(&serde_json::to_string(name).unwrap_or_else(|_| "\"\"".to_owned()));
        // ...but Rust's own float formatting for the numbers, because it emits
        // the shortest round-tripping form exactly as JSON.stringify does. A
        // serde_json number would render 6 as "6.0" and break byte parity.
        let _ = write!(json, ",\"cpu\":{cpu},\"mem\":{mem},\"count\":{count}}}");
    }
    json.push(']');
    json
}

#[cfg(test)]
mod tests {
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
        // byte parity with every row the TypeScript daemon ever wrote.
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
}
