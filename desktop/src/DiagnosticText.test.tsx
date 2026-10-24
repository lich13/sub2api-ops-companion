// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import DiagnosticText from "./DiagnosticText";
import { ProxyQualityReport } from "./ProxyDialogs";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
it("displays and copies complete plain text without interpreting markup", async () => {
  const copy = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText: copy },
    configurable: true,
  });
  const host = document.createElement("div"),
    root = createRoot(host);
  const text =
    "https://example.invalid/?token=fixture\n  password=fixture\n<script>fixture</script>\n" +
    "失败".repeat(400);
  await act(async () => root.render(<DiagnosticText text={text} />));
  expect(host.querySelector("span")?.textContent).toBe(text);
  expect(host.querySelector("script")).toBeNull();
  await act(async () => host.querySelector("button")!.click());
  expect(copy).toHaveBeenCalledWith(text);
  await act(async () => root.unmount());
});
it("keeps each quality report failure complete and copyable", async () => {
  const copy = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText: copy },
    configurable: true,
  });
  const host = document.createElement("div"),
    root = createRoot(host);
  document.body.append(host);
  const message =
    "HTTP 403\n  https://example.invalid/?token=fixture\n" +
    "详情".repeat(1000);
  await act(async () =>
    root.render(
      <ProxyQualityReport
        name="fixture"
        close={() => {}}
        result={{
          score: 0,
          grade: "F",
          summary: "fixture failure",
          exit_ip: "",
          country: "",
          base_latency_ms: null,
          items: [
            {
              target: "https://example.invalid",
              status: "fail",
              http_status: 403,
              latency_ms: 1,
              message,
            },
          ],
        }}
      />,
    ),
  );
  const detail = host.querySelector("tbody td:last-child")!;
  expect(detail.querySelector("span")?.textContent).toBe(message);
  await act(async () => detail.querySelector("button")!.click());
  expect(copy).toHaveBeenCalledWith(message);
  await act(async () => root.unmount());
  host.remove();
});
