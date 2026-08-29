import { SQL } from "bun";
import { afterEach, beforeEach, describe, expect, spyOn, test } from "bun:test";
import { closeDbs, setDbConnectionsForTesting } from "../../src/db";
import { resetDaemonStateForTesting, runTick } from "../../src/index";
import {
	DEBUG_MIGRATIONS,
	HISTORICAL_MIGRATIONS,
	migrate,
} from "../../src/migrations";
import * as telemetry from "../../src/telemetry";
import type { TelemetrySample } from "../../src/types";

function createMockSample(
	overrides: Partial<TelemetrySample> = {},
): TelemetrySample {
	return {
		ts: "2026-08-28T00:00:00.000Z",
		charge_pct: 75,
		status: "Discharging",
		energy_wh: 40,
		energy_full_wh: 50,
		energy_design_wh: 50,
		power_w: 12.0,
		voltage_v: 11.8,
		voltage_design_v: 11.4,
		cycle_count: 50,
		estimated_cycle_count: 5.0,
		battery_temp_c: 32.0,
		health_pct: 92.0,
		is_charging: false,
		is_present: true,
		time_to_empty_s: 12000,
		time_to_full_s: null,
		cpu_temp_c: 45.0,
		gpu_temp_c: null,
		nvme_temp_c: null,
		cpu_pct: 10.0,
		mem_pct: 25.0,
		top_processes: null,
		cpu_freq_mhz: 2400,
		gpu_pct: null,
		gpu_power_w: null,
		load1: 0.5,
		...overrides,
	};
}

describe("daemon tick cadence scenario", () => {
	let histSql: SQL;
	let debugSql: SQL;
	let readSpy: ReturnType<typeof spyOn> | null = null;

	beforeEach(async () => {
		resetDaemonStateForTesting();
		histSql = new SQL("sqlite://:memory:");
		debugSql = new SQL("sqlite://:memory:");

		await migrate(histSql, HISTORICAL_MIGRATIONS, "battery.db");
		await migrate(debugSql, DEBUG_MIGRATIONS, "debug.db");

		setDbConnectionsForTesting(histSql, debugSql);
	});

	afterEach(async () => {
		if (readSpy) {
			readSpy.mockRestore();
			readSpy = null;
		}
		await closeDbs();
		resetDaemonStateForTesting();
	});

	test("logs 1s flight samples to debug.db and 60s downsampled rows to battery.db", async () => {
		let callCount = 0;
		readSpy = spyOn(telemetry, "readTelemetry").mockImplementation(async () => {
			callCount++;
			return createMockSample({
				ts: new Date(1700000000000 + callCount * 1000).toISOString(),
				charge_pct: 75,
				energy_wh: 40 - callCount * 0.01,
			});
		});

		// Execute 61 ticks (tick 0 through 60)
		for (let i = 0; i <= 60; i++) {
			await runTick();
		}

		// debug.db should receive all 61 samples
		const debugRows =
			(await debugSql`SELECT count(*) as count FROM samples;`) as Array<{
				count: number;
			}>;
		expect(Number(debugRows[0].count)).toBe(61);

		// battery.db should receive tick 0 and tick 60 (2 samples)
		const histRows =
			(await histSql`SELECT count(*) as count FROM samples;`) as Array<{
				count: number;
			}>;
		expect(Number(histRows[0].count)).toBe(2);
	});

	test("skips recording and resets alert state when battery is not present", async () => {
		readSpy = spyOn(telemetry, "readTelemetry").mockImplementation(async () =>
			createMockSample({ is_present: false }),
		);

		await runTick();

		const debugRows =
			(await debugSql`SELECT count(*) as count FROM samples;`) as Array<{
				count: number;
			}>;
		expect(Number(debugRows[0].count)).toBe(0);
	});
});
