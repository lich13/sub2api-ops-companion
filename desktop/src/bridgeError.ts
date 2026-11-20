import type { BridgeFailure } from "./protocol";

export class BridgeError extends Error {
  constructor(readonly failure: BridgeFailure) {
    super(failure.message);
  }
  static changed() {
    return new BridgeError({
      code: "connection_changed",
      message: "连接已切换；请求保留在原连接",
    });
  }
}

export function bridgeFailure(error: unknown): BridgeFailure | undefined {
  if (error instanceof BridgeError) return error.failure;
  if (
    typeof error === "object" &&
    error !== null &&
    "code" in error &&
    typeof error.code === "string" &&
    "message" in error &&
    typeof error.message === "string"
  ) {
    return error as BridgeFailure;
  }
}

export function credentialsRequired(error: unknown) {
  const code = bridgeFailure(error)?.code;
  return code === "auth_rejected" || code === "missing_credentials";
}

export function ignoredFailure(error: unknown) {
  const code = bridgeFailure(error)?.code;
  return code === "connection_changed" || code === "background";
}
