import { value } from "./dep.ts";

export const doubled: number = value * 2;
export const url = import.meta.url;
export default function factory(_kiana: unknown): number {
	return doubled;
}
