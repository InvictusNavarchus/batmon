#!/usr/bin/env bun
/**
 * batmon – battery health logger → SQLite
 *
 * Reads sysfs (auto-detects energy_* vs charge_* batteries),
 * shells out to UPower D-Bus for smoothed time estimates,
 * reads system temps via `sensors -j` as thermal proxy,
 * and stores everything in a local SQLite database.
 *
 * Designed to run as a systemd user timer (every 60 s).
 */

import { Database } from "bun:sqlite";
import { readFileSync, existsSync, mkdirSync, readdirSync } from "node:fs";
import { join } from "node:path";

// ── configuration ────────────────────────────────────────────────────
const SYSFS     = "/sys/class/power_supply/BAT0";
const DB_DIR    = join(process.env.HOME ?? "/tmp", ".local/share/batmon");
const DB_PATH   = join(DB_DIR, "battery.db");

const TEMP_WARN       = 45;   // °C – battery temp (if sensor exists)
const TEMP_CRIT       = 50;   // °C
const CAP_WARN        = 80;   // % of design capacity
const CPU_HOT_CHARGING = 85;  // °C – warn if charging while system is hot
// ─────────────────────────────────────────────────────────────────────

// ── sysfs helpers ────────────────────────────────────────────────────
function sysfsPath(name: string): string { return join(SYSFS, name); }
function exists(name: string): boolean   { return existsSync(sysfsPath(name)); }
function read(name: string): string      { return readFileSync(sysfsPath(name), "utf-8").trim(); }
function readNum(name: string): number   { return Number(read(name)); }
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
  } catch { /* no hwmon */ }

  return null;
}

// ── system temps via `sensors -j` (thermal environment proxy) ────────
interface SystemTemps {
  cpu_c:  number | null;
  gpu_c:  number | null;
  nvme_c: number | null;
}

/** Shape of `sensors -j` output */
interface SensorsData {
  [adapter: string]: {
    [feature: string]: {
      [subfeature: string]: number;
    };
  };
}

function readSystemTemps(): SystemTemps {
  try {
    const { stdout, exitCode } = Bun.spawnSync(["sensors", "-j"]);
    if (exitCode !== 0) return { cpu_c: null, gpu_c: null, nvme_c: null };
    const data: SensorsData = JSON.parse(stdout.toString());

    const find = (prefix: string): SensorsData[string] | undefined =>
      Object.entries(data).find(([k]) => k.startsWith(prefix))?.[1];

    const cpu  = find("k10temp")?.["Tctl"]?.["temp1_input"]
              ?? find("coretemp")?.["Package id 0"]?.["temp1_input"]
              ?? null;
    const gpu  = find("amdgpu")?.["edge"]?.["temp1_input"]
              ?? find("i915")?.["temp1"]?.["temp1_input"]
              ?? null;
    const nvme = find("nvme")?.["Composite"]?.["temp1_input"] ?? null;

    return { cpu_c: cpu, gpu_c: gpu, nvme_c: nvme };
  } catch {
    return { cpu_c: null, gpu_c: null, nvme_c: null };
  }
}

