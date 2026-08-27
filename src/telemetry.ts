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
export function readBatteryTemp(): number | null {
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

export function readCpuPct(): number | null {
	try {
		const stat = readFileSync("/proc/stat", "utf-8");
		const line = stat.split("\n")[0];
		if (!line?.startsWith("cpu ")) return null;
		const parts = line.trim().split(/\s+/).slice(1).map(Number);
		const idle = parts[3] + (parts[4] || 0); // idle + iowait
		const total = parts.reduce((acc, v) => acc + v, 0);

		if (!prevCpu) {
			prevCpu = { idle, total };
			return null;
		}

		const totalDelta = total - prevCpu.total;
		const idleDelta = idle - prevCpu.idle;
		prevCpu = { idle, total };

		if (totalDelta <= 0) return 0;
		const pct = (1 - idleDelta / totalDelta) * 100;
		return Math.round(Math.max(0, Math.min(100, pct)) * 10) / 10;
	} catch {
		return null;
	}
}

export function readMemPct(): number | null {
	try {
		const meminfo = readFileSync("/proc/meminfo", "utf-8");
		let totalKb: number | null = null;
		let availKb: number | null = null;
		let freeKb = 0;
		let buffersKb = 0;
		let cachedKb = 0;

		for (const line of meminfo.split("\n")) {
			if (line.startsWith("MemTotal:"))
				totalKb = Number(line.replace(/\D+/g, ""));
			else if (line.startsWith("MemAvailable:"))
				availKb = Number(line.replace(/\D+/g, ""));
			else if (line.startsWith("MemFree:"))
				freeKb = Number(line.replace(/\D+/g, ""));
			else if (line.startsWith("Buffers:"))
				buffersKb = Number(line.replace(/\D+/g, ""));
			else if (line.startsWith("Cached:"))
				cachedKb = Number(line.replace(/\D+/g, ""));
		}

		if (totalKb === null || totalKb <= 0) return null;

		const effectiveAvail = availKb ?? freeKb + buffersKb + cachedKb;
		const pct = ((totalKb - effectiveAvail) / totalKb) * 100;
		return Math.round(Math.max(0, Math.min(100, pct)) * 10) / 10;
	} catch {
		return null;
	}
}

export function readCpuFreqMhz(): number | null {
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

export function readGpuPct(): number | null {
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

export function readTopProcesses(): string | null {
	try {
		const { stdout, exitCode } = Bun.spawnSync([
			"ps",
			"-eo",
			"comm,%cpu,%mem",
			"--sort=-%cpu",
		]);
		if (exitCode !== 0) return null;

		const lines = stdout.toString().trim().split("\n").slice(1);
		const map = new Map<string, { cpu: number; mem: number; count: number }>();

		for (const line of lines) {
			const trimmed = line.trim();
			if (!trimmed) continue;
			const match = trimmed.match(/^(.*?)\s+([\d.]+)\s+([\d.]+)$/);
			if (!match) continue;
			const name = match[1];
			if (name === "ps") continue; // filter self-sampling artifact
			const cpu = Number(match[2]);
			const mem = Number(match[3]);

			const existing = map.get(name);
			if (existing) {
				existing.cpu += cpu;
				existing.mem += mem;
				existing.count += 1;
			} else {
				map.set(name, { cpu, mem, count: 1 });
			}
		}

		const top: TopProcessGroup[] = Array.from(map.entries())
			.map(([name, data]) => ({
				name,
				cpu: Math.round(data.cpu * 10) / 10,
				mem: Math.round(data.mem * 10) / 10,
				count: data.count,
			}))
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
			const [l1] = readFileSync(p, "utf-8").trim().split(" ");
			const num = Number(l1);
			return Number.isFinite(num) ? Math.round(num * 100) / 100 : null;
		}
	} catch {
		/* no /proc/loadavg */
	}
	return null;
}

// ── energy: auto-detect energy_* (µWh) vs charge_* (µAh) ────────────
export function readEnergy(): { now: number; full: number; design: number } {
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
export function readPower(): number {
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

export const readBattery = readTelemetry;
