export interface BatterySample {
	ts: string;
	percentage: number;
	status: string;
	energy_wh: number;
	energy_full_wh: number;
	energy_design: number;
	power_w: number;
	voltage_v: number;
	voltage_design: number;
	cycle_count: number;
	temperature_c: number | null;
	capacity_pct: number;
	is_charging: boolean;
	is_present: boolean;
	time_to_empty_s: number | null;
	time_to_full_s: number | null;
	cpu_temp_c: number | null;
	gpu_temp_c: number | null;
	nvme_temp_c: number | null;
}

export interface SystemTemps {
	cpu_c: number | null;
	gpu_c: number | null;
	nvme_c: number | null;
}

/** Shape of `sensors -j` output */
export interface SensorsData {
	[adapter: string]: {
		[feature: string]: {
			[subfeature: string]: number;
		};
	};
}

export interface NotificationOptions {
	title: string;
	body: string;
	urgency?: "low" | "normal" | "critical";
	icon?: string;
}
