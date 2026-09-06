#!/usr/bin/env bun
/**
 * batmon – battery health logger & flight recorder → SQLite
 *
 * Dual-tier monitoring:
 * - 1-second flight recorder samples → debug.db (auto-pruned rolling window)
 * - 60-second historical downsampled telemetry → battery.db (permanent)
 * - Collects sysfs, procfs, UPower D-Bus, sensors -j, and per-PID delta metrics.
 */

import { AlertManager } from "./alerts";
import {
	DEBUG_RETENTION_HOURS,
	DEBUG_SAMPLE_INTERVAL_MS,
	HISTORICAL_SAMPLE_INTERVAL_TICKS,
	PRUNE_INTERVAL_TICKS,
} from "./config";
import {
	closeDbs,
	computeEstimatedCycles,
	getLatestHistoricalSample,
	getLatestSample,
	pruneDebug,
	store,
	storeDebug,
} from "./db";
import { readTelemetry } from "./telemetry";
import type { TelemetrySample } from "./types";

let alertManager = new AlertManager();
let prevDebugSample: TelemetrySample | null = null;
let tickCount = 0;
let isRunning = true;
let isTicking = false;

async function runTick(): Promise<void> {
	try {
		const sample = await readTelemetry();
		if (!sample.is_present) {
			alertManager.reset();
			return;
		}

		if (prevDebugSample === null) {
			prevDebugSample =
				(await getLatestSample()) ?? (await getLatestHistoricalSample());
		}

		sample.estimated_cycle_count = computeEstimatedCycles(
			sample,
			prevDebugSample,
		);
		prevDebugSample = sample;

		// 1. Flight recorder: store every 1s sample to debug.db
		await storeDebug(sample);

		// 2. Alert Check: stateful evaluation with hysteresis
		alertManager.check(sample);

		// 3. Historical: store downsampled sample to battery.db every 60s
		if (tickCount % HISTORICAL_SAMPLE_INTERVAL_TICKS === 0) {
			const histSample = { ...sample };
			await store(histSample);
		}

		// 4. Batch prune debug.db every 5 minutes (300 ticks)
		if (tickCount > 0 && tickCount % PRUNE_INTERVAL_TICKS === 0) {
			await pruneDebug(DEBUG_RETENTION_HOURS);
		}
	} catch (err) {
		console.error("batmon tick error:", err);
	} finally {
		tickCount++;
	}
}

async function executeTick(): Promise<void> {
	if (isTicking || !isRunning) return;
	isTicking = true;
	try {
		await runTick();
	} finally {
		isTicking = false;
	}
}

async function runOneshot(sampleIntervalMs = 500): Promise<void> {
	// Sample once to prime CPU and process delta baselines
	const warmup = await readTelemetry();
	if (!warmup.is_present) process.exit(0);

	if (sampleIntervalMs > 0) {
		await Bun.sleep(sampleIntervalMs);
	}

	// Second sample captures valid non-null CPU% and process CPU delta rankings
	const sample = await readTelemetry();
	if (!sample.is_present) process.exit(0);

	await store(sample);
	await storeDebug(sample);
	const oneshotAlerts = new AlertManager();
	oneshotAlerts.check(sample);
	await closeDbs();
}

// ── Signal Handling ──────────────────────────────────────────────────
export async function shutdown(): Promise<void> {
	isRunning = false;
	await closeDbs();
	process.exit(0);
}

export function resetDaemonStateForTesting(): void {
	alertManager = new AlertManager();
	prevDebugSample = null;
	tickCount = 0;
	isRunning = true;
	isTicking = false;
}

export { executeTick, runOneshot, runTick };

// ── Main Entrypoint ──────────────────────────────────────────────────
if (import.meta.main) {
	process.on("SIGINT", shutdown);
	process.on("SIGTERM", shutdown);

	if (process.argv.includes("--oneshot")) {
		try {
			await runOneshot();
		} catch (err) {
			console.error("batmon oneshot error:", err);
			process.exit(1);
		}
	} else {
		// Daemon mode: execute first tick immediately, then enter 1s interval loop
		await executeTick();
		const interval = setInterval(async () => {
			if (!isRunning) {
				clearInterval(interval);
				return;
			}
			await executeTick();
		}, DEBUG_SAMPLE_INTERVAL_MS);
	}
}
