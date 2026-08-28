import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { SYSFS } from "./config";
import type {
	SensorsData,
	SystemTemps,
	TelemetrySample,
	TopProcessGroup,
} from "./types";

// ── sysfs helpers ────────────────────────────────────────────────────
function sysfsPath(name: string): string {
	return join(SYSFS, name);
}

function exists(name: string): boolean {
	return existsSync(sysfsPath(name));
}

function read(name: string): string {
	return readFileSync(sysfsPath(name), "utf-8").trim();
}

function readNum(name: string): number {
	return Number(read(name));
}

function readOpt(name: string): number | null {
	if (!exists(name)) return null;
	const v = Number(read(name));
	return Number.isFinite(v) ? v : null;
}

// ── temperature: battery sensor (may not exist) ──────────────────────
function readBatteryTemp(): number | null {
	const direct = readOpt("temp");
	if (direct !== null && direct > 0) return direct / 10;

	try {
		for (const e of readdirSync(SYSFS)) {
			if (!e.startsWith("hwmon")) continue;
			for (let i = 1; i <= 3; i++) {
				const p = join(SYSFS, e, `temp${i}_input`);
				if (existsSync(p)) {
					const v = Number(readFileSync(p, "utf-8").trim());
					if (Number.isFinite(v) && v > 0) return v / 1000;
				}
			}
		}
	} catch {
		/* no hwmon */
	}

	return null;
}

// ── system temps & power via `sensors -j` (thermal environment proxy) ─
export function readSystemTemps(): SystemTemps {
	try {
		const { stdout, exitCode } = Bun.spawnSync(["sensors", "-j"]);
		if (exitCode !== 0)
			return { cpu_c: null, gpu_c: null, nvme_c: null, gpu_power_w: null };
		const data: SensorsData = JSON.parse(stdout.toString());

		const find = (prefix: string): SensorsData[string] | undefined =>
			Object.entries(data).find(([k]) => k.startsWith(prefix))?.[1];

		const cpu =
			find("k10temp")?.Tctl?.temp1_input ??
			find("coretemp")?.["Package id 0"]?.temp1_input ??
			null;
		const gpu =
			find("amdgpu")?.edge?.temp1_input ??
			find("i915")?.temp1?.temp1_input ??
			null;
		const nvme = find("nvme")?.Composite?.temp1_input ?? null;
		const gpuPower =
			find("amdgpu")?.PPT?.power1_input ??
			find("amdgpu")?.PPT?.power1_average ??
			null;

		return {
			cpu_c: cpu,
			gpu_c: gpu,
			nvme_c: nvme,
			gpu_power_w: gpuPower !== null ? Math.round(gpuPower * 100) / 100 : null,
		};
	} catch {
		return { cpu_c: null, gpu_c: null, nvme_c: null, gpu_power_w: null };
	}
}

// ── native CPU, Memory, GPU & Process telemetry readers ───────────────
let prevCpu: { idle: number; total: number } | null = null;

function readCpuPct(): number | null {
	try {
		const stat = readFileSync("/proc/stat", "utf-8");
		const [cpuHeaderLine] = stat.split("\n");
		if (!cpuHeaderLine?.startsWith("cpu ")) return null;

		// /proc/stat columns: cpu user nice system idle iowait irq softirq steal guest guest_nice
		const [
			,
			user = 0,
			nice = 0,
			system = 0,
			idle = 0,
			iowait = 0,
			irq = 0,
			softirq = 0,
			steal = 0,
		] = cpuHeaderLine.trim().split(/\s+/).map(Number);

		const idleTime = idle + iowait;
		const totalTime =
			user + nice + system + idle + iowait + irq + softirq + steal;

		if (!prevCpu) {
			prevCpu = { idle: idleTime, total: totalTime };
			return null;
		}

		const totalDelta = totalTime - prevCpu.total;
		const idleDelta = idleTime - prevCpu.idle;
		prevCpu = { idle: idleTime, total: totalTime };

		if (totalDelta <= 0) return 0;
		const pct = (1 - idleDelta / totalDelta) * 100;
		return Math.round(Math.max(0, Math.min(100, pct)) * 10) / 10;
	} catch {
		return null;
	}
}

