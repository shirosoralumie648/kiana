/**
 * Titlebar Spinner Extension
 *
 * Shows a braille spinner animation in the terminal title while the agent is working.
 * Uses `ctx.ui.setTitle()` to update the terminal title via the extension API.
 *
 * Usage:
 *   kiana --extension examples/extensions/titlebar-spinner.ts
 */

import path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/kiana-coding-agent";

const BRAILLE_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

function getBaseTitle(kiana: ExtensionAPI): string {
	const cwd = path.basename(process.cwd());
	const session = kiana.getSessionName();
	return session ? `Kiana - ${session} - ${cwd}` : `Kiana - ${cwd}`;
}

export default function (kiana: ExtensionAPI) {
	let timer: ReturnType<typeof setInterval> | null = null;
	let frameIndex = 0;

	function stopAnimation(ctx: ExtensionContext) {
		if (timer) {
			clearInterval(timer);
			timer = null;
		}
		frameIndex = 0;
		ctx.ui.setTitle(getBaseTitle(kiana));
	}

	function startAnimation(ctx: ExtensionContext) {
		stopAnimation(ctx);
		timer = setInterval(() => {
			const frame = BRAILLE_FRAMES[frameIndex % BRAILLE_FRAMES.length];
			const cwd = path.basename(process.cwd());
			const session = kiana.getSessionName();
			const title = session ? `${frame} Kiana - ${session} - ${cwd}` : `${frame} Kiana - ${cwd}`;
			ctx.ui.setTitle(title);
			frameIndex++;
		}, 80);
	}

	kiana.on("agent_start", async (_event, ctx) => {
		startAnimation(ctx);
	});

	kiana.on("agent_settled", async (_event, ctx) => {
		stopAnimation(ctx);
	});

	kiana.on("session_shutdown", async (_event, ctx) => {
		stopAnimation(ctx);
	});
}
