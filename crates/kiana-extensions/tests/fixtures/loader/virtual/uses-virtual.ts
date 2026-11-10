import { Type } from "typebox";
import * as models from "@earendil-works/kiana-ai/models";

export const isObjectBuilder = typeof Type.Object;
export const modelsKey = models.key;
(globalThis as unknown as { __firstModels: unknown }).__firstModels = models;
export default modelsKey;