function readMemPct(): number | null {
	try {
		const meminfo = readFileSync("/proc/meminfo", "utf-8");
		const mem = new Map<string, number>();

		for (const line of meminfo.split("\n")) {
			const colonIdx = line.indexOf(":");
			if (colonIdx === -1) continue;
			const key = line.slice(0, colonIdx).trim();
			const valKb = Number.parseInt(line.slice(colonIdx + 1), 10);
			if (Number.isFinite(valKb)) mem.set(key, valKb);
		}

		const totalKb = mem.get("MemTotal");
		if (!totalKb || totalKb <= 0) return null;

		const availKb =
			mem.get("MemAvailable") ??
			(mem.get("MemFree") ?? 0) +
				(mem.get("Buffers") ?? 0) +
				(mem.get("Cached") ?? 0);

		const usedPct = ((totalKb - availKb) / totalKb) * 100;
		return Math.round(Math.max(0, Math.min(100, usedPct)) * 10) / 10;
	} catch {
		return null;
	}
}

function readCpuFreqMhz(): number | null {
	try {
		const cpuBase = "/sys/devices/system/cpu";
		if (existsSync(cpuBase)) {
			let sumKhz = 0;
			let count = 0;
			for (const entry of readdirSync(cpuBase)) {
				if (!/^cpu\d+$/.test(entry)) continue;
				const freqFile = join(cpuBase, entry, "cpufreq", "scaling_cur_freq");
				if (existsSync(freqFile)) {
					const khz = Number(readFileSync(freqFile, "utf-8").trim());
					if (Number.isFinite(khz) && khz > 0) {
						sumKhz += khz;
						count++;
					}
				}
			}
			if (count > 0) {
				return Math.round(sumKhz / count / 1000);
			}
		}

		// Fallback: /proc/cpuinfo
		const cpuinfo = readFileSync("/proc/cpuinfo", "utf-8");
		let sumMhz = 0;
		let count = 0;
		for (const line of cpuinfo.split("\n")) {
			if (line.startsWith("cpu MHz")) {
				const mhz = Number(line.split(":")[1]?.trim());
				if (Number.isFinite(mhz) && mhz > 0) {
					sumMhz += mhz;
					count++;
				}
			}
		}
		if (count > 0) return Math.round(sumMhz / count);
	} catch {}
	return null;
}

function readGpuPct(): number | null {
	try {
		const drmBase = "/sys/class/drm";
		if (existsSync(drmBase)) {
			for (const entry of readdirSync(drmBase)) {
				if (!/^card\d+$/.test(entry)) continue;
				const amdPath = join(drmBase, entry, "device", "gpu_busy_percent");
				if (existsSync(amdPath)) {
					const val = Number(readFileSync(amdPath, "utf-8").trim());
					if (Number.isFinite(val) && val >= 0)
						return Math.round(val * 10) / 10;
				}
			}
		}
	} catch {}
	return null;
}

let prevProcMap = new Map<number, number>();
let prevProcTotalCpuTicks = 0;
let cachedMemTotalKb = 0;

function getMemTotalKb(): number {
	if (cachedMemTotalKb > 0) return cachedMemTotalKb;
	try {
		const meminfo = readFileSync("/proc/meminfo", "utf-8");
		for (const line of meminfo.split("\n")) {
			if (line.startsWith("MemTotal:")) {
				cachedMemTotalKb = Number.parseInt(line.replace(/\D+/g, ""), 10) || 1;
				return cachedMemTotalKb;
			}
		}
	} catch {}
	return 1;
}