// ── energy: auto-detect energy_* (µWh) vs charge_* (µAh) ────────────
function readEnergy(): { now: number; full: number; design: number } {
  if (exists("energy_now")) {
    return {
      now:    readNum("energy_now")         / 1_000_000,
      full:   readNum("energy_full")        / 1_000_000,
      design: readNum("energy_full_design") / 1_000_000,
    };
  }
  // charge-based: µAh × design voltage → Wh
  const vDesign = readNum("voltage_min_design") / 1_000_000;
  return {
    now:    readNum("charge_now")         / 1_000_000 * vDesign,
    full:   readNum("charge_full")        / 1_000_000 * vDesign,
    design: readNum("charge_full_design") / 1_000_000 * vDesign,
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
function upowerProp(prop: string): number | null {
  try {
    const { stdout, exitCode } = Bun.spawnSync([
      "busctl", "get-property",
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

// ── types ────────────────────────────────────────────────────────────
interface BatterySample {
  ts:              string;
  percentage:      number;
  status:          string;
  energy_wh:       number;
  energy_full_wh:  number;
  energy_design:   number;
  power_w:         number;
  voltage_v:       number;
  voltage_design:  number;
  cycle_count:     number;
  temperature_c:   number | null;
  capacity_pct:    number;
  is_charging:     boolean;
  is_present:      boolean;
  time_to_empty_s: number | null;
  time_to_full_s:  number | null;
  cpu_temp_c:      number | null;
  gpu_temp_c:      number | null;
  nvme_temp_c:     number | null;
}

// ── read ─────────────────────────────────────────────────────────────
function readBattery(): BatterySample {
  const status     = read("status");
  const energy     = readEnergy();
  const powerW     = readPower();
  const voltageV   = readNum("voltage_now") / 1_000_000;
  const voltDesign = readNum("voltage_min_design") / 1_000_000;
  const isCharging = status === "Charging";
  const sysTemps   = readSystemTemps();
  const battTemp   = readBatteryTemp();

  let tte = upowerProp("TimeToEmpty");
  let ttf = upowerProp("TimeToFull");
  if (tte === null && !isCharging && powerW > 0.5)
    tte = Math.round(energy.now / powerW * 3600);
  if (ttf === null && isCharging && powerW > 0.5)
    ttf = Math.round((energy.full - energy.now) / powerW * 3600);

  return {
    ts:              new Date().toISOString(),
    percentage:      readNum("capacity"),
    status,
    energy_wh:       Math.round(energy.now    * 1000) / 1000,
    energy_full_wh:  Math.round(energy.full   * 1000) / 1000,
    energy_design:   Math.round(energy.design * 1000) / 1000,
    power_w:         Math.round(powerW * 1000) / 1000,
    voltage_v:       voltageV,
    voltage_design:  voltDesign,
    cycle_count:     readOpt("cycle_count") ?? 0,
    temperature_c:   battTemp,
    capacity_pct:    energy.design > 0
                       ? Math.round(energy.full / energy.design * 10000) / 100
                       : 100,
    is_charging:     isCharging,
    is_present:      read("present") === "1",
    time_to_empty_s: tte,
    time_to_full_s:  ttf,
    cpu_temp_c:      sysTemps.cpu_c,
    gpu_temp_c:      sysTemps.gpu_c,
    nvme_temp_c:     sysTemps.nvme_c,
  };
}

// ── store ────────────────────────────────────────────────────────────
const SCHEMA = /* sql */ `
  CREATE TABLE IF NOT EXISTS samples (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    ts              TEXT    NOT NULL,
    percentage      REAL,
    status          TEXT,
    energy_wh       REAL,
    energy_full_wh  REAL,
    energy_design   REAL,
    power_w         REAL,
    voltage_v       REAL,
    voltage_design  REAL,
    cycle_count     INTEGER,
    temperature_c   REAL,
    capacity_pct    REAL,
    is_charging     INTEGER,
    is_present      INTEGER,
    time_to_empty_s INTEGER,
    time_to_full_s  INTEGER,
    cpu_temp_c      REAL,
    gpu_temp_c      REAL,
    nvme_temp_c     REAL
  );
  CREATE INDEX IF NOT EXISTS idx_ts ON samples(ts);
`;

function store(s: BatterySample): void {
  mkdirSync(DB_DIR, { recursive: true });
  const db = new Database(DB_PATH, { create: true });
  db.exec("PRAGMA journal_mode = WAL;");
  db.exec("PRAGMA busy_timeout = 5000;");
  db.exec(SCHEMA);
  db.prepare(`
    INSERT INTO samples
      (ts, percentage, status, energy_wh, energy_full_wh,
       energy_design, power_w, voltage_v, voltage_design,
       cycle_count, temperature_c, capacity_pct,
       is_charging, is_present, time_to_empty_s, time_to_full_s,
       cpu_temp_c, gpu_temp_c, nvme_temp_c)
    VALUES
      ($ts,$pct,$st,$enow,$efull,$edes,$pw,$volt,$vdes,
       $cyc,$temp,$cap,$chg,$pres,$tte,$ttf,
       $cpu,$gpu,$nvme)
  `).run({
    $ts: s.ts, $pct: s.percentage, $st: s.status,
    $enow: s.energy_wh, $efull: s.energy_full_wh, $edes: s.energy_design,
    $pw: s.power_w, $volt: s.voltage_v, $vdes: s.voltage_design,
    $cyc: s.cycle_count, $temp: s.temperature_c, $cap: s.capacity_pct,
    $chg: s.is_charging ? 1 : 0, $pres: s.is_present ? 1 : 0,
    $tte: s.time_to_empty_s, $ttf: s.time_to_full_s,
    $cpu: s.cpu_temp_c, $gpu: s.gpu_temp_c, $nvme: s.nvme_temp_c,
  });
  db.close();
}

// ── alerts ───────────────────────────────────────────────────────────
function alert(s: BatterySample): void {
  const msgs: string[] = [];

  if (s.temperature_c !== null) {
    if (s.temperature_c >= TEMP_CRIT)
      msgs.push(`🔴 CRITICAL: battery ${s.temperature_c.toFixed(1)} °C – unplug NOW`);
    else if (s.temperature_c >= TEMP_WARN)
      msgs.push(`🟠 WARNING: battery ${s.temperature_c.toFixed(1)} °C`);
  }
  if (s.capacity_pct < CAP_WARN)
    msgs.push(`🟡 Battery health ${s.capacity_pct.toFixed(1)}% of design`);
  if (s.is_charging && s.voltage_v > s.voltage_design * 1.15)
    msgs.push(`🟠 Voltage ${s.voltage_v.toFixed(2)} V well above design ${s.voltage_design} V`);
  if (s.is_charging && s.cpu_temp_c !== null && s.cpu_temp_c > CPU_HOT_CHARGING)
    msgs.push(`🟠 Charging while CPU at ${s.cpu_temp_c.toFixed(0)} °C – heat-soak risk`);

  for (const m of msgs) {
    Bun.spawn(["notify-send", "-u", "critical", "batmon", m], {
      stdout: "ignore", stderr: "ignore",
    }).exited.catch(() => {});
    console.error(m);
  }
}

// ── main ─────────────────────────────────────────────────────────────
try {
  const sample = readBattery();
  if (!sample.is_present) process.exit(0);
  store(sample);
  alert(sample);
} catch (err) {
  console.error("batmon:", err);
  process.exit(1);
}