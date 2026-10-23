import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "./bridge";
import { open, save } from "@tauri-apps/plugin-dialog";
import { readTextFile, writeTextFile } from "@tauri-apps/plugin-fs";
import { exportProxyFile, readProxyFile } from "./proxyData";
vi.mock("./bridge", () => ({ api: vi.fn(), preview: false }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(), save: vi.fn() }));
vi.mock("@tauri-apps/plugin-fs", () => ({
  readTextFile: vi.fn(),
  writeTextFile: vi.fn(),
}));
beforeEach(() => {
  vi.resetAllMocks();
});
describe("native proxy file handoff", () => {
  it("does not read credentials or write a file after save cancellation", async () => {
    vi.mocked(save).mockResolvedValue(null);
    expect(await exportProxyFile("/proxy-management/data?ids=7")).toBe(false);
    expect(api).not.toHaveBeenCalled();
    expect(writeTextFile).not.toHaveBeenCalled();
  });
  it("passes the selected Android content URI directly to the official filesystem", async () => {
    vi.mocked(open).mockResolvedValue("content://fixture/selected.json");
    vi.mocked(readTextFile).mockResolvedValue('{"proxies":[]}');
    expect(await readProxyFile()).toBe('{"proxies":[]}');
    expect(readTextFile).toHaveBeenCalledWith(
      "content://fixture/selected.json",
    );
  });
  it("exports only after selection and retains upstream JSON data", async () => {
    const data = {
      proxies: [{ host: "::1", password: "fixture" }],
      accounts: [],
    };
    vi.mocked(save).mockResolvedValue("content://fixture/export.json");
    vi.mocked(api).mockResolvedValue(data as never);
    expect(await exportProxyFile("/proxy-management/data?ids=7")).toBe(true);
    expect(api).toHaveBeenCalledWith("GET", "/proxy-management/data?ids=7");
    expect(writeTextFile).toHaveBeenCalledWith(
      "content://fixture/export.json",
      JSON.stringify(data, null, 2),
    );
  });
  it("discards an export if the connection changes before the response", async () => {
    let connected = true;
    vi.mocked(save).mockResolvedValue("content://fixture/export.json");
    vi.mocked(api).mockImplementation(async () => {
      connected = false;
      return { proxies: [] } as never;
    });
    expect(
      await exportProxyFile("/proxy-management/data", () => connected),
    ).toBe(false);
    expect(writeTextFile).not.toHaveBeenCalled();
  });
});
