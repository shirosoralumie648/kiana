export const marker = { loads: 0 };

globalThis.__counterDepLoads = (globalThis.__counterDepLoads ?? 0) + 1;

export const depModuleLoads = globalThis.__counterDepLoads;
