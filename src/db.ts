import { mkdirSync } from "node:fs";
import { SQL } from "bun";
import {
	DB_DIR,
	DB_PATH,
	DEBUG_DB_PATH,
	DEBUG_RETENTION_HOURS,
} from "./config";
import type { BatterySample } from "./types";

let histSql: SQL | null = null;
let debugSql: SQL | null = null;

// ── estimated cycle calculation ──────────────────────────────────────
export function computeEstimatedCycles(
	curr: BatterySample,
	prev: BatterySample | null,
): number {
	if (!prev || curr.energy_design <= 0) return 0;

	const prevCycles = prev.estimated_cycle_count ?? 0;

	if (!curr.is_charging && prev.energy_wh > curr.energy_wh) {
		const deltaWh = prev.energy_wh - curr.energy_wh;
		if (deltaWh > 0 && deltaWh <= curr.energy_design) {
			const deltaCycles = deltaWh / curr.energy_design;
			return Math.round((prevCycles + deltaCycles) * 10000) / 10000;
		}
	}

	return prevCycles;
}

// ── historical database (battery.db) ─────────────────────────────────
export async function initHistoricalDb(): Promise<SQL> {
	if (histSql) return histSql;

	mkdirSync(DB_DIR, { recursive: true });
	const sql = new SQL(`sqlite://${DB_PATH}`);

	await sql`PRAGMA journal_mode = WAL;`;
	await sql`PRAGMA synchronous = NORMAL;`;
	await sql`PRAGMA wal_autocheckpoint = 100;`;
	await sql`PRAGMA busy_timeout = 5000;`;
	await sql`
		CREATE TABLE IF NOT EXISTS samples (
			id                    INTEGER PRIMARY KEY AUTOINCREMENT,
			ts                    TEXT    NOT NULL,
			percentage            REAL,
			status                TEXT,
			energy_wh             REAL,
			energy_full_wh        REAL,
			energy_design         REAL,
			power_w               REAL,
			voltage_v             REAL,
			voltage_design        REAL,
			cycle_count           INTEGER,
			estimated_cycle_count REAL,
			temperature_c         REAL,
			capacity_pct          REAL,
			is_charging           INTEGER,
			is_present            INTEGER,
			time_to_empty_s       INTEGER,
			time_to_full_s        INTEGER,
			cpu_temp_c            REAL,
			gpu_temp_c            REAL,
			nvme_temp_c           REAL,
			cpu_pct               REAL,
			mem_pct               REAL,
			top_processes         TEXT
		);
	`;
	await sql`CREATE INDEX IF NOT EXISTS idx_ts ON samples(ts);`;

	const cols = await sql`PRAGMA table_info(samples);`;
	const existingCols = new Set(cols.map((c: { name: string }) => c.name));

	const migrations: Record<string, string> = {
		estimated_cycle_count: "REAL",
		cpu_pct: "REAL",
		mem_pct: "REAL",
		top_processes: "TEXT",
	};

	for (const [col, type] of Object.entries(migrations)) {
		if (!existingCols.has(col)) {
			await sql.unsafe(`ALTER TABLE samples ADD COLUMN ${col} ${type};`);
		}
	}

	histSql = sql;
	return histSql;
}

export async function store(s: BatterySample): Promise<BatterySample | null> {
	const sql = await initHistoricalDb();

	const rows = await sql`SELECT * FROM samples ORDER BY id DESC LIMIT 1;`;
	const prev = rows.length > 0 ? (rows[0] as unknown as BatterySample) : null;

	s.estimated_cycle_count = computeEstimatedCycles(s, prev);

	await sql`INSERT INTO samples ${sql(s)}`;
	return prev;
}

// ── debug flight recorder database (debug.db) ─────────────────────────
export async function initDebugDb(): Promise<SQL> {
	if (debugSql) return debugSql;

	mkdirSync(DB_DIR, { recursive: true });
	const sql = new SQL(`sqlite://${DEBUG_DB_PATH}`);

	await sql`PRAGMA journal_mode = WAL;`;
	await sql`PRAGMA synchronous = NORMAL;`;
	await sql`PRAGMA wal_autocheckpoint = 100;`;
	await sql`PRAGMA busy_timeout = 5000;`;
	await sql`
		CREATE TABLE IF NOT EXISTS samples_debug (
			id                    INTEGER PRIMARY KEY AUTOINCREMENT,
			ts                    TEXT    NOT NULL,
			percentage            REAL,
			status                TEXT,
			energy_wh             REAL,
			energy_full_wh        REAL,
			energy_design         REAL,
			power_w               REAL,
			voltage_v             REAL,
			voltage_design        REAL,
			cycle_count           INTEGER,
			estimated_cycle_count REAL,
			temperature_c         REAL,
			capacity_pct          REAL,
			is_charging           INTEGER,
			is_present            INTEGER,
			time_to_empty_s       INTEGER,
			time_to_full_s        INTEGER,
			cpu_temp_c            REAL,
			gpu_temp_c            REAL,
			nvme_temp_c           REAL,
			cpu_pct               REAL,
			mem_pct               REAL,
			top_processes         TEXT
		);
	`;
	await sql`CREATE INDEX IF NOT EXISTS idx_debug_ts ON samples_debug(ts);`;

	debugSql = sql;
	return debugSql;
}

export async function storeDebug(s: BatterySample): Promise<void> {
	const sql = await initDebugDb();
	await sql`INSERT INTO samples_debug ${sql(s)}`;
}

export async function pruneDebug(hours = DEBUG_RETENTION_HOURS): Promise<void> {
	const sql = await initDebugDb();
	await sql.unsafe(
		`DELETE FROM samples_debug WHERE ts < datetime('now', '-${hours} hours');`,
	);
}

// ── cleanup ──────────────────────────────────────────────────────────
export async function closeDbs(): Promise<void> {
	if (histSql) {
		await histSql.close();
		histSql = null;
	}
	if (debugSql) {
		await debugSql.close();
		debugSql = null;
	}
}
