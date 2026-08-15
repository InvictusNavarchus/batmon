import { mkdirSync } from "node:fs";
import { SQL } from "bun";
import { DB_DIR, DB_PATH } from "./config";
import type { BatterySample } from "./types";

// ── store ────────────────────────────────────────────────────────────
export async function store(s: BatterySample): Promise<BatterySample | null> {
	mkdirSync(DB_DIR, { recursive: true });
	const sql = new SQL(`sqlite://${DB_PATH}`);

	await sql`PRAGMA journal_mode = WAL;`;
	await sql`PRAGMA busy_timeout = 5000;`;
	await sql`
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
	`;
	await sql`CREATE INDEX IF NOT EXISTS idx_ts ON samples(ts);`;

	const rows = await sql`SELECT * FROM samples ORDER BY id DESC LIMIT 1;`;
	const prev = rows.length > 0 ? (rows[0] as unknown as BatterySample) : null;

	await sql`INSERT INTO samples ${sql(s)}`;
	await sql.close();

	return prev;
}
