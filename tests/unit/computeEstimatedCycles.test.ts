import { describe, expect, test } from "bun:test";
import { computeEstimatedCycles } from "../../src/db";
import type { BatterySample } from "../../src/types";

function createMockSample(
	overrides: Partial<BatterySample> = {},
): BatterySample {
	return {
		ts: "2026-08-28T00:00:00.000Z",
		charge_pct: 80,
		status: "Discharging",
		energy_wh: 40,
		energy_full_wh: 50,
		energy_design_wh: 50,
		power_w: 10,
		voltage_v: 12.0,
		voltage_design_v: 12.0,
		cycle_count: 100,
		estimated_cycle_count: 5.0,
		battery_temp_c: 30,
		health_pct: 90,
		is_charging: false,
		is_present: true,
		time_to_empty_s: 3600,
		time_to_full_s: null,
		cpu_temp_c: 45,
		gpu_temp_c: null,
		nvme_temp_c: null,
		cpu_pct: 10,
		mem_pct: 20,
		top_processes: null,
		cpu_freq_mhz: 2400,
		gpu_pct: null,
		gpu_power_w: null,
		load1: 0.5,
		...overrides,
	};
}

describe("computeEstimatedCycles", () => {
	test("returns 0 when previous sample is null", () => {
		const curr = createMockSample();
		expect(computeEstimatedCycles(curr, null)).toBe(0);
	});

	test("returns 0 when energy_design_wh is zero or negative", () => {
		const prev = createMockSample({ energy_wh: 45, estimated_cycle_count: 2 });
		const currZero = createMockSample({ energy_wh: 40, energy_design_wh: 0 });
		const currNegative = createMockSample({
			energy_wh: 40,
			energy_design_wh: -10,
		});

		expect(computeEstimatedCycles(currZero, prev)).toBe(0);
		expect(computeEstimatedCycles(currNegative, prev)).toBe(0);
	});

	test("does not increase cycle count while charging", () => {
		const prev = createMockSample({
			energy_wh: 40,
			estimated_cycle_count: 3.5,
			is_charging: true,
		});
		const curr = createMockSample({
			energy_wh: 35,
			energy_design_wh: 50,
			estimated_cycle_count: 0,
			is_charging: true,
		});

		expect(computeEstimatedCycles(curr, prev)).toBe(3.5);
	});

	test("increments cycle count proportionally when discharging within 1 cycle", () => {
		const prev = createMockSample({
			energy_wh: 50,
			estimated_cycle_count: 1.0,
			is_charging: false,
		});
		const curr = createMockSample({
			energy_wh: 45,
			energy_design_wh: 50,
			is_charging: false,
		});

		// delta = 5 Wh / 50 Wh = 0.1 cycles
		expect(computeEstimatedCycles(curr, prev)).toBe(1.1);
	});

	test("increments exactly 1 cycle when deltaWh equals energy_design_wh", () => {
		const prev = createMockSample({
			energy_wh: 50,
			estimated_cycle_count: 2.0,
			is_charging: false,
		});
		const curr = createMockSample({
			energy_wh: 0,
			energy_design_wh: 50,
			is_charging: false,
		});

		expect(computeEstimatedCycles(curr, prev)).toBe(3.0);
	});

	test("ignores spurious energy delta larger than design capacity", () => {
		const prev = createMockSample({
			energy_wh: 100,
			estimated_cycle_count: 2.5,
			is_charging: false,
		});
		const curr = createMockSample({
			energy_wh: 10,
			energy_design_wh: 50, // delta is 90 > 50
			is_charging: false,
		});

		expect(computeEstimatedCycles(curr, prev)).toBe(2.5);
	});

	test("rounds cycle count to 4 decimal places", () => {
		const prev = createMockSample({
			energy_wh: 50,
			estimated_cycle_count: 1.0,
			is_charging: false,
		});
		const curr = createMockSample({
			energy_wh: 49.333333,
			energy_design_wh: 50,
			is_charging: false,
		});

		const result = computeEstimatedCycles(curr, prev);
		// delta = 0.666667 / 50 = 0.01333334 -> 1.0133
		expect(result).toBe(1.0133);
	});

	test("handles undefined or null estimated_cycle_count on previous sample", () => {
		const prev = createMockSample({
			energy_wh: 50,
			estimated_cycle_count: undefined as unknown as number,
			is_charging: false,
		});
		const curr = createMockSample({
			energy_wh: 45,
			energy_design_wh: 50,
			is_charging: false,
		});

		expect(computeEstimatedCycles(curr, prev)).toBe(0.1);
	});
});
