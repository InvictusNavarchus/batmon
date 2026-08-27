import type { SQL } from "bun";

export interface Migration {
	version: number;
	name: string;
	up: (sql: SQL) => Promise<void>;
}

async function addColumnIfNotExists(
	sql: SQL,
	table: string,
	column: string,
	type: string,
): Promise<void> {
	const cols = await sql`PRAGMA table_info(${sql(table)});`;
	const exists = cols.some((c: { name: string }) => c.name === column);
	if (!exists) {
		await sql.unsafe(`ALTER TABLE ${table} ADD COLUMN ${column} ${type};`);
	}
}

async function renameColumnIfExists(
	sql: SQL,
	table: string,
	oldColumn: string,
	newColumn: string,
): Promise<void> {
	const cols = await sql`PRAGMA table_info(${sql(table)});`;
	const hasOld = cols.some((c: { name: string }) => c.name === oldColumn);
	const hasNew = cols.some((c: { name: string }) => c.name === newColumn);
	if (hasOld && !hasNew) {
		await sql.unsafe(
			`ALTER TABLE ${table} RENAME COLUMN ${oldColumn} TO ${newColumn};`,
		);
	}
}

async function renameTableIfExists(
	sql: SQL,
	oldTable: string,
	newTable: string,
): Promise<void> {
	const tables =
		await sql`SELECT name FROM sqlite_master WHERE type='table' AND name=${oldTable};`;
	const newTables =
		await sql`SELECT name FROM sqlite_master WHERE type='table' AND name=${newTable};`;
	if (tables.length > 0 && newTables.length === 0) {
		await sql.unsafe(`ALTER TABLE ${oldTable} RENAME TO ${newTable};`);
	}
}

// ── Historical Database Migrations (battery.db) ──────────────────────
export const HISTORICAL_MIGRATIONS: Migration[] = [
	{
		version: 1,
		name: "create_initial_samples_table",
		up: async (sql) => {
			await sql`
				CREATE TABLE IF NOT EXISTS samples (
					id               INTEGER PRIMARY KEY AUTOINCREMENT,
					ts               TEXT    NOT NULL,
					charge_pct       REAL,
					status           TEXT,
					energy_wh        REAL,
					energy_full_wh   REAL,
					energy_design_wh REAL,
					power_w          REAL,
					voltage_v        REAL,
					voltage_design_v REAL,
					cycle_count      INTEGER,
					battery_temp_c   REAL,
					health_pct       REAL,
					is_charging      INTEGER,
					is_present       INTEGER,
					time_to_empty_s  INTEGER,
					time_to_full_s   INTEGER,
					cpu_temp_c       REAL,
					gpu_temp_c       REAL,
					nvme_temp_c      REAL
				);
			`;
			await sql`CREATE INDEX IF NOT EXISTS idx_ts ON samples(ts);`;
		},
	},
	{
		version: 2,
		name: "add_estimated_cycle_count",
		up: async (sql) => {
			await addColumnIfNotExists(
				sql,
				"samples",
				"estimated_cycle_count",
				"REAL",
			);
		},
	},
	{
		version: 3,
		name: "add_glances_and_system_telemetry",
		up: async (sql) => {
			await addColumnIfNotExists(sql, "samples", "cpu_pct", "REAL");
			await addColumnIfNotExists(sql, "samples", "mem_pct", "REAL");
			await addColumnIfNotExists(sql, "samples", "top_processes", "TEXT");
		},
	},
	{
		version: 4,
		name: "add_flight_telemetry_metrics",
		up: async (sql) => {
			await addColumnIfNotExists(sql, "samples", "cpu_freq_mhz", "REAL");
			await addColumnIfNotExists(sql, "samples", "gpu_pct", "REAL");
			await addColumnIfNotExists(sql, "samples", "gpu_power_w", "REAL");
			await addColumnIfNotExists(sql, "samples", "load1", "REAL");
		},
	},
	{
		version: 5,
		name: "standardize_column_names",
		up: async (sql) => {
			await renameColumnIfExists(sql, "samples", "percentage", "charge_pct");
			await renameColumnIfExists(sql, "samples", "capacity_pct", "health_pct");
			await renameColumnIfExists(
				sql,
				"samples",
				"energy_design",
				"energy_design_wh",
			);
			await renameColumnIfExists(
				sql,
				"samples",
				"voltage_design",
				"voltage_design_v",
			);
			await renameColumnIfExists(
				sql,
				"samples",
				"temperature_c",
				"battery_temp_c",
			);
		},
	},
];

// ── Debug Flight Recorder Migrations (debug.db) ───────────────────────
export const DEBUG_MIGRATIONS: Migration[] = [
	{
		version: 1,
		name: "create_debug_samples_table",
		up: async (sql) => {
			await sql`
				CREATE TABLE IF NOT EXISTS samples (
					id                    INTEGER PRIMARY KEY AUTOINCREMENT,
					ts                    TEXT    NOT NULL,
					charge_pct            REAL,
					status                TEXT,
					energy_wh             REAL,
					energy_full_wh        REAL,
					energy_design_wh      REAL,
					power_w               REAL,
					voltage_v             REAL,
					voltage_design_v      REAL,
					cycle_count           INTEGER,
					estimated_cycle_count REAL,
					battery_temp_c        REAL,
					health_pct            REAL,
					is_charging           INTEGER,
					is_present            INTEGER,
					time_to_empty_s       INTEGER,
					time_to_full_s        INTEGER,
					cpu_temp_c            REAL,
					gpu_temp_c            REAL,
					nvme_temp_c           REAL,
					cpu_pct               REAL,
					mem_pct               REAL,
					top_processes         TEXT,
					cpu_freq_mhz          REAL,
					gpu_pct               REAL,
					gpu_power_w           REAL,
					load1                 REAL
				);
			`;
			await sql`CREATE INDEX IF NOT EXISTS idx_debug_ts ON samples(ts);`;
		},
	},
	{
		version: 2,
		name: "standardize_debug_column_names",
		up: async (sql) => {
			for (const table of ["samples", "samples_debug"]) {
				await renameColumnIfExists(sql, table, "percentage", "charge_pct");
				await renameColumnIfExists(sql, table, "capacity_pct", "health_pct");
				await renameColumnIfExists(
					sql,
					table,
					"energy_design",
					"energy_design_wh",
				);
				await renameColumnIfExists(
					sql,
					table,
					"voltage_design",
					"voltage_design_v",
				);
				await renameColumnIfExists(
					sql,
					table,
					"temperature_c",
					"battery_temp_c",
				);
			}
		},
	},
	{
		version: 3,
		name: "rename_samples_debug_to_samples",
		up: async (sql) => {
			await renameTableIfExists(sql, "samples_debug", "samples");
		},
	},
];

// ── Migration Runner ──────────────────────────────────────────────────
export async function migrate(
	sql: SQL,
	migrations: Migration[],
	_dbName = "database",
): Promise<void> {
	const res = await sql`PRAGMA user_version;`;
	const currentVersion = Number(res[0]?.user_version ?? 0);

	for (const migration of migrations) {
		if (migration.version > currentVersion) {
			await migration.up(sql);
			await sql.unsafe(`PRAGMA user_version = ${migration.version};`);
		}
	}
}
