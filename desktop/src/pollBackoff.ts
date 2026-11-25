export const adaptivePollDelaysMs = [35, 80, 180, 350, 500] as const;

export function nextAdaptiveDelay(step: number): number {
  return adaptivePollDelaysMs[
    Math.min(Math.max(0, step), adaptivePollDelaysMs.length - 1)
  ];
}
