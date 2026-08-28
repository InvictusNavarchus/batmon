import { describe, expect, test } from "bun:test";
import {
	CAP_WARN,
	CHARGE_CRIT_WARN,
	CHARGE_HIGH_WARN,
	CHARGE_LOW_WARN,
	CPU_HOT_CHARGING,
	DB_DIR,
	DB_PATH,
	DEBUG_DB_PATH,
	DEBUG_RETENTION_HOURS,
	DEBUG_SAMPLE_INTERVAL_MS,
	HISTORICAL_SAMPLE_INTERVAL_TICKS,
	PRUNE_INTERVAL_TICKS,
	SYSFS,
	TEMP_CRIT,
	TEMP_WARN,
} from "../../src/config";

describe("config thresholds and invariants", () => {
	test("battery charge thresholds are ordered logically", () => {
		expect(CHARGE_CRIT_WARN).toBeGreaterThan(0);
		expect(CHARGE_LOW_WARN).toBeGreaterThan(CHARGE_CRIT_WARN);
		expect(CHARGE_HIGH_WARN).toBeGreaterThan(CHARGE_LOW_WARN);
		expect(CHARGE_HIGH_WARN).toBeLessThanOrEqual(100);
	});

	test("temperature thresholds are ordered logically", () => {
		expect(TEMP_WARN).toBeGreaterThan(0);
		expect(TEMP_CRIT).toBeGreaterThan(TEMP_WARN);
		expect(CPU_HOT_CHARGING).toBeGreaterThan(TEMP_CRIT);
	});

	test("capacity health warning threshold is a valid percentage", () => {
		expect(CAP_WARN).toBeGreaterThan(0);
		expect(CAP_WARN).toBeLessThanOrEqual(100);
	});

	test("timing intervals and retention are positive integers", () => {
		expect(DEBUG_SAMPLE_INTERVAL_MS).toBeGreaterThan(0);
		expect(HISTORICAL_SAMPLE_INTERVAL_TICKS).toBeGreaterThan(0);
		expect(PRUNE_INTERVAL_TICKS).toBeGreaterThan(0);
		expect(DEBUG_RETENTION_HOURS).toBeGreaterThan(0);
	});

	test("file paths are formatted correctly", () => {
		expect(SYSFS).toMatch(/^\/sys\//);
		expect(DB_PATH).toBe(`${DB_DIR}/battery.db`);
		expect(DEBUG_DB_PATH).toBe(`${DB_DIR}/debug.db`);
	});
});
