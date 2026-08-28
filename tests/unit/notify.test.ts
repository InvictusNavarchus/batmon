import { afterEach, beforeEach, describe, expect, spyOn, test } from "bun:test";
import { notify } from "../../src/alerts";

describe("notify", () => {
	let spawnSpy: ReturnType<typeof spyOn>;
	let consoleSpy: ReturnType<typeof spyOn>;

	beforeEach(() => {
		spawnSpy = spyOn(Bun, "spawn").mockImplementation(() => {
			return {
				exited: Promise.resolve(0),
			} as unknown as ReturnType<typeof Bun.spawn>;
		});
		consoleSpy = spyOn(console, "error").mockImplementation(() => {});
	});

	afterEach(() => {
		spawnSpy.mockRestore();
		consoleSpy.mockRestore();
	});

	test("spawns notify-send with expected arguments and logs to console.error", () => {
		notify({
			title: "Low Battery",
			body: "15% remaining",
			urgency: "critical",
			icon: "battery-caution",
		});

		expect(spawnSpy).toHaveBeenCalledTimes(1);
		const calledArgs = spawnSpy.mock.calls[0][0];
		expect(calledArgs).toEqual([
			"notify-send",
			"-a",
			"batmon",
			"-c",
			"device",
			"-u",
			"critical",
			"-i",
			"battery-caution",
			"Low Battery",
			"15% remaining",
		]);

		expect(consoleSpy).toHaveBeenCalledWith(
			"[batmon] [CRITICAL] Low Battery: 15% remaining",
		);
	});

	test("uses default values for urgency and icon when omitted", () => {
		notify({
			title: "Charge Notice",
			body: "Plugged in",
		});

		const calledArgs = spawnSpy.mock.calls[0][0];
		expect(calledArgs).toEqual([
			"notify-send",
			"-a",
			"batmon",
			"-c",
			"device",
			"-u",
			"normal",
			"-i",
			"battery",
			"Charge Notice",
			"Plugged in",
		]);

		expect(consoleSpy).toHaveBeenCalledWith(
			"[batmon] [NORMAL] Charge Notice: Plugged in",
		);
	});
});
