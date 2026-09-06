import { describe, expect, test } from "bun:test";
import { computeEstimatedCycles } from "../../src/db";
import type { BatterySample, PowerState } from "../../src/types";

function createMockSample(
	overrides: Partial<BatterySample> = {},
): BatterySample {
	const status =
		overrides.status ?? (overrides.is_charging ? "Charging" : "Discharging");
	const isCharging = overrides.is_charging ?? status === "Charging";
	const powerState =
		overrides.power_state ??
		(isCharging
			? "charging"
			: status === "Discharging"
				? "discharging"
				: "ac_idle");

	return {
		ts: "2026-08-28T00:00:00.000Z",
		charge_pct: 80,
		status,
		power_state: powerState,
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
		is_charging: isCharging,
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
		boot_id: "mock-boot-id",
		uptime_s: 12345.6,
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

	test("does not increase cycle count when ac_idle / float charge dipping while plugged in", () => {
		const prev = createMockSample({
			energy_wh: 50,
			estimated_cycle_count: 1.0,
			status: "Full",
			power_state: "ac_idle",
			is_charging: false,
		});
		const curr = createMockSample({
			energy_wh: 49.9,
			energy_design_wh: 50,
			status: "Full",
			power_state: "ac_idle",
			is_charging: false,
		});

		expect(computeEstimatedCycles(curr, prev)).toBe(1.0);
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

	test("preserves full double-precision floating point without write-time truncation", () => {
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
		// delta = 0.666667 / 50 = 0.01333334 -> 1.01333334
		expect(result).toBeCloseTo(1.01333334, 6);
	});

	test("accumulates sub-milliwatt-hour micro-increments over successive ticks without freezing", () => {
		let currentCycles = 5.0;
		const designWh = 60;
		// Discharging at 8W: 1s delta = (8 / 3600) Wh = 0.00222 Wh
		const deltaWhPerSec = 8 / 3600;
		let energy = 50;

		for (let i = 0; i < 100; i++) {
			const prev = createMockSample({
				energy_wh: energy,
				estimated_cycle_count: currentCycles,
				energy_design_wh: designWh,
				is_charging: false,
			});
			energy -= deltaWhPerSec;
			const curr = createMockSample({
				energy_wh: energy,
				energy_design_wh: designWh,
				is_charging: false,
			});
			currentCycles = computeEstimatedCycles(curr, prev);
		}

		// 100 * (8 / 3600) / 60 = 0.0037037... cycles
		expect(currentCycles).toBeGreaterThan(5.0035);
		expect(currentCycles).toBeLessThan(5.004);
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

	test("carries forward previous cycles without integrating offline delta when boot_id changes", () => {
		const prev = createMockSample({
			energy_wh: 50,
			estimated_cycle_count: 5.0,
			energy_design_wh: 50,
			boot_id: "old-boot-uuid",
			uptime_s: 36000,
			is_charging: false,
		});
		const curr = createMockSample({
			energy_wh: 40, // 10 Wh lower after sitting powered off
			energy_design_wh: 50,
			boot_id: "new-boot-uuid",
			uptime_s: 15,
			is_charging: false,
		});

		// Must not add 10Wh (0.2 cycles) across the reboot gap
		expect(computeEstimatedCycles(curr, prev)).toBe(5.0);
	});

	test("carries forward previous cycles without integrating offline delta when uptime_s decreases", () => {
		const prev = createMockSample({
			energy_wh: 50,
			estimated_cycle_count: 5.0,
			energy_design_wh: 50,
			boot_id: "same-boot-uuid",
			uptime_s: 50000,
			is_charging: false,
		});
		const curr = createMockSample({
			energy_wh: 40,
			energy_design_wh: 50,
			boot_id: "same-boot-uuid",
			uptime_s: 30, // Decreased uptime indicates host rebooted
			is_charging: false,
		});

		expect(computeEstimatedCycles(curr, prev)).toBe(5.0);
	});

	test("applies delta normally when boot_id matches and uptime is continuous", () => {
		const prev = createMockSample({
			energy_wh: 50,
			estimated_cycle_count: 5.0,
			energy_design_wh: 50,
			boot_id: "boot-uuid-1",
			uptime_s: 1000,
			is_charging: false,
		});
		const curr = createMockSample({
			energy_wh: 45,
			energy_design_wh: 50,
			boot_id: "boot-uuid-1",
			uptime_s: 1001,
			is_charging: false,
		});

		// 5 Wh / 50 Wh = 0.1 cycles -> 5.1
		expect(computeEstimatedCycles(curr, prev)).toBe(5.1);
	});

	test("derives power state from status when power_state is missing on legacy sample", () => {
		const prev = createMockSample({
			estimated_cycle_count: 5.0,
			energy_wh: 40,
			energy_design_wh: 50,
		});
		const curr = createMockSample({
			estimated_cycle_count: 5.0,
			energy_wh: 35,
			energy_design_wh: 50,
			status: "Discharging",
			power_state: undefined as unknown as PowerState,
		});

		const cycles = computeEstimatedCycles(curr, prev);
		// 5.0 + (40 - 35) / 50 = 5.0 + 0.1 = 5.1
		expect(cycles).toBeCloseTo(5.1, 5);
	});
});
