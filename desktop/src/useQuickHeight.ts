import { useLayoutEffect, useRef } from "react";
import { command, preview } from "./bridge";

export function useQuickHeight(enabled: boolean, expanded: boolean) {
  const body = useRef<HTMLDivElement>(null);
  const last = useRef(0);
  const measure = useRef(() => {});
  useLayoutEffect(() => {
    if (!enabled || !body.current) return;
    const inner = body.current,
      content = inner.parentElement!,
      main = content.parentElement!;
    const resize = () => {
      const padding = getComputedStyle(content);
      const chrome = [...main.children]
        .filter((element) => element !== content)
        .reduce(
          (total, element) => total + element.getBoundingClientRect().height,
          0,
        );
      const natural =
        chrome +
        inner.getBoundingClientRect().height +
        parseFloat(padding.paddingTop) +
        parseFloat(padding.paddingBottom);
      const height = Math.min(
        520,
        Math.max(expanded ? 420 : 128, Math.ceil(natural)),
      );
      if (height === last.current) return;
      last.current = height;
      if (preview) main.parentElement!.style.height = `${height}px`;
      void command("resize_quick", { height }).catch(() => {
        last.current = 0;
      });
    };
    measure.current = resize;
    const observer = new ResizeObserver(resize);
    observer.observe(inner);
    for (const element of main.children)
      if (element !== content) observer.observe(element);
    resize();
    return () => observer.disconnect();
  }, [enabled, expanded]);
  useLayoutEffect(() => {
    measure.current();
  });
  return body;
}
