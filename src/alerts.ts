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
export function alert(curr: BatterySample, prev: BatterySample | null): void {
	const prevCharging = prev !== null ? Boolean(prev.is_charging) : null;
	const prevPct = prev !== null ? prev.percentage : null;

	if (curr.is_charging && curr.percentage >= CHARGE_HIGH_WARN) {
		const justCrossed =
			prevPct === null || prevCharging === false || prevPct < CHARGE_HIGH_WARN;
		if (justCrossed) {
			notify({
				title: "Battery Charge Target Reached",
				body: `Level reached ${curr.percentage}% – unplug charger to preserve health`,
				urgency: "normal",
				icon: "battery-full-charging",
			});
		}
	}

	if (!curr.is_charging && curr.percentage <= CHARGE_LOW_WARN) {
		const justCrossed =
			prevPct === null || prevCharging === true || prevPct > CHARGE_LOW_WARN;
		if (justCrossed) {
			notify({
				title: "Low Battery",
				body: `${curr.percentage}% remaining – plug in charger`,
				urgency: "normal",
				icon: "battery-caution",
			});
		}
	}

	if (!curr.is_charging && curr.percentage <= CHARGE_CRIT_WARN) {
		const justCrossed =
			prevPct === null || prevCharging === true || prevPct > CHARGE_CRIT_WARN;
		if (justCrossed) {
			notify({
				title: "CRITICAL: Battery Low",
				body: `${curr.percentage}% remaining – connect charger immediately`,
				urgency: "critical",
				icon: "battery-empty",
			});
		}
	}

	if (curr.temperature_c !== null) {
		if (curr.temperature_c >= TEMP_CRIT) {
			notify({
				title: "CRITICAL: Battery Overheating",
				body: `Battery at ${curr.temperature_c.toFixed(1)} °C – unplug immediately`,
				urgency: "critical",
				icon: "dialog-warning",
			});
		} else if (curr.temperature_c >= TEMP_WARN) {
			notify({
				title: "Warning: High Battery Temperature",
				body: `Battery at ${curr.temperature_c.toFixed(1)} °C`,
				urgency: "normal",
				icon: "dialog-warning",
			});
		}
	}

	if (curr.capacity_pct < CAP_WARN) {
		notify({
			title: "Battery Health Notice",
			body: `Battery health at ${curr.capacity_pct.toFixed(1)}% of design capacity`,
			urgency: "normal",
			icon: "battery-caution",
		});
	}

	if (curr.is_charging && curr.voltage_v > curr.voltage_design * 1.15) {
		notify({
			title: "Warning: Over-Voltage Charging",
			body: `Voltage ${curr.voltage_v.toFixed(2)} V well above design ${curr.voltage_design} V`,
			urgency: "normal",
			icon: "dialog-warning",
		});
	}

	if (
		curr.is_charging &&
		curr.cpu_temp_c !== null &&
		curr.cpu_temp_c > CPU_HOT_CHARGING
	) {
		notify({
			title: "Warning: Heat-Soak Risk",
			body: `Charging while CPU at ${curr.cpu_temp_c.toFixed(0)} °C`,
			urgency: "normal",
			icon: "dialog-warning",
		});
	}
}
