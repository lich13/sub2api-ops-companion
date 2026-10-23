import { describe, expect, it, vi } from "vitest";
import {
  parseImport,
  parseProxyLines,
  proxyAddress,
  proxyUrl,
} from "./proxyData";
vi.mock("./bridge", () => ({ api: vi.fn(), preview: true }));
describe("proxy files and addresses", () => {
  it("parses IPv6, escaped credentials and duplicates without echoing invalid input", () => {
    const data = parseProxyLines(
      "socks5://fixture%40user:p%3Aa@[::1]:1080\nsocks5://fixture%40user:p%3Aa@[::1]:1080\nhttp://fixture-secret@bad\nhttps://127.0.0.1:65536",
    );
    expect(data.duplicates).toBe(1);
    expect(data.errors).toEqual([3, 4]);
    expect(data.proxies[0]).toMatchObject({
      host: "::1",
      username: "fixture@user",
      password: "p:a",
    });
    expect(proxyAddress(data.proxies[0])).toBe("[::1]:1080");
    expect(
      proxyUrl(
        {
          ...data.proxies[0],
          expires_at: null,
          id: 1,
          name: "fixture",
          status: "active",
          username_set: true,
          password_set: true,
        },
        { username: "fixture@user", password: "p:a" },
      ),
    ).toBe("socks5://fixture%40user:p%3Aa@[::1]:1080");
  });
  it("accepts upstream exports and rejects account imports and malformed JSON safely", () => {
    expect(
      parseImport('{"proxies":[{"host":"::1"}],"accounts":[]}').proxies,
    ).toHaveLength(1);
    expect(() => parseImport('{"proxies":[{}],"accounts":[{}]}')).toThrow(
      "仅支持代理数据",
    );
    expect(() => parseImport('{"password":"fixture-secret",')).toThrow(
      /^JSON 格式无效$/,
    );
    expect(() => parseImport('{"proxies":[]}')).toThrow("1–1000");
  });
});
