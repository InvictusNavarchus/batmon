import {
	CAP_HYSTERESIS_PCT,
	CAP_WARN,
	CHARGE_CRIT_WARN,
	CHARGE_HIGH_WARN,
	CHARGE_HYSTERESIS_PCT,
	CHARGE_LOW_WARN,
	CPU_HEAT_DEBOUNCE_SAMPLES,
	CPU_HOT_CHARGING,
	CPU_TEMP_HYSTERESIS_C,
	TEMP_CRIT,
	TEMP_HYSTERESIS_C,
	TEMP_WARN,
	VOLTAGE_CLEAR_RATIO,
	VOLTAGE_OVER_RATIO,
} from "./config";
import type { BatterySample, NotificationOptions } from "./types";

// ── notifications ───────────────────────────────────────────────────
export function notify({
	title,
	body,
	urgency = "normal",
	icon = "battery",
}: NotificationOptions): void {
	Bun.spawn(
		[
			"notify-send",
			"-a",
			"batmon",
			"-c",
			"device",
			"-u",
			urgency,
			"-i",
			icon,
			title,
			body,
		],
		{
			stdout: "ignore",
			stderr: "ignore",
		},
	).exited.catch(() => {});
	console.error(`[batmon] [${urgency.toUpperCase()}] ${title}: ${body}`);
}

// ── alert manager ───────────────────────────────────────────────────
export class AlertManager {
	private highChargeFired = false;
	private lowChargeFired = false;
	private critChargeFired = false;
	private tempWarnFired = false;
	private tempCritFired = false;
	private healthWarnFired = false;
	private overvoltageFired = false;
	private cpuHotFired = false;
	private cpuHotSamples = 0;

	public reset(): void {
		this.highChargeFired = false;
		this.lowChargeFired = false;
		this.critChargeFired = false;
		this.tempWarnFired = false;
		this.tempCritFired = false;
		this.healthWarnFired = false;
		this.overvoltageFired = false;
		this.cpuHotFired = false;
		this.cpuHotSamples = 0;
	}

	public check(
		curr: BatterySample,
		notifyFn: (opts: NotificationOptions) => void = notify,
	): void {
		this.checkCharge(curr, notifyFn);
		this.checkBatteryTemp(curr, notifyFn);
		this.checkHealth(curr, notifyFn);
		this.checkVoltage(curr, notifyFn);
		this.checkCpuHeat(curr, notifyFn);
	}

	private checkCharge(
		curr: BatterySample,
		notifyFn: (opts: NotificationOptions) => void,
	): void {
		// Re-arm high charge alert strictly when battery level discharges below hysteresis band (< 75%)
		if (curr.charge_pct < CHARGE_HIGH_WARN - CHARGE_HYSTERESIS_PCT) {
			this.highChargeFired = false;
		}

		if (curr.is_charging) {
			// Reset discharging low/critical alert latches when connected to charger
			this.lowChargeFired = false;
			this.critChargeFired = false;

			if (curr.charge_pct >= CHARGE_HIGH_WARN) {
				if (!this.highChargeFired) {
					this.highChargeFired = true;
					notifyFn({
						title: "Battery Charge Target Reached",
						body: `Level reached ${curr.charge_pct}% – unplug charger to preserve health`,
						urgency: "normal",
						icon: "battery-full-charging",
					});
				}
			}
		} else {
			if (curr.charge_pct <= CHARGE_CRIT_WARN) {
				if (!this.critChargeFired) {
					this.critChargeFired = true;
					this.lowChargeFired = true; // Critical suppresses low alert
					notifyFn({
						title: "CRITICAL: Battery Low",
						body: `${curr.charge_pct}% remaining – connect charger immediately`,
						urgency: "critical",
						icon: "battery-empty",
					});
				}
			} else if (curr.charge_pct <= CHARGE_LOW_WARN) {
				// Re-arm critical if battery level recovered above critical hysteresis
				if (curr.charge_pct > CHARGE_CRIT_WARN + CHARGE_HYSTERESIS_PCT) {
					this.critChargeFired = false;
				}

				if (!this.lowChargeFired && !this.critChargeFired) {
					this.lowChargeFired = true;
					notifyFn({
						title: "Low Battery",
						body: `${curr.charge_pct}% remaining – plug in charger`,
						urgency: "normal",
						icon: "battery-caution",
					});
				}
			} else {
				// Discharging and above low threshold + hysteresis -> re-arm all
				if (curr.charge_pct > CHARGE_LOW_WARN + CHARGE_HYSTERESIS_PCT) {
					this.lowChargeFired = false;
				}
				if (curr.charge_pct > CHARGE_CRIT_WARN + CHARGE_HYSTERESIS_PCT) {
					this.critChargeFired = false;
				}
			}
		}
	}

