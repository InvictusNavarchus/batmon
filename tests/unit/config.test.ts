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
	test("battery charge thresholds match specification and are ordered logically", () => {
		expect(CHARGE_CRIT_WARN).toBe(10);
		expect(CHARGE_LOW_WARN).toBe(20);
		expect(CHARGE_HIGH_WARN).toBe(80);
		expect(CHARGE_CRIT_WARN).toBeGreaterThan(0);
		expect(CHARGE_LOW_WARN).toBeGreaterThan(CHARGE_CRIT_WARN);
		expect(CHARGE_HIGH_WARN).toBeGreaterThan(CHARGE_LOW_WARN);
		expect(CHARGE_HIGH_WARN).toBeLessThanOrEqual(100);
	});

	test("temperature thresholds match specification and are ordered logically", () => {
		expect(TEMP_WARN).toBe(45);
		expect(TEMP_CRIT).toBe(50);
		expect(CPU_HOT_CHARGING).toBe(85);
		expect(TEMP_WARN).toBeGreaterThan(0);
		expect(TEMP_CRIT).toBeGreaterThan(TEMP_WARN);
		expect(CPU_HOT_CHARGING).toBeGreaterThan(TEMP_CRIT);
	});

	test("capacity health warning threshold is a valid percentage", () => {
		expect(CAP_WARN).toBe(80);
		expect(CAP_WARN).toBeGreaterThan(0);
		expect(CAP_WARN).toBeLessThanOrEqual(100);
	});

	test("timing intervals and retention are positive integers", () => {
		expect(DEBUG_SAMPLE_INTERVAL_MS).toBeGreaterThan(0);
		expect(Number.isInteger(DEBUG_SAMPLE_INTERVAL_MS)).toBe(true);

		expect(HISTORICAL_SAMPLE_INTERVAL_TICKS).toBeGreaterThan(0);
		expect(Number.isInteger(HISTORICAL_SAMPLE_INTERVAL_TICKS)).toBe(true);

		expect(PRUNE_INTERVAL_TICKS).toBeGreaterThan(0);
		expect(Number.isInteger(PRUNE_INTERVAL_TICKS)).toBe(true);

		expect(DEBUG_RETENTION_HOURS).toBeGreaterThan(0);
		expect(Number.isInteger(DEBUG_RETENTION_HOURS)).toBe(true);
	});

	test("file paths are formatted correctly", () => {
		expect(SYSFS).toMatch(/^\/sys\//);
		expect(DB_PATH).toBe(`${DB_DIR}/battery.db`);
		expect(DEBUG_DB_PATH).toBe(`${DB_DIR}/debug.db`);
	});
});
