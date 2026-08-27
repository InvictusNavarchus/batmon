import type { GlancesTelemetry, TopProcessGroup } from "./types";

export const GLANCES_BASE_URL = "http://127.0.0.1:61208/api/4";

interface GlancesQuicklook {
	cpu?: number;
	mem?: number;
	cpu_hz_current?: number;
	gpu_proc?: number;
	load?: number;
}

interface GlancesProgram {
	name?: string;
	cpu_percent?: number;
	memory_percent?: number;
	nprocs?: number;
}

/**
 * Reads CPU, memory, clock speed, GPU %, load, and top 5 process groups from Glances REST API v4.
 * Gracefully falls back to null values if Glances is not running or unreachable.
 */
export async function readGlances(
	baseUrl = GLANCES_BASE_URL,
	timeoutMs = 150,
): Promise<GlancesTelemetry> {
	try {
		const signal = AbortSignal.timeout(timeoutMs);

		const [quicklookRes, programListRes] = await Promise.all([
			fetch(`${baseUrl}/quicklook`, { signal })
				.then((r) => (r.ok ? (r.json() as Promise<GlancesQuicklook>) : null))
				.catch(() => null),
			fetch(`${baseUrl}/programlist`, { signal })
				.then((r) => (r.ok ? (r.json() as Promise<GlancesProgram[]>) : null))
				.catch(() => null),
		]);

		if (!quicklookRes && !programListRes) {
			return {
				cpu_pct: null,
				mem_pct: null,
				top_processes: null,
				cpu_freq_mhz: null,
				gpu_pct: null,
				load1: null,
			};
		}

		const topProcesses: TopProcessGroup[] | null = Array.isArray(programListRes)
			? [...programListRes]
					.sort(
						(a, b) =>
							(b.cpu_percent ?? 0) - (a.cpu_percent ?? 0) ||
							(b.memory_percent ?? 0) - (a.memory_percent ?? 0),
					)
					.slice(0, 5)
					.map((p) => ({
						name: p.name ?? "unknown",
						cpu: Math.round((p.cpu_percent ?? 0) * 10) / 10,
						mem: Math.round((p.memory_percent ?? 0) * 10) / 10,
						count: p.nprocs ?? 1,
					}))
			: null;

		return {
			cpu_pct:
				typeof quicklookRes?.cpu === "number"
					? Math.round(quicklookRes.cpu * 10) / 10
					: null,
			mem_pct:
				typeof quicklookRes?.mem === "number"
					? Math.round(quicklookRes.mem * 10) / 10
					: null,
			top_processes:
				topProcesses && topProcesses.length > 0
					? JSON.stringify(topProcesses)
					: null,
			cpu_freq_mhz:
				typeof quicklookRes?.cpu_hz_current === "number" &&
				quicklookRes.cpu_hz_current > 0
					? Math.round(quicklookRes.cpu_hz_current / 1_000_000)
					: null,
			gpu_pct:
				typeof quicklookRes?.gpu_proc === "number"
					? Math.round(quicklookRes.gpu_proc * 10) / 10
					: null,
			load1:
				typeof quicklookRes?.load === "number"
					? Math.round(quicklookRes.load * 100) / 100
					: null,
		};
	} catch {
		return {
			cpu_pct: null,
			mem_pct: null,
			top_processes: null,
			cpu_freq_mhz: null,
			gpu_pct: null,
			load1: null,
		};
	}
}
