import { describe, expect, test } from "bun:test";
import { alert } from "../../src/alerts";
import type { BatterySample, NotificationOptions } from "../../src/types";

function createMockSample(
	overrides: Partial<BatterySample> = {},
): BatterySample {
	return {
		ts: "2026-08-28T00:00:00.000Z",
		charge_pct: 50,
		status: "Discharging",
		energy_wh: 30,
		energy_full_wh: 60,
		energy_design_wh: 60,
		power_w: 10,
		voltage_v: 12.0,
		voltage_design_v: 12.0,
		cycle_count: 50,
		estimated_cycle_count: 50,
		battery_temp_c: 30,
		health_pct: 95,
		is_charging: false,
		is_present: true,
		time_to_empty_s: 7200,
		time_to_full_s: null,
		cpu_temp_c: 45,
		gpu_temp_c: null,
		nvme_temp_c: null,
		cpu_pct: 5,
		mem_pct: 20,
		top_processes: null,
		cpu_freq_mhz: 2400,
		gpu_pct: null,
		gpu_power_w: null,
		load1: 0.5,
		...overrides,
	};
}

describe("alert logic and edge-crossing triggers", () => {
	test("fires high charge alert only on initial edge crossing", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		const prev79 = createMockSample({ charge_pct: 79, is_charging: true });
		const curr80 = createMockSample({ charge_pct: 80, is_charging: true });
		const curr81 = createMockSample({ charge_pct: 81, is_charging: true });

		// 79% -> 80% while charging (crossing 80% threshold)
		alert(curr80, prev79, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Battery Charge Target Reached");
		expect(notifications[0].urgency).toBe("normal");

		// 80% -> 81% while charging (already crossed)
		notifications.length = 0;
		alert(curr81, curr80, notifyFn);
		expect(notifications.length).toBe(0);
	});

	test("fires low battery alert only on initial downward edge crossing", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		const prev21 = createMockSample({ charge_pct: 21, is_charging: false });
		const curr20 = createMockSample({ charge_pct: 20, is_charging: false });
		const curr19 = createMockSample({ charge_pct: 19, is_charging: false });

		// 21% -> 20% discharging
		alert(curr20, prev21, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Low Battery");

		// 20% -> 19% discharging (already below 20%, not below crit 10%)
		notifications.length = 0;
		alert(curr19, curr20, notifyFn);
		expect(notifications.length).toBe(0);
	});

	test("fires both low and critical alerts when jumping across both thresholds in one sample", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		const prev25 = createMockSample({ charge_pct: 25, is_charging: false });
		const curr10 = createMockSample({ charge_pct: 10, is_charging: false });

		alert(curr10, prev25, notifyFn);
		expect(notifications.map((n) => n.title)).toEqual([
			"Low Battery",
			"CRITICAL: Battery Low",
		]);
		expect(
			notifications.find((n) => n.title.includes("CRITICAL"))?.urgency,
		).toBe("critical");
	});

	test("fires only critical alert when dropping from low to critical", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		const prev11 = createMockSample({ charge_pct: 11, is_charging: false });
		const curr10 = createMockSample({ charge_pct: 10, is_charging: false });

		alert(curr10, prev11, notifyFn);
		expect(notifications.map((n) => n.title)).toEqual([
			"CRITICAL: Battery Low",
		]);
	});

	test("fires battery temperature warning and critical alerts", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		// Warning level (45 <= temp < 50)
		alert(createMockSample({ battery_temp_c: 46 }), null, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Warning: High Battery Temperature");
		expect(notifications[0].urgency).toBe("normal");

		// Critical level (temp >= 50)
		notifications.length = 0;
		alert(createMockSample({ battery_temp_c: 52 }), null, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("CRITICAL: Battery Overheating");
		expect(notifications[0].urgency).toBe("critical");
	});

	test("fires health alert when capacity health is degraded below threshold", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		alert(createMockSample({ health_pct: 75 }), null, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Battery Health Notice");
	});

	test("fires over-voltage warning only when actively charging", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		// Charging with voltage > 1.15x design voltage (12.0 * 1.15 = 13.8V -> 14.0V)
		alert(
			createMockSample({
				is_charging: true,
				voltage_v: 14.0,
				voltage_design_v: 12.0,
			}),
			null,
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Warning: Over-Voltage Charging");

		// Discharging with same voltage does not fire over-voltage
		notifications.length = 0;
		alert(
			createMockSample({
				is_charging: false,
				voltage_v: 14.0,
				voltage_design_v: 12.0,
			}),
			null,
			notifyFn,
		);
		expect(notifications.length).toBe(0);
	});

	test("fires heat-soak risk warning when charging while CPU is hot", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		// Charging with CPU temp >= 85°C
		alert(
			createMockSample({
				is_charging: true,
				cpu_temp_c: 88,
			}),
			null,
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Warning: Heat-Soak Risk");

		// Discharging with high CPU temp does not fire heat-soak alert
		notifications.length = 0;
		alert(
			createMockSample({
				is_charging: false,
				cpu_temp_c: 88,
			}),
			null,
			notifyFn,
		);
		expect(notifications.length).toBe(0);
	});
});
