import * as models from "@earendil-works/kiana-ai/models";

(globalThis as unknown as { __secondModels: unknown }).__secondModels = models;
export const key: string = models.key;
