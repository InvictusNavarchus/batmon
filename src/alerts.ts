import {
	CAP_WARN,
	CHARGE_CRIT_WARN,
	CHARGE_HIGH_WARN,
	CHARGE_LOW_WARN,
	CPU_HOT_CHARGING,
	TEMP_CRIT,
	TEMP_WARN,
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

// ── alerts ───────────────────────────────────────────────────────────
export function alert(
	curr: BatterySample,
	prev: BatterySample | null,
	notifyFn: (opts: NotificationOptions) => void = notify,
): void {
	const prevCharging = prev !== null ? Boolean(prev.is_charging) : null;
	const prevPct = prev !== null ? prev.charge_pct : null;

	if (curr.is_charging && curr.charge_pct >= CHARGE_HIGH_WARN) {
		const justCrossed =
			prevPct === null || prevCharging === false || prevPct < CHARGE_HIGH_WARN;
		if (justCrossed) {
			notifyFn({
				title: "Battery Charge Target Reached",
				body: `Level reached ${curr.charge_pct}% – unplug charger to preserve health`,
				urgency: "normal",
				icon: "battery-full-charging",
			});
		}
	}

	if (!curr.is_charging && curr.charge_pct <= CHARGE_LOW_WARN) {
		const justCrossed =
			prevPct === null || prevCharging === true || prevPct > CHARGE_LOW_WARN;
		if (justCrossed) {
			notifyFn({
				title: "Low Battery",
				body: `${curr.charge_pct}% remaining – plug in charger`,
				urgency: "normal",
				icon: "battery-caution",
			});
		}
	}

	if (!curr.is_charging && curr.charge_pct <= CHARGE_CRIT_WARN) {
		const justCrossed =
			prevPct === null || prevCharging === true || prevPct > CHARGE_CRIT_WARN;
		if (justCrossed) {
			notifyFn({
				title: "CRITICAL: Battery Low",
				body: `${curr.charge_pct}% remaining – connect charger immediately`,
				urgency: "critical",
				icon: "battery-empty",
			});
		}
	}

	if (curr.battery_temp_c !== null) {
		if (curr.battery_temp_c >= TEMP_CRIT) {
			notifyFn({
				title: "CRITICAL: Battery Overheating",
				body: `Battery at ${curr.battery_temp_c.toFixed(1)} °C – unplug immediately`,
				urgency: "critical",
				icon: "dialog-warning",
			});
		} else if (curr.battery_temp_c >= TEMP_WARN) {
			notifyFn({
				title: "Warning: High Battery Temperature",
				body: `Battery at ${curr.battery_temp_c.toFixed(1)} °C`,
				urgency: "normal",
				icon: "dialog-warning",
			});
		}
	}

	if (curr.health_pct < CAP_WARN) {
		notifyFn({
			title: "Battery Health Notice",
			body: `Battery health at ${curr.health_pct.toFixed(1)}% of design capacity`,
			urgency: "normal",
			icon: "battery-caution",
		});
	}

	if (curr.is_charging && curr.voltage_v > curr.voltage_design_v * 1.15) {
		notifyFn({
			title: "Warning: Over-Voltage Charging",
			body: `Voltage ${curr.voltage_v.toFixed(2)} V well above design ${curr.voltage_design_v} V`,
			urgency: "normal",
			icon: "dialog-warning",
		});
	}

	if (
		curr.is_charging &&
		curr.cpu_temp_c !== null &&
		curr.cpu_temp_c >= CPU_HOT_CHARGING
	) {
		notifyFn({
			title: "Warning: Heat-Soak Risk",
			body: `Charging while CPU at ${curr.cpu_temp_c.toFixed(0)} °C`,
			urgency: "normal",
			icon: "dialog-warning",
		});
	}
}
