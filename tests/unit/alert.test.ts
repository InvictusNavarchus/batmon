import { describe, expect, test } from "bun:test";
import { AlertManager } from "../../src/alerts";
import type { BatterySample, NotificationOptions } from "../../src/types";

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
				: status === "Full" || status === "Not charging"
					? "ac_idle"
					: "unknown");

	return {
		ts: "2026-08-28T00:00:00.000Z",
		charge_pct: 50,
		status,
		power_state: powerState,
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
		is_charging: isCharging,
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
		boot_id: "mock-boot-id",
		uptime_s: 12345.6,
		...overrides,
	};
}

describe("AlertManager stateful engine with hysteresis & suppression", () => {
	test("fires high charge alert once and enforces 5% hysteresis deadband", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// Charging: 79% -> 80% (crosses CHARGE_HIGH_WARN 80%)
		manager.check(
			createMockSample({ charge_pct: 80, is_charging: true }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Battery Charge Target Reached");
		expect(notifications[0].urgency).toBe("normal");

		// Flapping: 80% -> 79% -> 80% (within 5% hysteresis deadband, threshold is < 75%)
		notifications.length = 0;
		manager.check(
			createMockSample({ charge_pct: 79, is_charging: true }),
			notifyFn,
		);
		manager.check(
			createMockSample({ charge_pct: 80, is_charging: true }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Drops below 75% (80 - 5 = 75%), re-arming the alert
		manager.check(
			createMockSample({ charge_pct: 74, is_charging: true }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Charges back up to 80% -> re-fires
		manager.check(
			createMockSample({ charge_pct: 80, is_charging: true }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Battery Charge Target Reached");
	});

	test("does not re-fire high charge alert when charging to 100% full or toggling charge status above 75%", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// Crosses 80%
		manager.check(
			createMockSample({
				charge_pct: 80,
				is_charging: true,
				status: "Charging",
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(1);

		// Continues charging to 90%, 95%, 100%
		notifications.length = 0;
		manager.check(
			createMockSample({
				charge_pct: 90,
				is_charging: true,
				status: "Charging",
			}),
			notifyFn,
		);
		manager.check(
			createMockSample({
				charge_pct: 95,
				is_charging: true,
				status: "Charging",
			}),
			notifyFn,
		);
		manager.check(
			createMockSample({
				charge_pct: 100,
				is_charging: true,
				status: "Charging",
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Reaches 100% Full (kernel reports is_charging: false, status: "Full")
		manager.check(
			createMockSample({ charge_pct: 100, is_charging: false, status: "Full" }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Trickle top-off at 100% toggles status back to "Charging"
		manager.check(
			createMockSample({
				charge_pct: 100,
				is_charging: true,
				status: "Charging",
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Momentarily unplugging at 85% and plugging back in at 84% does not re-fire
		manager.check(
			createMockSample({
				charge_pct: 85,
				is_charging: false,
				status: "Discharging",
			}),
			notifyFn,
		);
		manager.check(
			createMockSample({
				charge_pct: 84,
				is_charging: true,
				status: "Charging",
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);
	});

	test("suppresses Low alert when jumping straight to Critical battery level", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// Jumping from 25% to 9% discharging (both <= 20% and <= 10% match, but Critical must suppress Low)
		manager.check(
			createMockSample({ charge_pct: 9, is_charging: false }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("CRITICAL: Battery Low");
		expect(notifications[0].urgency).toBe("critical");
	});

	test("escalates from Low to Critical alert cleanly while discharging", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// 21% -> 20% discharging
		manager.check(
			createMockSample({ charge_pct: 20, is_charging: false }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Low Battery");
		expect(notifications[0].urgency).toBe("normal");

		// 20% -> 19% (already fired low, not critical yet)
		notifications.length = 0;
		manager.check(
			createMockSample({ charge_pct: 19, is_charging: false }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// 19% -> 10% (crosses critical threshold)
		manager.check(
			createMockSample({ charge_pct: 10, is_charging: false }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("CRITICAL: Battery Low");
		expect(notifications[0].urgency).toBe("critical");
	});

	test("re-arms low battery alerts when plugged in or charge rises above hysteresis band", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// Fire low alert at 20%
		manager.check(
			createMockSample({ charge_pct: 20, is_charging: false }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Low Battery");

		// Fluctuate 20% -> 22% -> 20% (does not clear because 22% <= 20% + 5%)
		notifications.length = 0;
		manager.check(
			createMockSample({ charge_pct: 22, is_charging: false }),
			notifyFn,
		);
		manager.check(
			createMockSample({ charge_pct: 20, is_charging: false }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Plug in charger -> resets discharging alerts
		manager.check(
			createMockSample({ charge_pct: 20, is_charging: true }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Unplug at 20% -> re-fires
		manager.check(
			createMockSample({ charge_pct: 20, is_charging: false }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Low Battery");
	});

	test("does not fire low or critical battery alert when on AC power (ac_idle) or unknown", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// Capped at 15% on AC power (status: "Not charging" -> power_state: "ac_idle")
		manager.check(
			createMockSample({
				charge_pct: 15,
				is_charging: false,
				status: "Not charging",
				power_state: "ac_idle",
			}),
			notifyFn,
		);
		// Even at 5% critical level, should not nag to connect charger if already on AC
		manager.check(
			createMockSample({
				charge_pct: 5,
				is_charging: false,
				status: "Not charging",
				power_state: "ac_idle",
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Unknown power state (e.g. unreadable sysfs) should also stay silent
		manager.check(
			createMockSample({
				charge_pct: 10,
				is_charging: false,
				status: "Unknown",
				power_state: "unknown",
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Now unplugging and discharging at 10% critical should fire critical alert
		manager.check(
			createMockSample({
				charge_pct: 10,
				is_charging: false,
				status: "Discharging",
				power_state: "discharging",
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("CRITICAL: Battery Low");

		// Re-connecting charger (entering ac_idle) resets the alert latch
		notifications.length = 0;
		manager.check(
			createMockSample({
				charge_pct: 10,
				is_charging: false,
				status: "Not charging",
				power_state: "ac_idle",
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Discharging again fires alert again
		manager.check(
			createMockSample({
				charge_pct: 10,
				is_charging: false,
				status: "Discharging",
				power_state: "discharging",
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
	});

	test("handles battery temperature warning, critical escalation, and thermal hysteresis", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// Normal to Warning level (30 -> 46 °C)
		manager.check(createMockSample({ battery_temp_c: 46 }), notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Warning: High Battery Temperature");
		expect(notifications[0].urgency).toBe("normal");

		// Persisting / flapping in Warning (46 -> 44 -> 46 °C, deadband is < 42 °C)
		notifications.length = 0;
		manager.check(createMockSample({ battery_temp_c: 44 }), notifyFn);
		manager.check(createMockSample({ battery_temp_c: 46 }), notifyFn);
		expect(notifications.length).toBe(0);

		// Escalate to Critical (51 °C) while discharging
		manager.check(
			createMockSample({ battery_temp_c: 51, is_charging: false }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("CRITICAL: Battery Overheating");
		expect(notifications[0].urgency).toBe("critical");
		expect(notifications[0].body).toContain("reduce system load immediately");

		// Persisting / flapping in Critical (51 -> 48 -> 51 °C, deadband is < 47 °C)
		notifications.length = 0;
		manager.check(
			createMockSample({ battery_temp_c: 48, is_charging: false }),
			notifyFn,
		);
		manager.check(
			createMockSample({ battery_temp_c: 51, is_charging: false }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Cool down completely to 35 °C -> re-arms
		manager.check(createMockSample({ battery_temp_c: 35 }), notifyFn);
		expect(notifications.length).toBe(0);

		// Heat up directly to Critical while charging -> fires Critical and advises unplugging
		manager.check(
			createMockSample({ battery_temp_c: 51, is_charging: true }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("CRITICAL: Battery Overheating");
		expect(notifications[0].body).toContain("unplug charger immediately");
	});

	test("ignores transient null sensor reads without triggering false edges", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// Initial warm temp
		manager.check(createMockSample({ battery_temp_c: 46 }), notifyFn);
		expect(notifications.length).toBe(1);

		// Sensor temporarily returns null on next tick
		notifications.length = 0;
		manager.check(createMockSample({ battery_temp_c: null }), notifyFn);
		expect(notifications.length).toBe(0);

		// Sensor recovers with 46 °C -> should NOT re-fire
		manager.check(createMockSample({ battery_temp_c: 46 }), notifyFn);
		expect(notifications.length).toBe(0);
	});

	test("fires health alert once and enforces hysteresis before re-arming", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// Health degraded to 78%
		manager.check(createMockSample({ health_pct: 78 }), notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Battery Health Notice");

		// Flapping around 79% (deadband threshold is >= 82%)
		notifications.length = 0;
		manager.check(createMockSample({ health_pct: 79 }), notifyFn);
		manager.check(createMockSample({ health_pct: 78 }), notifyFn);
		expect(notifications.length).toBe(0);

		// Recovers above 82%
		manager.check(createMockSample({ health_pct: 83 }), notifyFn);
		expect(notifications.length).toBe(0);

		// Degrades again to 78% -> re-fires
		manager.check(createMockSample({ health_pct: 78 }), notifyFn);
		expect(notifications.length).toBe(1);
	});

	test("fires over-voltage warning and guards zero design voltage", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// Should not fire when voltage_design_v is 0
		manager.check(
			createMockSample({
				is_charging: true,
				voltage_v: 12.0,
				voltage_design_v: 0,
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Over-voltage: 14.0V > 12.0V * 1.15 (13.8V)
		manager.check(
			createMockSample({
				is_charging: true,
				voltage_v: 14.0,
				voltage_design_v: 12.0,
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Warning: Over-Voltage Charging");

		// Hovering in over-voltage state does not re-fire
		notifications.length = 0;
		manager.check(
			createMockSample({
				is_charging: true,
				voltage_v: 14.2,
				voltage_design_v: 12.0,
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);
	});

	test("fires CPU heat-soak risk warning while charging after sustained debounce samples and enforces hysteresis", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// CPU hot (88 °C >= 85 °C) while charging requires 3 sustained samples
		manager.check(
			createMockSample({ is_charging: true, cpu_temp_c: 88 }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		manager.check(
			createMockSample({ is_charging: true, cpu_temp_c: 88 }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		manager.check(
			createMockSample({ is_charging: true, cpu_temp_c: 88 }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("Warning: Heat-Soak Risk");
		expect(notifications[0].body).toContain(
			"unplug charger to preserve health",
		);

		// Flapping (88 -> 82 -> 88 °C, deadband is < 80 °C)
		notifications.length = 0;
		manager.check(
			createMockSample({ is_charging: true, cpu_temp_c: 82 }),
			notifyFn,
		);
		manager.check(
			createMockSample({ is_charging: true, cpu_temp_c: 88 }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);
	});

	test("ignores transient CPU temperature spikes shorter than debounce threshold", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// 2 spikes at 89 °C followed by a drop to 84 °C while charging
		manager.check(
			createMockSample({ is_charging: true, cpu_temp_c: 89 }),
			notifyFn,
		);
		manager.check(
			createMockSample({ is_charging: true, cpu_temp_c: 89 }),
			notifyFn,
		);
		manager.check(
			createMockSample({ is_charging: true, cpu_temp_c: 84 }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Single spike again -> counter was reset, so still no alert
		manager.check(
			createMockSample({ is_charging: true, cpu_temp_c: 89 }),
			notifyFn,
		);
		expect(notifications.length).toBe(0);
	});

	test("fires CRITICAL Thermal Anomaly alert on high temperature during low workload (Aug 20 scenario)", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// August 20 incident: CPU at 84.1 °C during near-idle (8% CPU, 2.5W draw)
		const idleSample = createMockSample({
			is_charging: false,
			cpu_temp_c: 84.1,
			cpu_pct: 8,
			power_w: 2.5,
		});

		manager.check(idleSample, notifyFn);
		manager.check(idleSample, notifyFn);
		expect(notifications.length).toBe(0);

		// 3rd sustained sample triggers critical thermal anomaly
		manager.check(idleSample, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("CRITICAL: Thermal Anomaly");
		expect(notifications[0].urgency).toBe("critical");
		expect(notifications[0].body).toContain("84 °C");
		expect(notifications[0].body).toContain("8% CPU");

		// Deadband test: temperature in deadband (78 °C >= 75 °C) does not re-fire
		notifications.length = 0;
		manager.check(
			createMockSample({
				is_charging: false,
				cpu_temp_c: 78,
				cpu_pct: 8,
				power_w: 2.5,
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Cooldown below hysteresis band (< 75 °C) clears anomaly latch
		manager.check(
			createMockSample({
				is_charging: false,
				cpu_temp_c: 74,
				cpu_pct: 8,
				power_w: 2.5,
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Heating back up requires 3 sustained samples again
		manager.check(idleSample, notifyFn);
		manager.check(idleSample, notifyFn);
		expect(notifications.length).toBe(0);

		manager.check(idleSample, notifyFn);
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("CRITICAL: Thermal Anomaly");
	});

	test("stays completely silent during high-workload gaming or compilation even at 88 °C", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// Heavy workload (85% CPU / 45W discharging): 88 °C is expected dissipation
		const gamingSample = createMockSample({
			is_charging: false,
			cpu_temp_c: 88,
			cpu_pct: 85,
			power_w: 45.0,
		});

		for (let i = 0; i < 5; i++) {
			manager.check(gamingSample, notifyFn);
		}

		// Zero alerts generated because workload is active, not anomalous
		expect(notifications.length).toBe(0);
	});

	test("resets thermal anomaly latch when connected to charger and formats qualifying workload metric", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		// Trigger anomaly with low power (10W) but elevated CPU% (60%): body should show power, not 60% CPU
		const lowPowerSample = createMockSample({
			is_charging: false,
			cpu_temp_c: 84,
			cpu_pct: 60,
			power_w: 10.0,
		});

		for (let i = 0; i < 3; i++) {
			manager.check(lowPowerSample, notifyFn);
		}
		expect(notifications.length).toBe(1);
		expect(notifications[0].title).toBe("CRITICAL: Thermal Anomaly");
		expect(notifications[0].body).toContain("10.0 W");
		expect(notifications[0].body).not.toContain("60% CPU");

		// Connecting to charger resets the anomaly fired latch
		notifications.length = 0;
		manager.check(
			createMockSample({
				is_charging: true,
				cpu_temp_c: 72,
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);

		// Unplugging and experiencing 3 sustained anomaly samples re-fires the alert
		for (let i = 0; i < 3; i++) {
			manager.check(lowPowerSample, notifyFn);
		}
		expect(notifications.length).toBe(1);
	});

	test("reset() clears all alert latches and debounce counters", () => {
		const notifications: NotificationOptions[] = [];
		const notifyFn = (opts: NotificationOptions) => notifications.push(opts);
		const manager = new AlertManager();

		manager.check(
			createMockSample({ charge_pct: 80, is_charging: true }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);

		// Partial CPU anomaly debounce streak
		manager.check(
			createMockSample({
				cpu_temp_c: 84,
				cpu_pct: 5,
				power_w: 3.0,
				is_charging: false,
			}),
			notifyFn,
		);
		manager.check(
			createMockSample({
				cpu_temp_c: 84,
				cpu_pct: 5,
				power_w: 3.0,
				is_charging: false,
			}),
			notifyFn,
		);

		notifications.length = 0;
		manager.reset();

		// After reset, checking same sample fires charge alert again
		manager.check(
			createMockSample({ charge_pct: 80, is_charging: true }),
			notifyFn,
		);
		expect(notifications.length).toBe(1);

		// Anomaly streak was reset, so 1 more sample does not fire anomaly alert
		notifications.length = 0;
		manager.check(
			createMockSample({
				cpu_temp_c: 84,
				cpu_pct: 5,
				power_w: 3.0,
				is_charging: false,
			}),
			notifyFn,
		);
		expect(notifications.length).toBe(0);
	});
});
