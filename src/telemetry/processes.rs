//! Per-process CPU and memory accounting.
//!
//! The most expensive thing the daemon does: roughly four hundred `/proc/PID`
//! directories opened, read and parsed every second.
//!
//! The read buffer, both PID maps and the group table are owned by the reader
//! and reused across ticks, so none of them is rebuilt. Two things still
//! allocate per process — the path passed to `open`, and the command name — and
//! that is a deliberate stopping point rather than an oversight.
//!
//! Measured on this machine at 490 processes: listing `/proc` costs 0.35 ms,
//! listing plus opening and reading every `stat` costs 7.28 ms, and the complete
//! scan including parsing, grouping, ranking and rendering costs 8.08 ms. Ninety
//! percent of it is kernel-side `open`/`read`/`close` that no amount of
//! allocation discipline touches; the remaining user-space work is around
//! 0.8 ms out of a 12 ms sample. Removing the last two allocations would buy a
//! fraction of that, at the cost of a reusable path buffer and borrowed names
//! threaded through the grouping — which is not a trade worth making here.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::PathBuf;

use crate::formats::round_to;

/// How many process groups appear in the recorded snapshot.
const TOP_GROUPS: usize = 5;

/// Memory page size assumed when converting RSS pages to kilobytes.
///
/// Hard-coded, and correct for x86-64 and the common arm64 configuration. A kernel built with 16K or 64K pages would report
/// proportionally low memory here. Reading the real value is a behaviour change
/// and belongs in its own commit.
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
        // the shortest round-tripping form. A serde_json number would render 6
        // as "6.0", which every row already on disk spells "6".
        let _ = write!(json, ",\"cpu\":{cpu},\"mem\":{mem},\"count\":{count}}}");
    }
    json.push(']');
    json
}

#[cfg(test)]
mod tests;
