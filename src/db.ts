import { mkdirSync } from "node:fs";
import { SQL } from "bun";
import {
	DB_DIR,
	DB_PATH,
	DEBUG_DB_PATH,
	DEBUG_RETENTION_HOURS,
} from "./config";
import { DEBUG_MIGRATIONS, HISTORICAL_MIGRATIONS, migrate } from "./migrations";
import type { BatterySample } from "./types";

let histSql: SQL | null = null;
let debugSql: SQL | null = null;

// ── estimated cycle calculation ──────────────────────────────────────
export function computeEstimatedCycles(
	curr: BatterySample,
	prev: BatterySample | null,
): number {
	if (!prev || curr.energy_design_wh <= 0) return 0;

	const prevCycles = prev.estimated_cycle_count ?? 0;

	if (!curr.is_charging && prev.energy_wh > curr.energy_wh) {
		const deltaWh = prev.energy_wh - curr.energy_wh;
		if (deltaWh > 0 && deltaWh <= curr.energy_design_wh) {
			const deltaCycles = deltaWh / curr.energy_design_wh;
			return Math.round((prevCycles + deltaCycles) * 10000) / 10000;
		}
	}

	return prevCycles;
}

// ── historical database (battery.db) ─────────────────────────────────
async function initHistoricalDb(): Promise<SQL> {
	if (histSql) return histSql;

	mkdirSync(DB_DIR, { recursive: true });
	const sql = new SQL(`sqlite://${DB_PATH}`);

	await sql`PRAGMA journal_mode = WAL;`;
	await sql`PRAGMA synchronous = NORMAL;`;
	await sql`PRAGMA wal_autocheckpoint = 100;`;
	await sql`PRAGMA busy_timeout = 5000;`;

	await migrate(sql, HISTORICAL_MIGRATIONS, "battery.db");

	histSql = sql;
	return histSql;
}

export async function getLatestHistoricalSample(): Promise<BatterySample | null> {
	const sql = await initHistoricalDb();
	const rows = await sql`SELECT * FROM samples ORDER BY id DESC LIMIT 1;`;
	return rows.length > 0 ? (rows[0] as unknown as BatterySample) : null;
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
async function initDebugDb(): Promise<SQL> {
	if (debugSql) return debugSql;

	mkdirSync(DB_DIR, { recursive: true });
	const sql = new SQL(`sqlite://${DEBUG_DB_PATH}`);

	await sql`PRAGMA journal_mode = WAL;`;
	await sql`PRAGMA synchronous = NORMAL;`;
	await sql`PRAGMA wal_autocheckpoint = 100;`;
	await sql`PRAGMA busy_timeout = 5000;`;

	await migrate(sql, DEBUG_MIGRATIONS, "debug.db");

	debugSql = sql;
	return debugSql;
}

export async function getLatestSample(): Promise<BatterySample | null> {
	const sql = await initDebugDb();
	const rows = await sql`SELECT * FROM samples ORDER BY id DESC LIMIT 1;`;
	return rows.length > 0 ? (rows[0] as unknown as BatterySample) : null;
}

export async function storeDebug(s: BatterySample): Promise<void> {
	const sql = await initDebugDb();
	await sql`INSERT INTO samples ${sql(s)}`;
}

export async function pruneDebug(hours = DEBUG_RETENTION_HOURS): Promise<void> {
	const sql = await initDebugDb();
	await sql.unsafe(
		`DELETE FROM samples WHERE julianday(ts) < julianday('now', '-${hours} hours');`,
	);
}

// ── cleanup & test helpers ───────────────────────────────────────────
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

export function setDbConnectionsForTesting(
	historical: SQL | null = null,
	debug: SQL | null = null,
): void {
	if (histSql && histSql !== historical) {
		histSql.close().catch(() => {});
	}
	if (debugSql && debugSql !== debug) {
		debugSql.close().catch(() => {});
	}
	histSql = historical;
	debugSql = debug;
}
