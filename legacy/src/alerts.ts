import {
	CAP_HYSTERESIS_PCT,
	CAP_WARN,
	CHARGE_CRIT_WARN,
	CHARGE_HIGH_WARN,
	CHARGE_HYSTERESIS_PCT,
	CHARGE_LOW_WARN,
	CPU_ANOMALY_DEBOUNCE_SAMPLES,
	CPU_ANOMALY_HYSTERESIS_C,
	CPU_ANOMALY_MAX_LOAD_PCT,
	CPU_ANOMALY_MAX_POWER_W,
	CPU_ANOMALY_TEMP,
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
	private anomalyFired = false;
	private anomalySamples = 0;

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
		this.anomalyFired = false;
		this.anomalySamples = 0;
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
		this.checkThermalAnomaly(curr, notifyFn);
	}

	private checkCharge(
		curr: BatterySample,
		notifyFn: (opts: NotificationOptions) => void,
	): void {
		// Re-arm high charge alert strictly when battery level discharges below hysteresis band (< 75%)
		if (curr.charge_pct < CHARGE_HIGH_WARN - CHARGE_HYSTERESIS_PCT) {
			this.highChargeFired = false;
		}

		if (curr.is_charging || curr.power_state === "charging") {
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
		} else if (curr.power_state === "ac_idle") {
			// Connected to AC but idle/capped: reset low/critical alert latches without prompting to connect charger
			this.lowChargeFired = false;
			this.critChargeFired = false;
		} else if (curr.power_state === "discharging") {
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
		// Heat-soak warning protects battery health specifically while charging
		if (curr.cpu_temp_c === null || !curr.is_charging) {
			this.cpuHotSamples = 0;
			if (!curr.is_charging) {
				this.cpuHotFired = false;
			}
			return;
		}

		if (curr.cpu_temp_c >= CPU_HOT_CHARGING) {
			this.cpuHotSamples++;
			if (
				!this.cpuHotFired &&
				this.cpuHotSamples >= CPU_HEAT_DEBOUNCE_SAMPLES
			) {
				this.cpuHotFired = true;
				notifyFn({
					title: "Warning: Heat-Soak Risk",
					body: `Charging while CPU at ${curr.cpu_temp_c.toFixed(0)} °C – unplug charger to preserve health`,
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

	private checkThermalAnomaly(
		curr: BatterySample,
		notifyFn: (opts: NotificationOptions) => void,
	): void {
		// When charging, thermal management is handled by checkCpuHeat (heat-soak warning)
		if (curr.cpu_temp_c === null || curr.is_charging) {
			this.anomalySamples = 0;
			if (curr.is_charging) {
				this.anomalyFired = false;
			}
			return;
		}

		// Low workload indicator: low CPU% (<= 20%) or low discharge power (<= 12W)
		const isLowCpu =
			curr.cpu_pct !== null && curr.cpu_pct <= CPU_ANOMALY_MAX_LOAD_PCT;
		const isLowPower =
			curr.power_w > 0 && curr.power_w <= CPU_ANOMALY_MAX_POWER_W;
		const isLowLoad = isLowCpu || isLowPower;

		if (curr.cpu_temp_c >= CPU_ANOMALY_TEMP && isLowLoad) {
			this.anomalySamples++;
			if (
				!this.anomalyFired &&
				this.anomalySamples >= CPU_ANOMALY_DEBOUNCE_SAMPLES
			) {
				this.anomalyFired = true;
				const loadDetail = isLowCpu
					? `${(curr.cpu_pct as number).toFixed(0)}% CPU`
					: `${curr.power_w.toFixed(1)} W`;
				notifyFn({
					title: "CRITICAL: Thermal Anomaly",
					body: `CPU at ${curr.cpu_temp_c.toFixed(0)} °C during low workload (${loadDetail}) – check cooling fans & ventilation`,
					urgency: "critical",
					icon: "dialog-error",
				});
			}
		} else if (curr.cpu_temp_c < CPU_ANOMALY_TEMP - CPU_ANOMALY_HYSTERESIS_C) {
			this.anomalySamples = 0;
			this.anomalyFired = false;
		} else if (!isLowLoad && !this.anomalyFired) {
			// Active workload (e.g. gaming / compilation) -> reset debounce streak
			this.anomalySamples = 0;
		} else if (!this.anomalyFired) {
			this.anomalySamples = 0;
		}
	}
}
