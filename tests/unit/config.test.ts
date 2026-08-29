import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
	discoverBatteryPath,
	HISTORICAL_SAMPLE_INTERVAL_TICKS,
	POWER_SUPPLY_BASE,
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
		expect(POWER_SUPPLY_BASE).toBe("/sys/class/power_supply");
		expect(DB_PATH).toBe(`${DB_DIR}/battery.db`);
		expect(DEBUG_DB_PATH).toBe(`${DB_DIR}/debug.db`);
	});
});

describe("discoverBatteryPath auto-discovery", () => {
	let tempBaseDir: string | null = null;

	afterEach(() => {
		if (tempBaseDir) {
			rmSync(tempBaseDir, { recursive: true, force: true });
			tempBaseDir = null;
		}
	});

	test("discovers standard BAT0 when AC adapter and BAT0 are present", () => {
		tempBaseDir = mkdtempSync(join(tmpdir(), "batmon-ps-test-"));

		const adp = join(tempBaseDir, "ADP1");
		mkdirSync(adp);
		writeFileSync(join(adp, "type"), "Mains\n");

		const bat0 = join(tempBaseDir, "BAT0");
		mkdirSync(bat0);
		writeFileSync(join(bat0, "type"), "Battery\n");

		const discovered = discoverBatteryPath(tempBaseDir);
		expect(discovered).toBe(bat0);
	});

	test("discovers BAT1 when only BAT1 is present", () => {
		tempBaseDir = mkdtempSync(join(tmpdir(), "batmon-ps-test-"));

		const ac = join(tempBaseDir, "AC");
		mkdirSync(ac);
		writeFileSync(join(ac, "type"), "Mains\n");

		const bat1 = join(tempBaseDir, "BAT1");
		mkdirSync(bat1);
		writeFileSync(join(bat1, "type"), "Battery\n");

		const discovered = discoverBatteryPath(tempBaseDir);
		expect(discovered).toBe(bat1);
	});

	test("prioritizes system battery over peripheral devices (scope: Device)", () => {
		tempBaseDir = mkdtempSync(join(tmpdir(), "batmon-ps-test-"));

		// Mouse battery appearing first alphabetically
		const mouse = join(tempBaseDir, "hid-0005:004c:0269-battery");
		mkdirSync(mouse);
		writeFileSync(join(mouse, "type"), "Battery\n");
		writeFileSync(join(mouse, "scope"), "Device\n");

		// Laptop system battery
		const bat0 = join(tempBaseDir, "BAT0");
		mkdirSync(bat0);
		writeFileSync(join(bat0, "type"), "Battery\n");
		writeFileSync(join(bat0, "scope"), "System\n");

		const discovered = discoverBatteryPath(tempBaseDir);
		expect(discovered).toBe(bat0);
	});

	test("discovers non-BAT named system battery like macsmc-battery", () => {
		tempBaseDir = mkdtempSync(join(tmpdir(), "batmon-ps-test-"));

		const macBat = join(tempBaseDir, "macsmc-battery");
		mkdirSync(macBat);
		writeFileSync(join(macBat, "type"), "Battery\n");
		writeFileSync(join(macBat, "scope"), "System\n");

		const discovered = discoverBatteryPath(tempBaseDir);
		expect(discovered).toBe(macBat);
	});

	test("picks BAT0 when multiple system batteries exist (sorted preference)", () => {
		tempBaseDir = mkdtempSync(join(tmpdir(), "batmon-ps-test-"));

		const bat0 = join(tempBaseDir, "BAT0");
		mkdirSync(bat0);
		writeFileSync(join(bat0, "type"), "Battery\n");

		const bat1 = join(tempBaseDir, "BAT1");
		mkdirSync(bat1);
		writeFileSync(join(bat1, "type"), "Battery\n");

		const discovered = discoverBatteryPath(tempBaseDir);
		expect(discovered).toBe(bat0);
	});

	test("falls back to BAT0 when base directory does not exist", () => {
		const nonExistent = join(tmpdir(), `non-existent-ps-${Date.now()}`);
		const discovered = discoverBatteryPath(nonExistent);
		expect(discovered).toBe(join(nonExistent, "BAT0"));
	});

	test("falls back to BAT0 when power supply directory contains no batteries", () => {
		tempBaseDir = mkdtempSync(join(tmpdir(), "batmon-ps-test-"));

		const adp = join(tempBaseDir, "ADP1");
		mkdirSync(adp);
		writeFileSync(join(adp, "type"), "Mains\n");

		const usb = join(tempBaseDir, "ucsi-source-psy-USBC000:001");
		mkdirSync(usb);
		writeFileSync(join(usb, "type"), "USB\n");

		const discovered = discoverBatteryPath(tempBaseDir);
		expect(discovered).toBe(join(tempBaseDir, "BAT0"));
	});

	test("ignores unreadable or corrupt entries and continues scanning", () => {
		tempBaseDir = mkdtempSync(join(tmpdir(), "batmon-ps-test-"));

		// Directory without type file
		const emptyDev = join(tempBaseDir, "empty_dev");
		mkdirSync(emptyDev);

		// Valid battery
		const bat0 = join(tempBaseDir, "BAT0");
		mkdirSync(bat0);
		writeFileSync(join(bat0, "type"), "Battery\n");

		const discovered = discoverBatteryPath(tempBaseDir);
		expect(discovered).toBe(bat0);
	});
});
