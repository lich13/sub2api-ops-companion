import { bridgeFailure } from "./bridgeError";
import type { ModelOption } from "./protocol";
import { api } from "./bridge";
export type AccountModel = ModelOption & { type?: string };

const MODEL_CACHE_LIMIT = 64;
let activeConnectionKey = "";
const modelDirectoryCache = new Map<string, AccountModel[]>();

export function bindAccountModelsConnection(connectionKey: string) {
  if (activeConnectionKey !== connectionKey) {
    activeConnectionKey = connectionKey;
    modelDirectoryCache.clear();
  }
  return () => {
    if (activeConnectionKey === connectionKey) {
      activeConnectionKey = "";
      modelDirectoryCache.clear();
    }
  };
}

export async function readAccountModels<T extends AccountModel = AccountModel>(
  id: number,
  refresh = false,
  connectionKey = activeConnectionKey,
): Promise<T[]> {
  const cacheKey = connectionKey ? `${connectionKey}:${id}` : "";
  if (!refresh && cacheKey) {
    const cached = modelDirectoryCache.get(cacheKey);
    if (cached) return cached as T[];
  }
  try {
    const list = await api<T[]>(
      "GET",
      `/accounts/${id}/models?purpose=model_test${refresh ? "&refresh=true" : ""}`,
    );
    if (cacheKey) {
      if (modelDirectoryCache.size >= MODEL_CACHE_LIMIT) {
        const oldest = modelDirectoryCache.keys().next().value;
        if (oldest !== undefined) modelDirectoryCache.delete(oldest);
      }
      modelDirectoryCache.set(cacheKey, list);
    }
    return list;
  } catch (error) {
    if (cacheKey && modelDirectoryRevoked(error)) {
      modelDirectoryCache.delete(cacheKey);
    }
    throw error;
  }
}
export function modelDirectoryRevoked(error: unknown) {
  const code = bridgeFailure(error)?.code;
  return code === "auth_rejected" || code === "model_directory_revoked";
}
