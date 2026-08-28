import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readSystemTemps, upowerProp } from "../../src/telemetry";

describe("telemetry parsers", () => {
	let spawnSyncSpy: ReturnType<typeof spyOn> | null = null;
	let tempHwmonDir: string | null = null;

	afterEach(() => {
		if (spawnSyncSpy) {
			spawnSyncSpy.mockRestore();
			spawnSyncSpy = null;
		}
		if (tempHwmonDir) {
			rmSync(tempHwmonDir, { recursive: true, force: true });
			tempHwmonDir = null;
		}
	});

	describe("readSystemTemps sysfs hwmon parser", () => {
		test("parses AMD k10temp, amdgpu, nvme, and GPU power correctly", () => {
			tempHwmonDir = mkdtempSync(join(tmpdir(), "batmon-hwmon-test-"));

			// hwmon0: AMD GPU
			const hwmon0 = join(tempHwmonDir, "hwmon0");
			mkdirSync(hwmon0);
			writeFileSync(join(hwmon0, "name"), "amdgpu\n");
			writeFileSync(join(hwmon0, "temp1_input"), "42000\n");
			writeFileSync(join(hwmon0, "power1_input"), "15345000\n");

			// hwmon1: NVMe
			const hwmon1 = join(tempHwmonDir, "hwmon1");
			mkdirSync(hwmon1);
			writeFileSync(join(hwmon1, "name"), "nvme\n");
			writeFileSync(join(hwmon1, "temp1_input"), "38000\n");

			// hwmon2: AMD CPU k10temp
			const hwmon2 = join(tempHwmonDir, "hwmon2");
			mkdirSync(hwmon2);
			writeFileSync(join(hwmon2, "name"), "k10temp\n");
			writeFileSync(join(hwmon2, "temp1_input"), "48500\n");

			const result = readSystemTemps(tempHwmonDir);
			expect(result).toEqual({
				cpu_c: 48.5,
				gpu_c: 42.0,
				nvme_c: 38.0,
				gpu_power_w: 15.35,
			});
		});

		test("parses Intel coretemp and i915 graphics correctly", () => {
			tempHwmonDir = mkdtempSync(join(tmpdir(), "batmon-hwmon-test-"));

			// hwmon0: Intel coretemp with Package id label
			const hwmon0 = join(tempHwmonDir, "hwmon0");
			mkdirSync(hwmon0);
			writeFileSync(join(hwmon0, "name"), "coretemp\n");
			writeFileSync(join(hwmon0, "temp1_label"), "Package id 0\n");
			writeFileSync(join(hwmon0, "temp1_input"), "54000\n");

			// hwmon1: Intel i915
			const hwmon1 = join(tempHwmonDir, "hwmon1");
			mkdirSync(hwmon1);
			writeFileSync(join(hwmon1, "name"), "i915\n");
			writeFileSync(join(hwmon1, "temp1_input"), "47000\n");

			const result = readSystemTemps(tempHwmonDir);
			expect(result).toEqual({
				cpu_c: 54.0,
				gpu_c: 47.0,
				nvme_c: null,
				gpu_power_w: null,
			});
		});

		test("parses ARM soc_thermal and power1_average correctly", () => {
			tempHwmonDir = mkdtempSync(join(tmpdir(), "batmon-hwmon-test-"));

			const hwmon0 = join(tempHwmonDir, "hwmon0");
			mkdirSync(hwmon0);
			writeFileSync(join(hwmon0, "name"), "soc_thermal\n");
			writeFileSync(join(hwmon0, "temp1_input"), "39200\n");

			const hwmon1 = join(tempHwmonDir, "hwmon1");
			mkdirSync(hwmon1);
			writeFileSync(join(hwmon1, "name"), "amdgpu\n");
			writeFileSync(join(hwmon1, "temp1_input"), "41000\n");
			writeFileSync(join(hwmon1, "power1_average"), "12500000\n");

			const result = readSystemTemps(tempHwmonDir);
			expect(result).toEqual({
				cpu_c: 39.2,
				gpu_c: 41.0,
				nvme_c: null,
				gpu_power_w: 12.5,
			});
		});

		test("returns all nulls when hwmon directory does not exist", () => {
			const nonExistent = join(tmpdir(), `non-existent-hwmon-${Date.now()}`);
			expect(readSystemTemps(nonExistent)).toEqual({
				cpu_c: null,
				gpu_c: null,
				nvme_c: null,
				gpu_power_w: null,
			});
		});

		test("returns all nulls when hwmon directory contains unknown drivers", () => {
			tempHwmonDir = mkdtempSync(join(tmpdir(), "batmon-hwmon-test-"));
			const hwmon0 = join(tempHwmonDir, "hwmon0");
			mkdirSync(hwmon0);
			writeFileSync(join(hwmon0, "name"), "unknown_device\n");
			writeFileSync(join(hwmon0, "temp1_input"), "50000\n");

			expect(readSystemTemps(tempHwmonDir)).toEqual({
				cpu_c: null,
				gpu_c: null,
				nvme_c: null,
				gpu_power_w: null,
			});
		});

		test("handles malformed/non-numeric sysfs entries gracefully", () => {
			tempHwmonDir = mkdtempSync(join(tmpdir(), "batmon-hwmon-test-"));
			const hwmon0 = join(tempHwmonDir, "hwmon0");
			mkdirSync(hwmon0);
			writeFileSync(join(hwmon0, "name"), "k10temp\n");
			writeFileSync(join(hwmon0, "temp1_input"), "invalid_number\n");

			expect(readSystemTemps(tempHwmonDir)).toEqual({
				cpu_c: null,
				gpu_c: null,
				nvme_c: null,
				gpu_power_w: null,
			});
		});

		test("accepts 0W GPU power and 0°C or valid sub-zero temperatures down to -50°C", () => {
			tempHwmonDir = mkdtempSync(join(tmpdir(), "batmon-hwmon-test-"));

			// hwmon0: AMD GPU in D3cold (0 W) and 0°C
			const hwmon0 = join(tempHwmonDir, "hwmon0");
			mkdirSync(hwmon0);
			writeFileSync(join(hwmon0, "name"), "amdgpu\n");
			writeFileSync(join(hwmon0, "temp1_input"), "0\n");
			writeFileSync(join(hwmon0, "power1_input"), "0\n");

			// hwmon1: CPU at exact boundary of -50°C (-50000 m°C)
			const hwmon1 = join(tempHwmonDir, "hwmon1");
			mkdirSync(hwmon1);
			writeFileSync(join(hwmon1, "name"), "k10temp\n");
			writeFileSync(join(hwmon1, "temp1_input"), "-50000\n");

			const result = readSystemTemps(tempHwmonDir);
			expect(result).toEqual({
				cpu_c: -50.0,
				gpu_c: 0,
				nvme_c: null,
				gpu_power_w: 0,
			});
		});

		test("preserves GPU temperature and power pairing on multi-GPU systems", () => {
			tempHwmonDir = mkdtempSync(join(tmpdir(), "batmon-hwmon-test-"));

			// hwmon0: Intel i915 (iGPU) appearing first in directory scan
			const hwmon0 = join(tempHwmonDir, "hwmon0");
			mkdirSync(hwmon0);
			writeFileSync(join(hwmon0, "name"), "i915\n");
			writeFileSync(join(hwmon0, "temp1_input"), "45000\n");

			// hwmon1: AMD dGPU with temperature and power appearing second
			const hwmon1 = join(tempHwmonDir, "hwmon1");
			mkdirSync(hwmon1);
			writeFileSync(join(hwmon1, "name"), "amdgpu\n");
			writeFileSync(join(hwmon1, "temp1_input"), "58500\n");
			writeFileSync(join(hwmon1, "power1_input"), "25000000\n");

			const result = readSystemTemps(tempHwmonDir);
			// Must prioritize amdgpu temperature so temp (58.5) and power (25.0) come from the same device
			expect(result).toEqual({
				cpu_c: null,
				gpu_c: 58.5,
				nvme_c: null,
				gpu_power_w: 25.0,
			});
		});
	});

	describe("upowerProp parser", () => {
		test("parses busctl property output correctly", () => {
			spawnSyncSpy = spyOn(Bun, "spawnSync").mockReturnValue({
				stdout: Buffer.from("x 7200\n"),
				exitCode: 0,
			} as unknown as ReturnType<typeof Bun.spawnSync>);

			expect(upowerProp("TimeToEmpty")).toBe(7200);
		});

		test("returns null for non-positive or missing property output", () => {
			spawnSyncSpy = spyOn(Bun, "spawnSync").mockReturnValue({
				stdout: Buffer.from("x 0\n"),
				exitCode: 0,
			} as unknown as ReturnType<typeof Bun.spawnSync>);

			expect(upowerProp("TimeToEmpty")).toBeNull();
		});

		test("returns null when busctl returns non-zero exit code", () => {
			spawnSyncSpy = spyOn(Bun, "spawnSync").mockReturnValue({
				stdout: Buffer.from(""),
				exitCode: 1,
			} as unknown as ReturnType<typeof Bun.spawnSync>);

			expect(upowerProp("TimeToEmpty")).toBeNull();
		});

		test("returns null when busctl execution throws", () => {
			spawnSyncSpy = spyOn(Bun, "spawnSync").mockImplementation(() => {
				throw new Error("D-Bus inaccessible");
			});

			expect(upowerProp("TimeToEmpty")).toBeNull();
		});
	});
});
