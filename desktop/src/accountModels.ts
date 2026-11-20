import { bridgeFailure } from "./bridgeError";
import type { ModelOption } from "./protocol";
import { api } from "./bridge";
export type AccountModel = ModelOption & { type?: string };
export function readAccountModels<T extends AccountModel = AccountModel>(
  id: number,
  refresh = false,
) {
  return api<T[]>(
    "GET",
    `/accounts/${id}/models?purpose=model_test${refresh ? "&refresh=true" : ""}`,
  );
}
export function modelDirectoryRevoked(error: unknown) {
  const code = bridgeFailure(error)?.code;
  return code === "auth_rejected" || code === "model_directory_revoked";
}
