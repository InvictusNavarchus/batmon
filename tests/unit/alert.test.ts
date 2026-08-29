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

	test("fires battery temperature warning and critical alerts only on threshold crossing", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		// Normal to Warning level (30 -> 46 °C)
		const prevNormal = createMockSample({ battery_temp_c: 30 });
		const currWarn1 = createMockSample({ battery_temp_c: 46 });
		alert(currWarn1, prevNormal, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Warning: High Battery Temperature");
		expect(notifications[0].urgency).toBe("normal");

		// Persisting in Warning level (46 -> 47 °C) - should NOT re-fire
		notifications.length = 0;
		const currWarn2 = createMockSample({ battery_temp_c: 47 });
		alert(currWarn2, currWarn1, notifyFn);
		expect(notifications.length).toBe(0);

		// Warning to Critical level (47 -> 51 °C) - should fire Critical with discharging advice
		notifications.length = 0;
		const currCritDischarging = createMockSample({
			battery_temp_c: 51,
			is_charging: false,
		});
		alert(currCritDischarging, currWarn2, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("CRITICAL: Battery Overheating");
		expect(notifications[0].urgency).toBe("critical");
		expect(notifications[0].body).toContain("reduce system load immediately");

		// When charging, critical overheating advises unplugging
		notifications.length = 0;
		const currCritCharging = createMockSample({
			battery_temp_c: 51,
			is_charging: true,
		});
		alert(currCritCharging, currWarn2, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].body).toContain("unplug charger immediately");

		// Persisting in Critical level (51 -> 52 °C) - should NOT re-fire
		notifications.length = 0;
		const currCrit2 = createMockSample({ battery_temp_c: 52 });
		alert(currCrit2, currCritDischarging, notifyFn);
		expect(notifications.length).toBe(0);
	});

	test("fires health alert only on initial downward crossing below threshold", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		// Crossing from 82% to 78% health
		const prevGood = createMockSample({ health_pct: 82 });
		const currDegraded = createMockSample({ health_pct: 78 });
		alert(currDegraded, prevGood, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Battery Health Notice");

		// Persisting in degraded state (78% -> 77%) - should NOT re-fire
		notifications.length = 0;
		const currStillDegraded = createMockSample({ health_pct: 77 });
		alert(currStillDegraded, currDegraded, notifyFn);
		expect(notifications.length).toBe(0);
	});

	test("fires over-voltage warning only on initial crossing while charging and guards zero design voltage", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		// Should not fire over-voltage when voltage_design_v is 0
		const zeroDesignCurr = createMockSample({
			is_charging: true,
			voltage_v: 12.0,
			voltage_design_v: 0,
		});
		alert(zeroDesignCurr, null, notifyFn);
		expect(notifications.length).toBe(0);

		const prevNormal = createMockSample({
			is_charging: true,
			voltage_v: 12.5,
			voltage_design_v: 12.0,
		});
		const currOver = createMockSample({
			is_charging: true,
			voltage_v: 14.0,
			voltage_design_v: 12.0,
		});

		// 12.5V -> 14.0V while charging
		alert(currOver, prevNormal, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Warning: Over-Voltage Charging");

		// Persisting over-voltage (14.0V -> 14.2V) - should NOT re-fire
		notifications.length = 0;
		const currStillOver = createMockSample({
			is_charging: true,
			voltage_v: 14.2,
			voltage_design_v: 12.0,
		});
		alert(currStillOver, currOver, notifyFn);
		expect(notifications.length).toBe(0);
	});

	test("fires heat-soak risk warning only on initial crossing while charging", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);

		const prevWarm = createMockSample({
			is_charging: true,
			cpu_temp_c: 80,
		});
		const currHot = createMockSample({
			is_charging: true,
			cpu_temp_c: 88,
		});

		// 80°C -> 88°C while charging
		alert(currHot, prevWarm, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Warning: Heat-Soak Risk");

		// Persisting hot CPU while charging (88°C -> 89°C) - should NOT re-fire
		notifications.length = 0;
		const currStillHot = createMockSample({
			is_charging: true,
			cpu_temp_c: 89,
		});
		alert(currStillHot, currHot, notifyFn);
		expect(notifications.length).toBe(0);
	});
});
