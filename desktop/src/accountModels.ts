import { api } from "./bridge";
export type AccountModel = { id: string; display_name?: string; type?: string };
export function readAccountModels<T extends AccountModel = AccountModel>(id: number, refresh = false) {
  return api<T[]>("GET", `/accounts/${id}/models?purpose=model_test${refresh ? "&refresh=true" : ""}`);
}
export const modelDirectoryRevoked = (error: unknown) => /http_(401|403)\b/.test(String(error));
