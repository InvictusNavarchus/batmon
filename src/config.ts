import { join } from "node:path";

// ── paths ────────────────────────────────────────────────────────────
export const SYSFS = "/sys/class/power_supply/BAT0";
export const DB_DIR = join(process.env.HOME ?? "/tmp", ".local/share/batmon");
export const DB_PATH = join(DB_DIR, "battery.db");

// ── threshold constants ──────────────────────────────────────────────
export const TEMP_WARN = 45; // °C – battery temp (if sensor exists)
export const TEMP_CRIT = 50; // °C
export const CAP_WARN = 80; // % of design capacity (battery health wear)
export const CHARGE_HIGH_WARN = 80; // % – unplug reminder
export const CHARGE_LOW_WARN = 20; // % – plug-in reminder
export const CHARGE_CRIT_WARN = 10; // % – critical low battery
export const CPU_HOT_CHARGING = 85; // °C – warn if charging while system is hot
