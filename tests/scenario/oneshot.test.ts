import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { AlertManager } from "../../src/alerts";
import * as db from "../../src/db";
import { runOneshot } from "../../src/index";
import * as telemetry from "../../src/telemetry";
import type { TelemetrySample } from "../../src/types";

function createMockSample(
	overrides: Partial<TelemetrySample> = {},
): TelemetrySample {
	return {
		ts: "2026-08-28T00:00:00.000Z",
		charge_pct: 80,
		status: "Charging",
		energy_wh: 40,
		energy_full_wh: 50,
		energy_design_wh: 50,
		power_w: 15.0,
		voltage_v: 12.2,
		voltage_design_v: 11.4,
		cycle_count: 50,
		estimated_cycle_count: 5.0,
		battery_temp_c: 30.0,
		health_pct: 95.0,
		is_charging: true,
		is_present: true,
		time_to_empty_s: null,
		time_to_full_s: 3600,
		cpu_temp_c: 50.0,
		gpu_temp_c: null,
		nvme_temp_c: null,
		cpu_pct: 8.0,
		mem_pct: 30.0,
		top_processes: null,
		cpu_freq_mhz: 2800,
		gpu_pct: null,
		gpu_power_w: null,
		load1: 0.3,
		...overrides,
	};
}

describe("runOneshot scenario", () => {
	const activeSpies: Array<{ mockRestore: () => void }> = [];

	afterEach(() => {
		for (const spy of activeSpies) {
			spy.mockRestore();
		}
		activeSpies.length = 0;
	});

	test("records a single sample to both databases and closes connections", async () => {
		const mockSample = createMockSample();
		const readSpy = spyOn(telemetry, "readTelemetry").mockResolvedValue(
			mockSample,
		);
		const storeSpy = spyOn(db, "store").mockResolvedValue(null);
		const storeDebugSpy = spyOn(db, "storeDebug").mockResolvedValue();
		const alertSpy = spyOn(AlertManager.prototype, "check").mockImplementation(
			() => {},
		);
		const closeSpy = spyOn(db, "closeDbs").mockResolvedValue();

		activeSpies.push(readSpy, storeSpy, storeDebugSpy, alertSpy, closeSpy);

		await runOneshot();

		expect(storeSpy).toHaveBeenCalledTimes(1);
		expect(storeSpy).toHaveBeenCalledWith(mockSample);

		expect(storeDebugSpy).toHaveBeenCalledTimes(1);
		expect(storeDebugSpy).toHaveBeenCalledWith(mockSample);

		expect(alertSpy).toHaveBeenCalledTimes(1);
		expect(alertSpy).toHaveBeenCalledWith(mockSample);

		expect(closeSpy).toHaveBeenCalledTimes(1);
	});

	test("exits early if battery is not present", async () => {
		const readSpy = spyOn(telemetry, "readTelemetry").mockResolvedValue(
			createMockSample({ is_present: false }),
		);
		const storeSpy = spyOn(db, "store").mockResolvedValue(null);
		const exitSpy = spyOn(process, "exit").mockImplementation((() => {
			throw new Error("process.exit called");
		}) as unknown as typeof process.exit);

		activeSpies.push(readSpy, storeSpy, exitSpy);

		await expect(runOneshot()).rejects.toThrow("process.exit called");
		expect(storeSpy).not.toHaveBeenCalled();
	});
});