	private checkBatteryTemp(
		curr: BatterySample,
		notifyFn: (opts: NotificationOptions) => void,
	): void {
		// Ignore null sensor reads without resetting latches (avoids glitch false triggers)
		if (curr.battery_temp_c === null) return;

		if (curr.battery_temp_c >= TEMP_CRIT) {
			if (!this.tempCritFired) {
				this.tempCritFired = true;
				this.tempWarnFired = true; // Critical suppresses warning alert
				const advice = curr.is_charging
					? "unplug charger immediately"
					: "reduce system load immediately";
				notifyFn({
					title: "CRITICAL: Battery Overheating",
					body: `Battery at ${curr.battery_temp_c.toFixed(1)} °C – ${advice}`,
					urgency: "critical",
					icon: "dialog-warning",
				});
			}
		} else if (curr.battery_temp_c >= TEMP_WARN) {
			// Re-arm critical if temp dropped below critical hysteresis band
			if (curr.battery_temp_c < TEMP_CRIT - TEMP_HYSTERESIS_C) {
				this.tempCritFired = false;
			}

			if (!this.tempWarnFired && !this.tempCritFired) {
				this.tempWarnFired = true;
				notifyFn({
					title: "Warning: High Battery Temperature",
					body: `Battery at ${curr.battery_temp_c.toFixed(1)} °C`,
					urgency: "normal",
					icon: "dialog-warning",
				});
			}
		} else {
			if (curr.battery_temp_c < TEMP_WARN - TEMP_HYSTERESIS_C) {
				this.tempWarnFired = false;
			}
			if (curr.battery_temp_c < TEMP_CRIT - TEMP_HYSTERESIS_C) {
				this.tempCritFired = false;
			}
		}
	}

	private checkHealth(
		curr: BatterySample,
		notifyFn: (opts: NotificationOptions) => void,
	): void {
		if (curr.health_pct < CAP_WARN) {
			if (!this.healthWarnFired) {
				this.healthWarnFired = true;
				notifyFn({
					title: "Battery Health Notice",
					body: `Battery health at ${curr.health_pct.toFixed(1)}% of design capacity`,
					urgency: "normal",
					icon: "battery-caution",
				});
			}
		} else if (curr.health_pct >= CAP_WARN + CAP_HYSTERESIS_PCT) {
			this.healthWarnFired = false;
		}
	}

	private checkVoltage(
		curr: BatterySample,
		notifyFn: (opts: NotificationOptions) => void,
	): void {
		if (curr.is_charging && curr.voltage_design_v > 0 && curr.voltage_v > 0) {
			if (curr.voltage_v > curr.voltage_design_v * VOLTAGE_OVER_RATIO) {
				if (!this.overvoltageFired) {
					this.overvoltageFired = true;
					notifyFn({
						title: "Warning: Over-Voltage Charging",
						body: `Voltage ${curr.voltage_v.toFixed(2)} V well above design ${curr.voltage_design_v} V`,
						urgency: "normal",
						icon: "dialog-warning",
					});
				}
			} else if (
				curr.voltage_v <=
				curr.voltage_design_v * VOLTAGE_CLEAR_RATIO
			) {
				this.overvoltageFired = false;
			}
		} else if (!curr.is_charging) {
			this.overvoltageFired = false;
		}
	}

	private checkCpuHeat(
		curr: BatterySample,
		notifyFn: (opts: NotificationOptions) => void,
	): void {
		if (curr.cpu_temp_c === null) {
			this.cpuHotSamples = 0;
			return;
		}

		if (curr.cpu_temp_c >= CPU_HOT_CHARGING) {
			this.cpuHotSamples++;
			if (
				!this.cpuHotFired &&
				this.cpuHotSamples >= CPU_HEAT_DEBOUNCE_SAMPLES
			) {
				this.cpuHotFired = true;
				const title = curr.is_charging
					? "Warning: Heat-Soak Risk"
					: "Warning: High CPU Temperature";
				const body = curr.is_charging
					? `Charging while CPU at ${curr.cpu_temp_c.toFixed(0)} °C`
					: `CPU at ${curr.cpu_temp_c.toFixed(0)} °C`;
				notifyFn({
					title,
					body,
					urgency: "normal",
					icon: "dialog-warning",
				});
			}
		} else if (curr.cpu_temp_c < CPU_HOT_CHARGING - CPU_TEMP_HYSTERESIS_C) {
			this.cpuHotSamples = 0;
			this.cpuHotFired = false;
		} else {
			// In deadband between (CPU_HOT_CHARGING - CPU_TEMP_HYSTERESIS_C) and CPU_HOT_CHARGING:
			// If not yet tripped, reset consecutive streak so non-sustained spikes do not accumulate.
			if (!this.cpuHotFired) {
				this.cpuHotSamples = 0;
			}
		}
	}
}
