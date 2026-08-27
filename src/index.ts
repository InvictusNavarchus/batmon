#!/usr/bin/env bun
/**
 * batmon – battery health logger & flight recorder → SQLite
 *
 * Dual-tier monitoring:
 * - 1-second flight recorder samples → debug.db (auto-pruned rolling window)
 * - 60-second historical downsampled telemetry → battery.db (permanent)
 * - Collects sysfs, UPower D-Bus, sensors -j, and Glances REST API metrics.
 */

import { alert } from "./alerts";
import {
	DEBUG_RETENTION_HOURS,
	DEBUG_SAMPLE_INTERVAL_MS,
	HISTORICAL_SAMPLE_INTERVAL_TICKS,
	PRUNE_INTERVAL_TICKS,
} from "./config";
import { closeDbs, pruneDebug, store, storeDebug } from "./db";
import { readBattery } from "./telemetry";
import type { BatterySample } from "./types";

let prevHistorical: BatterySample | null = null;
let tickCount = 0;
let isRunning = true;
let isTicking = false;

async function runTick(): Promise<void> {
	try {
		const sample = await readBattery();
		if (!sample.is_present) return;

		// 1. Flight recorder: store every 1s sample to debug.db
		await storeDebug(sample);

		// 2. Historical: store every 60s sample to battery.db and trigger alerts
		if (tickCount % HISTORICAL_SAMPLE_INTERVAL_TICKS === 0) {
			prevHistorical = await store(sample);
			alert(sample, prevHistorical);
		}

		// 3. Batch prune debug.db every 5 minutes (300 ticks)
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
	const sample = await readBattery();
	if (!sample.is_present) process.exit(0);
	const prev = await store(sample);
	await storeDebug(sample);
	alert(sample, prev);
	await closeDbs();
}

// ── Signal Handling ──────────────────────────────────────────────────
async function shutdown(): Promise<void> {
	isRunning = false;
	await closeDbs();
	process.exit(0);
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);

// ── Main Entrypoint ──────────────────────────────────────────────────
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