function readTopProcesses(): string | null {
	try {
		// Read total system CPU ticks from /proc/stat
		const stat = readFileSync("/proc/stat", "utf-8");
		const [cpuHeaderLine] = stat.split("\n");
		if (!cpuHeaderLine?.startsWith("cpu ")) return null;

		const [
			,
			user = 0,
			nice = 0,
			system = 0,
			idle = 0,
			iowait = 0,
			irq = 0,
			softirq = 0,
			steal = 0,
		] = cpuHeaderLine.trim().split(/\s+/).map(Number);

		const currentTotalCpuTicks =
			user + nice + system + idle + iowait + irq + softirq + steal;
		const deltaTotalCpu = currentTotalCpuTicks - prevProcTotalCpuTicks;
		prevProcTotalCpuTicks = currentTotalCpuTicks;

		const totalMemKb = getMemTotalKb();
		const pageSizeKb = 4; // Linux x86_64/arm64 standard page size (4 KB)

		const currentMap = new Map<number, number>();
		const groupMap = new Map<
			string,
			{ cpuTicks: number; rssPages: number; count: number }
		>();

		const entries = readdirSync("/proc");
		for (const entry of entries) {
			const firstChar = entry.charCodeAt(0);
			if (firstChar < 48 || firstChar > 57) continue; // skip non-PID entries

			const pid = Number(entry);
			try {
				const statContent = readFileSync(`/proc/${pid}/stat`, "utf-8");
				const openParen = statContent.indexOf("(");
				const closeParen = statContent.lastIndexOf(")");
				if (openParen === -1 || closeParen === -1) continue;

				const name = statContent.substring(openParen + 1, closeParen);
				const rest = statContent.substring(closeParen + 2).split(" ");

				// utime (field 14: index 11) & stime (field 15: index 12)
				// rss pages (field 24: index 21)
				const utime = Number(rest[11]) || 0;
				const stime = Number(rest[12]) || 0;
				const rss = Number(rest[21]) || 0;
				const ticks = utime + stime;

				currentMap.set(pid, ticks);

				const prevTicks = prevProcMap.get(pid);
				const deltaTicks =
					prevTicks !== undefined ? Math.max(0, ticks - prevTicks) : 0;

				const group = groupMap.get(name);
				if (group) {
					group.cpuTicks += deltaTicks;
					group.rssPages += rss;
					group.count += 1;
				} else {
					groupMap.set(name, {
						cpuTicks: deltaTicks,
						rssPages: rss,
						count: 1,
					});
				}
			} catch {
				// Process exited between readdir and stat read (expected in Linux VFS)
			}
		}

		prevProcMap = currentMap;

		if (deltaTotalCpu <= 0 || groupMap.size === 0) return null;

		const top: TopProcessGroup[] = Array.from(groupMap.entries())
			.map(([name, data]) => {
				const cpuPct = (data.cpuTicks / deltaTotalCpu) * 100;
				const memKb = data.rssPages * pageSizeKb;
				const memPct = (memKb / totalMemKb) * 100;
				return {
					name,
					cpu: Math.round(cpuPct * 10) / 10,
					mem: Math.round(memPct * 10) / 10,
					count: data.count,
				};
			})
			.sort((a, b) => b.cpu - a.cpu || b.mem - a.mem)
			.slice(0, 5);

		return top.length > 0 ? JSON.stringify(top) : null;
	} catch {
		return null;
	}
}

function readSysfsLoad1(): number | null {
	try {
		const p = "/proc/loadavg";
		if (existsSync(p)) {
			const [load1Str] = readFileSync(p, "utf-8").trim().split(/\s+/);
			const num = Number(load1Str);
			return Number.isFinite(num) ? Math.round(num * 100) / 100 : null;
		}
	} catch {
		/* no /proc/loadavg */
	}
	return null;
}

