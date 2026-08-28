import { SQL } from "bun";
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
	DEBUG_MIGRATIONS,
	HISTORICAL_MIGRATIONS,
	migrate,
} from "../../src/migrations";

describe("migrations", () => {
	let sql: SQL;

	beforeEach(() => {
		sql = new SQL("sqlite://:memory:");
	});

	afterEach(async () => {
		await sql.close();
	});

	test("applies all historical migrations to a fresh in-memory database", async () => {
		await migrate(sql, HISTORICAL_MIGRATIONS, "battery.db");

		const versionRows = await sql`PRAGMA user_version;`;
		expect(Number(versionRows[0].user_version)).toBe(5);

		const columns = (await sql`PRAGMA table_info(samples);`) as Array<{
			name: string;
			type: string;
		}>;
		const columnNames = columns.map((c) => c.name);

		expect(columnNames).toContain("id");
		expect(columnNames).toContain("ts");
		expect(columnNames).toContain("charge_pct");
		expect(columnNames).toContain("health_pct");
		expect(columnNames).toContain("energy_design_wh");
		expect(columnNames).toContain("voltage_design_v");
		expect(columnNames).toContain("battery_temp_c");
		expect(columnNames).toContain("estimated_cycle_count");
		expect(columnNames).toContain("cpu_pct");
		expect(columnNames).toContain("mem_pct");
		expect(columnNames).toContain("top_processes");
		expect(columnNames).toContain("cpu_freq_mhz");
		expect(columnNames).toContain("gpu_pct");
		expect(columnNames).toContain("gpu_power_w");
		expect(columnNames).toContain("load1");

		const indexes = (await sql`PRAGMA index_list(samples);`) as Array<{
			name: string;
		}>;
		expect(indexes.some((idx) => idx.name === "idx_ts")).toBe(true);
	});

	test("applies all debug migrations to a fresh in-memory database", async () => {
		await migrate(sql, DEBUG_MIGRATIONS, "debug.db");

		const versionRows = await sql`PRAGMA user_version;`;
		expect(Number(versionRows[0].user_version)).toBe(3);

		const columns = (await sql`PRAGMA table_info(samples);`) as Array<{
			name: string;
			type: string;
		}>;
		const columnNames = columns.map((c) => c.name);

		expect(columnNames).toContain("id");
		expect(columnNames).toContain("ts");
		expect(columnNames).toContain("charge_pct");
		expect(columnNames).toContain("estimated_cycle_count");
		expect(columnNames).toContain("load1");

		const indexes = (await sql`PRAGMA index_list(samples);`) as Array<{
			name: string;
		}>;
		expect(indexes.some((idx) => idx.name === "idx_debug_ts")).toBe(true);
	});

	test("is idempotent when run repeatedly on the same database", async () => {
		await migrate(sql, HISTORICAL_MIGRATIONS, "battery.db");
		await migrate(sql, HISTORICAL_MIGRATIONS, "battery.db");

		const versionRows = await sql`PRAGMA user_version;`;
		expect(Number(versionRows[0].user_version)).toBe(5);
	});

	test("applies incremental migrations from intermediate version", async () => {
		// Apply first 2 migrations manually
		await HISTORICAL_MIGRATIONS[0].up(sql);
		await HISTORICAL_MIGRATIONS[1].up(sql);
		await sql.unsafe("PRAGMA user_version = 2;");

		const beforeCols = (await sql`PRAGMA table_info(samples);`) as Array<{
			name: string;
		}>;
		expect(beforeCols.map((c) => c.name)).not.toContain("cpu_pct");

		// Run migration runner
		await migrate(sql, HISTORICAL_MIGRATIONS, "battery.db");

		const afterVersion = await sql`PRAGMA user_version;`;
		expect(Number(afterVersion[0].user_version)).toBe(5);

		const afterCols = (await sql`PRAGMA table_info(samples);`) as Array<{
			name: string;
		}>;
		expect(afterCols.map((c) => c.name)).toContain("cpu_pct");
	});

	test("renames legacy columns correctly without losing data", async () => {
		// Setup legacy table with old column names
		await sql`
			CREATE TABLE samples (
				id INTEGER PRIMARY KEY AUTOINCREMENT,
				ts TEXT NOT NULL,
				percentage REAL,
				capacity_pct REAL,
				energy_design REAL,
				voltage_design REAL,
				temperature_c REAL
			);
		`;
		await sql`
			INSERT INTO samples (ts, percentage, capacity_pct, energy_design, voltage_design, temperature_c)
			VALUES ('2026-08-28T00:00:00.000Z', 85.5, 95.0, 52.4, 11.4, 32.1);
		`;
		await sql.unsafe("PRAGMA user_version = 4;");

		// Run migration 5
		await migrate(sql, HISTORICAL_MIGRATIONS, "battery.db");

		const rows = (await sql`SELECT * FROM samples WHERE id = 1;`) as Array<{
			charge_pct: number;
			health_pct: number;
			energy_design_wh: number;
			voltage_design_v: number;
			battery_temp_c: number;
		}>;

		expect(rows.length).toBe(1);
		expect(rows[0].charge_pct).toBe(85.5);
		expect(rows[0].health_pct).toBe(95.0);
		expect(rows[0].energy_design_wh).toBe(52.4);
		expect(rows[0].voltage_design_v).toBe(11.4);
		expect(rows[0].battery_temp_c).toBe(32.1);
	});
});
