#!/usr/bin/env bun
/**
 * batmon – battery health logger → SQLite
 *
 * Reads sysfs (auto-detects energy_* vs charge_* batteries),
 * shells out to UPower D-Bus for smoothed time estimates,
 * reads system temps via `sensors -j` as thermal proxy,
 * and stores everything in a local SQLite database.
 *
 * Designed to run as a systemd user timer (every 60 s).
 */

import { alert } from "./alerts";
import { store } from "./db";
import { readBattery } from "./telemetry";

// ── main ─────────────────────────────────────────────────────────────
try {
	const sample = readBattery();
	if (!sample.is_present) process.exit(0);
	const prev = await store(sample);
	alert(sample, prev);
} catch (err) {
	console.error("batmon:", err);
	process.exit(1);
}
