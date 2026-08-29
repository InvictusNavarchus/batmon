import { SQL } from "bun";
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
	closeDbs,
	getLatestHistoricalSample,
	getLatestSample,
	pruneDebug,
	setDbConnectionsForTesting,
	store,
	storeDebug,
} from "../../src/db";
import {
	DEBUG_MIGRATIONS,
	HISTORICAL_MIGRATIONS,
	migrate,
} from "../../src/migrations";
import type { BatterySample } from "../../src/types";

function createMockSample(
	overrides: Partial<BatterySample> = {},
): BatterySample {
	return {
		ts: new Date().toISOString(),
		charge_pct: 85,
		status: "Discharging",
		energy_wh: 45,
		energy_full_wh: 50,
		energy_design_wh: 50,
		power_w: 12.5,
		voltage_v: 11.5,
		voltage_design_v: 11.4,
		cycle_count: 42,
		estimated_cycle_count: 5.0,
		battery_temp_c: 28.5,
		health_pct: 90.0,
		is_charging: false,
		is_present: true,
		time_to_empty_s: 12000,
		time_to_full_s: null,
		cpu_temp_c: 48.0,
		gpu_temp_c: null,
		nvme_temp_c: null,
		cpu_pct: 12.5,
		mem_pct: 35.0,
		top_processes: JSON.stringify([
			{ name: "bun", cpu: 5.0, mem: 2.0, count: 1 },
		]),
		cpu_freq_mhz: 2800,
		gpu_pct: null,
		gpu_power_w: null,
		load1: 0.85,
		...overrides,
	};
}

describe("database operations (store, getLatest, prune)", () => {
	let histSql: SQL;
	let debugSql: SQL;

	beforeEach(async () => {
		histSql = new SQL("sqlite://:memory:");
		debugSql = new SQL("sqlite://:memory:");

		await migrate(histSql, HISTORICAL_MIGRATIONS, "battery.db");
		await migrate(debugSql, DEBUG_MIGRATIONS, "debug.db");

		setDbConnectionsForTesting(histSql, debugSql);
	});

	afterEach(async () => {
		await closeDbs();
	});

	test("store inserts a sample and returns previous historical sample", async () => {
		expect(await getLatestHistoricalSample()).toBeNull();

		const firstSample = createMockSample({
			ts: "2026-08-28T00:00:00.000Z",
			energy_wh: 50,
			estimated_cycle_count: 0,
		});

		const prevOfFirst = await store(firstSample);
		expect(prevOfFirst).toBeNull();

		const latest = await getLatestHistoricalSample();
		expect(latest).not.toBeNull();
		expect(latest?.charge_pct).toBe(85);
		expect(latest?.estimated_cycle_count).toBe(0);

		// Store second sample discharging by 5 Wh (5/50 = 0.1 cycles)
		const secondSample = createMockSample({
			ts: "2026-08-28T00:01:00.000Z",
			energy_wh: 45,
		});

		const prevOfSecond = await store(secondSample);
		expect(prevOfSecond?.energy_wh).toBe(50);

		const latestAfterSecond = await getLatestHistoricalSample();
		expect(latestAfterSecond?.estimated_cycle_count).toBe(0.1);
	});

	test("storeDebug inserts flight recorder samples into debug database and getLatestSample retrieves the most recent record", async () => {
		expect(await getLatestSample()).toBeNull();
		const sample1 = createMockSample({
			ts: "2026-08-28T00:00:01.000Z",
			charge_pct: 70,
			estimated_cycle_count: 2,
		});
		const sample2 = createMockSample({
			ts: "2026-08-28T00:00:02.000Z",
			charge_pct: 80,
			estimated_cycle_count: 3,
		});

		// first sample
		await storeDebug(sample1);
		let latest = await getLatestSample();
		expect(latest).not.toBeNull();
		expect(latest?.charge_pct).toBe(70);
		expect(latest?.estimated_cycle_count).toBe(2);

		// second sample
		await storeDebug(sample2);
		latest = await getLatestSample();
		expect(latest).not.toBeNull();
		expect(latest?.charge_pct).toBe(80);
		expect(latest?.estimated_cycle_count).toBe(3);

		const rows =
			(await debugSql`SELECT count(*) as count FROM samples;`) as Array<{
				count: number;
			}>;
		expect(Number(rows[0].count)).toBe(2);
	});

	test("pruneDebug removes samples older than retention hours", async () => {
		// Insert an old sample (10 hours ago)
		const tenHoursAgo = new Date(Date.now() - 10 * 3600 * 1000).toISOString();
		const oldSample = createMockSample({ ts: tenHoursAgo });

		// Insert a fresh sample (now)
		const nowSample = createMockSample({ ts: new Date().toISOString() });

		await storeDebug(oldSample);
		await storeDebug(nowSample);

		const rows =
			(await debugSql`SELECT count(*) as count FROM samples;`) as Array<{
				count: number;
			}>;
		expect(Number(rows[0].count)).toBe(2);

		// Prune older than 6 hours
		await pruneDebug(6);

		const survivingRows = (await debugSql`SELECT ts FROM samples;`) as Array<{
			ts: string;
		}>;
		expect(survivingRows.length).toBe(1);
		expect(survivingRows[0].ts).toBe(nowSample.ts);
	});
});