// ── energy: auto-detect energy_* (µWh) vs charge_* (µAh) ────────────
function readEnergy(): { now: number; full: number; design: number } {
	if (exists("energy_now")) {
		return {
			now: readNum("energy_now") / 1_000_000,
			full: readNum("energy_full") / 1_000_000,
			design: readNum("energy_full_design") / 1_000_000,
		};
	}
	// charge-based: µAh × design voltage → Wh
	const vDesign = readNum("voltage_min_design") / 1_000_000;
	return {
		now: (readNum("charge_now") / 1_000_000) * vDesign,
		full: (readNum("charge_full") / 1_000_000) * vDesign,
		design: (readNum("charge_full_design") / 1_000_000) * vDesign,
	};
}

// ── power: prefer power_now, fall back to current × voltage ──────────
function readPower(): number {
	const pw = readOpt("power_now");
	if (pw !== null) return pw / 1_000_000;
	const i = readOpt("current_now");
	const v = readOpt("voltage_now");
	if (i !== null && v !== null) return (i * v) / 1e12;
	return 0;
}

// ── UPower D-Bus for smoothed time estimates ─────────────────────────
export function upowerProp(prop: string): number | null {
	try {
		const { stdout, exitCode } = Bun.spawnSync([
			"busctl",
			"get-property",
			"org.freedesktop.UPower",
			"/org/freedesktop/UPower/devices/battery_BAT0",
			"org.freedesktop.UPower.Device",
			prop,
		]);
		if (exitCode !== 0) return null;
		const num = Number(stdout.toString().replace(/^[a-z]\s+/, ""));
		return Number.isFinite(num) && num > 0 ? num : null;
	} catch {
		return null;
	}
}

// ── read ─────────────────────────────────────────────────────────────
export async function readTelemetry(): Promise<TelemetrySample> {
	const status = read("status");
	const energy = readEnergy();
	const powerW = readPower();
	const voltageV = readNum("voltage_now") / 1_000_000;
	const voltDesign = readNum("voltage_min_design") / 1_000_000;
	const isCharging = status === "Charging";
	const sysTemps = readSystemTemps();
	const battTemp = readBatteryTemp();

	let tte = upowerProp("TimeToEmpty");
	let ttf = upowerProp("TimeToFull");
	if (tte === null && !isCharging && powerW > 0.5)
		tte = Math.round((energy.now / powerW) * 3600);
	if (ttf === null && isCharging && powerW > 0.5)
		ttf = Math.round(((energy.full - energy.now) / powerW) * 3600);

	const cpuPct = readCpuPct();
	const memPct = readMemPct();
	const cpuFreq = readCpuFreqMhz();
	const gpuPct = readGpuPct();
	const topProcs = readTopProcesses();
	const load1 = readSysfsLoad1();

	return {
		ts: new Date().toISOString(),
		charge_pct: readNum("capacity"),
		status,
		energy_wh: Math.round(energy.now * 1000) / 1000,
		energy_full_wh: Math.round(energy.full * 1000) / 1000,
		energy_design_wh: Math.round(energy.design * 1000) / 1000,
		power_w: Math.round(powerW * 1000) / 1000,
		voltage_v: voltageV,
		voltage_design_v: voltDesign,
		cycle_count: readOpt("cycle_count"),
		estimated_cycle_count: 0,
		battery_temp_c: battTemp,
		health_pct:
			energy.design > 0
				? Math.round((energy.full / energy.design) * 10000) / 100
				: 100,
		is_charging: isCharging,
		is_present: read("present") === "1",
		time_to_empty_s: tte,
		time_to_full_s: ttf,
		cpu_temp_c: sysTemps.cpu_c,
		gpu_temp_c: sysTemps.gpu_c,
		nvme_temp_c: sysTemps.nvme_c,
		cpu_pct: cpuPct,
		mem_pct: memPct,
		top_processes: topProcs,
		cpu_freq_mhz: cpuFreq,
		gpu_pct: gpuPct,
		gpu_power_w: sysTemps.gpu_power_w,
		load1,
	};
}
