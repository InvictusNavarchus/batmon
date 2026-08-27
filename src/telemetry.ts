import { existsSync, readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { SYSFS } from "./config";
import { readGlances } from "./glances";
import type { SensorsData, SystemTemps, TelemetrySample } from "./types";

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

// ── native sysfs fallbacks for clock & load ──────────────────────────
function readSysfsCpuFreq(): number | null {
	try {
		const p = "/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq";
		if (existsSync(p)) {
			const khz = Number(readFileSync(p, "utf-8").trim());
			return Number.isFinite(khz) && khz > 0 ? Math.round(khz / 1000) : null;
		}
	} catch {
		/* no cpufreq sysfs */
	}
	return null;
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
	const glances = await readGlances();

	let tte = upowerProp("TimeToEmpty");
	let ttf = upowerProp("TimeToFull");
	if (tte === null && !isCharging && powerW > 0.5)
		tte = Math.round((energy.now / powerW) * 3600);
	if (ttf === null && isCharging && powerW > 0.5)
		ttf = Math.round(((energy.full - energy.now) / powerW) * 3600);

	const cpuFreq = glances.cpu_freq_mhz ?? readSysfsCpuFreq();
	const load1 = readSysfsLoad1();

	return {
		ts: new Date().toISOString(),
		percentage: readNum("capacity"),
		status,
		energy_wh: Math.round(energy.now * 1000) / 1000,
		energy_full_wh: Math.round(energy.full * 1000) / 1000,
		energy_design: Math.round(energy.design * 1000) / 1000,
		power_w: Math.round(powerW * 1000) / 1000,
		voltage_v: voltageV,
		voltage_design: voltDesign,
		cycle_count: readOpt("cycle_count"),
		estimated_cycle_count: 0,
		temperature_c: battTemp,
		capacity_pct:
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
		cpu_pct: glances.cpu_pct,
		mem_pct: glances.mem_pct,
		top_processes: glances.top_processes,
		cpu_freq_mhz: cpuFreq,
		gpu_pct: glances.gpu_pct,
		gpu_power_w: sysTemps.gpu_power_w,
		load1,
	};
}

export const readBattery = readTelemetry;
