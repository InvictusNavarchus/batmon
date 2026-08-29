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
let prevHistorical: TelemetrySample | null = null;
let prevSample: TelemetrySample | null = null;
let tickCount = 0;
let isRunning = true;
let isTicking = false;

async function runTick(): Promise<void> {
	try {
		const sample = await readTelemetry();
		if (!sample.is_present) {
			alertManager.reset();
			prevSample = null;
			return;
		}

		if (prevSample === null) {
			prevSample = await getLatestSample();
		}

		if (prevHistorical === null) {
			prevHistorical = await getLatestHistoricalSample();
		}

		sample.estimated_cycle_count = computeEstimatedCycles(
			sample,
			prevHistorical,
		);

		// 1. Flight recorder: store every 1s sample to debug.db
		await storeDebug(sample);

		// 2. Alert Check: stateful evaluation with hysteresis
		alertManager.check(sample);
		prevSample = sample;

		// 3. Historical: store downsampled sample to battery.db every 60s
		if (tickCount % HISTORICAL_SAMPLE_INTERVAL_TICKS === 0) {
			await store(sample);
			prevHistorical = sample;
		}

		// 4. Batch prune debug.db every 5 minutes (300 ticks)
		if (tickCount > 0 && tickCount % PRUNE_INTERVAL_TICKS === 0) {
			await pruneDebug(DEBUG_RETENTION_HOURS);
		}

		tickCount++;
	} catch (err) {
		console.error("batmon tick error:", err);
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

async function runOneshot(): Promise<void> {
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
	prevSample = null;
	prevHistorical = null;
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
