import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

export const POWER_SUPPLY_BASE = "/sys/class/power_supply";

/**
 * Auto-discovers the primary system battery sysfs directory.
 * Scans `/sys/class/power_supply/` for power supply devices with type "Battery",
 * prioritizing internal system batteries over peripheral devices.
 */
export function discoverBatteryPath(baseDir = POWER_SUPPLY_BASE): string {
	try {
		if (!existsSync(baseDir)) {
			return join(baseDir, "BAT0");
		}

		const entries = readdirSync(baseDir).sort();
		const candidates: Array<{
			path: string;
			isSystem: boolean;
			isBatName: boolean;
		}> = [];

		for (const entry of entries) {
			const entryPath = join(baseDir, entry);
			const typeFile = join(entryPath, "type");
			if (!existsSync(typeFile)) continue;

			try {
				const type = readFileSync(typeFile, "utf-8").trim();
				if (type.toLowerCase() !== "battery") continue;

				let isSystem = true;
				const scopeFile = join(entryPath, "scope");
				if (existsSync(scopeFile)) {
					const scope = readFileSync(scopeFile, "utf-8").trim().toLowerCase();
					if (scope === "device") {
						isSystem = false;
					}
				}

				const isBatName =
					/^bat\d*$/i.test(entry) || entry.toLowerCase().startsWith("bat");
				candidates.push({ path: entryPath, isSystem, isBatName });
			} catch {
				// Ignore unreadable entries
			}
		}

		if (candidates.length === 0) {
			return join(baseDir, "BAT0");
		}

		// 1. Prefer system-level battery with standard BAT* naming (e.g. BAT0, BAT1)
		// 2. Fall back to any system-level battery (e.g. macsmc-battery)
		// 3. Fall back to any discovered battery candidate
		const bestCandidate =
			candidates.find((c) => c.isSystem && c.isBatName) ??
			candidates.find((c) => c.isSystem) ??
			candidates[0];

		return bestCandidate.path;
	} catch {
		return join(baseDir, "BAT0");
	}
}

export const SYSFS = discoverBatteryPath();
export const DB_DIR = join(process.env.HOME ?? "/tmp", ".local/share/batmon");
export const DB_PATH = join(DB_DIR, "battery.db");
export const DEBUG_DB_PATH = join(DB_DIR, "debug.db");

// ── flight recorder & loop configuration ─────────────────────────────
export const DEBUG_RETENTION_HOURS = 6;
export const DEBUG_SAMPLE_INTERVAL_MS = 1000; // 1s
export const HISTORICAL_SAMPLE_INTERVAL_TICKS = 60; // 60s (every 60 debug ticks)
export const PRUNE_INTERVAL_TICKS = 300; // 5 min (every 300 debug ticks)

// ── threshold constants ──────────────────────────────────────────────
export const TEMP_WARN = 45; // °C – battery temp (if sensor exists)
export const TEMP_CRIT = 50; // °C
export const CAP_WARN = 80; // % of design capacity (battery health wear)
export const CHARGE_HIGH_WARN = 80; // % – unplug reminder
export const CHARGE_LOW_WARN = 20; // % – plug-in reminder
export const CHARGE_CRIT_WARN = 10; // % – critical low battery
export const CPU_HOT_CHARGING = 85; // °C – warn if charging while system is hot
