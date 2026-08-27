import { join } from "node:path";

export const SYSFS = "/sys/class/power_supply/BAT0";
export const DB_DIR = join(process.env.HOME ?? "/tmp", ".local/share/batmon");
export const DB_PATH = join(DB_DIR, "battery.db");
export const DEBUG_DB_PATH = join(DB_DIR, "debug.db");

// ── flight recorder & loop configuration ─────────────────────────────
const envRetention = Number(process.env.BATMON_DEBUG_RETENTION_HOURS);
export const DEBUG_RETENTION_HOURS =
	Number.isFinite(envRetention) && envRetention > 0 ? envRetention : 6;
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
