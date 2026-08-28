import { describe, expect, spyOn, test } from "bun:test";
import { readSystemTemps, upowerProp } from "../../src/telemetry";

describe("readSystemTemps parser", () => {
	test("parses AMD k10temp, amdgpu, nvme, and GPU power correctly", () => {
		const mockSensorsJson = JSON.stringify({
			"k10temp-pci-00c3": {
				Adapter: "PCI adapter",
				Tctl: {
					temp1_input: 48.5,
					temp1_max: 95.0,
				},
			},
			"amdgpu-pci-0300": {
				Adapter: "PCI adapter",
				edge: {
					temp1_input: 42.0,
				},
				PPT: {
					power1_input: 15.345,
				},
			},
			"nvme-pci-0100": {
				Adapter: "PCI adapter",
				Composite: {
					temp1_input: 38.0,
				},
			},
		});

		const spawnSyncSpy = spyOn(Bun, "spawnSync").mockReturnValue({
			stdout: Buffer.from(mockSensorsJson),
			exitCode: 0,
		} as unknown as ReturnType<typeof Bun.spawnSync>);

		const result = readSystemTemps();
		expect(result).toEqual({
			cpu_c: 48.5,
			gpu_c: 42.0,
			nvme_c: 38.0,
			gpu_power_w: 15.35,
		});

		spawnSyncSpy.mockRestore();
	});

	test("parses Intel coretemp and i915 graphics correctly", () => {
		const mockSensorsJson = JSON.stringify({
			"coretemp-isa-0000": {
				Adapter: "ISA adapter",
				"Package id 0": {
					temp1_input: 54.0,
				},
			},
			"i915-pci-0002": {
				Adapter: "PCI adapter",
				temp1: {
					temp1_input: 47.0,
				},
			},
		});

		const spawnSyncSpy = spyOn(Bun, "spawnSync").mockReturnValue({
			stdout: Buffer.from(mockSensorsJson),
			exitCode: 0,
		} as unknown as ReturnType<typeof Bun.spawnSync>);

		const result = readSystemTemps();
		expect(result).toEqual({
			cpu_c: 54.0,
			gpu_c: 47.0,
			nvme_c: null,
			gpu_power_w: null,
		});

		spawnSyncSpy.mockRestore();
	});

	test("returns all nulls if sensors exitCode is non-zero or JSON is invalid", () => {
		const spawnSyncSpy = spyOn(Bun, "spawnSync").mockReturnValue({
			stdout: Buffer.from("invalid json"),
			exitCode: 1,
		} as unknown as ReturnType<typeof Bun.spawnSync>);

		expect(readSystemTemps()).toEqual({
			cpu_c: null,
			gpu_c: null,
			nvme_c: null,
			gpu_power_w: null,
		});

		spawnSyncSpy.mockRestore();
	});
});

describe("upowerProp parser", () => {
	test("parses busctl property output correctly", () => {
		const spawnSyncSpy = spyOn(Bun, "spawnSync").mockReturnValue({
			stdout: Buffer.from("x 7200\n"),
			exitCode: 0,
		} as unknown as ReturnType<typeof Bun.spawnSync>);

		expect(upowerProp("TimeToEmpty")).toBe(7200);

		spawnSyncSpy.mockRestore();
	});

	test("returns null for non-positive or missing property output", () => {
		const spawnSyncSpy = spyOn(Bun, "spawnSync").mockReturnValue({
			stdout: Buffer.from("x 0\n"),
			exitCode: 0,
		} as unknown as ReturnType<typeof Bun.spawnSync>);

		expect(upowerProp("TimeToEmpty")).toBeNull();

		spawnSyncSpy.mockRestore();
	});

	test("returns null when busctl returns non-zero exit code or throws", () => {
		const spawnSyncSpy = spyOn(Bun, "spawnSync").mockReturnValue({
			stdout: Buffer.from(""),
			exitCode: 1,
		} as unknown as ReturnType<typeof Bun.spawnSync>);

		expect(upowerProp("TimeToEmpty")).toBeNull();

		spawnSyncSpy.mockRestore();
	});
});
