import { name } from "probe-pkg";

export const dependency: string = name;
export default function factory(): string {
	return dependency;
}
