export type SdkMode = "http" | "mock";

export interface LulaConfig {
  baseUrl: string;
  mode: SdkMode;
  token?: string;
}

const defaultConfig: LulaConfig = {
  baseUrl: "",
  mode: "mock",
};

let currentConfig: LulaConfig = { ...defaultConfig };

export function configure(config: Partial<LulaConfig>): void {
  currentConfig = { ...currentConfig, ...config };
}

export function getConfig(): LulaConfig {
  return currentConfig;
}

export function resetConfig(): void {
  currentConfig = { ...defaultConfig };
}
