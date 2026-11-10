// Pure-JS values of the `@earendil-works/kiana-coding-agent` virtual namespace (design §8.6).
//
// `defineTool` is the identity function from types.ts; `serializeConversation`,
// `formatSize`, `truncateLine` and `withFileMutationQueue` are ports of
// `core/compaction/utils.ts`, `core/tools/truncate.ts` and
// `core/tools/file-mutation-queue.ts`. Every other runtime export is backed by the
// coding-agent bridge.

/** Identity helper: extensions use it purely for type inference. */
export function defineTool(definition) {
	return definition;
}

// ---------------------------------------------------------------------------
// core/tools/truncate.ts#formatSize / #truncateLine
// ---------------------------------------------------------------------------

const GREP_MAX_LINE_LENGTH = 500;

/** Format bytes as human-readable size (JS number formatting, like the TS template literals). */
export function formatSize(bytes) {
	if (bytes < 1024) {
		return `${bytes}B`;
	} else if (bytes < 1024 * 1024) {
		return `${(bytes / 1024).toFixed(1)}KB`;
	} else {
		return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
	}
}

/** Truncate a single line to max UTF-16 code units, adding the `[truncated]` suffix. */
export function truncateLine(line, maxChars = GREP_MAX_LINE_LENGTH) {
	if (line.length <= maxChars) {
		return { text: line, wasTruncated: false };
	}
	return { text: `${line.slice(0, maxChars)}... [truncated]`, wasTruncated: true };
}

// ---------------------------------------------------------------------------
// core/compaction/utils.ts#serializeConversation
// ---------------------------------------------------------------------------

const TOOL_RESULT_MAX_CHARS = 2000;

/** `contentText(content, "")` for the message shapes this module handles. */
function contentText(content, separator = "\n") {
	if (typeof content === "string") return content;
	return content
		.filter((block) => block.type === "text")
		.map((block) => block.text)
		.join(separator);
}

function truncateForSummary(text, maxChars) {
	if (text.length <= maxChars) return text;
	const truncatedChars = text.length - maxChars;
	return `${text.slice(0, maxChars)}\n\n[... ${truncatedChars} more characters truncated]`;
}

/** Serialize LLM messages to text for summarization. */
export function serializeConversation(messages) {
	const parts = [];

	for (const msg of messages) {
		if (msg.role === "user") {
			const content = contentText(msg.content, "");
			if (content) parts.push(`[User]: ${content}`);
		} else if (msg.role === "assistant") {
			const toolCalls = [];

			for (const block of msg.content) {
				if (block.type === "toolCall") {
					const args = block.arguments;
					const argsStr = Object.entries(args)
						.map(([k, v]) => `${k}=${JSON.stringify(v)}`)
						.join(", ");
					toolCalls.push(`${block.name}(${argsStr})`);
				}
			}

			if (msg.content.some((block) => block.type === "text")) {
				parts.push(`[Assistant]: ${contentText(msg.content)}`);
			}
			if (toolCalls.length > 0) {
				parts.push(`[Assistant tool calls]: ${toolCalls.join("; ")}`);
			}
		} else if (msg.role === "toolResult") {
			const content = contentText(msg.content, "");
			if (content) {
				parts.push(`[Tool result]: ${truncateForSummary(content, TOOL_RESULT_MAX_CHARS)}`);
			}
		}
	}

	return parts.join("\n\n");
}

// ---------------------------------------------------------------------------
// core/tools/file-mutation-queue.ts#withFileMutationQueue
// ---------------------------------------------------------------------------

const fileMutationQueues = new Map();
let registrationQueue = Promise.resolve();

function isMissingPathError(error) {
	return (
		typeof error === "object" &&
		error !== null &&
		"code" in error &&
		(error.code === "ENOENT" || error.code === "ENOTDIR")
	);
}

/**
 * Resolve a Node builtin at call time: the node-globals unit fills
 * `globalThis.__kiana_node_shims`; the CJS loader's `require` is the fallback.
 */
function nodeModule(name) {
	const shims = globalThis.__kiana_node_shims;
	if (shims !== null && shims !== undefined) {
		if (shims[name] !== undefined) return shims[name];
		const bare = name.slice(0, 5) === "node:" ? name.slice(5) : name;
		if (shims[bare] !== undefined) return shims[bare];
	}
	const makeRequire = globalThis.__kiana_make_require;
	if (typeof makeRequire === "function") {
		return makeRequire("<kiana-virtual>", "/")(name);
	}
	throw new Error(`kiana: node builtin ${name} is not available (install node-globals first)`);
}

async function getMutationQueueKey(filePath) {
	const path = nodeModule("node:path");
	const fs = nodeModule("node:fs/promises");
	const resolvedPath = path.resolve(filePath);
	try {
		return await fs.realpath(resolvedPath);
	} catch (error) {
		if (isMissingPathError(error)) return resolvedPath;
		throw error;
	}
}

/**
 * Serialize file mutation operations targeting the same file.
 * Operations for different files still run in parallel.
 */
export async function withFileMutationQueue(filePath, fn) {
	const registration = registrationQueue.then(async () => {
		const key = await getMutationQueueKey(filePath);
		const currentQueue = fileMutationQueues.get(key) ?? Promise.resolve();

		let releaseNext;
		const nextQueue = new Promise((resolveQueue) => {
			releaseNext = resolveQueue;
		});
		const chainedQueue = currentQueue.then(() => nextQueue);
		fileMutationQueues.set(key, chainedQueue);

		return { key, currentQueue, chainedQueue, releaseNext };
	});
	registrationQueue = registration.then(
		() => undefined,
		() => undefined,
	);

	const { key, currentQueue, chainedQueue, releaseNext } = await registration;
	await currentQueue;
	try {
		return await fn();
	} finally {
		releaseNext();
		if (fileMutationQueues.get(key) === chainedQueue) {
			fileMutationQueues.delete(key);
		}
	}
}
